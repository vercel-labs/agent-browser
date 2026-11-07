//! Firefox DevTools (remote debugging) protocol client: `<len>:<json>` frames.

use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{
    split, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::sync::{broadcast, oneshot};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

type Pending = Arc<Mutex<HashMap<String, VecDeque<oneshot::Sender<Value>>>>>;

/** A connected DevTools protocol session; clones share the same connection. */
#[derive(Clone)]
pub struct RdpClient {
    writer: Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>,
    pending: Pending,
    events: broadcast::Sender<Value>,
    greeting: Value,
    reader: Arc<AbortOnDrop>,
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut BufReader<R>) -> Option<Value> {
    let mut len = Vec::new();
    r.read_until(b':', &mut len).await.ok()?;
    len.pop()?;
    let n: usize = std::str::from_utf8(&len).ok()?.parse().ok()?;
    let mut buf = vec![0; n];
    r.read_exact(&mut buf).await.ok()?;
    serde_json::from_slice(&buf).ok()
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

    /// Connects to a DevTools server on a loopback TCP port.
    pub async fn connect_tcp(port: u16) -> Result<Self, String> {
        let s = tokio::net::TcpStream::connect(("127.0.0.1", port))
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
        let greeting = read_frame(&mut rd)
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
                    p.lock().unwrap().get_mut(&from).and_then(|q| q.pop_front())
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
            events,
            greeting,
            reader: Arc::new(AbortOnDrop(reader)),
        })
    }

    /// Returns the server greeting message.
    pub fn greeting(&self) -> &Value {
        &self.greeting
    }

    /// Subscribes to unsolicited actor events.
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    /// Sends a request to an actor and returns its next reply; an `error` reply becomes `Err`.
    pub async fn request(&self, to: &str, kind: &str, extra: Value) -> Result<Value, String> {
        self.request_timeout(to, kind, extra, DEFAULT_TIMEOUT).await
    }

    async fn request_timeout(
        &self,
        to: &str,
        kind: &str,
        extra: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let mut body = json!({"to": to, "type": kind});
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        let body = body.to_string();
        let (tx, rx) = oneshot::channel();
        {
            let mut w = self.writer.lock().await;
            self.pending
                .lock()
                .unwrap()
                .entry(to.to_string())
                .or_default()
                .push_back(tx);
            if let Err(e) = w
                .write_all(format!("{}:{}", body.len(), body).as_bytes())
                .await
            {
                if let Some(q) = self.pending.lock().unwrap().get_mut(to) {
                    q.pop_back();
                }
                return Err(format!("DevTools send failed: {e}"));
            }
        }
        let reply = tokio::time::timeout(timeout, rx).await;
        match reply {
            Ok(Ok(reply)) => match reply.get("error") {
                Some(e) => Err(format!(
                    "{}: {}",
                    e.as_str().unwrap_or("error"),
                    reply["message"].as_str().unwrap_or("")
                )),
                None => Ok(reply),
            },
            Ok(Err(_)) => Err("DevTools connection closed".to_string()),
            Err(_) => Err(format!("DevTools request {kind} to {to} timed out")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

    async fn put(s: &mut DuplexStream, v: Value) {
        let b = v.to_string();
        s.write_all(format!("{}:{}", b.len(), b).as_bytes())
            .await
            .unwrap();
    }

    async fn get(s: &mut DuplexStream) -> Value {
        let mut len = Vec::new();
        loop {
            let b = s.read_u8().await.unwrap();
            if b == b':' {
                break;
            }
            len.push(b);
        }
        let n: usize = String::from_utf8(len).unwrap().parse().unwrap();
        let mut buf = vec![0; n];
        s.read_exact(&mut buf).await.unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    async fn pair() -> (RdpClient, DuplexStream) {
        let (a, mut b) = tokio::io::duplex(65536);
        put(&mut b, json!({"from":"root","applicationType":"browser"})).await;
        let c = RdpClient::from_stream(a).await.unwrap();
        (c, b)
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
        let c2 = c.clone();
        let r2 = tokio::spawn(async move { c2.request("act1", "second", json!({})).await });
        assert_eq!(get(&mut s).await["type"], "second");
        put(&mut s, json!({"from":"act1","n":"late"})).await;
        put(&mut s, json!({"from":"act1","n":"second"})).await;
        assert_eq!(r2.await.unwrap().unwrap()["n"], "second");
    }
}
