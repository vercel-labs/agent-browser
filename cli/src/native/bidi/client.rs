use super::types::{LocalValue, RemoteValue};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

const BUFFERED_EVENT_CAPACITY: usize = 65_536;
const BUFFERED_EVENT_BYTES: [usize; 2] = [64 << 20, 8 << 20];

/// The sandbox every script that must not see page patches runs in.
pub(crate) const SANDBOX: &str = "agent-browser";

#[derive(Debug)]
pub(crate) struct BidiError {
    pub code: Option<String>,
    pub message: String,
}

impl From<String> for BidiError {
    fn from(message: String) -> Self {
        Self {
            code: None,
            message,
        }
    }
}

impl fmt::Display for BidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{code}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, BidiError>>>>>;

struct PendingCommand<'a> {
    pending: &'a Pending,
    id: u64,
}

impl Drop for PendingCommand<'_> {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}

type EventPredicate = fn(&Value) -> bool;

struct Subscriber {
    tx: mpsc::Sender<(Value, usize)>,
    queued_bytes: Arc<[AtomicUsize; 2]>,
    lost: Arc<AtomicBool>,
    predicate: Option<EventPredicate>,
}

/// A subscriber's event queue. Reading an event frees its share of the queue's byte budget.
pub struct Events {
    rx: mpsc::Receiver<(Value, usize)>,
    queued_bytes: Arc<[AtomicUsize; 2]>,
    lost: Arc<AtomicBool>,
}

impl Events {
    pub(crate) async fn recv(&mut self) -> Option<Value> {
        self.rx.recv().await.map(|queued| self.release(queued))
    }

    pub(crate) fn try_recv(&mut self) -> Result<Value, mpsc::error::TryRecvError> {
        self.rx.try_recv().map(|queued| self.release(queued))
    }

