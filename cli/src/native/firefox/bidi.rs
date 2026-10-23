//! WebDriver BiDi client over a WebSocket.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, oneshot};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type WsSink = futures_util::stream::SplitSink<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    Message,
>;

/** A connected BiDi session socket; the reader task stops when the client is dropped. */
pub struct BidiClient {
    sink: tokio::sync::Mutex<WsSink>,
    pending: Pending,
    events: broadcast::Sender<Value>,
    next_id: AtomicU64,
    reader: tokio::task::JoinHandle<()>,
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
        let (events, _) = broadcast::channel(1024);
        let (p, ev) = (pending.clone(), events.clone());
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                match v["type"].as_str() {
                    Some("event") => {
                        let _ = ev.send(v);
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
            events,
            next_id: AtomicU64::new(1),
            reader,
        })
    }

    /// Subscribes to BiDi events (`type: event` messages).
    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
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
        if self.reader.is_finished() {
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

    async fn call_function_target(
        &self,
        target: Value,
        js: &str,
        this: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
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
        let r = self.send("script.callFunction", params).await?;
        if r["type"] == "exception" {
            return Err(r["exceptionDetails"]["text"]
                .as_str()
                .unwrap_or("script exception")
                .to_string());
        }
        Ok(remote_to_json(&r["result"]))
    }
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
        Some("string" | "number" | "boolean" | "bigint" | "date") => val.clone(),
        _ => Value::Null,
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
        let mut ev = c.subscribe();
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
