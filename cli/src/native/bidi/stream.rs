use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, watch, Mutex};

use super::client::BidiClient;
use super::contexts::Contexts;
use super::input::{self, Origin};
use super::tabs;
use crate::native::stream::{FirefoxTabs, FrameMetadata, StreamServer};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const TAB_INTERVAL: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

/// The BiDi connection and tab the dashboard is looking at.
#[derive(Clone)]
pub struct FirefoxLive {
    pub bidi: Arc<BidiClient>,
    pub context: String,
    pub contexts: Contexts,
    /// Modifier mask held for each browsing context.
    pub modifiers: Arc<Mutex<HashMap<String, u8>>>,
}

/// Owns the only task that serves the Firefox dashboard, and the channels that tell it what to show.
pub struct Poller {
    task: tokio::task::JoinHandle<()>,
    target: watch::Sender<Option<FirefoxLive>>,
    bidi: Arc<BidiClient>,
    tabs: Arc<FirefoxTabs>,
    registration: u64,
}

impl Poller {
    /// Whether this poller is active for the given BiDi session.
    pub fn serves(&self, bidi: &Arc<BidiClient>) -> bool {
        Arc::ptr_eq(&self.bidi, bidi) && !self.task.is_finished()
    }

    /// Points the dashboard at `live`: the tab it shows and the tabs the session has now.
    pub fn show(&self, live: FirefoxLive) {
        self.target.send_replace(Some(live));
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.task.abort();
        self.tabs.detach(self.registration);
    }
}

/// Starts the single task that serves this Firefox session's dashboard, for as long as the session lives.
pub fn spawn_poller(live: FirefoxLive, server: Arc<StreamServer>) -> Poller {
    let bidi = Arc::clone(&live.bidi);
    let (target, receiver) = watch::channel(Some(live));
    let tabs = server.firefox_tabs();
    let (requests, request_rx) = mpsc::unbounded_channel();
    let registration = tabs.attach(requests);
    Poller {
        task: tokio::spawn(poll(server, receiver, request_rx)),
        target,
        bidi,
        tabs,
        registration,
    }
}

struct Shot {
    context: String,
    data: String,
    width: u32,
    height: u32,
}

/// The browsing-context events the live view acts on, so page logs and network traffic cannot
/// crowd them out of the subscription.
fn live_view_event(event: &Value) -> bool {
    matches!(
        event["method"].as_str(),
        Some(
            "browsingContext.contextCreated"
                | "browsingContext.contextDestroyed"
                | "browsingContext.userPromptOpened"
                | "browsingContext.userPromptClosed"
        )
    )
}