    pub(crate) fn has_lost_events(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    fn release(&self, (event, size): (Value, usize)) -> Value {
        self.queued_bytes[event_class(&event)].fetch_sub(size, Ordering::Relaxed);
        event
    }
}

fn event_class(event: &Value) -> usize {
    usize::from(event["method"] == "log.entryAdded")
}
type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

/// A connected BiDi session socket; the reader task stops when the client is dropped.
pub struct BidiClient {
    sink: tokio::sync::Mutex<WsSink>,
    pending: Pending,
    buffered_events: Arc<Mutex<Vec<Subscriber>>>,
    pub(crate) lost_events: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
    next_id: AtomicU64,
    pub(crate) reader: tokio::task::JoinHandle<()>,
}

impl Drop for BidiClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl BidiClient {
    /// Connects to a BiDi WebSocket URL and starts the reader task.
    pub async fn connect(url: &str) -> Result<Self, String> {
        let config = WebSocketConfig {
            max_message_size: None,
            max_frame_size: None,
            ..Default::default()
        };
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), false)
            .await
            .map_err(|e| format!("BiDi connect failed: {e}"))?;
        let (sink, mut stream) = ws.split();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let buffered_events = Arc::new(Mutex::new(Vec::<Subscriber>::new()));
        let lost_events = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let (p, subscribers, lost, reader_closed) = (
            pending.clone(),
            buffered_events.clone(),
            lost_events.clone(),
            closed.clone(),
        );
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match v["type"].as_str() {
                    Some("event") => {
                        let size = text.len();
                        let class = event_class(&v);
                        subscribers.lock().unwrap().retain(|subscriber| {
                            if subscriber.tx.is_closed() {
                                return false;
                            }
                            if subscriber.predicate.is_some_and(|matches| !matches(&v)) {
                                return true;
                            }
                            let queued = &subscriber.queued_bytes[class];
                            let fits = queued.fetch_add(size, Ordering::Relaxed) + size
                                <= BUFFERED_EVENT_BYTES[class];
                            if fits && subscriber.tx.try_send((v.clone(), size)).is_ok() {
                                return true;
                            }
                            queued.fetch_sub(size, Ordering::Relaxed);
                            if subscriber.tx.is_closed() {
                                return false;
                            }
                            if class == 0 {
                                lost.store(true, Ordering::Release);
                                subscriber.lost.store(true, Ordering::Release);
                            }
                            class != 0 || subscriber.predicate.is_none()
                        });
                    }
                    Some(kind @ ("success" | "error")) => {
                        let Some(tx) = v["id"]
                            .as_u64()
                            .and_then(|id| p.lock().unwrap().remove(&id))
                        else {
                            continue;
                        };
                        let _ = tx.send(if kind == "success" {
                            Ok(v["result"].clone())
                        } else {
                            let code = v["error"].as_str().map(str::to_string);
                            let message = v["message"].as_str().unwrap_or("");
                            Err(BidiError {
                                message: if code.is_some() {
                                    message.to_string()
                                } else {
                                    format!("unknown error: {message}")
                                },
                                code,
                            })
                        });
                    }
                    _ => {}
                }
            }
            reader_closed.store(true, Ordering::Release);
            p.lock().unwrap().clear();
            subscribers.lock().unwrap().clear();
        });
        Ok(Self {
            sink: tokio::sync::Mutex::new(sink),
            pending,
            buffered_events,
            lost_events,
            closed,
            next_id: AtomicU64::new(1),
            reader,
        })
    }

    pub fn take_event_loss(&self) -> bool {
        self.lost_events.swap(false, Ordering::AcqRel)
    }

    #[allow(dead_code)]
    pub(crate) fn subscribe_buffered(&self) -> Events {
        self.subscribe_events(None)
    }

    /// Subscribes to matching events. A non-log overflow closes the subscription.
    pub(crate) fn subscribe_matching(&self, predicate: fn(&Value) -> bool) -> Events {
        self.subscribe_events(Some(predicate))
    }

    fn subscribe_events(&self, predicate: Option<EventPredicate>) -> Events {
        let (tx, rx) = mpsc::channel(BUFFERED_EVENT_CAPACITY);
        let queued_bytes = Arc::<[AtomicUsize; 2]>::default();
        let lost = Arc::new(AtomicBool::new(false));
        let mut subscribers = self.buffered_events.lock().unwrap();
        if !self.closed.load(Ordering::Acquire) {
            subscribers.push(Subscriber {
                tx,
                queued_bytes: queued_bytes.clone(),
                lost: lost.clone(),
                predicate,
            });
        }
        Events {
            rx,
            queued_bytes,
            lost,
        }
    }

    /// True once the socket reader has stopped, so no command can be answered.
    pub fn is_closed(&self) -> bool {
        self.reader.is_finished()
    }

    /// Sends a command with the default 30s timeout and returns its `result`.
    pub async fn send(&self, method: &str, params: Value) -> Result<Value, String> {
        self.send_raw(method, params)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn send_raw(&self, method: &str, params: Value) -> Result<Value, BidiError> {
        self.send_with_deadline(method, params, Some(DEFAULT_TIMEOUT))
            .await
    }

    /// Sends a command and fails with a timeout error if no reply arrives in time.
    pub async fn send_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.send_with_deadline(method, params, Some(timeout))
            .await
            .map_err(|e| e.to_string())
    }

    /// Sends a command and waits for its reply, leaving the deadline to the caller.
    /// Use it when an enclosing timeout already bounds the command, so the client's
    /// own default cannot cut the wait short.
    pub async fn send_without_deadline(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        self.send_without_deadline_raw(method, params)
            .await
            .map_err(|e| e.to_string())
    }

    pub(crate) async fn send_without_deadline_raw(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, BidiError> {
        self.send_with_deadline(method, params, None).await
    }

    async fn send_with_deadline(
        &self,
        method: &str,
        params: Value,
        deadline: Option<Duration>,
    ) -> Result<Value, BidiError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _command = PendingCommand {
            pending: &self.pending,
            id,
        };
        if self.closed.load(Ordering::Acquire) {
            return Err("BiDi connection closed".to_string().into());
        }
        let body = json!({"id": id, "method": method, "params": params}).to_string();
        self.sink
            .lock()
            .await
            .send(Message::Text(body))
            .await
            .map_err(|e| format!("BiDi send failed: {e}"))?;
        let reply = async {
            rx.await
                .map_err(|_| BidiError::from("BiDi connection closed".to_string()))?
        };
        match deadline {
            Some(deadline) => tokio::time::timeout(deadline, reply)
                .await
                .map_err(|_| format!("BiDi command {method} timed out"))?,
            None => reply.await,
        }
    }

    /// Sends a command without registering a response waiter.
    pub async fn send_no_wait(&self, method: &str, params: Value) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if self.reader.is_finished() {
            return Err("BiDi connection closed".to_string());
        }
        let body = json!({"id": id, "method": method, "params": params}).to_string();
        self.sink
            .lock()
            .await
            .send(Message::Text(body))
            .await
            .map_err(|e| format!("BiDi send failed: {e}"))
    }

    /// Runs `script.callFunction` in a browsing context and returns the decoded result.
    pub async fn call_function(
        &self,
        context: &str,
        js: &str,
        this: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        self.call_function_target(json!({"context": context}), js, this, args, DEFAULT_TIMEOUT)
            .await
    }

    /// Runs a function with a bounded timeout, including cleanup of a timed-out request.
    pub(crate) async fn call_function_timeout(
        &self,
        context: &str,
        js: &str,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.call_function_target(json!({"context": context}), js, None, vec![], timeout)
            .await
    }

    /// Runs a function in a named sandbox, isolated from the page's JavaScript globals.
    pub(crate) async fn call_function_in_sandbox(
        &self,
        context: &str,
        sandbox: &str,
        js: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        self.call_function_target(
            json!({"context": context, "sandbox": sandbox}),
            js,
            None,
            args,
            DEFAULT_TIMEOUT,
        )
        .await
    }

    /// Runs a function in a named sandbox with `this` set to a remote object reference.
    pub(crate) async fn call_function_on_in_sandbox(
        &self,
        context: &str,
        sandbox: &str,
        js: &str,
        this: Value,
    ) -> Result<Value, String> {
        self.call_function_target(
            json!({"context": context, "sandbox": sandbox}),
            js,
            Some(this),
            Vec::new(),
            DEFAULT_TIMEOUT,
        )
        .await
    }

    async fn call_function_target(
        &self,
        target: Value,
        js: &str,
        this: Option<Value>,
        args: Vec<Value>,
        timeout: Duration,
    ) -> Result<Value, String> {
        let mut params = json!({
            "functionDeclaration": js,
            "target": target,
            "arguments": args.iter().map(LocalValue::from).collect::<Vec<_>>(),
            "awaitPromise": true,
            "resultOwnership": "none",
        });
        if let Some(t) = this {
            params["this"] = t;
        }
        let r = self
            .send_timeout("script.callFunction", params, timeout)
            .await?;
        if r["type"] == "exception" {
            return Err(r["exceptionDetails"]["text"]
                .as_str()
                .unwrap_or("script exception")
                .to_string());
        }
        Ok(remote_to_json(&r["result"]))
    }
}

