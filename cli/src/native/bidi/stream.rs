use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{watch, Mutex};

use super::client::BidiClient;
use super::contexts::Contexts;
use super::input::{self, Origin};
use super::session::dialog_blocks_tab;
use super::tabs;
use crate::native::stream::{FrameMetadata, StreamServer};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const TAB_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// The BiDi connection and tab the dashboard is looking at.
#[derive(Clone)]
pub struct FirefoxLive {
    pub bidi: Arc<BidiClient>,
    pub context: String,
    pub contexts: Contexts,
    /// The session's tabs as (context, `t<N>` id, label), in id order.
    pub modifiers: Arc<Mutex<HashMap<String, u8>>>,
}

/// Owns the only task that serves the Firefox dashboard, and the channel that tells it what to show.
pub struct Poller {
    task: tokio::task::JoinHandle<()>,
    target: watch::Sender<Option<FirefoxLive>>,
}

impl Poller {
    /// Points the dashboard at `live`: the tab it shows and the tabs the session has now.
    pub fn show(&self, live: FirefoxLive) {
        self.target.send_replace(Some(live));
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Starts the single task that serves this Firefox session's dashboard, for as long as the session lives.
pub fn spawn_poller(live: FirefoxLive, server: Arc<StreamServer>) -> Poller {
    let (target, receiver) = watch::channel(Some(live));
    Poller {
        task: tokio::spawn(poll(server, receiver)),
        target,
    }
}

struct Shot {
    context: String,
    data: String,
    width: u32,
    height: u32,
}

/// Publishes frames, viewport, tabs and dialog state for the dashboard, and only while one is connected.
async fn poll(server: Arc<StreamServer>, mut target: watch::Receiver<Option<FirefoxLive>>) {
    let Some(mut live) = target.borrow_and_update().clone() else {
        return;
    };
    let mut events = live.bidi.subscribe_buffered();
    let mut paused = false;
    let mut frame: Option<tokio::task::JoinHandle<Option<Shot>>> = None;
    let mut listing: Option<tokio::task::JoinHandle<Option<Vec<Value>>>> = None;
    let mut frame_tick = tokio::time::interval(POLL_INTERVAL);
    frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tab_tick = tokio::time::interval(TAB_INTERVAL);
    tab_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(e) = events.recv() => {
                live.contexts.apply(&e);
                let context = e["params"]["context"].as_str().unwrap_or_default();
                let blocks = dialog_blocks_tab(live.contexts.tab_of(context).or(Some(context)), &live.context);
                match (e["method"].as_str(), blocks) {
                    (Some("browsingContext.userPromptOpened"), true) => {
                        paused = true;
                        if let Some(task) = frame.take() {
                            task.abort();
                        }
                    }
                    (Some("browsingContext.userPromptClosed"), true) => paused = false,
                    _ => {}
                }
            }
            Ok(()) = target.changed() => {
                let Some(next) = target.borrow_and_update().clone() else {
                    break;
                };
                if next.context != live.context {
                    if let Some(task) = frame.take() {
                        task.abort();
                    }
                }
                live = next;
                if server.has_clients().await {
                    if let Some(task) = listing.take() {
                        task.abort();
                    }
                    listing = Some(list_tabs(&live));
                }
            }
            result = async { frame.as_mut().unwrap().await }, if frame.is_some() => {
                frame = None;
                if let Ok(Some(shot)) = result {
                    let shown = server.firefox_live_context().await;
                    if !paused && shown.as_deref() == Some(shot.context.as_str()) {
                        publish_frame(&shot, &server).await;
                    }
                }
            }
            result = async { listing.as_mut().unwrap().await }, if listing.is_some() => {
                listing = None;
                if let Ok(Some(tabs)) = result {
                    server.broadcast_tabs(&tabs).await;
                }
            }
            _ = frame_tick.tick() => {
                if live.bidi.is_closed() {
                    let (width, height) = server.viewport().await;
                    server.broadcast_status(false, false, width, height, "firefox").await;
                    break;
                }
                if frame.is_none() && !paused && server.has_clients().await {
                    frame = Some(capture_frame(&live, server.screencast_quality()));
                }
            }
            _ = tab_tick.tick() => {
                if listing.is_none() && server.has_clients().await {
                    listing = Some(list_tabs(&live));
                }
            }
        }
    }
}

/// Asks Firefox for the tab list without holding the poller's event loop.
fn list_tabs(live: &FirefoxLive) -> tokio::task::JoinHandle<Option<Vec<Value>>> {
    let live = live.clone();
    tokio::spawn(async move { tabs::stream_tabs(&live.bidi, &live.contexts, &live.context).await })
}

/// Asks Firefox for a frame without holding the poller's event loop.
fn capture_frame(live: &FirefoxLive, quality: i32) -> tokio::task::JoinHandle<Option<Shot>> {
    let live = live.clone();
    let quality = quality as f64 / 100.0;
    tokio::spawn(async move { capture(&live, quality).await })
}

