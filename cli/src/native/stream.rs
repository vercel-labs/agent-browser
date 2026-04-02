use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch, Mutex, Notify, RwLock};
use tokio_tungstenite::tungstenite::Message;

use super::cdp::client::CdpClient;
use super::network;
#[cfg(windows)]
use crate::connection::get_port_for_session;
use crate::connection::get_socket_dir;
#[cfg(windows)]
use crate::connection::resolve_port;
use crate::install::get_dashboard_dir;

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

pub struct StreamServer {
    port: u16,
    session_name: String,
    frame_tx: broadcast::Sender<String>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    /// The active CDP page session ID (from Target.attachToTarget).
    cdp_session_id: Arc<RwLock<Option<String>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    dashboard_dir: Option<PathBuf>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    last_frame: Arc<RwLock<Option<String>>>,
    recording: Arc<Mutex<bool>>,
    shutdown_tx: watch::Sender<bool>,
    accept_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    cdp_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl StreamServer {
    pub async fn start(
        preferred_port: u16,
        client: Arc<CdpClient>,
        session_id: String,
    ) -> Result<Self, String> {
        let client_slot = Arc::new(RwLock::new(Some(client)));
        let (server, _) = Self::start_inner(preferred_port, client_slot, session_id, true).await?;
        Ok(server)
    }

    /// Start the stream server without a CDP client.
    /// Returns the server and a shared slot to set the client when the browser launches.
    /// Input messages are ignored until the client is set.
    /// When `allow_port_fallback` is true, binding to an occupied port falls back to an
    /// OS-assigned port (used by daemon startup). When false, the error propagates
    /// (used by the runtime `stream_enable` command).
    pub async fn start_without_client(
        preferred_port: u16,
        session_id: String,
        allow_port_fallback: bool,
    ) -> Result<(Self, Arc<RwLock<Option<Arc<CdpClient>>>>), String> {
        let client_slot = Arc::new(RwLock::new(None::<Arc<CdpClient>>));
        Self::start_inner(preferred_port, client_slot, session_id, allow_port_fallback).await
    }

    /// Resolve the dashboard directory if it exists.
    fn resolve_dashboard_dir() -> Option<PathBuf> {
        let dir = dirs::home_dir()?.join(".agent-browser").join("dashboard");
        if dir.join("index.html").exists() {
            Some(dir)
        } else {
            None
        }
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

    /// Update the stored viewport dimensions used by status messages and screencast.
    /// Also notifies the screencast event loop to restart with the new dimensions.
    pub async fn set_viewport(&self, width: u32, height: u32) {
        *self.viewport_width.lock().await = width;
        *self.viewport_height.lock().await = height;
        self.client_notify.notify_one();
    }

    /// Get the current viewport dimensions.
    pub async fn viewport(&self) -> (u32, u32) {
        let w = *self.viewport_width.lock().await;
        let h = *self.viewport_height.lock().await;
        (w, h)
    }

    /// Override the cached screencast state for explicit CLI start/stop commands.
    pub async fn set_screencasting(&self, active: bool) {
        let mut guard = self.screencasting.lock().await;
        *guard = active;
    }

    /// Update and broadcast the recording state.
    pub async fn set_recording(&self, active: bool, engine: &str) {
        *self.recording.lock().await = active;
        let connected = self.client_slot.read().await.is_some();
        let sc = *self.screencasting.lock().await;
        let (vw, vh) = self.viewport().await;
        self.broadcast_status(connected, sc, vw, vh, engine).await;
    }

    /// Shut down the accept loop and background CDP listener, releasing the bound port.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);

        if let Some(task) = self.accept_task.lock().await.take() {
            let _ = task.await;
        }
        if let Some(task) = self.cdp_task.lock().await.take() {
            let _ = task.await;
        }
    }

    async fn start_inner(
        preferred_port: u16,
        client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
        session_id: String,
        allow_port_fallback: bool,
    ) -> Result<(Self, Arc<RwLock<Option<Arc<CdpClient>>>>), String> {
        let addr = format!("127.0.0.1:{}", preferred_port);
        let listener = match TcpListener::bind(&addr).await {
            Ok(l) => l,
            Err(_) if allow_port_fallback && preferred_port != 0 => {
                TcpListener::bind("127.0.0.1:0")
                    .await
                    .map_err(|e| format!("Failed to bind stream server: {}", e))?
            }
            Err(e) => return Err(format!("Failed to bind stream server: {}", e)),
        };

        let actual_addr = listener
            .local_addr()
            .map_err(|e| format!("Failed to get stream address: {}", e))?;
        let port = actual_addr.port();

        let dashboard_dir = Self::resolve_dashboard_dir();

        let (frame_tx, _) = broadcast::channel::<String>(64);
        let client_count = Arc::new(Mutex::new(0usize));
        let client_notify = Arc::new(Notify::new());
        let screencasting = Arc::new(Mutex::new(false));
        let cdp_session_id = Arc::new(RwLock::new(None::<String>));
        let viewport_width = Arc::new(Mutex::new(1280u32));
        let viewport_height = Arc::new(Mutex::new(720u32));
        let last_tabs = Arc::new(RwLock::new(Vec::<Value>::new()));
        let last_engine = Arc::new(RwLock::new("chrome".to_string()));
        let last_frame = Arc::new(RwLock::new(None::<String>));
        let recording = Arc::new(Mutex::new(false));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let frame_tx_clone = frame_tx.clone();
        let client_count_clone = client_count.clone();
        let client_slot_clone = client_slot.clone();
        let notify_clone = client_notify.clone();
        let screencasting_clone = screencasting.clone();
        let cdp_session_clone = cdp_session_id.clone();

        let vw_clone = viewport_width.clone();
        let vh_clone = viewport_height.clone();
        let dashboard_dir_clone = dashboard_dir.clone();
        let last_tabs_clone = last_tabs.clone();
        let last_engine_clone = last_engine.clone();
        let last_frame_clone = last_frame.clone();
        let recording_clone = recording.clone();
        let accept_shutdown_rx = shutdown_rx.clone();
        let session_name_clone = session_id.clone();
        let accept_task = tokio::spawn(async move {
            accept_loop(
                listener,
                frame_tx_clone,
                client_count_clone,
                client_slot_clone,
                notify_clone,
                screencasting_clone,
                cdp_session_clone,
                vw_clone,
                vh_clone,
                dashboard_dir_clone,
                last_tabs_clone,
                last_engine_clone,
                last_frame_clone,
                recording_clone,
                accept_shutdown_rx,
                session_name_clone,
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
        let vw_bg = viewport_width.clone();
        let vh_bg = viewport_height.clone();
        let last_frame_bg = last_frame.clone();
        let last_tabs_bg = last_tabs.clone();
        let last_engine_bg = last_engine.clone();
        let recording_bg = recording.clone();
        let cdp_task = tokio::spawn(async move {
            cdp_event_loop(
                frame_tx_bg,
                client_slot_bg,
                client_notify_bg,
                screencasting_bg,
                client_count_bg,
                cdp_session_bg,
                vw_bg,
                vh_bg,
                last_frame_bg,
                last_tabs_bg,
                last_engine_bg,
                recording_bg,
                shutdown_rx,
            )
            .await;
        });

        Ok((
            Self {
                port,
                session_name: session_id,
                frame_tx,
                client_count,
                client_slot: client_slot.clone(),
                cdp_session_id,
                client_notify,
                screencasting,
                viewport_width,
                viewport_height,
                dashboard_dir,
                last_tabs,
                last_engine,
                last_frame,
                recording,
                shutdown_tx,
                accept_task: Mutex::new(Some(accept_task)),
                cdp_task: Mutex::new(Some(cdp_task)),
            },
            client_slot,
        ))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Broadcast a raw frame string (legacy).
    pub fn broadcast_frame(&self, frame_json: &str) {
        let s = frame_json.to_string();
        if let Ok(mut lf) = self.last_frame.try_write() {
            *lf = Some(s.clone());
        }
        let _ = self.frame_tx.send(s);
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
        let s = msg.to_string();
        if let Ok(mut lf) = self.last_frame.try_write() {
            *lf = Some(s.clone());
        }
        let _ = self.frame_tx.send(s);
    }

    /// Broadcast a status message to all connected clients.
    pub async fn broadcast_status(
        &self,
        connected: bool,
        screencasting: bool,
        viewport_width: u32,
        viewport_height: u32,
        engine: &str,
    ) {
        {
            let mut guard = self.last_engine.write().await;
            *guard = engine.to_string();
        }
        let rec = *self.recording.lock().await;
        let msg = json!({
            "type": "status",
            "connected": connected,
            "screencasting": screencasting,
            "viewportWidth": viewport_width,
            "viewportHeight": viewport_height,
            "engine": engine,
            "recording": rec,
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

    /// Broadcast a command event when a command begins executing.
    pub fn broadcast_command(&self, action: &str, id: &str, params: &Value) {
        let msg = json!({
            "type": "command",
            "action": action,
            "id": id,
            "params": params,
            "timestamp": timestamp_ms(),
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast a result event after a command finishes executing.
    pub fn broadcast_result(
        &self,
        id: &str,
        action: &str,
        success: bool,
        data: &Value,
        duration_ms: u64,
    ) {
        let msg = json!({
            "type": "result",
            "id": id,
            "action": action,
            "success": success,
            "data": data,
            "duration_ms": duration_ms,
            "timestamp": timestamp_ms(),
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast a console event from the browser.
    pub fn broadcast_console(&self, level: &str, text: &str, args: &[Value]) {
        let mut msg = json!({
            "type": "console",
            "level": level,
            "text": text,
            "timestamp": timestamp_ms(),
        });
        if !args.is_empty() {
            msg.as_object_mut()
                .unwrap()
                .insert("args".to_string(), Value::Array(args.to_vec()));
        }
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast a page error (uncaught exception) from the browser.
    pub fn broadcast_page_error(&self, text: &str, line: Option<i64>, column: Option<i64>) {
        let msg = json!({
            "type": "page_error",
            "text": text,
            "line": line,
            "column": column,
            "timestamp": timestamp_ms(),
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Broadcast the current tab list so the dashboard can render a tab bar.
    /// Also caches the list so newly connected WebSocket clients receive it immediately.
    pub async fn broadcast_tabs(&self, tabs: &[Value]) {
        {
            let mut guard = self.last_tabs.write().await;
            *guard = tabs.to_vec();
        }
        let msg = json!({
            "type": "tabs",
            "tabs": tabs,
            "timestamp": timestamp_ms(),
        });
        let _ = self.frame_tx.send(msg.to_string());
    }

    /// Whether the dashboard directory is available.
    pub fn has_dashboard(&self) -> bool {
        self.dashboard_dir.is_some()
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
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    dashboard_dir: Option<PathBuf>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    last_frame: Arc<RwLock<Option<String>>>,
    recording: Arc<Mutex<bool>>,
    mut shutdown_rx: watch::Receiver<bool>,
    session_name: String,
) {
    let dashboard_dir = dashboard_dir.map(Arc::from);
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
                let dd = dashboard_dir.clone();
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
                        dd,
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
    dashboard_dir: Option<Arc<PathBuf>>,
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
        handle_http_request(
            stream,
            &request,
            n,
            dashboard_dir.as_deref().map(|p| p.as_path()),
            &last_tabs,
            &last_engine,
            &session_name,
        )
        .await;
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

    // Send initial status with current viewport dimensions
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

        // Send the most recent screencast frame so new clients see content immediately
        if let Some(ref cached) = *last_frame.read().await {
            let _ = ws_tx.send(Message::Text(cached.clone())).await;
        }
    }

    // Notify the CDP event loop that a client connected (may trigger auto-start screencast)
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
#[allow(clippy::too_many_arguments)]
async fn cdp_event_loop(
    frame_tx: broadcast::Sender<String>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    client_notify: Arc<Notify>,
    screencasting: Arc<Mutex<bool>>,
    client_count: Arc<Mutex<usize>>,
    cdp_session_id: Arc<RwLock<Option<String>>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_frame: Arc<RwLock<Option<String>>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
    recording: Arc<Mutex<bool>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    loop {
        // Wait until we're notified of a client/connection change
        tokio::select! {
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    let session_id = cdp_session_id.read().await.clone();
                    if *screencasting.lock().await {
                        if let Some(ref client) = *client_slot.read().await {
                            let _ = client
                                .send_command_no_params("Page.stopScreencast", session_id.as_deref())
                                .await;
                        }
                        let mut sc = screencasting.lock().await;
                        *sc = false;
                    }
                    return;
                }
            }
            _ = client_notify.notified() => {}
        }

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

                // Use the current viewport dimensions for screencast
                let vw = *viewport_width.lock().await;
                let vh = *viewport_height.lock().await;

                let eng = last_engine.read().await.clone();
                let supports_screencast = eng == "chrome";

                if supports_screencast {
                    let _ = client_arc
                        .send_command(
                            "Page.startScreencast",
                            Some(json!({
                                "format": "jpeg",
                                "quality": 80,
                                "maxWidth": vw,
                                "maxHeight": vh,
                                "everyNthFrame": 1,
                            })),
                            session_id.as_deref(),
                        )
                        .await;
                }

                {
                    let mut sc = screencasting.lock().await;
                    *sc = supports_screencast;
                }

                // Broadcast connection status with current viewport
                let rec = *recording.lock().await;
                let status = json!({
                    "type": "status",
                    "connected": true,
                    "screencasting": supports_screencast,
                    "viewportWidth": vw,
                    "viewportHeight": vh,
                    "engine": eng,
                    "recording": rec,
                });
                let _ = frame_tx.send(status.to_string());

                // Process CDP events in real-time until client disconnects or CDP closes
                loop {
                    tokio::select! {
                        changed = shutdown_rx.changed() => {
                            if changed.is_err() || *shutdown_rx.borrow() {
                                if supports_screencast {
                                    let session_id = cdp_session_id.read().await.clone();
                                    let _ = client_arc
                                        .send_command_no_params("Page.stopScreencast", session_id.as_deref())
                                        .await;
                                }
                                let mut sc = screencasting.lock().await;
                                *sc = false;
                                return;
                            }
                        }
                        event = event_rx.recv() => {
                            match event {
                                Ok(evt) => {
                                    if evt.method == "Page.frameNavigated" {
                                        if let Some(frame) = evt.params.get("frame") {
                                            let is_main = frame
                                                .get("parentId")
                                                .and_then(|v| v.as_str())
                                                .is_none_or(|s| s.is_empty());
                                            if is_main {
                                                if let Some(url) = frame.get("url").and_then(|v| v.as_str()) {
                                                    // Update the cached tab list so the active tab URL is current
                                                    {
                                                        let mut tabs = last_tabs.write().await;
                                                        for tab in tabs.iter_mut() {
                                                            if tab.get("active").and_then(|v| v.as_bool()).unwrap_or(false) {
                                                                tab.as_object_mut().map(|o| o.insert("url".to_string(), json!(url)));
                                                            }
                                                        }
                                                    }
                                                    let msg = json!({
                                                        "type": "url",
                                                        "url": url,
                                                        "timestamp": timestamp_ms(),
                                                    });
                                                    let _ = frame_tx.send(msg.to_string());
                                                }
                                            }
                                        }
                                    } else if evt.method == "Page.screencastFrame" {
                                        if let Some(sid) = evt.params.get("sessionId").and_then(|v| v.as_i64()) {
                                            let _ = client_arc.send_command(
                                                "Page.screencastFrameAck",
                                                Some(json!({ "sessionId": sid })),
                                                evt.session_id.as_deref(),
                                            ).await;
                                        }

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
                                            let msg_str = msg.to_string();
                                            {
                                                let mut lf = last_frame.write().await;
                                                *lf = Some(msg_str.clone());
                                            }
                                            let _ = frame_tx.send(msg_str);
                                        }
                                    } else if evt.method == "Runtime.consoleAPICalled" {
                                        let level = evt.params.get("type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("log");
                                        let raw_args = evt.params.get("args")
                                            .and_then(|v| v.as_array())
                                            .cloned()
                                            .unwrap_or_default();
                                        let text = network::format_console_args(&raw_args);
                                        if !text.is_empty() {
                                            let mut msg = json!({
                                                "type": "console",
                                                "level": level,
                                                "text": text,
                                                "timestamp": timestamp_ms(),
                                            });
                                            if !raw_args.is_empty() {
                                                msg.as_object_mut().unwrap().insert(
                                                    "args".to_string(),
                                                    Value::Array(raw_args),
                                                );
                                            }
                                            let _ = frame_tx.send(msg.to_string());
                                        }
                                    } else if evt.method == "Runtime.exceptionThrown" {
                                        let text = evt.params.get("exceptionDetails")
                                            .and_then(|d| {
                                                d.get("exception")
                                                    .and_then(|e| e.get("description").and_then(|v| v.as_str()))
                                                    .or_else(|| d.get("text").and_then(|v| v.as_str()))
                                            })
                                            .unwrap_or("Unknown error");
                                        let line = evt.params.get("exceptionDetails")
                                            .and_then(|d| d.get("lineNumber").and_then(|v| v.as_i64()));
                                        let column = evt.params.get("exceptionDetails")
                                            .and_then(|d| d.get("columnNumber").and_then(|v| v.as_i64()));
                                        let msg = json!({
                                            "type": "page_error",
                                            "text": text,
                                            "line": line,
                                            "column": column,
                                            "timestamp": timestamp_ms(),
                                        });
                                        let _ = frame_tx.send(msg.to_string());
                                    }
                                }
                                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                                Err(broadcast::error::RecvError::Closed) => break,
                            }
                        }
                        // Also check for notify (client count change, CDP client change, session switch, or viewport change)
                        _ = client_notify.notified() => {
                            let count = *client_count.lock().await;
                            let new_session_id = cdp_session_id.read().await.clone();
                            if count == 0 {
                                if supports_screencast {
                                    let _ = client_arc
                                        .send_command_no_params("Page.stopScreencast", session_id.as_deref())
                                        .await;
                                }
                                let mut sc = screencasting.lock().await;
                                *sc = false;
                                break;
                            }
                            let client_changed = {
                                let guard = client_slot.read().await;
                                let same = guard
                                    .as_ref()
                                    .is_some_and(|c| Arc::ptr_eq(c, &client_arc));
                                !same
                            };
                            let session_changed = new_session_id != session_id;
                            let new_vw = *viewport_width.lock().await;
                            let new_vh = *viewport_height.lock().await;
                            let viewport_changed = new_vw != vw || new_vh != vh;
                            if client_changed || session_changed || viewport_changed {
                                if supports_screencast {
                                    let _ = client_arc
                                        .send_command_no_params("Page.stopScreencast", session_id.as_deref())
                                        .await;
                                }
                                let mut sc = screencasting.lock().await;
                                *sc = false;
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
        "status" => {
            // Client requesting status -- handled via broadcast_status from the caller
        }
        _ => {}
    }
}

const CORS_HEADERS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n";

/// Serve an HTTP request for dashboard static files or the fallback page.
async fn handle_http_request(
    mut stream: tokio::net::TcpStream,
    request: &str,
    peeked_len: usize,
    dashboard_dir: Option<&Path>,
    last_tabs: &Arc<RwLock<Vec<Value>>>,
    last_engine: &Arc<RwLock<String>>,
    session_name: &str,
) {
    let mut discard = vec![0u8; peeked_len];
    let _ = stream.read_exact(&mut discard).await;

    let first_line = request.lines().next().unwrap_or("");
    let method = first_line.split_whitespace().next().unwrap_or("GET");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    // Handle CORS preflight
    if method == "OPTIONS" {
        let response = format!(
            "HTTP/1.1 204 No Content\r\n{CORS_HEADERS}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    // Handle POST /api/sessions (spawn new session)
    if method == "POST" && path == "/api/sessions" {
        let body_str = extract_http_body(request).unwrap_or("");
        let result = spawn_session(body_str).await;
        let (status, resp_body) = match result {
            Ok(msg) => ("200 OK", msg),
            Err(e) => (
                "400 Bad Request",
                format!(
                    r#"{{"success":false,"error":{}}}"#,
                    serde_json::to_string(&e).unwrap_or_else(|_| format!("\"{}\"", e))
                ),
            ),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
            resp_body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(resp_body.as_bytes()).await;
        return;
    }

    // Handle POST /api/command
    if method == "POST" && path == "/api/command" {
        let body = extract_http_body(request).unwrap_or("");
        let result = relay_command_to_daemon(session_name, body).await;
        let (status, resp_body) = match result {
            Ok(resp) => ("200 OK", resp),
            Err(e) => (
                "502 Bad Gateway",
                format!(
                    r#"{{"success":false,"error":{}}}"#,
                    serde_json::to_string(&e).unwrap_or_else(|_| format!("\"{}\"", e))
                ),
            ),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
            resp_body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(resp_body.as_bytes()).await;
        return;
    }

    let (status, content_type, body): (&str, &str, Vec<u8>) = if path == "/api/sessions" {
        (
            "200 OK",
            "application/json; charset=utf-8",
            discover_sessions().into_bytes(),
        )
    } else if path == "/api/tabs" {
        let tabs = last_tabs.read().await;
        (
            "200 OK",
            "application/json; charset=utf-8",
            serde_json::to_string(&*tabs)
                .unwrap_or_else(|_| "[]".to_string())
                .into_bytes(),
        )
    } else if path == "/api/status" {
        let engine = last_engine.read().await;
        (
            "200 OK",
            "application/json; charset=utf-8",
            format!(r#"{{"engine":"{}"}}"#, *engine).into_bytes(),
        )
    } else {
        match dashboard_dir {
            Some(dir) => serve_static_file(dir, path),
            None => (
                "200 OK",
                "text/html; charset=utf-8",
                DASHBOARD_NOT_INSTALLED_HTML.as_bytes().to_vec(),
            ),
        }
    };

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
        status,
        content_type,
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&body).await;
}

/// Extract the HTTP body from a raw request string (headers + body in one buffer).
fn extract_http_body(request: &str) -> Option<&str> {
    // Body starts after the first blank line (\r\n\r\n)
    request
        .find("\r\n\r\n")
        .map(|pos| &request[pos + 4..])
        .or_else(|| request.find("\n\n").map(|pos| &request[pos + 2..]))
}

/// Relay a command JSON body to the daemon and return the response.
async fn relay_command_to_daemon(session_name: &str, body: &str) -> Result<String, String> {
    let mut cmd: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;

    if cmd.get("id").is_none() {
        let id = format!(
            "dash-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        cmd["id"] = json!(id);
    }

    let mut json_str = serde_json::to_string(&cmd).map_err(|e| e.to_string())?;
    json_str.push('\n');

    #[cfg(unix)]
    let stream = {
        let socket_path = get_socket_dir().join(format!("{}.sock", session_name));
        tokio::net::UnixStream::connect(&socket_path)
            .await
            .map_err(|e| format!("Failed to connect to daemon: {}", e))?
    };

    #[cfg(windows)]
    let stream = {
        let port = resolve_port(session_name);
        tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .map_err(|e| format!("Failed to connect to daemon: {}", e))?
    };

    let (reader, mut writer) = tokio::io::split(stream);

    writer
        .write_all(json_str.as_bytes())
        .await
        .map_err(|e| format!("Failed to send command: {}", e))?;

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response_line = String::new();
    buf_reader
        .read_line(&mut response_line)
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    Ok(response_line.trim().to_string())
}

fn serve_static_file(dir: &Path, url_path: &str) -> (&'static str, &'static str, Vec<u8>) {
    let clean = url_path.trim_start_matches('/');
    let file_path = if clean.is_empty() {
        dir.join("index.html")
    } else {
        let joined = dir.join(clean);
        if joined.is_file() {
            joined
        } else {
            dir.join("index.html")
        }
    };

    match std::fs::read(&file_path) {
        Ok(content) => {
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let ct = match ext {
                "html" => "text/html; charset=utf-8",
                "js" => "application/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "json" => "application/json; charset=utf-8",
                "svg" => "image/svg+xml",
                "png" => "image/png",
                "ico" => "image/x-icon",
                _ => "application/octet-stream",
            };
            ("200 OK", ct, content)
        }
        Err(_) => (
            "404 Not Found",
            "text/html; charset=utf-8",
            b"<html><body><p>404 Not Found</p></body></html>".to_vec(),
        ),
    }
}

const DASHBOARD_NOT_INSTALLED_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>agent-browser</title>
<style>
body { font-family: system-ui, sans-serif; display: flex; justify-content: center; align-items: center; height: 100vh; margin: 0; background: #0a0a0a; color: #e5e5e5; }
.card { text-align: center; max-width: 400px; }
code { background: #262626; padding: 2px 8px; border-radius: 4px; font-size: 14px; }
</style>
</head>
<body>
<div class="card">
<h2>Dashboard not installed</h2>
<p>Run <code>agent-browser dashboard install</code> to download the dashboard.</p>
</div>
</body>
</html>"#;

/// Discover all active streaming sessions by reading `*.stream` files.
/// Stale entries (dead process) are removed on the fly.
fn discover_sessions() -> String {
    let dir = get_socket_dir();
    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(session) = name_str.strip_suffix(".stream") {
                if let Ok(port_str) = std::fs::read_to_string(entry.path()) {
                    if let Ok(port) = port_str.trim().parse::<u16>() {
                        let pid_path = dir.join(format!("{}.pid", session));
                        if is_process_alive(&pid_path) {
                            let engine_path = dir.join(format!("{}.engine", session));
                            let engine = std::fs::read_to_string(&engine_path)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or_else(|| "chrome".to_string());

                            let provider_path = dir.join(format!("{}.provider", session));
                            let provider = std::fs::read_to_string(&provider_path)
                                .ok()
                                .filter(|s| !s.trim().is_empty());

                            let extensions = read_extensions_metadata(&dir, session);

                            let mut entry = json!({
                                "session": session,
                                "port": port,
                                "engine": engine.trim(),
                            });
                            if let Some(ref p) = provider {
                                entry["provider"] = json!(p.trim());
                            }
                            if !extensions.is_empty() {
                                entry["extensions"] = json!(extensions);
                            }
                            sessions.push(entry);
                        } else {
                            let _ = std::fs::remove_file(entry.path());
                        }
                    }
                }
            }
        }
    }

    serde_json::to_string(&sessions).unwrap_or_else(|_| "[]".to_string())
}

fn read_extensions_metadata(dir: &std::path::Path, session: &str) -> Vec<Value> {
    let ext_path = dir.join(format!("{}.extensions", session));
    let ext_str = match std::fs::read_to_string(&ext_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    ext_str
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .filter_map(|path| {
            let manifest_path = std::path::Path::new(path).join("manifest.json");
            let manifest_str = std::fs::read_to_string(&manifest_path).ok()?;
            let manifest: Value = serde_json::from_str(&manifest_str).ok()?;

            let name = manifest
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let version = manifest
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = manifest
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let mut ext = json!({
                "name": name,
                "version": version,
                "path": path,
            });
            if let Some(desc) = description {
                ext["description"] = json!(desc);
            }
            Some(ext)
        })
        .collect()
}

fn is_process_alive(pid_path: &Path) -> bool {
    let pid_str = match std::fs::read_to_string(pid_path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        // On non-Unix, just check if the pid file exists
        true
    }
}

fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

/// Standalone dashboard HTTP server (no browser, no WebSocket streaming).
/// Serves static files and `/api/sessions` for session discovery.
pub async fn run_dashboard_server(port: u16) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind dashboard server on {}: {}", addr, e);
            return;
        }
    };

    let dashboard_dir: Arc<PathBuf> = Arc::from(get_dashboard_dir());

    loop {
        let Ok((stream, _addr)) = listener.accept().await else {
            break;
        };
        let dash_dir = dashboard_dir.clone();
        tokio::spawn(async move {
            handle_dashboard_connection(stream, dash_dir).await;
        });
    }
}

async fn handle_dashboard_connection(
    mut stream: tokio::net::TcpStream,
    dashboard_dir: Arc<PathBuf>,
) {
    use tokio::io::AsyncReadExt;

    let mut buf = vec![0u8; 8192];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let first_line = std::str::from_utf8(&buf[..n])
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .to_string();
    let method = first_line.split_whitespace().next().unwrap_or("GET");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    if method == "OPTIONS" {
        let response = format!(
            "HTTP/1.1 204 No Content\r\n{CORS_HEADERS}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    if method == "POST" && (path == "/api/sessions" || path == "/api/exec" || path == "/api/kill") {
        let body_str = read_post_body(&mut stream, &buf, n).await;
        let result = if path == "/api/exec" {
            exec_cli(&body_str).await
        } else if path == "/api/kill" {
            kill_session(&body_str).await
        } else {
            spawn_session(&body_str).await
        };
        let (status, resp_body) = match result {
            Ok(msg) => ("200 OK", msg),
            Err(e) => (
                "400 Bad Request",
                format!(
                    r#"{{"success":false,"error":{}}}"#,
                    serde_json::to_string(&e).unwrap_or_else(|_| format!("\"{}\"", e))
                ),
            ),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
            resp_body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(resp_body.as_bytes()).await;
        return;
    }

    let (status, content_type, body): (&str, &str, Vec<u8>) = if path == "/api/sessions" {
        (
            "200 OK",
            "application/json; charset=utf-8",
            discover_sessions().into_bytes(),
        )
    } else if dashboard_dir.join("index.html").exists() {
        serve_static_file(&dashboard_dir, path)
    } else {
        (
            "200 OK",
            "text/html; charset=utf-8",
            DASHBOARD_NOT_INSTALLED_HTML.as_bytes().to_vec(),
        )
    };

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
        status,
        content_type,
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&body).await;
}

/// Read the full POST body from a request. First checks if the body is already
/// present in the initial read buffer; if not, reads remaining bytes based on
/// Content-Length.
async fn read_post_body(stream: &mut tokio::net::TcpStream, initial: &[u8], n: usize) -> String {
    use tokio::io::AsyncReadExt;
    let header_str = String::from_utf8_lossy(&initial[..n]);
    let body = extract_http_body(&header_str).unwrap_or("").to_string();

    if !body.is_empty() {
        return body;
    }

    let cl = header_str
        .lines()
        .find_map(|l| {
            let lower = l.to_lowercase();
            lower
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
        })
        .unwrap_or(0);

    if cl > 0 {
        let mut remaining = vec![0u8; cl];
        if stream.read_exact(&mut remaining).await.is_ok() {
            return String::from_utf8_lossy(&remaining).to_string();
        }
    }

    String::new()
}

/// Execute an agent-browser CLI command and return JSON with stdout/stderr.
async fn exec_cli(body: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let args: Vec<String> = parsed
        .get("args")
        .and_then(|v| v.as_array())
        .ok_or("Missing \"args\" array")?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    if args.is_empty() {
        return Err("Empty args array".to_string());
    }

    let exe = std::env::current_exe().map_err(|e| format!("Cannot resolve executable: {}", e))?;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .arg("--json")
        .env_remove("AGENT_BROWSER_DASHBOARD")
        .env_remove("AGENT_BROWSER_DASHBOARD_PORT")
        .env_remove("AGENT_BROWSER_STREAM_PORT");

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to execute: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    Ok(json!({
        "success": output.status.success(),
        "exit_code": output.status.code(),
        "stdout": stdout,
        "stderr": stderr,
    })
    .to_string())
}

/// Kill a session daemon by sending SIGTERM, then SIGKILL if it survives.
/// Cleans up socket/pid/stream/engine files afterward.
async fn kill_session(body: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let session = parsed
        .get("session")
        .and_then(|v| v.as_str())
        .ok_or("Missing \"session\" field")?;

    if session.is_empty() || session.len() > 64 {
        return Err("Session name must be 1-64 characters".to_string());
    }

    let dir = get_socket_dir();
    let pid_path = dir.join(format!("{}.pid", session));

    let pid_str = std::fs::read_to_string(&pid_path)
        .map_err(|_| format!("No PID file for session '{}'", session))?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .map_err(|_| format!("Invalid PID in file: {}", pid_str.trim()))?;

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }

    for ext in &["pid", "sock", "stream", "engine", "extensions"] {
        let _ = std::fs::remove_file(dir.join(format!("{}.{}", session, ext)));
    }

    Ok(json!({ "success": true, "killed_pid": pid }).to_string())
}

/// Spawn a new session daemon from a POST /api/sessions request.
async fn spawn_session(body: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let session = parsed
        .get("session")
        .and_then(|v| v.as_str())
        .ok_or("Missing \"session\" field")?;

    if session.is_empty() || session.len() > 64 {
        return Err("Session name must be 1-64 characters".to_string());
    }

    let exe = std::env::current_exe().map_err(|e| format!("Cannot resolve executable: {}", e))?;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("open")
        .arg("about:blank")
        .arg("--session")
        .arg(session);

    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let status = cmd
        .status()
        .await
        .map_err(|e| format!("Failed to spawn session: {}", e))?;

    if status.success() {
        Ok(format!(
            r#"{{"success":true,"session":{}}}"#,
            serde_json::to_string(session).unwrap_or_default()
        ))
    } else {
        Err(format!("Session process exited with {}", status))
    }
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
