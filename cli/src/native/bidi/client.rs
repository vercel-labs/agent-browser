use super::network::AbortOnDrop;
use super::types::{LocalValue, RemoteValue};
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const BUFFERED_EVENT_CAPACITY: usize = 65_536;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_MESSAGE_SIZE: usize = 256 * 1024 * 1024;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

/// A connected BiDi session socket; the reader task stops when the client is dropped.
pub struct BidiClient {
    sink: tokio::sync::Mutex<WsSink>,
    pending: Pending,
    buffered_events: Arc<Mutex<Vec<mpsc::Sender<Value>>>>,
    next_id: AtomicU64,
    reader: AbortOnDrop,
}

impl BidiClient {
    /// Connects to a BiDi WebSocket URL and starts the reader task.
    pub async fn connect(url: &str) -> Result<Self, String> {
        let config = WebSocketConfig {
            max_message_size: Some(MAX_MESSAGE_SIZE),
            max_frame_size: Some(MAX_MESSAGE_SIZE),
            ..Default::default()
        };
        let (ws, _) = tokio_tungstenite::connect_async_with_config(url, Some(config), false)
            .await
            .map_err(|e| format!("BiDi connect failed: {e}"))?;
        let (sink, mut stream) = ws.split();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let buffered_events = Arc::new(Mutex::new(Vec::<mpsc::Sender<Value>>::new()));
        let (p, be) = (pending.clone(), buffered_events.clone());
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match v["type"].as_str() {
                    Some("event") => {
                        be.lock().unwrap().retain(|tx| {
                            !matches!(
                                tx.try_send(v.clone()),
                                Err(mpsc::error::TrySendError::Closed(_))
                            )
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
                            Err(format!(
                                "{}: {}",
                                v["error"].as_str().unwrap_or("unknown error"),
                                v["message"].as_str().unwrap_or("")
                            ))
                        });
                    }
                    _ => {}
                }
            }
            p.lock().unwrap().clear();
        });
        Ok(Self {
            sink: tokio::sync::Mutex::new(sink),
            pending,
            buffered_events,
            next_id: AtomicU64::new(1),
            reader: AbortOnDrop(reader),
        })
    }

    /// Queues events without blocking the reader; events past the 65,536-item capacity are dropped.
    pub fn subscribe_buffered(&self) -> mpsc::Receiver<Value> {
        let (tx, rx) = mpsc::channel(BUFFERED_EVENT_CAPACITY);
        self.buffered_events.lock().unwrap().push(tx);
        rx
    }

    /// Returns whether the BiDi WebSocket reader has stopped.
    pub fn is_closed(&self) -> bool {
        self.reader.0.is_finished()
    }

    /// Sends a command with the default 30s timeout and returns its `result`.
    pub async fn send(&self, method: &str, params: Value) -> Result<Value, String> {
        self.send_timeout(method, params, DEFAULT_TIMEOUT).await
    }

    /// Sends a command and fails with a timeout error if no reply arrives in time.
    pub async fn send_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.is_closed() {
            self.pending.lock().unwrap().remove(&id);
            return Err("BiDi connection closed".to_string());
        }
        let body = json!({"id": id, "method": method, "params": params}).to_string();
        if let Err(e) = self.sink.lock().await.send(Message::Text(body)).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(format!("BiDi send failed: {e}"));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("BiDi connection closed".to_string()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("BiDi command {method} timed out"))
            }
        }
    }

    /// Sends a command without registering a response waiter.
    pub async fn send_no_wait(&self, method: &str, params: Value) -> Result<(), String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if self.is_closed() {
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

    /// Sends a command with serializable params and deserializes its `result`.
    pub async fn send_typed<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<R, String> {
        let params =
            serde_json::to_value(params).map_err(|e| format!("Failed to serialize params: {e}"))?;
        serde_json::from_value(self.send(method, params).await?)
            .map_err(|e| format!("Failed to deserialize BiDi response for {method}: {e}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use tokio::io::AsyncWriteExt;
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
    async fn oversized_incoming_message_closes_connection() {
        let (url, listener) = mock().await;
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let mut frame = vec![0x81, 0x7f];
            frame.extend_from_slice(&(MAX_MESSAGE_SIZE as u64 + 1).to_be_bytes());
            websocket.get_mut().write_all(&frame).await.unwrap();
            std::future::pending::<()>().await;
            drop(websocket);
        });
        let client = BidiClient::connect(&url).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !client.is_closed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
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
    }
}
