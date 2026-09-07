mod cdp_loop;
pub(crate) mod chat;
mod dashboard;
mod discovery;
mod http;
mod websocket;

pub use cdp_loop::{ack_screencast_frame, start_screencast, stop_screencast};
pub use dashboard::{
    is_valid_dashboard_access_token, normalize_dashboard_allowed_origins, run_dashboard_server,
};

use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch, Mutex, Notify, RwLock};

use super::cdp::client::CdpClient;

/// Screencast encoding for the live stream, from `AGENT_BROWSER_STREAM_*`.
///
/// Invariant: `max_width` and `max_height` are overrides, so `None` keeps the
/// session viewport. Defaulting them to a fixed size would downscale every
/// viewport larger than it.
///
/// No format knob here on purpose: a `frame` message carries no format field, so
/// a daemon-wide switch would leave connected clients decoding blind, and png
/// measures about 3x larger.
///
/// Not an invariant, though. `screencast_start` reconfigures the same shared
/// screencast, so an explicit png request does reach stream clients mid-flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreencastConfig {
    pub quality: i32,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
}

impl Default for ScreencastConfig {
    fn default() -> Self {
        Self {
            quality: 80,
            max_width: None,
            max_height: None,
        }
    }
}

impl ScreencastConfig {
    pub fn from_env() -> Self {
        Self::parse(
            std::env::var("AGENT_BROWSER_STREAM_QUALITY")
                .ok()
                .as_deref(),
            std::env::var("AGENT_BROWSER_STREAM_MAX_WIDTH")
                .ok()
                .as_deref(),
            std::env::var("AGENT_BROWSER_STREAM_MAX_HEIGHT")
                .ok()
                .as_deref(),
        )
    }

    /// Split from `from_env` so the parsing rules are testable without mutating
    /// process environment, which races across parallel tests.
    ///
    /// Invariant: an unset or unusable value leaves the default in place. A
    /// stream that silently stops is worse than one that ignores a typo.
    fn parse(quality: Option<&str>, max_width: Option<&str>, max_height: Option<&str>) -> Self {
        let d = Self::default();
        // CDP types these as signed, so a value past i32::MAX makes it reject
        // the whole command. The caller discards that error, so the stream
        // would stop with no frames and no signal.
        let dimension = |v: Option<&str>| {
            v.and_then(|s| s.trim().parse::<u32>().ok())
                .filter(|n| *n > 0 && *n <= i32::MAX as u32)
        };
        Self {
            // Clamped, since CDP accepts an out-of-range quality and ignores it,
            // which reads as "the setting does nothing".
            quality: quality
                .and_then(|s| s.trim().parse::<i32>().ok())
                .map(|q| q.clamp(0, 100))
                .unwrap_or(d.quality),
            max_width: dimension(max_width),
            max_height: dimension(max_height),
        }
    }
}

