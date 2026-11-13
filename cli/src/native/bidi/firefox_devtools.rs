use super::network::AbortOnDrop;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{
    split, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{broadcast, oneshot};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FRAME_SIZE: usize = 256 * 1024 * 1024;

type Pending = Arc<Mutex<HashMap<String, VecDeque<(u64, oneshot::Sender<Value>)>>>>;

/// A tab from DevTools `listTabs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tab {
    pub actor: String,
    pub selected: bool,
    /// False for a tab Firefox has not loaded, which has no page and no BiDi context.
    pub loaded: bool,
}

const ACTOR_GONE_CODES: [&str; 2] = ["noSuchActor", "tabDestroyed"];

/// The text `request` returns as the start of an error reply with `code`.
fn code_prefix(code: &str) -> String {
    format!("{code}: ")
}

/// Whether a request failed because its actor is gone, as when a tab closes or unloads mid-request.
pub(crate) fn actor_is_gone(error: &str) -> bool {
    ACTOR_GONE_CODES
        .iter()
        .any(|code| error.starts_with(&code_prefix(code)))
}

/// A connected DevTools protocol session; clones share the same connection.
#[derive(Clone)]
pub struct RdpClient {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    pending: Pending,
    next_request: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    events: broadcast::Sender<Value>,
    _reader: Arc<AbortOnDrop>,
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Option<Value> {
    let mut length = Vec::new();
    reader.take(21).read_until(b':', &mut length).await.ok()?;
    let length = length.strip_suffix(b":")?;
    let frame_size: usize = std::str::from_utf8(length).ok()?.parse().ok()?;
    if frame_size > MAX_FRAME_SIZE {
        return None;
    }
    let mut buffer = vec![0; frame_size];
    reader.read_exact(&mut buffer).await.ok()?;
    serde_json::from_slice(&buffer).ok()
}

impl RdpClient {
    /// Connects to a DevTools server on a unix socket.
    #[cfg(unix)]
    pub async fn connect_unix(path: &std::path::Path) -> Result<Self, String> {
        let s = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| format!("DevTools connect failed: {e}"))?;
        Self::from_stream(s).await
    }

    /// Connects to a DevTools server on a TCP endpoint.
    pub async fn connect_tcp(host: &str, port: u16) -> Result<Self, String> {
        let s = tokio::net::TcpStream::connect((host, port))
            .await
            .map_err(|e| format!("DevTools connect failed: {e}"))?;
        Self::from_stream(s).await
    }