/// Captures the tab's viewport and size, giving up quickly so a stuck page cannot hold the dashboard.
async fn capture(live: &FirefoxLive, quality: f64) -> Option<Shot> {
    let shot = live
        .bidi
        .send_timeout(
            "browsingContext.captureScreenshot",
            json!({
                "context": &live.context,
                "origin": "viewport",
                "format": { "type": "image/jpeg", "quality": quality },
            }),
            REQUEST_TIMEOUT,
        )
        .await
        .ok()?;
    let data = shot["data"].as_str()?.to_string();
    let size = live
        .bidi
        .call_function_timeout(
            &live.context,
            "() => [innerWidth, innerHeight]",
            REQUEST_TIMEOUT,
        )
        .await
        .unwrap_or_default();
    Some(Shot {
        context: live.context.clone(),
        data,
        width: size[0].as_f64().unwrap_or(0.0) as u32,
        height: size[1].as_f64().unwrap_or(0.0) as u32,
    })
}

/// Publishes a frame together with the viewport it was captured at, so both describe the tab shown.
async fn publish_frame(shot: &Shot, server: &StreamServer) {
    if shot.width > 0 && shot.height > 0 && server.viewport().await != (shot.width, shot.height) {
        server.set_viewport(shot.width, shot.height).await;
        server
            .broadcast_status(true, false, shot.width, shot.height, "firefox")
            .await;
    }
    let (device_width, device_height) = server.viewport().await;
    let timestamp = crate::native::stream::timestamp_ms();
    server.broadcast_screencast_frame(
        &shot.data,
        &FrameMetadata {
            device_width,
            device_height,
            timestamp,
            ..Default::default()
        },
    );
}

fn input_actions(msg_type: &str, msg: &Value, held: u8) -> Option<(Value, u8)> {
    let text = |field: &str| msg.get(field).and_then(Value::as_str).unwrap_or("");
    let number = |field: &str| msg.get(field).and_then(Value::as_f64).unwrap_or(0.0);
    let origin = Origin::Viewport {
        x: number("x"),
        y: number("y"),
    };
    let mut actions = match (msg_type, text("eventType")) {
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
    }?;
    let modifiers = [(1, "Alt"), (2, "Control"), (4, "Meta"), (8, "Shift")];
    let mut desired = msg
        .get("modifiers")
        .and_then(Value::as_u64)
        .map(|mask| (mask & 15) as u8)
        .unwrap_or(held);
    let keyboard = msg_type == "input_keyboard";
    let mut prefix = Vec::new();
    for (bit, key) in modifiers {
        if keyboard && text("key") == key {
            if text("eventType") == "keyDown" {
                desired |= bit;
            } else {
                desired &= !bit;
            }
        } else if (held ^ desired) & bit != 0 {
            let event = if desired & bit != 0 {
                "keyDown"
            } else {
                "keyUp"
            };
            prefix.push(input::key_event_actions(event, key)[0]["actions"][0].clone());
        }
    }
    if keyboard {
        actions[0]["id"] = json!("dashboard-key");
        actions[0]["actions"]
            .as_array_mut()
            .unwrap()
            .splice(0..0, prefix);
    } else if !prefix.is_empty() {
        actions[0]["actions"]
            .as_array_mut()
            .unwrap()
            .splice(0..0, (0..prefix.len()).map(|_| json!({"type": "pause"})));
        actions.as_array_mut().unwrap().push(json!({
            "type": "key", "id": "dashboard-key", "actions": prefix,
        }));
    }
    Some((actions, desired))
}

