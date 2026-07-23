use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch, Mutex, Notify, RwLock};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::native::cdp::client::CdpClient;

use super::http::handle_http_request;
use super::{is_allowed_origin, timestamp_ms};

/// Highest per-client frame rate a client may request via the `config` message.
const MAX_CONFIGURABLE_FPS: u32 = 120;

/// Earliest instant the next frame may be sent, given the last send and the
/// current cap. `fps == 0` (uncapped) returns `last_sent` itself, which is
/// already in the past, so the next frame is eligible immediately.
fn deadline_from(last_sent: Instant, fps: u32) -> Instant {
    if fps > 0 {
        last_sent + Duration::from_micros(1_000_000 / fps as u64)
    } else {
        last_sent
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn accept_loop(
    listener: TcpListener,
    frame_tx: broadcast::Sender<String>,
    frame_watch: watch::Receiver<Option<Arc<String>>>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    recording: Arc<Mutex<bool>>,
    mut shutdown_rx: watch::Receiver<bool>,
    session_name: String,
) {
    let session_name: Arc<str> = Arc::from(session_name);
    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break;
                }
            }
            accept_result = listener.accept() => {
                let Ok((stream, addr)) = accept_result else {
                    break;
                };
                let frame_tx = frame_tx.clone();
                let frame_watch = frame_watch.clone();
                let client_count = client_count.clone();
                let client_slot = client_slot.clone();
                let client_notify = client_notify.clone();
                let screencasting = screencasting.clone();
                let cdp_session_id = cdp_session_id.clone();
                let vw = viewport_width.clone();
                let vh = viewport_height.clone();
                let lt = last_tabs.clone();
                let le = last_engine.clone();
                let rec = recording.clone();
                let shutdown_rx = shutdown_rx.clone();
                let sn = session_name.clone();

                tokio::spawn(async move {
                    handle_connection(
                        stream,
                        addr,
                        frame_tx,
                        frame_watch,
                        client_count,
                        client_slot,
                        client_notify,
                        screencasting,
                        cdp_session_id,
                        vw,
                        vh,
                        lt,
                        le,
                        rec,
                        shutdown_rx,
                        sn,
                    )
                    .await;
                });
            }
        }
    }
}

fn is_websocket_upgrade(request: &str) -> bool {
    request.lines().any(|line| {
        if let Some((name, value)) = line.split_once(':') {
            name.trim().eq_ignore_ascii_case("upgrade")
                && value.trim().eq_ignore_ascii_case("websocket")
        } else {
            false
        }
    })
}

/// Peek at the TCP stream to dispatch between WebSocket upgrade and plain HTTP.
#[allow(clippy::too_many_arguments)]
async fn handle_connection(
    stream: TcpStream,
    addr: SocketAddr,
    frame_tx: broadcast::Sender<String>,
    frame_watch: watch::Receiver<Option<Arc<String>>>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    recording: Arc<Mutex<bool>>,
    shutdown_rx: watch::Receiver<bool>,
    session_name: Arc<str>,
) {
    let mut buf = [0u8; 4096];
    let n = match stream.peek(&mut buf).await {
        Ok(n) => n,
        Err(_) => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]);

    if is_websocket_upgrade(&request) {
        let frame_rx = frame_tx.subscribe();
        handle_ws_client(
            stream,
            addr,
            frame_rx,
            frame_watch,
            client_count,
            client_slot,
            client_notify,
            screencasting,
            cdp_session_id,
            viewport_width,
            viewport_height,
            last_tabs,
            last_engine,
            recording,
            shutdown_rx,
        )
        .await;
    } else {
        handle_http_request(stream, &buf[..n], &last_tabs, &last_engine, &session_name).await;
    }
}

