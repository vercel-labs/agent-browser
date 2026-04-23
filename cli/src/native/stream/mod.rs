mod cdp_loop;
pub(crate) mod chat;
mod dashboard;
mod discovery;
mod http;
mod websocket;

pub use cdp_loop::{ack_screencast_frame, start_screencast, stop_screencast};
pub use dashboard::run_dashboard_server;

use serde_json::{json, Value};
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch, Mutex, Notify, RwLock};

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

    /// Update the stored viewport dimensions and restart the active screencast (if any)
    /// so frames are captured at the new size.
    pub async fn set_viewport(&self, width: u32, height: u32) {
        let mut vw = self.viewport_width.lock().await;
        let mut vh = self.viewport_height.lock().await;
        if *vw == width && *vh == height {
            return;
        }
        *vw = width;
        *vh = height;
        drop(vw);
        drop(vh);
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
        let last_tabs_clone = last_tabs.clone();
        let last_engine_clone = last_engine.clone();
        let last_frame_clone = last_frame.clone();
        let recording_clone = recording.clone();
        let accept_shutdown_rx = shutdown_rx.clone();
        let session_name_clone = session_id.clone();
        let accept_task = tokio::spawn(async move {
            websocket::accept_loop(
                listener,
                frame_tx_clone,
                client_count_clone,
                client_slot_clone,
                notify_clone,
                screencasting_clone,
                cdp_session_clone,
                vw_clone,
                vh_clone,
                last_tabs_clone,
                last_engine_clone,
                last_frame_clone,
                recording_clone,
                accept_shutdown_rx,
                session_name_clone,
            )
            .await;
        });

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
            cdp_loop::cdp_event_loop(
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
}

pub(crate) fn timestamp_ms() -> u64 {
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
