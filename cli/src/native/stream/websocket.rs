use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch, Mutex, Notify, RwLock};
use tokio_tungstenite::tungstenite::Message;

use crate::native::cdp::client::CdpClient;

use super::http::handle_http_request;
use super::{is_allowed_origin, timestamp_ms};

#[allow(clippy::too_many_arguments)]
pub(super) async fn accept_loop(
    listener: TcpListener,
    frame_tx: broadcast::Sender<String>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    last_frame: Arc<RwLock<Option<String>>>,
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
                let client_count = client_count.clone();
                let client_slot = client_slot.clone();
                let client_notify = client_notify.clone();
                let screencasting = screencasting.clone();
                let cdp_session_id = cdp_session_id.clone();
                let vw = viewport_width.clone();
                let vh = viewport_height.clone();
                let lt = last_tabs.clone();
                let le = last_engine.clone();
                let lf = last_frame.clone();
                let rec = recording.clone();
                let shutdown_rx = shutdown_rx.clone();
                let sn = session_name.clone();

                tokio::spawn(async move {
                    handle_connection(
                        stream,
                        addr,
                        frame_tx,
                        client_count,
                        client_slot,
                        client_notify,
                        screencasting,
                        cdp_session_id,
                        vw,
                        vh,
                        lt,
                        le,
                        lf,
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
    stream: tokio::net::TcpStream,
    addr: SocketAddr,
    frame_tx: broadcast::Sender<String>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    last_frame: Arc<RwLock<Option<String>>>,
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
            client_count,
            client_slot,
            client_notify,
            screencasting,
            cdp_session_id,
            viewport_width,
            viewport_height,
            last_tabs,
            last_engine,
            last_frame,
            recording,
            shutdown_rx,
        )
        .await;
    } else {
        handle_http_request(stream, &buf[..n], &last_tabs, &last_engine, &session_name).await;
    }
}

#[allow(clippy::result_large_err, clippy::too_many_arguments)]
async fn handle_ws_client(
    stream: tokio::net::TcpStream,
    _addr: SocketAddr,
    mut frame_rx: broadcast::Receiver<String>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    last_frame: Arc<RwLock<Option<String>>>,
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

    let (mut ws_tx, mut ws_rx) = ws_stream.split();

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

        if let Some(ref cached) = *last_frame.read().await {
            let _ = ws_tx.send(Message::Text(cached.clone())).await;
        }
    }

    client_notify.notify_one();

    loop {
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    let _ = ws_tx.send(Message::Close(None)).await;
                    break;
                }
            }
            frame = frame_rx.recv() => {
                match frame {
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
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        let guard = client_slot.read().await;
                        if let Some(ref client) = *guard {
                            let sid = cdp_session_id.read().await;
                            handle_client_message(&text, client.as_ref(), sid.as_deref()).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }

    {
        let mut count = client_count.lock().await;
        *count = count.saturating_sub(1);
    }

    client_notify.notify_one();
}

async fn handle_client_message(msg: &str, client: &CdpClient, session_id: Option<&str>) {
    let parsed: Value = match serde_json::from_str(msg) {
        Ok(v) => v,
        Err(_) => return,
    };

    let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

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