/// Handles one WebSocket client with two independent halves:
/// a reader task that dispatches input to CDP immediately (never queued behind
/// frame writes), and a writer loop that forwards broadcast messages and
/// delivers screencast frames latest-first with an optional per-client
/// frame-rate cap.
#[allow(clippy::result_large_err, clippy::too_many_arguments)]
async fn handle_ws_client(
    stream: TcpStream,
    _addr: SocketAddr,
    mut broadcast_rx: broadcast::Receiver<String>,
    mut frame_watch: watch::Receiver<Option<Arc<String>>>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    recording: Arc<Mutex<bool>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let callback =
        |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
         resp: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let origin = req
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());
            if !is_allowed_origin(origin.as_deref()) {
                let mut reject =
                    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(Some(
                        "Origin not allowed".to_string(),
                    ));
                *reject.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
                return Err(reject);
            }
            Ok(resp)
        };

    let ws_stream = match tokio_tungstenite::accept_hdr_async(stream, callback).await {
        Ok(ws) => ws,
        Err(_) => return,
    };

    {
        let mut count = client_count.lock().await;
        *count += 1;
    }

    let (mut ws_tx, ws_rx) = ws_stream.split();

    {
        let guard = client_slot.read().await;
        let connected = guard.is_some();
        let sc = *screencasting.lock().await;
        let vw = *viewport_width.lock().await;
        let vh = *viewport_height.lock().await;
        let eng = last_engine.read().await.clone();
        let rec = *recording.lock().await;
        let status = json!({
            "type": "status",
            "connected": connected,
            "screencasting": sc,
            "viewportWidth": vw,
            "viewportHeight": vh,
            "engine": eng,
            "recording": rec,
        });
        let _ = ws_tx.send(Message::Text(status.to_string())).await;

        let tabs = last_tabs.read().await;
        if !tabs.is_empty() {
            let tabs_msg = json!({
                "type": "tabs",
                "tabs": *tabs,
                "timestamp": timestamp_ms(),
            });
            let _ = ws_tx.send(Message::Text(tabs_msg.to_string())).await;
        }
    }

    // Seed the client with the most recent frame, marking it seen so the
    // writer loop below does not immediately re-send the same frame.
    let initial_frame = frame_watch.borrow_and_update().clone();
    if let Some(frame) = initial_frame {
        let _ = ws_tx.send(Message::Text((*frame).clone())).await;
    }

    client_notify.notify_one();

    // 0 means uncapped; set by the client via {"type":"config","maxFps":N}.
    // A watch channel (not an atomic) so a mid-stream config change wakes the
    // writer's select! below, letting it re-derive its throttle deadline
    // immediately instead of riding out a stale one computed from the old rate.
    let (max_fps_tx, mut max_fps_rx) = watch::channel::<u32>(0);
    let mut reader_task = tokio::spawn(reader_loop(
        ws_rx,
        client_slot.clone(),
        cdp_session_id.clone(),
        max_fps_tx,
    ));

    // The throttle deadline is always derived from the last send plus the
    // current interval, so a config change recomputes it against `last_sent`
    // rather than against whatever rate was active when the last frame went out.
    let mut next_allowed = Instant::now();
    let mut last_sent = Instant::now();
    let mut pending_frame = false;

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    let _ = ws_tx.send(Message::Close(None)).await;
                    break;
                }
            }
            _ = &mut reader_task => {
                break;
            }
            msg = broadcast_rx.recv() => {
                match msg {
                    Ok(data) => {
                        if ws_tx.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            changed = frame_watch.changed(), if !pending_frame => {
                if changed.is_err() {
                    break;
                }
                pending_frame = true;
            }
            changed = max_fps_rx.changed() => {
                // The client changed its cap. The sender lives as long as the
                // reader task, so an error here means the reader ended: break
                // and let cleanup run (mirrors the reader_task arm).
                if changed.is_err() {
                    break;
                }
                let fps = *max_fps_rx.borrow_and_update();
                // Re-derive the deadline from the last send. Loosening the cap
                // (or going uncapped) pulls next_allowed into the past so a
                // pending frame goes out immediately instead of waiting on the
                // old, slower deadline.
                next_allowed = deadline_from(last_sent, fps);
            }
            _ = tokio::time::sleep_until(next_allowed), if pending_frame => {
                // Reading at send time (not at arrival time) is what makes this
                // latest-frame-wins: frames that arrived during the throttle
                // window are skipped, never queued.
                let frame = frame_watch.borrow_and_update().clone();
                pending_frame = false;
                if let Some(frame) = frame {
                    if ws_tx.send(Message::Text((*frame).clone())).await.is_err() {
                        break;
                    }
                }
                last_sent = Instant::now();
                next_allowed = deadline_from(last_sent, *max_fps_rx.borrow());
            }
        }
    }

    reader_task.abort();

    {
        let mut count = client_count.lock().await;
        *count = count.saturating_sub(1);
    }

    client_notify.notify_one();
}

