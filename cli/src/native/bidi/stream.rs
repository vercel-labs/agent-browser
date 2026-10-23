use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use super::client::BidiClient;
use super::input::{self, Origin};
use crate::native::stream::{FrameMetadata, StreamServer};

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The BiDi connection and tab the dashboard is looking at.
#[derive(Clone)]
pub struct FirefoxLive {
    pub bidi: Arc<BidiClient>,
    pub context: String,
}

/// Stops the polling task when dropped.
pub struct Poller(tokio::task::JoinHandle<()>);

impl Drop for Poller {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Starts publishing JPEG frames of the tab into the stream while dashboard clients are connected.
pub fn spawn_poller(live: FirefoxLive, server: Arc<StreamServer>, paused: bool) -> Poller {
    Poller(tokio::spawn(poll(live, server, paused)))
}

async fn poll(live: FirefoxLive, server: Arc<StreamServer>, mut paused: bool) {
    let mut events = live.bidi.subscribe();
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(e) if e["params"]["context"] == live.context.as_str() => {
                    match e["method"].as_str() {
                        Some("browsingContext.userPromptOpened") => paused = true,
                        Some("browsingContext.userPromptClosed") => paused = false,
                        _ => {}
                    }
                }
                Err(RecvError::Closed) => break,
                _ => {}
            },
            _ = tick.tick() => {
                if !paused && server.has_clients().await {
                    publish_frame(&live, &server).await;
                }
            }
        }
    }
}

async fn publish_frame(live: &FirefoxLive, server: &StreamServer) {
    let quality = server.screencast_quality() as f64 / 100.0;
    let shot = live
        .bidi
        .send(
            "browsingContext.captureScreenshot",
            json!({
                "context": live.context,
                "origin": "viewport",
                "format": { "type": "image/jpeg", "quality": quality },
            }),
        )
        .await;
    let Some(data) = shot.as_ref().ok().and_then(|r| r["data"].as_str()) else {
        return;
    };
    let size = live
        .bidi
        .call_function(
            &live.context,
            "() => [innerWidth, innerHeight]",
            None,
            vec![],
        )
        .await
        .unwrap_or_default();
    let width = size[0].as_f64().unwrap_or(0.0) as u32;
    let height = size[1].as_f64().unwrap_or(0.0) as u32;
    if width > 0 && height > 0 && server.viewport().await != (width, height) {
        server.set_viewport(width, height).await;
        server
            .broadcast_status(true, false, width, height, "firefox")
            .await;
    }
    let (device_width, device_height) = server.viewport().await;
    let timestamp = crate::native::stream::timestamp_ms();
    server.broadcast_screencast_frame(
        data,
        &FrameMetadata {
            device_width,
            device_height,
            timestamp,
            ..Default::default()
        },
    );
}

fn input_actions(msg_type: &str, msg: &Value) -> Option<Value> {
    let text = |field: &str| msg.get(field).and_then(Value::as_str).unwrap_or("");
    let number = |field: &str| msg.get(field).and_then(Value::as_f64).unwrap_or(0.0);
    let origin = Origin::Viewport {
        x: number("x"),
        y: number("y"),
    };
    match (msg_type, text("eventType")) {
        ("input_mouse", "mouseMoved") => Some(input::pointer_hover(&origin)),
        ("input_mouse", "mousePressed") => Some(input::pointer_down(&origin, text("button"))),
        ("input_mouse", "mouseReleased") => Some(input::pointer_up(&origin, text("button"))),
        ("input_mouse", "mouseWheel") => Some(input::pointer_wheel(
            &origin,
            number("deltaX"),
            number("deltaY"),
        )),
        ("input_keyboard", "keyDown") => Some(input::key_event_actions("keyDown", text("key"))),
        ("input_keyboard", "keyUp") => Some(input::key_event_actions("keyUp", text("key"))),
        _ => None,
    }
}

/// Sends one dashboard mouse or keyboard message to the tab as `input.performActions`.
pub async fn dispatch_input(live: &FirefoxLive, msg_type: &str, msg: &Value) {
    if let Some(actions) = input_actions(msg_type, msg) {
        let _ = live
            .bidi
            .send(
                "input.performActions",
                json!({ "context": live.context, "actions": actions }),
            )
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_messages_become_key_actions_and_unknown_events_are_dropped() {
        let down = json!({"eventType": "keyDown", "key": "Enter"});
        let actions = input_actions("input_keyboard", &down).unwrap();
        assert_eq!(
            actions[0]["actions"][0],
            json!({"type": "keyDown", "value": "\u{E007}"})
        );
        assert!(input_actions("input_keyboard", &json!({"eventType": "char"})).is_none());
        assert!(input_actions("input_touch", &json!({})).is_none());
    }
}
