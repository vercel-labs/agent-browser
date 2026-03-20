use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio_tungstenite::tungstenite::Message;

use super::cdp::client::CdpClient;

/// Frame metadata from CDP Page.screencastFrame events.
#[derive(Debug, Clone)]
pub struct FrameMetadata {
    pub offset_top: f64,
    pub page_scale_factor: f64,
    pub device_width: u32,
    pub device_height: u32,
    pub scroll_offset_x: f64,
    pub scroll_offset_y: f64,
    pub timestamp: u64,
}

impl Default for FrameMetadata {
    fn default() -> Self {
        Self {
            offset_top: 0.0,
            page_scale_factor: 1.0,
            device_width: 1280,
            device_height: 720,
            scroll_offset_x: 0.0,
            scroll_offset_y: 0.0,
            timestamp: 0,
        }
    }
}

/// Screencast configuration read from AGENT_BROWSER_STREAM_* environment variables.
#[derive(Debug, Clone)]
pub struct ScreencastConfig {
    pub format: String,
    pub quality: i32,
    pub max_width: i32,
    pub max_height: i32,
}

impl Default for ScreencastConfig {
    fn default() -> Self {
        Self {
            format: std::env::var("AGENT_BROWSER_STREAM_FORMAT")
                .ok()
                .filter(|s| s == "jpeg" || s == "png")
                .unwrap_or_else(|| "jpeg".to_string()),
            quality: std::env::var("AGENT_BROWSER_STREAM_QUALITY")
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(80),
            max_width: std::env::var("AGENT_BROWSER_STREAM_MAX_WIDTH")
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(1280),
            max_height: std::env::var("AGENT_BROWSER_STREAM_MAX_HEIGHT")
                .ok()
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(720),
        }
    }
}

pub struct StreamServer {
    port: u16,
    frame_tx: broadcast::Sender<String>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    /// The active CDP page session ID (from Target.attachToTarget).
    cdp_session_id: Arc<RwLock<Option<String>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    screencast_config: Arc<ScreencastConfig>,
}

impl StreamServer {
    pub async fn start(
        preferred_port: u16,
        client: Arc<CdpClient>,
        session_id: String,
    ) -> Result<Self, String> {
        let client_slot = Arc::new(RwLock::new(Some(client)));
        let (server, _) = Self::start_inner(preferred_port, client_slot, session_id).await?;
        Ok(server)
    }

    /// Start the stream server without a CDP client (e.g. at daemon startup before browser launch).
    /// Returns the server and a shared slot to set the client when the browser launches.
    /// Input messages are ignored until the client is set.
    pub async fn start_without_client(
        preferred_port: u16,
        session_id: String,
    ) -> Result<(Self, Arc<RwLock<Option<Arc<CdpClient>>>>), String> {
        let client_slot = Arc::new(RwLock::new(None::<Arc<CdpClient>>));
        Self::start_inner(preferred_port, client_slot, session_id).await
    }

    /// Notify the background CDP listener that the client has changed (browser launched/closed).
    pub fn notify_client_changed(&self) {
        self.client_notify.notify_one();
    }

    /// Update the active CDP page session ID used for screencast commands.
    pub async fn set_cdp_session_id(&self, session_id: Option<String>) {
        let mut guard = self.cdp_session_id.write().await;
        *guard = session_id;
    }

    /// Check whether the server currently has active screencast running.
    pub async fn is_screencasting(&self) -> bool {
        *self.screencasting.lock().await
    }