/// Publishes frames, viewport, tabs and dialog state for the dashboard: frames only while one is
/// connected, tabs also whenever `/api/tabs` asks for them.
async fn poll(
    server: Arc<StreamServer>,
    mut target: watch::Receiver<Option<FirefoxLive>>,
    mut requests: mpsc::UnboundedReceiver<oneshot::Sender<Option<Vec<Value>>>>,
) {
    let Some(mut live) = target.borrow_and_update().clone() else {
        return;
    };
    let mut events = live.bidi.subscribe_matching(live_view_event);
    let mut prompts = HashSet::<String>::new();
    let mut frame: Option<tokio::task::JoinHandle<Option<Shot>>> = None;
    let mut listing: Option<tokio::task::JoinHandle<Option<Vec<Value>>>> = None;
    let mut waiting: Vec<oneshot::Sender<Option<Vec<Value>>>> = Vec::new();
    let mut frame_tick = tokio::time::interval(POLL_INTERVAL);
    frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut tab_tick = tokio::time::interval(TAB_INTERVAL);
    tab_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            Some(e) = events.recv() => {
                let context = e["params"]["context"].as_str().unwrap_or_default();
                let tab = live.contexts.tab_of(context).unwrap_or(context).to_string();
                for gone in live.contexts.apply(&e).contexts {
                    prompts.remove(&gone);
                }
                match e["method"].as_str() {
                    Some("browsingContext.userPromptOpened") => {
                        if tab == live.context {
                            if let Some(task) = frame.take() {
                                task.abort();
                            }
                        }
                        prompts.insert(tab);
                    }
                    Some("browsingContext.userPromptClosed") => {
                        prompts.remove(&tab);
                    }
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
                    if !prompts.contains(&live.context) && shown.as_deref() == Some(shot.context.as_str()) {
                        publish_frame(&shot, &server).await;
                    }
                }
            }
            Some(reply) = requests.recv() => {
                waiting.push(reply);
                if listing.is_none() {
                    listing = Some(list_tabs(&live));
                }
            }
            result = async { listing.as_mut().unwrap().await }, if listing.is_some() => {
                listing = None;
                let tabs = result.ok().flatten();
                for reply in waiting.drain(..) {
                    let _ = reply.send(tabs.clone());
                }
                if let Some(tabs) = tabs {
                    server.broadcast_tabs(&tabs).await;
                }
            }
            _ = frame_tick.tick() => {
                if live.bidi.is_closed() {
                    let (width, height) = server.viewport().await;
                    server.broadcast_status(false, false, width, height, "firefox").await;
                    break;
                }
                if frame.is_none()
                    && !live.context.is_empty()
                    && !prompts.contains(&live.context)
                    && server.has_clients().await
                {
                    frame = Some(capture_frame(
                        &live,
                        server.screencast_config.quality,
                        (
                            server.screencast_config.max_width,
                            server.screencast_config.max_height,
                        ),
                    ));
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
fn capture_frame(
    live: &FirefoxLive,
    quality: i32,
    max_size: (Option<u32>, Option<u32>),
) -> tokio::task::JoinHandle<Option<Shot>> {
    let live = live.clone();
    tokio::spawn(async move {
        let mut shot = capture(&live, quality as f64 / 100.0).await?;
        let frame = std::mem::take(&mut shot.data);
        shot.data = tokio::task::spawn_blocking(move || fit_frame(frame, max_size, quality))
            .await
            .ok()?;
        Some(shot)
    })
}

/// Shrinks a JPEG frame to fit the configured cap, keeping its proportions. Firefox captures at
/// the viewport size, so the cap is applied here. A frame inside the cap is returned unchanged.
fn fit_frame(frame: String, max_size: (Option<u32>, Option<u32>), quality: i32) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    if max_size == (None, None) {
        return frame;
    }
    let shrunk = || {
        let image = image::load_from_memory(&STANDARD.decode(&frame).ok()?).ok()?;
        let max_width = max_size.0.unwrap_or(image.width());
        let max_height = max_size.1.unwrap_or(image.height());
        if image.width() <= max_width && image.height() <= max_height {
            return None;
        }
        let resized = image
            .resize(max_width, max_height, image::imageops::FilterType::Triangle)
            .to_rgb8();
        let mut jpeg = Vec::new();
        let quality = quality.clamp(1, 100) as u8;
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, quality)
            .encode_image(&resized)
            .ok()?;
        Some(STANDARD.encode(jpeg))
    };
    shrunk().unwrap_or(frame)
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

/// Inserts literal clipboard text at the focused editable element in Firefox
/// without routing through WebDriver key actions (which interpret PUA code
/// points like U+E007/U+E004 as Enter/Tab).
const LITERAL_INSERT_TEXT_JS: &str = r#"function(text) {
    let doc = document;
    let el = doc.activeElement;
    while (el) {
        if (el.shadowRoot && el.shadowRoot.activeElement) {
            el = el.shadowRoot.activeElement;
            continue;
        }
        if (el.tagName === 'IFRAME') {
            try {
                if (el.contentDocument && el.contentDocument.activeElement) {
                    doc = el.contentDocument;
                    el = doc.activeElement;
                    continue;
                }
            } catch {}
        }
        break;
    }
    if (!el) return;
    if (el.tagName === 'TEXTAREA' || el.tagName === 'INPUT') {
        if (el.readOnly || el.disabled) return;
        if (!doc.execCommand || !doc.execCommand('insertText', false, text)) {
            const start = typeof el.selectionStart === 'number' ? el.selectionStart : el.value.length;
            const end = typeof el.selectionEnd === 'number' ? el.selectionEnd : el.value.length;
            if (typeof el.setRangeText === 'function') {
                el.setRangeText(text, start, end, 'end');
            } else {
                el.value = el.value.slice(0, start) + text + el.value.slice(end);
            }
            el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: text }));
        }
        return;
    }
    if (el.isContentEditable && doc.execCommand) {
        doc.execCommand('insertText', false, text);
    }
}"#;