/// Parse the `maxFps` value from a client `config` message, clamped to
/// `MAX_CONFIGURABLE_FPS`. Clamps on u64 before narrowing so oversized
/// values cap at the maximum instead of wrapping.
fn parse_config_max_fps(parsed: &Value) -> Option<u32> {
    parsed
        .get("maxFps")
        .and_then(|v| v.as_u64())
        .map(|fps| fps.min(MAX_CONFIGURABLE_FPS as u64) as u32)
}

/// Reads client messages and dispatches them without ever waiting on frame
/// delivery. Input events are forwarded to CDP sequentially to preserve
/// ordering (mouse move/press/release must not be reordered).
async fn reader_loop(
    mut ws_rx: SplitStream<WebSocketStream<TcpStream>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    max_fps: watch::Sender<u32>,
) {
    while let Some(msg) = ws_rx.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                let parsed: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if msg_type == "config" {
                    if let Some(fps) = parse_config_max_fps(&parsed) {
                        // send_replace (not send) so the writer is woken even
                        // when the value is unchanged; it must re-derive its
                        // deadline from the new rate on every config message.
                        let _ = max_fps.send_replace(fps);
                    }
                    continue;
                }
                let guard = client_slot.read().await;
                if let Some(ref client) = *guard {
                    let sid = cdp_session_id.read().await;
                    dispatch_input(msg_type, &parsed, client.as_ref(), sid.as_deref()).await;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            _ => {}
        }
    }
}

async fn dispatch_input(
    msg_type: &str,
    parsed: &Value,
    client: &CdpClient,
    session_id: Option<&str>,
) {
    match msg_type {
        "input_mouse" => {
            let _ = client
                .send_command(
                    "Input.dispatchMouseEvent",
                    Some(json!({
                        "type": parsed.get("eventType").and_then(|v| v.as_str()).unwrap_or("mouseMoved"),
                        "x": parsed.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "y": parsed.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "button": parsed.get("button").and_then(|v| v.as_str()).unwrap_or("none"),
                        "clickCount": parsed.get("clickCount").and_then(|v| v.as_i64()).unwrap_or(0),
                        "deltaX": parsed.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "deltaY": parsed.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        "modifiers": parsed.get("modifiers").and_then(|v| v.as_i64()).unwrap_or(0),
                    })),
                    session_id,
                )
                .await;
        }
        "input_keyboard" => {
            let _ = client
                .send_command(
                    "Input.dispatchKeyEvent",
                    Some(json!({
                        "type": parsed.get("eventType").and_then(|v| v.as_str()).unwrap_or("keyDown"),
                        "key": parsed.get("key"),
                        "code": parsed.get("code"),
                        "text": parsed.get("text"),
                        "windowsVirtualKeyCode": parsed.get("windowsVirtualKeyCode").and_then(|v| v.as_i64()).unwrap_or(0),
                        "modifiers": parsed.get("modifiers").and_then(|v| v.as_i64()).unwrap_or(0),
                    })),
                    session_id,
                )
                .await;
        }
        "input_touch" => {
            let _ = client
                .send_command(
                    "Input.dispatchTouchEvent",
                    Some(json!({
                        "type": parsed.get("eventType").and_then(|v| v.as_str()).unwrap_or("touchStart"),
                        "touchPoints": parsed.get("touchPoints").unwrap_or(&json!([])),
                        "modifiers": parsed.get("modifiers").and_then(|v| v.as_i64()).unwrap_or(0),
                    })),
                    session_id,
                )
                .await;
        }
        "status" => {}
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(v: serde_json::Value) -> Value {
        v
    }

    #[test]
    fn test_parse_config_max_fps_valid() {
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config", "maxFps": 10}))),
            Some(10)
        );
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config", "maxFps": 0}))),
            Some(0)
        );
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config", "maxFps": 120}))),
            Some(120)
        );
    }

    #[test]
    fn test_parse_config_max_fps_clamps_without_wrapping() {
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config", "maxFps": 500}))),
            Some(MAX_CONFIGURABLE_FPS)
        );
        // u32::MAX + 2 would wrap to 1 if narrowed before clamping.
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config", "maxFps": 4294967297u64}))),
            Some(MAX_CONFIGURABLE_FPS)
        );
    }

    #[test]
    fn test_parse_config_max_fps_invalid() {
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config"}))),
            None
        );
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config", "maxFps": -5}))),
            None
        );
        assert_eq!(
            parse_config_max_fps(&config(json!({"type": "config", "maxFps": "fast"}))),
            None
        );
    }
}
