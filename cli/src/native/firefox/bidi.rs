//! WebDriver BiDi client over a WebSocket.

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

#[derive(Clone, Copy)]
enum EventBuffer {
    Bounded,
    Lossless,
}

/// A subscriber's sending half: bounded by capacity and bytes, or lossless.
enum EventSender {
    Bounded(mpsc::Sender<(Value, usize)>),
    Lossless(mpsc::UnboundedSender<Value>),
}

impl EventSender {
    fn is_closed(&self) -> bool {
        match self {
            Self::Bounded(tx) => tx.is_closed(),
            Self::Lossless(tx) => tx.is_closed(),
        }
    }
}

enum EventReceiver {
    Bounded(mpsc::Receiver<(Value, usize)>),
    Lossless(mpsc::UnboundedReceiver<Value>),
}

struct Subscriber {
    tx: EventSender,
    queued_bytes: Arc<[AtomicUsize; 2]>,
    lost: Arc<AtomicBool>,
    predicate: Option<EventPredicate>,
}

pub(crate) struct Events {
    rx: EventReceiver,
    queued_bytes: Arc<[AtomicUsize; 2]>,
    lost: Arc<AtomicBool>,
}

impl Events {
    pub(crate) async fn recv(&mut self) -> Option<Value> {
        let queued = match &mut self.rx {
            EventReceiver::Bounded(rx) => rx.recv().await,
            EventReceiver::Lossless(rx) => return rx.recv().await,
        }?;
        Some(self.release(queued))
    }

    pub(crate) fn try_recv(&mut self) -> Result<Value, mpsc::error::TryRecvError> {
        let queued = match &mut self.rx {
            EventReceiver::Bounded(rx) => rx.try_recv(),
            EventReceiver::Lossless(rx) => return rx.try_recv(),
        }?;
        Ok(self.release(queued))
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

/** A connected BiDi session socket; the reader task stops when the client is dropped. */
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
                            match &subscriber.tx {
                                EventSender::Lossless(tx) => tx.send(v.clone()).is_ok(),
                                EventSender::Bounded(tx) => {
                                    let queued = &subscriber.queued_bytes[class];
                                    let fits = queued.fetch_add(size, Ordering::Relaxed) + size
                                        <= BUFFERED_EVENT_BYTES[class];
                                    if fits && tx.try_send((v.clone(), size)).is_ok() {
                                        return true;
                                    }
                                    queued.fetch_sub(size, Ordering::Relaxed);
                                    if tx.is_closed() {
                                        return false;
                                    }
                                    if class == 0 {
                                        lost.store(true, Ordering::Release);
                                        subscriber.lost.store(true, Ordering::Release);
                                    }
                                    class != 0 || subscriber.predicate.is_none()
                                }
                            }
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
        self.subscribe_events(None, EventBuffer::Bounded)
    }

    /// Subscribes to matching events. A non-log overflow closes the subscription.
    pub(crate) fn subscribe_matching(&self, predicate: fn(&Value) -> bool) -> Events {
        self.subscribe_events(Some(predicate), EventBuffer::Bounded)
    }

    /// Subscribes to matching events that are never dropped and never evict the subscriber.
    pub(crate) fn subscribe_matching_lossless(&self, predicate: EventPredicate) -> Events {
        self.subscribe_events(Some(predicate), EventBuffer::Lossless)
    }

    fn subscribe_events(&self, predicate: Option<EventPredicate>, buffer: EventBuffer) -> Events {
        let queued_bytes = Arc::<[AtomicUsize; 2]>::default();
        let lost = Arc::new(AtomicBool::new(false));
        let (tx, rx) = match buffer {
            EventBuffer::Bounded => {
                let (tx, rx) = mpsc::channel(BUFFERED_EVENT_CAPACITY);
                (EventSender::Bounded(tx), EventReceiver::Bounded(rx))
            }
            EventBuffer::Lossless => {
                let (tx, rx) = mpsc::unbounded_channel();
                (EventSender::Lossless(tx), EventReceiver::Lossless(rx))
            }
        };
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

    /// Runs `script.callFunction` in a browsing context and returns the decoded result.
    pub async fn call_function(
        &self,
        context: &str,
        js: &str,
        this: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        self.call_function_target(json!({"context": context}), js, this, args)
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
        )
        .await
    }

    /// Runs a function in `context` with `this` set to a remote object
    /// reference, telling a script that threw apart from a failed command.
    pub(crate) async fn run_function(
        &self,
        context: &str,
        js: &str,
        this: Value,
        args: Vec<Value>,
    ) -> Result<Script, BidiError> {
        self.script_call(json!({"context": context}), js, Some(this), args)
            .await
    }