/// Invariant: frame ids are process-wide, never per server. A per-server
/// counter restarts on browser relaunch, and an ack pacing client that banked a
/// higher id would never receive another frame.
static FRAME_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn next_frame_seq() -> u64 {
    FRAME_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// A serialized frame plus the id ack pacing waits on.
///
/// Invariant: `seq` equals the `"seq"` inside `json`. Assign it once and use it
/// for both, so the writer never reparses the payload. `None` means the frame
/// carries no id and the writer must not hold for an ack.
pub(super) struct StreamFrame {
    pub(super) seq: Option<u64>,
    pub(super) json: String,
}

/// Frame id inside an already-serialized frame. Only the legacy
/// `broadcast_frame` needs it, once per publish; every other producer knows the
/// id before serializing.
fn seq_in_serialized_frame(frame: &str) -> Option<u64> {
    serde_json::from_str::<Value>(frame)
        .ok()
        .and_then(|v| v.get("seq").and_then(|s| s.as_u64()))
}

/// Shared activity clock for daemon commands and dashboard input.
///
/// The timestamp lets the idle-shutdown path re-check activity after waiting
/// for a command to release the daemon state lock. The notification wakes the
/// timer promptly for ordinary command and dashboard activity.
pub(crate) struct IdleActivity {
    last: std::sync::Mutex<Instant>,
    notify: Notify,
}

impl IdleActivity {
    pub(crate) fn new() -> Self {
        Self {
            last: std::sync::Mutex::new(Instant::now()),
            notify: Notify::new(),
        }
    }

    pub(crate) fn mark(&self) {
        *self.last.lock().unwrap_or_else(|err| err.into_inner()) = Instant::now();
        self.notify.notify_one();
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.last
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .elapsed()
    }

    pub(crate) async fn notified(&self) {
        self.notify.notified().await;
    }
}

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
    screencast_config: Arc<ScreencastConfig>,
    /// Latest-value channel for screencast frames. Slow clients skip straight
    /// to the newest frame instead of draining a stale ordered backlog.
    frame_watch: watch::Sender<Option<Arc<StreamFrame>>>,
    client_count: Arc<Mutex<usize>>,
    client_slot: Arc<RwLock<Option<Arc<CdpClient>>>>,
    /// The active CDP page session ID (from Target.attachToTarget).
    cdp_session_id: Arc<RwLock<Option<String>>>,
    client_notify: Arc<Notify>,
    /// Shared activity clock used by the daemon idle timer.
    idle_activity: Arc<IdleActivity>,
    screencasting: Arc<Mutex<bool>>,
    viewport_width: Arc<Mutex<u32>>,
    viewport_height: Arc<Mutex<u32>>,
    last_tabs: Arc<RwLock<Vec<Value>>>,
    last_engine: Arc<RwLock<String>>,
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
        let idle_activity = Arc::new(IdleActivity::new());
        let (server, _) =
            Self::start_inner(preferred_port, client_slot, session_id, true, idle_activity).await?;
        Ok(server)
    }

    /// Start the stream server without a CDP client.
    /// Returns the server and a shared slot to set the client when the browser launches.
    /// Input messages are ignored until the client is set.
    /// When `allow_port_fallback` is true, binding to an occupied port falls back to an
    /// OS-assigned port (used by daemon startup). When false, the error propagates
    /// (used by the runtime `stream_enable` command).
    /// `idle_activity` is daemon-owned so replacement servers reset the same idle timer.
    pub async fn start_without_client(
        preferred_port: u16,
        session_id: String,
        allow_port_fallback: bool,
        idle_activity: Arc<IdleActivity>,
    ) -> Result<(Self, Arc<RwLock<Option<Arc<CdpClient>>>>), String> {
        let client_slot = Arc::new(RwLock::new(None::<Arc<CdpClient>>));
        Self::start_inner(
            preferred_port,
            client_slot,
            session_id,
            allow_port_fallback,
            idle_activity,
        )
        .await
    }

    /// Notify the background CDP listener that the client has changed (browser launched/closed).
    pub fn notify_client_changed(&self) {
        self.client_notify.notify_one();
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
        idle_activity: Arc<IdleActivity>,
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
        let (frame_watch_tx, frame_watch_rx) = watch::channel::<Option<Arc<StreamFrame>>>(None);
        let screencast_config = Arc::new(ScreencastConfig::from_env());
        let client_count = Arc::new(Mutex::new(0usize));
        let client_notify = Arc::new(Notify::new());
        let screencasting = Arc::new(Mutex::new(false));
        let cdp_session_id = Arc::new(RwLock::new(None::<String>));
        let viewport_width = Arc::new(Mutex::new(1280u32));
        let viewport_height = Arc::new(Mutex::new(720u32));
        let last_tabs = Arc::new(RwLock::new(Vec::<Value>::new()));
        let last_engine = Arc::new(RwLock::new("chrome".to_string()));
        let recording = Arc::new(Mutex::new(false));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let frame_tx_clone = frame_tx.clone();
        let client_count_clone = client_count.clone();
        let client_slot_clone = client_slot.clone();
        let notify_clone = client_notify.clone();
        let idle_activity_clone = idle_activity.clone();
        let screencasting_clone = screencasting.clone();
        let cdp_session_clone = cdp_session_id.clone();

        let vw_clone = viewport_width.clone();
        let vh_clone = viewport_height.clone();
        let last_tabs_clone = last_tabs.clone();
        let last_engine_clone = last_engine.clone();
        let recording_clone = recording.clone();
        let accept_shutdown_rx = shutdown_rx.clone();
        let session_name_clone = session_id.clone();
        let frame_watch_accept = frame_watch_rx.clone();
        let accept_task = tokio::spawn(async move {
            websocket::accept_loop(
                listener,
                frame_tx_clone,
                frame_watch_accept,
                client_count_clone,
                client_slot_clone,
                notify_clone,
                idle_activity_clone,
                screencasting_clone,
                cdp_session_clone,
                vw_clone,
                vh_clone,
                last_tabs_clone,
                last_engine_clone,
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
        let last_tabs_bg = last_tabs.clone();
        let last_engine_bg = last_engine.clone();
        let recording_bg = recording.clone();
        let frame_watch_bg = frame_watch_tx.clone();
        let screencast_cfg_bg = screencast_config.clone();
        let cdp_task = tokio::spawn(async move {
            cdp_loop::cdp_event_loop(
                frame_tx_bg,
                frame_watch_bg,
                screencast_cfg_bg,
                client_slot_bg,
                client_notify_bg,
                screencasting_bg,
                client_count_bg,
                cdp_session_bg,
                vw_bg,
                vh_bg,
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
                frame_watch: frame_watch_tx,
                screencast_config,
                client_count,
                client_slot: client_slot.clone(),
                cdp_session_id,
                client_notify,
                idle_activity,
                screencasting,
                viewport_width,
                viewport_height,
                last_tabs,
                last_engine,
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

    /// Broadcast a raw frame string (legacy). The caller owns the payload, so
    /// the id is read back out of it. A payload without one is delivered but
    /// never held for an ack.
    pub fn broadcast_frame(&self, frame_json: &str) {
        self.frame_watch.send_replace(Some(Arc::new(StreamFrame {
            seq: seq_in_serialized_frame(frame_json),
            json: frame_json.to_string(),
        })));
    }

    /// Broadcast a screencast frame with structured metadata.
    pub fn broadcast_screencast_frame(&self, base64_data: &str, metadata: &FrameMetadata) {
        let seq = next_frame_seq();
        let msg = json!({
            "type": "frame",
            "seq": seq,
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
        self.frame_watch.send_replace(Some(Arc::new(StreamFrame {
            seq: Some(seq),
            json: msg.to_string(),
        })));
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

    pub async fn bind_cdp_session_and_broadcast_tabs(
        &self,
        session_id: Option<String>,
        tabs: &[Value],
    ) {
        let mut session_guard = self.cdp_session_id.write().await;
        let mut tabs_guard = self.last_tabs.write().await;
        *session_guard = session_id;
        *tabs_guard = tabs.to_vec();
        let msg = json!({
            "type": "tabs",
            "tabs": tabs,
            "timestamp": timestamp_ms(),
        });
        let _ = self.frame_tx.send(msg.to_string());
        drop(tabs_guard);
        drop(session_guard);
        self.client_notify.notify_one();
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
    fn test_screencast_config_defaults_when_unset() {
        let c = ScreencastConfig::parse(None, None, None);
        assert_eq!(c, ScreencastConfig::default());
        // Dimensions stay unset so the session viewport wins.
        assert_eq!(c.max_width, None);
        assert_eq!(c.max_height, None);
    }

    #[test]
    fn test_screencast_config_reads_values() {
        let c = ScreencastConfig::parse(Some("30"), Some("640"), Some("360"));
        assert_eq!(c.quality, 30);
        assert_eq!(c.max_width, Some(640));
        assert_eq!(c.max_height, Some(360));
        // Whitespace is tolerated.
        assert_eq!(
            ScreencastConfig::parse(Some(" 55 "), None, None).quality,
            55
        );
    }

    /// CDP accepts an out-of-range quality and then ignores it, so the setting
    /// would look like it does nothing. Clamp instead.
    #[test]
    fn test_screencast_config_clamps_quality() {
        assert_eq!(
            ScreencastConfig::parse(Some("500"), None, None).quality,
            100
        );
        assert_eq!(ScreencastConfig::parse(Some("-5"), None, None).quality, 0);
        assert_eq!(ScreencastConfig::parse(Some("0"), None, None).quality, 0);
    }

    /// An unusable value leaves the default. A stream that silently stops is
    /// worse than one that ignores a typo.
    #[test]
    fn test_screencast_config_ignores_unusable_values() {
        let d = ScreencastConfig::default();
        assert_eq!(
            ScreencastConfig::parse(Some("high"), None, None).quality,
            d.quality
        );
        // Dropped, so the viewport is used: zero, negative, non-numeric, and
        // anything past i32::MAX, which CDP rejects outright.
        for bad in ["0", "-100", "wide", "4294967295", "2147483648"] {
            assert_eq!(
                ScreencastConfig::parse(None, Some(bad), Some(bad)).max_width,
                None
            );
            assert_eq!(
                ScreencastConfig::parse(None, Some(bad), Some(bad)).max_height,
                None
            );
        }
    }

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

    /// The legacy entry point reads the id out of its payload; no id means the
    /// writer never holds for an ack.
    #[test]
    fn test_seq_read_back_from_a_legacy_payload() {
        let frame = json!({"type": "frame", "seq": 7, "data": "abc"}).to_string();
        assert_eq!(seq_in_serialized_frame(&frame), Some(7));
        assert_eq!(
            seq_in_serialized_frame(&json!({"type": "frame"}).to_string()),
            None
        );
        assert_eq!(seq_in_serialized_frame("not json"), None);
    }

    #[test]
    fn test_frame_metadata_default() {
        let meta = FrameMetadata::default();
        assert_eq!(meta.device_width, 1280);
        assert_eq!(meta.device_height, 720);
        assert_eq!(meta.page_scale_factor, 1.0);
    }

    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    type WsClient = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn connect_client(port: u16) -> WsClient {
        connect_client_to(port, "/").await
    }

    async fn connect_client_to(port: u16, target: &str) -> WsClient {
        let (ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}{}", port, target))
                .await
                .expect("client connect");
        ws
    }

    /// Read until a "frame" arrives, skipping status and tabs. Timed out so a
    /// broken server fails instead of hanging.
    async fn next_frame(ws: &mut WsClient) -> Value {
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .expect("timed out waiting for frame")
                .expect("stream ended")
                .expect("ws error");
            if let Message::Text(text) = msg {
                let parsed: Value = serde_json::from_str(&text).expect("valid json");
                if parsed.get("type").and_then(|v| v.as_str()) == Some("frame") {
                    return parsed;
                }
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_new_client_receives_latest_frame_only() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t1".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        server.broadcast_frame(r#"{"type":"frame","data":"stale"}"#);
        server.broadcast_frame(r#"{"type":"frame","data":"fresh"}"#);

        let mut ws = connect_client(server.port()).await;
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("data").and_then(|v| v.as_str()), Some("fresh"));

        server.shutdown().await;
    }

    /// Guards the `StreamFrame` invariant: the id ack pacing waits on is the id
    /// the client sees on the wire. No other test reads both, so offsetting one
    /// of them is invisible to them and wedges ack pacing in production.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_ack_of_the_wire_id_releases_the_next_frame() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t14".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");
        let meta = FrameMetadata::default();

        let mut ws = connect_client_to(server.port(), "/?pacing=ack").await;
        server.broadcast_screencast_frame("aaa", &meta);
        let first = next_frame(&mut ws).await;
        let wire_seq = first
            .get("seq")
            .and_then(|v| v.as_u64())
            .expect("frame carries an id");

        server.broadcast_screencast_frame("bbb", &meta);
        expect_no_frame(&mut ws, 200).await;

        ws.send(Message::Text(format!(
            r#"{{"type":"ack","seq":{}}}"#,
            wire_seq
        )))
        .await
        .expect("send ack");
        let second = next_frame(&mut ws).await;
        assert_eq!(second.get("data").and_then(|v| v.as_str()), Some("bbb"));

        server.shutdown().await;
    }

    /// Teardown must leave no per-connection task alive. The connection task and
    /// its reader both hold a clone of the daemon activity clock, so its
    /// reference count is a liveness probe that needs no reading.
    ///
    /// Not reading is the point: reading un-parks a writer blocked in
    /// `ws_tx.send()`, which then exits on its own and hides whether anything
    /// owned it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_shutdown_leaves_no_connection_task_alive() {
        let idle = Arc::new(IdleActivity::new());
        let (server, _slot) =
            StreamServer::start_without_client(0, "t13".to_string(), true, idle.clone())
                .await
                .expect("server start");

        let connected_floor = Arc::strong_count(&idle);
        let _ws = connect_client(server.port()).await;

        // Park the writer: frames pile up and nothing is ever read.
        let big = "x".repeat(256 * 1024);
        for seq in 1..=20 {
            server.broadcast_frame(&format!(
                r#"{{"type":"frame","seq":{},"data":"{}"}}"#,
                seq, big
            ));
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            Arc::strong_count(&idle) > connected_floor,
            "a connected client should hold the activity clock"
        );

        let teardown = tokio::time::Instant::now();
        server.shutdown().await;
        let took = teardown.elapsed();
        assert!(
            took < std::time::Duration::from_secs(5),
            "teardown blocked {:?} on a client that is not reading",
            took
        );

        // Dropping the server releases its clone, so this test should be the
        // only holder left. Anything above one outlived teardown.
        drop(server);
        for _ in 0..100 {
            if Arc::strong_count(&idle) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            Arc::strong_count(&idle),
            1,
            "a per-connection task outlived shutdown and still holds the activity clock"
        );
    }

    /// The cached opening frame is charged against the cap like any other
    /// delivery. Asserted on the gap between the first two frames, because a
    /// rate averaged over seconds absorbs one mistimed frame.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_url_cap_charges_the_cached_opening_frame() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t11".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        server.broadcast_frame(r#"{"type":"frame","seq":1,"data":"seed"}"#);
        let mut ws = connect_client_to(server.port(), "/?maxFps=2").await;

        let seed = next_frame(&mut ws).await;
        assert_eq!(seed.get("seq").and_then(|v| v.as_u64()), Some(1));

        server.broadcast_frame(r#"{"type":"frame","seq":2,"data":"two"}"#);
        // 500ms cap: the next frame may not arrive inside 300ms of the seed.
        expect_no_frame(&mut ws, 300).await;
        let second = next_frame(&mut ws).await;
        assert_eq!(second.get("seq").and_then(|v| v.as_u64()), Some(2));

        server.shutdown().await;
    }

    /// A cap may not postpone a delivery that never happened, so a connection
    /// with no cached frame gets its first frame immediately.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_cap_does_not_delay_the_first_frame_without_a_cache() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t12".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client_to(server.port(), "/?maxFps=1").await;
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let sent_at = tokio::time::Instant::now();
        server.broadcast_frame(r#"{"type":"frame","seq":1,"data":"first"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(1));
        let waited = sent_at.elapsed();
        assert!(
            waited < std::time::Duration::from_millis(500),
            "first frame waited {:?} on a send that never happened",
            waited
        );

        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_max_fps_cap_skips_stale_frames() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t2".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client(server.port()).await;
        ws.send(Message::Text(r#"{"type":"config","maxFps":1}"#.to_string()))
            .await
            .expect("send config");
        // Let the reader task apply the config before frames start flowing.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        server.broadcast_frame(r#"{"type":"frame","data":"first"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("data").and_then(|v| v.as_str()), Some("first"));

        // Two frames inside one throttle window: the older one is skipped.
        server.broadcast_frame(r#"{"type":"frame","data":"skipped"}"#);
        server.broadcast_frame(r#"{"type":"frame","data":"latest"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("data").and_then(|v| v.as_str()), Some("latest"));

        server.shutdown().await;
    }

    /// Assert no frame arrives within `ms`. A plain `next_frame` would pass on
    /// the very behavior these tests forbid.
    async fn expect_no_frame(ws: &mut WsClient, ms: u64) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(ms);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return;
            }
            match tokio::time::timeout(remaining, ws.next()).await {
                Err(_) => return,
                Ok(Some(Ok(Message::Text(text)))) => {
                    let parsed: Value = serde_json::from_str(&text).expect("valid json");
                    assert_ne!(
                        parsed.get("type").and_then(|v| v.as_str()),
                        Some("frame"),
                        "frame delivered while the client owed an acknowledgement"
                    );
                }
                Ok(_) => {}
            }
        }
    }

    /// Ack pacing keeps one frame in flight, and frames produced while the
    /// client owes an ack collapse to the newest. Without it latest-frame-wins
    /// stops at the socket and a stalled client drains a stale backlog.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_ack_pacing_holds_one_frame_and_skips_to_newest() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t5".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client(server.port()).await;
        ws.send(Message::Text(
            r#"{"type":"config","pacing":"ack"}"#.to_string(),
        ))
        .await
        .expect("send config");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        server.broadcast_frame(r#"{"type":"frame","seq":1,"data":"one"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(1));

        // Two more frames while frame 1 is unacknowledged: neither may be sent.
        server.broadcast_frame(r#"{"type":"frame","seq":2,"data":"two"}"#);
        server.broadcast_frame(r#"{"type":"frame","seq":3,"data":"three"}"#);
        expect_no_frame(&mut ws, 300).await;

        // Acknowledging releases exactly one frame, and it is the newest, not
        // the next in order.
        ws.send(Message::Text(r#"{"type":"ack","seq":1}"#.to_string()))
            .await
            .expect("send ack");
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(3));

        server.shutdown().await;
    }

    /// Ack pacing declared on the URL covers the opening frame. A `config`
    /// message cannot: the cached frame is written before it arrives.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_url_ack_pacing_covers_the_opening_frame() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t10".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        server.broadcast_frame(r#"{"type":"frame","seq":1,"data":"seed"}"#);
        let mut ws = connect_client_to(server.port(), "/?pacing=ack").await;

        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(1));

        // Nothing else may follow while the seed is unacknowledged.
        server.broadcast_frame(r#"{"type":"frame","seq":2,"data":"two"}"#);
        server.broadcast_frame(r#"{"type":"frame","seq":3,"data":"three"}"#);
        expect_no_frame(&mut ws, 300).await;

        ws.send(Message::Text(r#"{"type":"ack","seq":1}"#.to_string()))
            .await
            .expect("send ack");
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(3));

        server.shutdown().await;
    }

    /// A client that acks an id ahead of the stream must not wedge itself. The
    /// watermark only moves forward, so a premature high ack would swallow every
    /// later ack and the writer would wait on a notification that never comes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_ack_ahead_of_the_stream_does_not_wedge_the_writer() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t9".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client(server.port()).await;
        ws.send(Message::Text(
            r#"{"type":"config","pacing":"ack"}"#.to_string(),
        ))
        .await
        .expect("send config");
        ws.send(Message::Text(r#"{"type":"ack","seq":9999999}"#.to_string()))
            .await
            .expect("send premature ack");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Delivery continues: each frame is covered by the watermark already
        // banked, so the writer never blocks on it.
        for (seq, data) in [(1, "one"), (2, "two"), (3, "three")] {
            server.broadcast_frame(&format!(
                r#"{{"type":"frame","seq":{},"data":"{}"}}"#,
                seq, data
            ));
            let frame = next_frame(&mut ws).await;
            assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(seq));
        }

        server.shutdown().await;
    }

    /// Returning to push pacing must release a frame that is still waiting on
    /// an acknowledgement the client will now never send.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_leaving_ack_pacing_releases_the_held_frame() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t6".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client(server.port()).await;
        ws.send(Message::Text(
            r#"{"type":"config","pacing":"ack"}"#.to_string(),
        ))
        .await
        .expect("send config");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        server.broadcast_frame(r#"{"type":"frame","seq":1,"data":"one"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(1));

        server.broadcast_frame(r#"{"type":"frame","seq":2,"data":"two"}"#);
        expect_no_frame(&mut ws, 200).await;

        ws.send(Message::Text(
            r#"{"type":"config","pacing":"push"}"#.to_string(),
        ))
        .await
        .expect("send config");
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(2));

        server.shutdown().await;
    }

    /// Frame ids keep climbing across server restarts. A per-server counter
    /// would restart at 1, and an ack pacing client that banked a higher id
    /// would treat every later frame as stale and stop receiving.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_frame_ids_survive_a_stream_server_restart() {
        let meta = FrameMetadata::default();

        let (first, _slot) = StreamServer::start_without_client(
            0,
            "t8".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");
        let mut ws = connect_client(first.port()).await;
        first.broadcast_screencast_frame("aaa", &meta);
        let before = next_frame(&mut ws)
            .await
            .get("seq")
            .and_then(|v| v.as_u64())
            .expect("frame carries an id");
        first.shutdown().await;

        let (second, _slot2) = StreamServer::start_without_client(
            0,
            "t8b".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server restart");
        let mut ws2 = connect_client(second.port()).await;
        second.broadcast_screencast_frame("bbb", &meta);
        let after = next_frame(&mut ws2)
            .await
            .get("seq")
            .and_then(|v| v.as_u64())
            .expect("frame carries an id");

        assert!(
            after > before,
            "frame id went backwards across a restart: {} then {}",
            before,
            after
        );

        second.shutdown().await;
    }

    /// A config message must not deadlock the reader against its own watch
    /// channel: a read guard held across `send_replace` silences the connection
    /// from the first config message onward.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_repeated_config_messages_keep_the_connection_live() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t7".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client(server.port()).await;
        for msg in [
            r#"{"type":"config","maxFps":30}"#,
            r#"{"type":"config","pacing":"ack"}"#,
            r#"{"type":"config","pacing":"push"}"#,
            r#"{"type":"config","maxFps":0}"#,
        ] {
            ws.send(Message::Text(msg.to_string()))
                .await
                .expect("send config");
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        server.broadcast_frame(r#"{"type":"frame","seq":9,"data":"live"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("seq").and_then(|v| v.as_u64()), Some(9));

        server.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_uncapped_client_receives_consecutive_frames() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t3".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client(server.port()).await;
        // Timed, because asserting only that both frames arrive passes at any
        // rate: a 1 fps default still delivers two, a second apart.
        let started = tokio::time::Instant::now();
        server.broadcast_frame(r#"{"type":"frame","data":"one"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("data").and_then(|v| v.as_str()), Some("one"));

        server.broadcast_frame(r#"{"type":"frame","data":"two"}"#);
        let frame = next_frame(&mut ws).await;
        assert_eq!(frame.get("data").and_then(|v| v.as_str()), Some("two"));

        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(250),
            "two frames took {:?}: delivery is being throttled, so the default is not uncapped",
            elapsed
        );

        server.shutdown().await;
    }

    /// Loosening a cap mid-stream takes effect immediately, rather than leaving
    /// the writer asleep on the deadline the old, slower rate computed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_loosening_cap_midstream_takes_effect_immediately() {
        let (server, _slot) = StreamServer::start_without_client(
            0,
            "t4".to_string(),
            true,
            Arc::new(IdleActivity::new()),
        )
        .await
        .expect("server start");

        let mut ws = connect_client(server.port()).await;
        ws.send(Message::Text(r#"{"type":"config","maxFps":1}"#.to_string()))
            .await
            .expect("send config");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // First frame is immediate and arms next_allowed = now + 1s (1 fps).
        server.broadcast_frame(r#"{"type":"frame","data":"a"}"#);
        assert_eq!(
            next_frame(&mut ws)
                .await
                .get("data")
                .and_then(|v| v.as_str()),
            Some("a")
        );

        // Loosen to uncapped, then send a frame. It must not wait out the
        // stale 1 fps deadline.
        ws.send(Message::Text(r#"{"type":"config","maxFps":0}"#.to_string()))
            .await
            .expect("send config uncapped");
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;

        let t0 = std::time::Instant::now();
        server.broadcast_frame(r#"{"type":"frame","data":"b"}"#);
        assert_eq!(
            next_frame(&mut ws)
                .await
                .get("data")
                .and_then(|v| v.as_str()),
            Some("b")
        );
        let elapsed = t0.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(300),
            "uncapped frame after loosening the cap stalled for {elapsed:?}"
        );

        server.shutdown().await;
    }
}