/// Sends dashboard input without waiting for a reply. Cached modifiers assume Firefox accepts successfully sent actions.
pub async fn dispatch_input(live: &FirefoxLive, msg_type: &str, msg: &Value) {
    let mut modifiers = live.modifiers.lock().await;
    let held = modifiers.get(&live.context).copied().unwrap_or(0);
    if let Some((actions, desired)) = input_actions(msg_type, msg, held) {
        if live
            .bidi
            .send_no_wait(
                "input.performActions",
                json!({ "context": &live.context, "actions": actions }),
            )
            .await
            .is_ok()
        {
            modifiers.insert(live.context.clone(), desired);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::session::tests::{fake_session, fake_session_with_replies, FakeReply};

    async fn dashboard(
        server: &StreamServer,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}", server.port()))
            .await
            .expect("dashboard should connect");
        ws
    }

    fn asked(calls: &crate::native::bidi::session::tests::Calls, method: &str) -> usize {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|m| *m == method)
            .count()
    }

    async fn wait_for(
        calls: &crate::native::bidi::session::tests::Calls,
        method: &str,
        wanted: usize,
        patience: Duration,
    ) -> usize {
        let deadline = tokio::time::Instant::now() + patience;
        loop {
            let seen = asked(calls, method);
            if seen >= wanted || tokio::time::Instant::now() > deadline {
                return seen;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_command_without_a_dashboard_client_asks_firefox_for_nothing() {
        use crate::native::actions::{execute_command, DaemonState};
        use crate::native::stream::{IdleActivity, StreamServer};

        let (ff, events, calls) = fake_session().await;
        std::mem::forget(events);
        let activity = Arc::new(IdleActivity::new());
        let (server, slot) =
            StreamServer::start_without_client(0, "firefox-idle".into(), true, activity.clone())
                .await
                .unwrap();
        let server = Arc::new(server);
        let mut state = DaemonState::new_with_stream(
            crate::session_settings::Running::defaults(),
            Arc::new(crate::session_settings::MemStore::default()),
            Some(slot),
            Some(Arc::clone(&server)),
            activity,
        );
        state.engine = "firefox".into();
        state.firefox = Some(ff);
        state.update_stream_client().await;
        calls.lock().unwrap().clear();

        execute_command(&json!({"id": "1", "action": "no-such-action"}), &mut state).await;
        tokio::time::sleep(TAB_INTERVAL + Duration::from_millis(200)).await;

        let observed = calls.lock().unwrap().clone();
        assert!(observed.is_empty(), "{observed:?}");
        server.shutdown().await;
    }

    #[tokio::test]
    async fn a_hanging_capture_does_not_stop_a_prompt_from_pausing_the_poller() {
        use crate::native::stream::{IdleActivity, StreamServer};

        let (ff, events, calls) = fake_session_with_replies(|request| {
            if request["method"] == "browsingContext.captureScreenshot" {
                FakeReply::Silent
            } else {
                FakeReply::Result(json!({}))
            }
        })
        .await;
        let activity = Arc::new(IdleActivity::new());
        let (server, _slot) =
            StreamServer::start_without_client(0, "firefox-prompt".into(), true, activity)
                .await
                .unwrap();
        let server = Arc::new(server);
        let live = FirefoxLive {
            bidi: Arc::clone(&ff.bidi),
            context: ff.contexts.active_tab().to_string(),
            contexts: ff.contexts.clone(),
            modifiers: Default::default(),
        };
        let _poller = spawn_poller(live, Arc::clone(&server));
        let _ws = dashboard(&server).await;

        assert_eq!(
            wait_for(
                &calls,
                "browsingContext.captureScreenshot",
                1,
                Duration::from_secs(5)
            )
            .await,
            1,
            "the poller should have one capture outstanding"
        );
        events
            .send(
                json!({"type": "event", "method": "browsingContext.userPromptOpened",
                "params": {"context": "ctx-active", "type": "alert"}}),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        events
            .send(
                json!({"type": "event", "method": "browsingContext.userPromptClosed",
                "params": {"context": "ctx-active"}}),
            )
            .unwrap();

        assert_eq!(
            wait_for(
                &calls,
                "browsingContext.captureScreenshot",
                2,
                Duration::from_millis(1200)
            )
            .await,
            2,
            "the prompt should pause and resume the poller while the capture hangs"
        );
        server.shutdown().await;
    }

    #[test]
    fn key_messages_become_key_actions_and_unknown_events_are_dropped() {
        let down = json!({"eventType": "keyDown", "key": "Enter"});
        let (actions, held) = input_actions("input_keyboard", &down, 0).unwrap();
        assert_eq!(held, 0);
        assert_eq!(actions[0]["id"], "dashboard-key");
        assert_eq!(
            actions[0]["actions"][0],
            json!({"type": "keyDown", "value": "\u{E007}"})
        );
        assert!(input_actions("input_keyboard", &json!({"eventType": "char"}), 0).is_none());
        assert!(input_actions("input_touch", &json!({}), 0).is_none());
    }

    #[test]
    fn explicit_modifiers_are_not_duplicated_and_pointer_input_waits_for_modifiers() {
        let down = json!({"eventType": "keyDown", "key": "Shift", "modifiers": 8});
        let (actions, held) = input_actions("input_keyboard", &down, 0).unwrap();
        assert_eq!(held, 8);
        assert_eq!(actions[0]["actions"].as_array().unwrap().len(), 1);
        let up = json!({"eventType": "keyUp", "key": "Shift", "modifiers": 0});
        let (actions, held) = input_actions("input_keyboard", &up, held).unwrap();
        assert_eq!(held, 0);
        assert_eq!(actions[0]["actions"].as_array().unwrap().len(), 1);
        for event in ["mouseMoved", "mousePressed", "mouseReleased", "mouseWheel"] {
            let (actions, held) = input_actions(
                "input_mouse",
                &json!({"eventType": event, "modifiers": 10}),
                0,
            )
            .unwrap();
            assert_eq!(held, 10);
            assert_eq!(actions[0]["actions"][0]["type"], "pause");
            assert_eq!(actions[0]["actions"][1]["type"], "pause");
            assert_eq!(
                actions[1]["actions"],
                json!([
                    {"type": "keyDown", "value": "\u{E009}"},
                    {"type": "keyDown", "value": "\u{E008}"},
                ])
            );
        }
    }
}