fn insert_text_call_params(context: &str, text: &str) -> Option<Value> {
    if text.is_empty() || text.len() > 1_048_576 {
        return None;
    }
    Some(json!({
        "functionDeclaration": LITERAL_INSERT_TEXT_JS,
        "target": { "context": context },
        "arguments": [{ "type": "string", "value": text }],
        "awaitPromise": false,
        "userActivation": true,
    }))
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
        ("input_keyboard", "keyDown" | "rawKeyDown") => {
            Some(input::key_event_actions("keyDown", text("key")))
        }
        ("input_keyboard", "keyUp") => Some(input::key_event_actions("keyUp", text("key"))),
        ("input_read_clipboard", _) if msg.get("cut").and_then(Value::as_bool) == Some(true) => {
            let mut down = input::key_event_actions("keyDown", "x");
            let up = input::key_event_actions("keyUp", "x");
            down[0]["actions"]
                .as_array_mut()
                .unwrap()
                .push(up[0]["actions"][0].clone());
            Some(down)
        }
        _ => None,
    }?;
    let modifiers = [(1, "Alt"), (2, "Control"), (4, "Meta"), (8, "Shift")];
    let mut desired = msg
        .get("modifiers")
        .and_then(Value::as_u64)
        .map(|mask| (mask & 15) as u8)
        .unwrap_or(held);
    let keyboard = msg_type == "input_keyboard" || msg_type == "input_read_clipboard";
    let mut prefix = Vec::new();
    for (bit, key) in modifiers {
        if msg_type == "input_keyboard" && text("key") == key {
            if matches!(text("eventType"), "keyDown" | "rawKeyDown") {
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
    if live.context.is_empty() {
        return;
    }
    if msg_type == "input_insert_text" {
        if let Some(params) = msg
            .get("text")
            .and_then(Value::as_str)
            .and_then(|s| insert_text_call_params(&live.context, s))
        {
            let _ = live.bidi.send_no_wait("script.callFunction", params).await;
        }
        return;
    }
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

    use crate::native::actions::DaemonState;
    use crate::native::stream::IdleActivity;

    #[test]
    fn a_frame_larger_than_the_cap_is_shrunk_in_proportion() {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;

        let mut jpeg = Vec::new();
        image::RgbImage::new(1280, 720)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let frame = STANDARD.encode(&jpeg);
        let size = |frame: &str| {
            let image = image::load_from_memory(&STANDARD.decode(frame).unwrap()).unwrap();
            (image.width(), image.height())
        };

        let capped = fit_frame(frame.clone(), (Some(640), Some(480)), 80);
        assert_eq!(size(&capped), (640, 360));
        assert_eq!(fit_frame(frame.clone(), (None, None), 80), frame);
        assert_eq!(fit_frame(frame.clone(), (Some(1920), None), 80), frame);
    }
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

        let (mut ff, events, calls) = fake_session().await;
        std::mem::forget(events);
        ff.data_collector = super::super::session::DataCollectorState::Active {
            id: "collector".into(),
            mode: super::super::session::DataCollectorMode::RequestsOnly,
        };
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
    async fn a_new_firefox_session_gets_its_own_poller() {
        use crate::native::actions::DaemonState;
        use crate::native::stream::{IdleActivity, StreamServer};

        let (first, first_events, _) = fake_session().await;
        let (second, second_events, calls) = fake_session().await;
        std::mem::forget((first_events, second_events));
        let activity = Arc::new(IdleActivity::new());
        let (server, slot) = StreamServer::start_without_client(
            0,
            "firefox-relaunch".into(),
            true,
            activity.clone(),
        )
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
        first.bidi.stop_reader();
        state.firefox = Some(first);
        state.update_stream_client().await;
        tokio::time::sleep(POLL_INTERVAL * 3).await;

        state.firefox = Some(second);
        state.update_stream_client().await;
        let _ws = dashboard(&server).await;
        assert!(
            wait_for(
                &calls,
                "browsingContext.captureScreenshot",
                1,
                Duration::from_secs(3)
            )
            .await
                >= 1,
            "the dashboard must get frames from the session that replaced a closed one"
        );
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
        let poller = spawn_poller(live.clone(), Arc::clone(&server));
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

        events
            .send(
                json!({"type": "event", "method": "browsingContext.userPromptOpened",
                "params": {"context": "ctx-active", "type": "alert"}}),
            )
            .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        poller.show(FirefoxLive {
            context: "ctx-other".into(),
            ..live
        });
        assert_eq!(
            wait_for(
                &calls,
                "browsingContext.captureScreenshot",
                3,
                Duration::from_millis(1200)
            )
            .await,
            3,
            "a prompt on the tab left behind must not pause the tab now shown"
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

    async fn firefox_http_server(name: &str) -> (Arc<StreamServer>, DaemonState) {
        let activity = Arc::new(IdleActivity::new());
        let (server, slot) =
            StreamServer::start_without_client(0, name.into(), true, Arc::clone(&activity))
                .await
                .unwrap();
        let server = Arc::new(server);
        let state = DaemonState::new_with_stream(
            crate::session_settings::Running::defaults(),
            Arc::new(crate::session_settings::MemStore::default()),
            Some(slot),
            Some(Arc::clone(&server)),
            activity,
        );
        (server, state)
    }

    async fn api_tabs(server: &StreamServer) -> Vec<Value> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", server.port()))
            .await
            .expect("api should connect");
        let request = format!(
            "GET /api/tabs HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            server.port()
        );
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("[]");
        serde_json::from_str(body).unwrap_or_default()
    }

    fn titled(request: &Value) -> FakeReply {
        match request["method"].as_str() {
            Some("script.callFunction") => FakeReply::Result(json!({
                "type": "success",
                "result": {"type": "string", "value": "Title"}
            })),
            _ => FakeReply::Result(json!({})),
        }
    }

    #[tokio::test]
    async fn api_tabs_lists_firefox_tabs_without_a_dashboard_client() {
        let (mut ff, _events, _) = fake_session_with_replies(|request| {
            if request["method"] == "browsingContext.getTree" {
                FakeReply::Result(json!({"contexts": [
                    {"context": "ctx-active", "url": "https://one.test/"},
                    {"context": "ctx-other", "url": "https://two.test/"}
                ]}))
            } else {
                titled(request)
            }
        })
        .await;
        ff.contexts
            .record(&json!({"context": "ctx-other", "url": "https://two.test/"}));
        let (server, mut state) = firefox_http_server("firefox-api-tabs").await;
        state.engine = "firefox".into();
        state.firefox = Some(ff);
        state.update_stream_client().await;

        let tabs = api_tabs(&server).await;

        assert_eq!(tabs.len(), 2, "{tabs:?}");
        assert_eq!(tabs[0]["tabId"], json!("t1"));
        assert_eq!(tabs[0]["active"], json!(true));
        assert_eq!(tabs[1]["url"], json!("https://two.test/"));
        assert_eq!(server.cached_tabs().await, tabs);
        server.shutdown().await;
    }

    #[tokio::test]
    async fn api_tabs_replaces_stale_chrome_tabs_after_switching_to_firefox() {
        let (ff, _events, _) = fake_session_with_replies(|request| {
            if request["method"] == "browsingContext.getTree" {
                FakeReply::Result(json!({
                    "contexts": [{"context": "ctx-active", "url": "https://firefox.test/"}]
                }))
            } else {
                titled(request)
            }
        })
        .await;
        let (server, mut state) = firefox_http_server("firefox-stale-tabs").await;
        server
            .broadcast_tabs(&[json!({
                "tabId": "t1",
                "targetId": "chrome-tab",
                "url": "https://chrome.test/"
            })])
            .await;

        state.engine = "firefox".into();
        state.firefox = Some(ff);
        state.update_stream_client().await;

        let tabs = api_tabs(&server).await;

        assert_eq!(tabs.len(), 1, "{tabs:?}");
        assert_eq!(tabs[0]["url"], json!("https://firefox.test/"));
        assert!(tabs
            .iter()
            .all(|tab| tab["targetId"] != json!("chrome-tab")));
        server.shutdown().await;
    }

    #[tokio::test]
    async fn api_tabs_reflects_new_popup_tabs_without_a_dashboard_client() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let popup_open = Arc::new(AtomicBool::new(false));
        let opened = Arc::clone(&popup_open);
        let (ff, events, _) = fake_session_with_replies(move |request| {
            if request["method"] != "browsingContext.getTree" {
                return titled(request);
            }
            let mut contexts = vec![json!({"context": "ctx-active", "url": "https://one.test/"})];
            if opened.load(Ordering::Relaxed) {
                contexts.push(json!({"context": "ctx-popup", "url": "https://popup.test/"}));
            }
            FakeReply::Result(json!({ "contexts": contexts }))
        })
        .await;
        let (server, mut state) = firefox_http_server("firefox-popup-tabs").await;
        state.engine = "firefox".into();
        state.firefox = Some(ff);
        state.update_stream_client().await;
        assert_eq!(api_tabs(&server).await.len(), 1);

        popup_open.store(true, Ordering::Relaxed);
        events
            .send(json!({
                "type": "event",
                "method": "browsingContext.contextCreated",
                "params": {"context": "ctx-popup", "url": "https://popup.test/", "parent": null}
            }))
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        let mut tabs = api_tabs(&server).await;
        while tabs.len() < 2 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tabs = api_tabs(&server).await;
        }

        assert_eq!(tabs.len(), 2, "{tabs:?}");
        assert!(tabs.iter().any(|tab| tab["targetId"] == json!("ctx-popup")));
        server.shutdown().await;
    }

    #[test]
    fn firefox_input_actions_handle_raw_key_down_and_clipboard_messages() {
        // Navigation keys emitted as rawKeyDown must translate to BiDi keyDown.
        let (nav, held) = input_actions(
            "input_keyboard",
            &json!({ "eventType": "rawKeyDown", "key": "ArrowLeft", "modifiers": 0 }),
            0,
        )
        .expect("rawKeyDown ArrowLeft should map to BiDi actions");
        assert_eq!(held, 0);
        assert_eq!(nav[0]["actions"][0]["type"], "keyDown");

        // Modifier-only rawKeyDown updates held modifier mask.
        let (_, held_ctrl) = input_actions(
            "input_keyboard",
            &json!({ "eventType": "rawKeyDown", "key": "Control", "modifiers": 2 }),
            0,
        )
        .expect("rawKeyDown Control should map to BiDi actions");
        assert_eq!(held_ctrl, 2);

        // Shortcut rawKeyDown (Ctrl+C) presses modifier prefix when not already held.
        let (copy, held_copy) = input_actions(
            "input_keyboard",
            &json!({ "eventType": "rawKeyDown", "key": "c", "modifiers": 2 }),
            0,
        )
        .expect("rawKeyDown Ctrl+C should map to BiDi actions");
        assert_eq!(held_copy, 2);
        assert_eq!(copy[0]["actions"].as_array().unwrap().len(), 2);

        // input_insert_text must never be translated to WebDriver key actions
        // (which treat PUA code points such as U+E007/U+E004 as Enter/Tab).
        assert!(input_actions("input_insert_text", &json!({ "text": "hi" }), 0).is_none());
        assert!(input_actions(
            "input_insert_text",
            &json!({ "text": "\u{E007}\u{E004}\n" }),
            0
        )
        .is_none());
        let params = insert_text_call_params("ctx-1", "a\u{E007}\u{E004}\nb")
            .expect("insert_text_call_params should accept literal PUA and newline text");
        assert_eq!(params["target"]["context"], "ctx-1");
        assert_eq!(params["arguments"][0]["type"], "string");
        assert_eq!(params["arguments"][0]["value"], "a\u{E007}\u{E004}\nb");
        assert!(insert_text_call_params("ctx-1", "").is_none());

        // input_read_clipboard with cut: true dispatches the cut keystroke in Firefox.
        let (cut, held_cut) = input_actions(
            "input_read_clipboard",
            &json!({ "cut": true, "modifiers": 2 }),
            0,
        )
        .expect("cut should map to BiDi actions");
        assert_eq!(held_cut, 2);
        assert!(cut[0]["actions"].as_array().unwrap().len() >= 3);
    }

    #[tokio::test]
    async fn firefox_dispatch_insert_text_uses_script_call_function_without_key_actions() {
        let (ff, events, calls) = fake_session().await;
        std::mem::forget(events);
        let live = FirefoxLive {
            bidi: ff.bidi,
            context: "ctx-form".to_string(),
            contexts: ff.contexts,
            modifiers: Default::default(),
        };
        dispatch_input(
            &live,
            "input_insert_text",
            &json!({ "text": "hello\u{E007}\u{E004}\nworld" }),
        )
        .await;
        assert_eq!(
            wait_for(&calls, "script.callFunction", 1, Duration::from_secs(2)).await,
            1
        );
        assert_eq!(asked(&calls, "input.performActions"), 0);
    }
}