    async fn start_inner(
        preferred_port: u16,
        client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
        _session_id: String,
    ) -> Result<(Self, Arc<RwLock<Option<Arc<CdpClient>>>>), String> {
        let addr = format!("127.0.0.1:{}", preferred_port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| format!("Failed to bind stream server: {}", e))?;

        let actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get stream address: {}", e))?;
        let port = actual_addr.port();

        let (frame_tx, _) = broadcast::channel::<String>(64);
        let client_count = Arc::new(Mutex::new(0usize));
        let client_notify = Arc::new(Notify::new());
        let screencasting = Arc::new(Mutex::new(false));
        let cdp_session_id = Arc::new(RwLock::new(None::<String>));
        let screencast_config = Arc::new(ScreencastConfig::default());

        let frame_tx_clone = frame_tx.clone();
        let client_count_clone = client_count.clone();
        let client_slot_clone = client_slot.clone();
        let notify_clone = client_notify.clone();
        let screencasting_clone = screencasting.clone();
        let cdp_session_clone = cdp_session_id.clone();
        let config_clone = screencast_config.clone();

        // WebSocket accept loop
        tokio::spawn(async move {
            accept_loop(
                listener,
                frame_tx_clone,
                client_count_clone,
                client_slot_clone,
                notify_clone,
                screencasting_clone,
                cdp_session_clone,
                config_clone,
            )
            .await;
        });

        // Background CDP event listener for real-time frame broadcasting
        let frame_tx_bg = frame_tx.clone();
        let client_slot_bg = client_slot.clone();
        let client_notify_bg = client_notify.clone();
        let screencasting_bg = screencasting.clone();
        let client_count_bg = client_count.clone();
        let cdp_session_bg = cdp_session_id.clone();
        let config_bg = screencast_config.clone();
        tokio::spawn(async move {
            cdp_event_loop(
                frame_tx_bg,
                client_slot_bg,
                client_notify_bg,
                screencasting_bg,
                client_count_bg,
                cdp_session_bg,
                config_bg,
            )
            .await;
        });

        Ok((
            Self {
                port,
                frame_tx,
                client_count,
                client_slot: client_slot.clone(),
                cdp_session_id,
                client_notify,
                screencasting,
                screencast_config,
            },
            client_slot,
        ))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Broadcast a raw frame string (legacy).
    pub fn broadcast_frame(&self, frame_json: &str) {
        let _ = self.frame_tx.send(frame_json.to_string());
    }

    /// Broadcast a screencast frame with structured metadata.
    pub fn broadcast_screencast_frame(&self, base64_data: &str, metadata: &FrameMetadata) {
        let msg = json!({
            "type": "frame",
            "data": base64_data,
            "metadata": {
                "offsetTop": metadata.offset_top,
                "pageScaleFactor": metadata.page_scale_factor,
                "deviceWidth": metadata.device_width,
                "deviceHeight": metadata.device_height,
                "scrollOffsetX": metadata.scroll_offset_x,
                "scrollOffsetY": metadata.scroll_offset_y,
                "timestamp": metadata.timestamp,
            }
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast a status message to all connected clients.
    pub fn broadcast_status(
        &self,
        connected: bool,
        screencasting: bool,
        viewport_width: u32,
        viewport_height: u32,
    ) {
        let msg = json!({
            "type": "status",
            "connected": connected,
            "screencasting": screencasting,
            "viewportWidth": viewport_width,
            "viewportHeight": viewport_height,
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast an error message to all connected clients.
    pub fn broadcast_error(&self, message: &str) {
        let msg = json!({
            "type": "error",
            "message": message,
        });
        let _ = self.frame_tx.send(msg.to_string());
    }
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop(
    listener: TcpListener,
    frame_tx: broadcast::Sender<String>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    screencast_config: Arc<ScreencastConfig>,
) {
    while let Ok((stream, addr)) = listener.accept().await {
        let frame_rx = frame_tx.subscribe();
        let client_count = client_count.clone();
        let client_slot = client_slot.clone();
        let client_notify = client_notify.clone();
        let screencasting = screencasting.clone();
        let cdp_session_id = cdp_session_id.clone();
        let screencast_config = screencast_config.clone();

        tokio::spawn(async move {
            handle_ws_client(
                stream,
                addr,
                frame_rx,
                client_count,
                client_slot,
                client_notify,
                screencasting,
                cdp_session_id,
                screencast_config,
            )
            .await;
        });
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
    screencast_config: Arc<ScreencastConfig>,
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

    // Send initial status (screencasting:false initially, matching 0.19.0)
    {
        let guard = client_slot.read().await;
        let connected = guard.is_some();
        let sc = *screencasting.lock().await;
        let status = json!({
            "type": "status",
            "connected": connected,
            "screencasting": sc,
            "viewportWidth": screencast_config.max_width,
            "viewportHeight": screencast_config.max_height,
        });
        let _ = ws_tx.send(Message::Text(status.to_string())).await;
    }

    // Notify the CDP event loop that a client connected (may trigger auto-start screencast)
    client_notify.notify_one();

    loop {
        tokio::select! {
            frame = frame_rx.recv() => {
                match frame {
                    Ok(data) => {
                        if ws_tx.send(Message::Text(data)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Slow consumer; skip missed frames and continue
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

    // Notify the CDP event loop that a client disconnected (may trigger auto-stop screencast)
    client_notify.notify_one();
}

/// Background task that subscribes to CDP events and broadcasts screencast frames in real-time.
/// Also handles auto-start/stop of screencast based on WebSocket client count.
async fn cdp_event_loop(
    frame_tx: broadcast::Sender<String>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    client_count: Arc<Mutex<usize>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    screencast_config: Arc<ScreencastConfig>,
) {
    loop {
        // Wait until we're notified of a client/connection change
        client_notify.notified().await;

        // Check if we have WS clients and a CDP client
        let count = *client_count.lock().await;
        let guard = client_slot.read().await;

        if count > 0 {
            if let Some(ref client) = *guard {
                // We have WS clients and a CDP client — start screencast and listen for frames
                let mut event_rx = client.subscribe();
                let client_arc = Arc::clone(client);
                drop(guard);

                // Get the CDP page session ID for targeted commands
                let session_id = cdp_session_id.read().await.clone();

                let _ = client_arc
                    .send_command(
                        "Page.startScreencast",
                        Some(json!({
                            "format": screencast_config.format,
                            "quality": screencast_config.quality,
                            "maxWidth": screencast_config.max_width,
                            "maxHeight": screencast_config.max_height,
                            "everyNthFrame": 1,
                        })),
                        session_id.as_deref(),
                    )
                    .await;

                {
                    let mut sc = screencasting.lock().await;
                    *sc = true;
                }

                // Broadcast screencasting:true status (matching 0.19.0 two-status sequence)
                let status = json!({
                    "type": "status",
                    "connected": true,
                    "screencasting": true,
                    "viewportWidth": screencast_config.max_width,
                    "viewportHeight": screencast_config.max_height,
                });
                let _ = frame_tx.send(status.to_string());

                // Process CDP events in real-time until client disconnects or CDP closes
                loop {
                    tokio::select! {
                        event = event_rx.recv() => {
                            match event {
                                Ok(evt) => {
                                    if evt.method == "Page.screencastFrame" {
                                        // Ack immediately (like 0.19.0)
                                        if let Some(sid) = evt.params.get("sessionId").and_then(|v| v.as_i64()) {
                                            let _ = client_arc.send_command(
                                                "Page.screencastFrameAck",
                                                Some(json!({ "sessionId": sid })),
                                                evt.session_id.as_deref(),
                                            ).await;
                                        }

                                        // Broadcast frame to WS clients
                                        if let Some(data) = evt.params.get("data").and_then(|v| v.as_str()) {
                                            let meta = evt.params.get("metadata");
                                            let msg = json!({
                                                "type": "frame",
                                                "data": data,
                                                "metadata": {
                                                    "offsetTop": meta.and_then(|m| m.get("offsetTop")).and_then(|v| v.as_f64()).unwrap_or(0.0),
                                                    "pageScaleFactor": meta.and_then(|m| m.get("pageScaleFactor")).and_then(|v| v.as_f64()).unwrap_or(1.0),
                                                    "deviceWidth": meta.and_then(|m| m.get("deviceWidth")).and_then(|v| v.as_u64()).unwrap_or(1280),
                                                    "deviceHeight": meta.and_then(|m| m.get("deviceHeight")).and_then(|v| v.as_u64()).unwrap_or(720),
                                                    "scrollOffsetX": meta.and_then(|m| m.get("scrollOffsetX")).and_then(|v| v.as_f64()).unwrap_or(0.0),
                                                    "scrollOffsetY": meta.and_then(|m| m.get("scrollOffsetY")).and_then(|v| v.as_f64()).unwrap_or(0.0),
                                                    "timestamp": meta.and_then(|m| m.get("timestamp")).and_then(|v| v.as_u64()).unwrap_or(0),
                                                }
                                            });
                                            let _ = frame_tx.send(msg.to_string());
                                        }
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        }
                        // Also check for notify (client count change or CDP client change)
                        _ = client_notify.notified() => {
                            let count = *client_count.lock().await;
                            let session_id = cdp_session_id.read().await.clone();
                            if count == 0 {
                                // All WS clients gone — stop screencast
                                let _ = client_arc
                                    .send_command_no_params("Page.stopScreencast", session_id.as_deref())
                                    .await;
                                let mut sc = screencasting.lock().await;
                                *sc = false;
                                break;
                            }
                            // Check if CDP client changed (browser closed/relaunched)
                            let client_changed = {
                                let guard = client_slot.read().await;
                                let same = guard
                                    .as_ref()
                                    .is_some_and(|c| Arc::ptr_eq(c, &client_arc));
                                !same
                            };
                            if client_changed {
                                // CDP client changed — stop our screencast and restart loop
                                let _ = client_arc
                                    .send_command_no_params("Page.stopScreencast", session_id.as_deref())
                                    .await;
                                let mut sc = screencasting.lock().await;
                                *sc = false;
                                // Re-notify so we pick up the new client in the outer loop
                                client_notify.notify_one();
                                break;
                            }
                        }
                    }
                }
            } else {
                drop(guard);
                // No CDP client yet — wait for next notification
            }
        } else {
            // No WS clients — if screencasting, stop it
            let was_screencasting = *screencasting.lock().await;
            if was_screencasting {
                if let Some(ref client) = *guard {
                    let session_id = cdp_session_id.read().await.clone();
                    let _ = client
                        .send_command_no_params("Page.stopScreencast", session_id.as_deref())
                        .await;
                }
                let mut sc = screencasting.lock().await;
                *sc = false;
            }
            drop(guard);
        }
    }
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
        "status" => {
            // Client requesting status -- handled via broadcast_status from the caller
        }
        _ => {}
    }
}

pub fn is_allowed_origin(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) => {
            if o.starts_with("file://") {
                return true;
            }
            if let Ok(url) = url::Url::parse(o) {
                let host = url.host_str().unwrap_or("");
                host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]"
            } else {
                false
            }
        }
    }
}

pub async fn start_screencast(
    client: &CdpClient,
    session_id: &str,
    format: &str,
    quality: i32,
    max_width: i32,
    max_height: i32,
) -> Result<(), String> {
    client
        .send_command(
            "Page.startScreencast",
            Some(json!({
                "format": format,
                "quality": quality,
                "maxWidth": max_width,
                "maxHeight": max_height,
                "everyNthFrame": 1,
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

pub async fn stop_screencast(client: &CdpClient, session_id: &str) -> Result<(), String> {
    client
        .send_command_no_params("Page.stopScreencast", Some(session_id))
        .await?;
    Ok(())
}

pub async fn ack_screencast_frame(
    client: &CdpClient,
    session_id: &str,
    screencast_session_id: i64,
) -> Result<(), String> {
    client
        .send_command(
            "Page.screencastFrameAck",
            Some(json!({ "sessionId": screencast_session_id })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_origin_none() {
        assert!(is_allowed_origin(None));
    }

    #[test]
    fn test_allowed_origin_file() {
        assert!(is_allowed_origin(Some("file:///path/to/file")));
    }

    #[test]
    fn test_allowed_origin_localhost() {
        assert!(is_allowed_origin(Some("http://localhost:3000")));
        assert!(is_allowed_origin(Some("http://127.0.0.1:8080")));
    }

    #[test]
    fn test_disallowed_origin() {
        assert!(!is_allowed_origin(Some("http://evil.com")));
    }

    #[test]
    fn test_frame_metadata_default() {
        let meta = FrameMetadata::default();
        assert_eq!(meta.device_width, 1280);
        assert_eq!(meta.device_height, 720);
        assert_eq!(meta.page_scale_factor, 1.0);
    }
}
