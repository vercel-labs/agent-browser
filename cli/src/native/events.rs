//! Experimental socket subscriptions for visibility transitions in the current Chrome document.
//! Build with `event-prototype`; disconnecting the client removes its observer.

use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, Mutex};

use super::actions::DaemonState;
use super::cdp::client::{CdpClient, RawCdpMessage};

#[derive(Deserialize)]
struct Request {
    selector: String,
    #[serde(default = "default_once")]
    once: bool,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

fn default_once() -> bool {
    true
}

fn default_timeout() -> u64 {
    10_000
}

struct Watch {
    client: Arc<CdpClient>,
    session: String,
    target: String,
    binding: String,
    id: String,
    receiver: broadcast::Receiver<RawCdpMessage>,
}

impl Watch {
    async fn create(state: &Mutex<DaemonState>) -> Result<Self, String> {
        let state = state.lock().await;
        if state.engine != "chrome" {
            return Err("The event prototype currently requires Chrome".into());
        }
        let browser = state.browser.as_ref().ok_or("Open a page first")?;
        let client = browser.client.clone();
        let id = uuid::Uuid::new_v4().simple().to_string();
        Ok(Self {
            receiver: client.subscribe_raw(),
            client,
            session: browser.active_session_id()?.to_owned(),
            target: browser.active_target_id()?.to_owned(),
            binding: format!("__ab_event_{id}"),
            id,
        })
    }

    async fn install(&self, request: &Request) -> Result<Value, String> {
        self.client
            .send_command(
                "Runtime.addBinding",
                Some(json!({"name": self.binding})),
                Some(&self.session),
            )
            .await?;
        let source = include_str!("event-observer.js")
            .replace("__BINDING_JSON__", &json!(self.binding).to_string())
            .replace("__SELECTOR_JSON__", &json!(request.selector).to_string());
        let result = self
            .client
            .send_command(
                "Runtime.evaluate",
                Some(json!({"expression": source, "returnByValue": true})),
                Some(&self.session),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err("Observer setup failed; check the CSS selector and document".into());
        }
        result
            .pointer("/result/value")
            .cloned()
            .ok_or_else(|| "Observer did not return its initial state".into())
    }

    async fn cleanup(&self) {
        let key = json!(format!("{}_stop", self.binding));
        let binding = json!(self.binding);
        let _ = self
            .client
            .send_command(
                "Runtime.evaluate",
                Some(json!({"expression": format!(
                    "globalThis[{key}]?.(); delete globalThis[{binding}]"
                )})),
                Some(&self.session),
            )
            .await;
        let _ = self
            .client
            .send_command(
                "Runtime.removeBinding",
                Some(json!({"name": self.binding})),
                Some(&self.session),
            )
            .await;
    }

    fn message(&self, name: &str, data: Value) -> Value {
        json!({
            "eventId": uuid::Uuid::new_v4().to_string(),
            "name": name,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "subscriptionId": self.id,
            "data": {"tabId": self.target, "observation": data},
            "cursor": null
        })
    }

    fn classify(&self, raw: &RawCdpMessage) -> Option<(String, Value, bool)> {
        let event: Value = serde_json::from_str(&raw.text).ok()?;
        let method = event.get("method")?.as_str()?;
        if method == "Target.targetDestroyed"
            && event.pointer("/params/targetId")?.as_str()? == self.target
        {
            return Some((
                "subscription.ended".into(),
                json!({"reason": "tab_closed"}),
                true,
            ));
        }
        if event.get("sessionId").and_then(Value::as_str) != Some(self.session.as_str()) {
            return None;
        }
        if method == "Runtime.executionContextsCleared" {
            return Some((
                "subscription.ended".into(),
                json!({"reason": "document_changed"}),
                true,
            ));
        }
        if method != "Runtime.bindingCalled"
            || event.pointer("/params/name")?.as_str()? != self.binding
        {
            return None;
        }
        let payload: Value =
            serde_json::from_str(event.pointer("/params/payload")?.as_str()?).ok()?;
        let name = match payload.get("state")?.as_str()? {
            "visible" => "element.visible",
            "hidden" => "element.hidden",
            _ => return None,
        };
        Some((name.into(), payload, false))
    }

    async fn stream<R, W>(
        &mut self,
        request: &Request,
        reader: &mut R,
        writer: &mut W,
    ) -> Result<(), String>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let initial = self.install(request).await?;
        send(writer, &self.message("subscription.ready", initial)).await?;
        let deadline = tokio::time::sleep(Duration::from_millis(request.timeout_ms));
        tokio::pin!(deadline);
        let mut input = Vec::new();
        loop {
            tokio::select! {
                result = reader.read_until(b'\n', &mut input) => {
                    if result.map_err(|e| e.to_string())? == 0 {
                        return Ok(());
                    }
                    let cancellation: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
                    if cancellation.get("action").and_then(Value::as_str) != Some("events_unsubscribe") {
                        return Err("Only events_unsubscribe is accepted on an event connection".into());
                    }
                    send(writer, &self.message("subscription.ended", json!({"reason": "cancelled"}))).await?;
                    return Ok(());
                }
                _ = &mut deadline => {
                    send(writer, &self.message("subscription.ended", json!({"reason": "timeout"}))).await?;
                    return Ok(());
                }
                received = self.receiver.recv() => {
                    let event = match received {
                        Ok(event) => event,
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            send(writer, &self.message("subscription.ended", json!({"reason": "event_gap", "missed": count}))).await?;
                            return Ok(());
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err("Browser event connection closed".into());
                        }
                    };
                    let Some((name, data, terminal)) = self.classify(&event) else {
                        continue;
                    };
                    send(writer, &self.message(&name, data)).await?;
                    if terminal {
                        return Ok(());
                    }
                    if request.once && name == "element.visible" {
                        send(writer, &self.message("subscription.ended", json!({"reason": "matched"}))).await?;
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn send<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<(), String> {
    let line = format!("{value}\n");
    tokio::time::timeout(Duration::from_secs(2), async {
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| "Event consumer stopped reading".to_string())?
    .map_err(|e| e.to_string())
}

/// Stream prototype events on one socket without holding the daemon command lock.
pub async fn serve<R, W>(
    command: &Value,
    state: Arc<Mutex<DaemonState>>,
    reader: &mut R,
    writer: &mut W,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut watch = None;
    let result: Result<(), String> = async {
        let request: Request =
            serde_json::from_value(command.clone()).map_err(|e| e.to_string())?;
        if request.selector.is_empty() || !(1..=60_000).contains(&request.timeout_ms) {
            return Err("Use a nonempty selector and timeout_ms between 1 and 60000".into());
        }
        let installed = watch.insert(Watch::create(&state).await?);
        installed.stream(&request, reader, writer).await
    }
    .await;
    if let Err(error) = result {
        let _ = send(
            writer,
            &json!({"name": "subscription.error", "error": error}),
        )
        .await;
    }
    if let Some(watch) = watch {
        let _ = tokio::time::timeout(Duration::from_secs(2), watch.cleanup()).await;
    }
}