    /// Reads the greeting from an open stream and starts the reader task.
    pub async fn from_stream<S>(stream: S) -> Result<Self, String>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (rd, wr) = split(stream);
        let mut rd = BufReader::new(rd);
        read_frame(&mut rd)
            .await
            .ok_or("DevTools greeting not received")?;
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (events, _) = broadcast::channel(1024);
        let (p, ev) = (pending.clone(), events.clone());
        let reader = tokio::spawn(async move {
            while let Some(msg) = read_frame(&mut rd).await {
                let is_event = msg.get("type").is_some() && msg.get("error").is_none();
                let from = msg["from"].as_str().unwrap_or("").to_string();
                let waiter = if is_event {
                    None
                } else {
                    p.lock()
                        .unwrap()
                        .get_mut(&from)
                        .and_then(|q| q.pop_front())
                        .map(|(_, sender)| sender)
                };
                match waiter {
                    Some(tx) => {
                        let _ = tx.send(msg);
                    }
                    None => {
                        let _ = ev.send(msg);
                    }
                }
            }
            p.lock().unwrap().clear();
        });
        Ok(Self {
            writer: Arc::new(tokio::sync::Mutex::new(Box::new(wr))),
            pending,
            next_request: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(AtomicBool::new(false)),
            events,
            _reader: Arc::new(AbortOnDrop(reader)),
        })
    }

    /// Subscribes to unsolicited actor events.
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// Sends a request to an actor and returns its next reply; an `error` reply becomes `Err`.
    pub async fn request(&self, to: &str, kind: &str, extra: Value) -> Result<Value, String> {
        self.request_timeout(to, kind, extra, DEFAULT_TIMEOUT).await
    }

    /// Lists the browser's tabs, marking the ones Firefox has not loaded.
    pub(crate) async fn tabs(&self) -> Result<Vec<Tab>, String> {
        let reply = self.request("root", "listTabs", json!({})).await?;
        let tabs = reply["tabs"]
            .as_array()
            .ok_or("Firefox DevTools reported no tabs")?;
        Ok(tabs
            .iter()
            .filter_map(|tab| {
                Some(Tab {
                    actor: tab["actor"].as_str()?.to_string(),
                    selected: tab["selected"].as_bool() == Some(true),
                    loaded: tab["isZombieTab"] != true,
                })
            })
            .collect())
    }

    async fn request_timeout(
        &self,
        to: &str,
        kind: &str,
        extra: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        if self.failed.load(Ordering::Acquire) {
            return Err("DevTools connection is unusable after a request timeout".to_string());
        }
        let mut body = json!({"to": to, "type": kind});
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let body = body.to_string();
        let (tx, rx) = oneshot::channel();
        let request_id = self.next_request.fetch_add(1, Ordering::Relaxed);
        {
            let mut w = self.writer.lock().await;
            self.pending
                .lock()
                .unwrap()
                .entry(to.to_string())
                .or_default()
                .push_back((request_id, tx));
            if let Err(e) = w
                .write_all(format!("{}:{}", body.len(), body).as_bytes())
                .await
            {
                if let Some(q) = self.pending.lock().unwrap().get_mut(to) {
                    q.retain(|(id, _)| *id != request_id);
                }
                return Err(format!("DevTools send failed: {e}"));
            }
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => match reply.get("error") {
                Some(e) => Err(format!(
                    "{}{}",
                    code_prefix(e.as_str().unwrap_or("error")),
                    reply["message"].as_str().unwrap_or("")
                )),
                None => Ok(reply),
            },
            Ok(Err(_)) => Err("DevTools connection closed".to_string()),
            Err(_) => {
                self.failed.store(true, Ordering::Release);
                self.pending.lock().unwrap().clear();
                Err(format!("DevTools request {kind} to {to} timed out"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncWriteExt, DuplexStream};

    async fn put(s: &mut DuplexStream, v: Value) {
        let b = v.to_string();
        s.write_all(format!("{}:{}", b.len(), b).as_bytes())
            .await
            .unwrap();
    }

    async fn get(s: &mut DuplexStream) -> Value {
        read_frame(&mut BufReader::with_capacity(1, s))
            .await
            .unwrap()
    }

    async fn pair() -> (RdpClient, DuplexStream) {
        let (a, mut b) = tokio::io::duplex(65536);
        put(&mut b, json!({"from":"root","applicationType":"browser"})).await;
        let c = RdpClient::from_stream(a).await.unwrap();
        (c, b)
    }

    #[tokio::test]
    async fn actor_gone_errors_are_recognized_from_the_reply_code() {
        let (c, mut s) = pair().await;
        for (code, gone) in [
            ("tabDestroyed", true),
            ("noSuchActor", true),
            ("unknownError", false),
        ] {
            let client = c.clone();
            let call =
                tokio::spawn(async move { client.request("root", "getTarget", json!({})).await });
            assert_eq!(get(&mut s).await["type"], "getTarget");
            put(
                &mut s,
                json!({"from": "root", "error": code, "message": "nope"}),
            )
            .await;
            let error = call.await.unwrap().unwrap_err();
            assert_eq!(actor_is_gone(&error), gone, "{error}");
        }
    }

    #[tokio::test]
    async fn tabs_report_loaded_and_selected_state() {
        let (c, mut s) = pair().await;
        let call = tokio::spawn(async move { c.tabs().await });
        let request = get(&mut s).await;
        assert_eq!(request["to"], "root");
        assert_eq!(request["type"], "listTabs");
        put(
            &mut s,
            json!({"from": "root", "tabs": [
                {"actor": "tab-unloaded", "selected": false, "isZombieTab": true},
                {"actor": "tab-other", "selected": false},
                {"actor": "tab-selected", "selected": true, "isZombieTab": false},
                {"selected": true},
            ]}),
        )
        .await;

        assert_eq!(
            call.await.unwrap().unwrap(),
            vec![
                Tab {
                    actor: "tab-unloaded".into(),
                    selected: false,
                    loaded: false,
                },
                Tab {
                    actor: "tab-other".into(),
                    selected: false,
                    loaded: true,
                },
                Tab {
                    actor: "tab-selected".into(),
                    selected: true,
                    loaded: true,
                },
            ]
        );
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_reading_payload() {
        for prefix in [
            b"268435457:".as_slice(),
            b"00000000000000000000000000000000",
        ] {
            let (mut writer, reader) = tokio::io::duplex(64);
            writer.write_all(prefix).await.unwrap();
            let mut reader = BufReader::new(reader);
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(1), read_frame(&mut reader))
                    .await
                    .unwrap(),
                None
            );
        }
    }

    #[tokio::test]
    async fn same_actor_requests_resolve_in_order_with_event_between() {
        let (c, mut s) = pair().await;
        let c1 = c.clone();
        let c2 = c.clone();
        let mut ev = c.subscribe();
        let r1 = tokio::spawn(async move { c1.request("act1", "first", json!({})).await });
        assert_eq!(get(&mut s).await["type"], "first");
        let r2 = tokio::spawn(async move { c2.request("act1", "second", json!({})).await });
        assert_eq!(get(&mut s).await["type"], "second");
        put(&mut s, json!({"from":"act1","n":1})).await;
        put(&mut s, json!({"from":"act1","type":"tick"})).await;
        put(&mut s, json!({"from":"act1","n":2})).await;
        assert_eq!(r1.await.unwrap().unwrap()["n"], 1);
        assert_eq!(r2.await.unwrap().unwrap()["n"], 2);
        assert_eq!(ev.recv().await.unwrap()["type"], "tick");
    }

    #[tokio::test]
    async fn timed_out_request_does_not_consume_the_next_reply() {
        let (c, mut s) = pair().await;
        let first = c.clone();
        let first = tokio::spawn(async move {
            first
                .request_timeout("act1", "first", json!({}), Duration::from_millis(20))
                .await
        });
        assert_eq!(get(&mut s).await["type"], "first");
        assert!(first.await.unwrap().unwrap_err().contains("timed out"));

        let error = c.request("act1", "second", json!({})).await.unwrap_err();
        assert!(
            error.contains("unusable after a request timeout"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn event_before_enable_reply_does_not_consume_request() {
        let (c, mut s) = pair().await;
        let mut ev = c.subscribe();
        let c1 = c.clone();
        let r = tokio::spawn(async move { c1.request("acc", "enable", json!({})).await });
        assert_eq!(get(&mut s).await["type"], "enable");
        put(
            &mut s,
            json!({"from":"acc","type":"canBeDisabledChange","canBeDisabled":false}),
        )
        .await;
        put(&mut s, json!({"from":"acc","enabled":true})).await;
        assert_eq!(r.await.unwrap().unwrap()["enabled"], true);
        assert_eq!(ev.recv().await.unwrap()["type"], "canBeDisabledChange");
    }

    #[tokio::test]
    async fn unanswered_request_times_out() {
        let (c, mut s) = pair().await;
        let e = c
            .request_timeout("act1", "enable", json!({}), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        assert!(e.contains("enable"), "{e}");
        assert!(e.contains("act1"), "{e}");
        assert_eq!(get(&mut s).await["type"], "enable");
    }

    #[tokio::test]
    async fn late_reply_after_timeout_does_not_resolve_next_request() {
        let (c, mut s) = pair().await;
        c.request_timeout("act1", "first", json!({}), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(get(&mut s).await["type"], "first");
        let error = c.request("act1", "second", json!({})).await.unwrap_err();
        assert!(
            error.contains("unusable after a request timeout"),
            "{error}"
        );
    }
}