    async fn call_function_target(
        &self,
        target: Value,
        js: &str,
        this: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match self
            .script_call(target, js, this, args)
            .await
            .map_err(|e| e.to_string())?
        {
            Script::Returned(value) => Ok(value),
            Script::Threw(text) => Err(text),
        }
    }

    async fn script_call(
        &self,
        target: Value,
        js: &str,
        this: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Script, BidiError> {
        let mut params = json!({
            "functionDeclaration": js,
            "target": target,
            "arguments": args.iter().map(to_local).collect::<Vec<_>>(),
            "awaitPromise": true,
            "resultOwnership": "none",
        });
        if let Some(t) = this {
            params["this"] = t;
        }
        let r = self.send_raw("script.callFunction", params).await?;
        if r["type"] == "exception" {
            return Ok(Script::Threw(
                r["exceptionDetails"]["text"]
                    .as_str()
                    .unwrap_or("script exception")
                    .to_string(),
            ));
        }
        Ok(Script::Returned(remote_to_json(&r["result"])))
    }
}

/// What a function the browser ran ended with.
pub(crate) enum Script {
    Returned(Value),
    /// The script threw; the text is the page's.
    Threw(String),
}

/// Wraps plain JSON as a BiDi LocalValue; values that already carry `type` or `sharedId` pass through.
pub fn to_local(v: &Value) -> Value {
    match v {
        Value::Object(o) if o.contains_key("type") || o.contains_key("sharedId") => v.clone(),
        Value::String(_) => json!({"type": "string", "value": v}),
        Value::Number(_) => json!({"type": "number", "value": v}),
        Value::Bool(_) => json!({"type": "boolean", "value": v}),
        Value::Null => json!({"type": "null"}),
        Value::Array(a) => {
            json!({"type": "array", "value": a.iter().map(to_local).collect::<Vec<_>>()})
        }
        Value::Object(o) => json!({
            "type": "object",
            "value": o.iter().map(|(k, x)| json!([k, to_local(x)])).collect::<Vec<_>>(),
        }),
    }
}

/// Converts a BiDi RemoteValue to JSON: nodes become `{sharedId}`, windows their context id.
pub fn remote_to_json(v: &Value) -> Value {
    let val = &v["value"];
    match v["type"].as_str() {
        Some("node") => json!({"sharedId": v["sharedId"]}),
        Some("window") => val["context"].clone(),
        Some("array" | "set") => Value::Array(
            val.as_array()
                .map(|a| a.iter().map(remote_to_json).collect())
                .unwrap_or_default(),
        ),
        Some("object" | "map") => Value::Object(
            val.as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    let k = match &p[0] {
                        Value::String(s) => s.clone(),
                        other => remote_to_json(other).as_str()?.to_string(),
                    };
                    Some((k, remote_to_json(&p[1])))
                })
                .collect(),
        ),
        Some("string" | "boolean" | "date") => val.clone(),
        Some("number") if val.is_number() => val.clone(),
        _ => Value::Null,
    }
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
    async fn a_lossless_subscription_keeps_every_matching_event_past_the_capacity() {
        let (url, l) = mock().await;
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let m: Value =
                serde_json::from_str(&ws.next().await.unwrap().unwrap().into_text().unwrap())
                    .unwrap();
            for index in 0..=BUFFERED_EVENT_CAPACITY {
                let event =
                    json!({"type":"event","method":"blocked.request","params":{"index":index}});
                ws.send(Message::Text(event.to_string())).await.unwrap();
                if index % 1024 == 0 {
                    let log =
                        json!({"type":"event","method":"log.entryAdded","params":{"index":index}});
                    ws.send(Message::Text(log.to_string())).await.unwrap();
                }
            }
            let reply = json!({"type":"success","id":m["id"],"result":{}});
            ws.send(Message::Text(reply.to_string())).await.unwrap();
            let _ = ws.next().await;
        });
        let c = BidiClient::connect(&url).await.unwrap();
        let mut events =
            c.subscribe_matching_lossless(|event| event["method"] == "blocked.request");
        c.send("a.b", json!({})).await.unwrap();
        let mut indexes = Vec::new();
        while let Ok(event) = events.try_recv() {
            assert_eq!(event["method"], "blocked.request");
            indexes.push(event["params"]["index"].as_u64().unwrap());
        }
        assert_eq!(indexes.len(), BUFFERED_EVENT_CAPACITY + 1);
        assert_eq!(indexes.first(), Some(&0));
        assert_eq!(indexes.last(), Some(&(BUFFERED_EVENT_CAPACITY as u64)));
        assert!(!events.has_lost_events());
        server.abort();
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
    }
}