/// Decodes a BiDi RemoteValue to plain JSON; undecodable values become null.
pub fn remote_to_json(v: &Value) -> Value {
    serde_json::from_value::<RemoteValue>(v.clone())
        .map(Value::from)
        .unwrap_or(Value::Null)
}

/// Returns the JavaScript token for a value JSON cannot carry, such as `undefined`, `NaN` or `10n`.
pub fn unserializable(v: &Value) -> Option<String> {
    let val = &v["value"];
    match v["type"].as_str() {
        Some("undefined") => Some("undefined".to_string()),
        Some("bigint") => val.as_str().map(|n| format!("{n}n")),
        Some("number") => val.as_str().map(str::to_string),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    async fn mock() -> (String, TcpListener) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        (format!("ws://{}", l.local_addr().unwrap()), l)
    }

    #[tokio::test]
    async fn matching_id_resolves_and_error_surfaces() {
        let (url, l) = mock().await;
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            for i in 0..2 {
                let m: Value =
                    serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap())
                        .unwrap();
                let id = m["id"].clone();
                let reply = if i == 0 {
                    json!({"type":"success","id":id,"result":{"ok":true}})
                } else {
                    json!({"type":"error","id":id,"error":"no such frame","message":"gone"})
                };
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
        });
        let c = BidiClient::connect(&url).await.unwrap();
        assert_eq!(c.send("a.b", json!({})).await.unwrap(), json!({"ok":true}));
        let e = c.send("a.c", json!({})).await.unwrap_err();
        assert_eq!(e, "no such frame: gone");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn event_reaches_subscribers_and_does_not_resolve_command() {
        let (url, l) = mock().await;
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let m: Value =
                serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap())
                    .unwrap();
            let id = m["id"].clone();
            ws.send(Message::Text(
                json!({"type":"event","method":"x.y","params":{"id":id}}).to_string(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"type":"success","id":id,"result":{}}).to_string(),
            ))
            .await
            .unwrap();
        });
        let c = BidiClient::connect(&url).await.unwrap();
        let mut ev = c.subscribe_buffered();
        let r = c.send("a.b", json!({})).await.unwrap();
        assert_eq!(r, json!({}));
        assert_eq!(ev.recv().await.unwrap()["method"], "x.y");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_log_flood_is_dropped_without_losing_other_events() {
        let (url, l) = mock().await;
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let text = "x".repeat(3 << 20);
            let log = json!({"type":"event","method":"log.entryAdded","params":{"text":text}});
            for _ in 0..2 {
                let m: Value =
                    serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap())
                        .unwrap();
                for _ in 0..4 {
                    ws.send(Message::Text(log.to_string())).await.unwrap();
                }
                for reply in [
                    json!({"type":"event","method":"x.y","params":{}}),
                    json!({"type":"success","id":m["id"],"result":{}}),
                ] {
                    ws.send(Message::Text(reply.to_string())).await.unwrap();
                }
            }
        });
        let c = BidiClient::connect(&url).await.unwrap();
        let mut events = c.subscribe_buffered();
        for _ in 0..2 {
            c.send("a.b", json!({})).await.unwrap();
            let mut methods = Vec::new();
            while let Ok(event) = events.try_recv() {
                methods.push(event["method"].as_str().unwrap().to_string());
            }
            assert_eq!(methods, ["log.entryAdded", "log.entryAdded", "x.y"]);
        }
        assert!(!c.take_event_loss());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unanswered_command_times_out() {
        let (url, l) = mock().await;
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let _ = ws.next().await;
            tokio::time::sleep(Duration::from_millis(500)).await;
        });
        let c = BidiClient::connect(&url).await.unwrap();
        let e = c
            .send_timeout("a.b", json!({}), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(e.contains("timed out"), "{e}");
        server.abort();
    }

    #[tokio::test]
    async fn cancelling_a_sent_command_releases_its_pending_reply() {
        let (url, listener) = mock().await;
        let (received, confirmed) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = socket.next().await.unwrap().unwrap();
            received.send(()).unwrap();
            let _ = socket.next().await;
        });
        let client = BidiClient::connect(&url).await.unwrap();
        let mut command = Box::pin(client.send_without_deadline_raw("test.pending", json!({})));
        tokio::select! {
            result = &mut command => panic!("unexpected reply: {result:?}"),
            got = confirmed => got.unwrap(),
        }
        assert_eq!(client.pending.lock().unwrap().len(), 1);
        drop(command);
        assert!(client.pending.lock().unwrap().is_empty());
        server.abort();
    }

    #[test]
    fn plain_json_becomes_local_values() {
        let to_local = |v: &Value| serde_json::to_value(LocalValue::from(v)).unwrap();
        assert_eq!(to_local(&json!(3)), json!({"type": "number", "value": 3}));
        assert_eq!(
            to_local(&json!(["a"])),
            json!({"type": "array", "value": [{"type": "string", "value": "a"}]})
        );
        let node = json!({"sharedId": "n"});
        assert_eq!(to_local(&node), node);
    }

    #[test]
    fn remote_values_decode() {
        let node = json!({"type":"node","sharedId":"n1","value":{"nodeType":1}});
        assert_eq!(remote_to_json(&node), json!({"sharedId":"n1"}));
        let win = json!({"type":"window","value":{"context":"ctx-9"}});
        assert_eq!(remote_to_json(&win), json!("ctx-9"));
        let nested = json!({"type":"array","value":[
            {"type":"number","value":1},
            {"type":"object","value":[["k",{"type":"string","value":"v"}],
                ["a",{"type":"array","value":[{"type":"null"}]}]]}
        ]});
        assert_eq!(remote_to_json(&nested), json!([1, {"k":"v","a":[null]}]));

        let non_json = json!({"type":"array","value":[
            {"type":"number","value":"NaN"},
            {"type":"number","value":"Infinity"},
            {"type":"number","value":"-Infinity"},
            {"type":"number","value":"-0"},
            {"type":"bigint","value":"10"},
            {"type":"object","value":[["nested",{"type":"bigint","value":"20"}]]}
        ]});
        assert_eq!(
            remote_to_json(&non_json),
            json!([null, null, null, null, null, {"nested": null}])
        );
    }
}
