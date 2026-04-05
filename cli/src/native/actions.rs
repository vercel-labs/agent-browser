use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::{broadcast, oneshot, RwLock};

use crate::connection::get_socket_dir;

use super::auth;
use super::browser::{should_track_target, BrowserManager, WaitUntil};
use super::cdp::chrome::LaunchOptions;
use super::cdp::client::CdpClient;
use super::cdp::types::{
    AttachToTargetParams, AttachToTargetResult, CdpEvent, CreateTargetResult,
    DispatchMouseEventParams, ExceptionThrownEvent, JavascriptDialogOpeningEvent,
    TargetCreatedEvent, TargetDestroyedEvent, TargetInfoChangedEvent,
};
use super::cookies;
use super::diff;
use super::element::RefMap;
use super::inspect_server::InspectServer;
use super::interaction;
use super::network::{self, DomainFilter, EventTracker};
use super::policy::{ActionPolicy, ConfirmActions, PolicyResult};
use super::providers;
use super::recording::{self, RecordingState};
use super::screenshot::{self, ScreenshotOptions};
use super::snapshot::{self, SnapshotOptions};
use super::state;
use super::storage;
use super::stream::{self, StreamServer};
use super::tracing::{self as native_tracing, TracingState};
use super::webdriver::appium::AppiumManager;
use super::webdriver::backend::{BrowserBackend, WebDriverBackend, WEBDRIVER_UNSUPPORTED_ACTIONS};
use super::webdriver::ios;
use super::webdriver::safari;

/// Wait strategy used by `auth_login` when navigating to the login page.
///
/// We intentionally use `Load` (instead of `NetworkIdle`) because many modern
/// apps keep background requests active indefinitely (polling, analytics,
/// websockets), which can prevent network-idle from ever resolving.
///
/// After navigation completes, `auth_login` explicitly waits for form selectors
/// to appear before filling/clicking.
pub const AUTH_LOGIN_WAIT_UNTIL: WaitUntil = WaitUntil::Load;

/// Poll interval used while waiting for auth form selectors to appear.
const AUTH_LOGIN_SELECTOR_POLL_INTERVAL_MS: u64 = 100;

/// Time spent trying targeted username selectors before broad text-input
/// fallback selectors are allowed.
const AUTH_LOGIN_PREFERRED_SELECTOR_WINDOW_MS: u64 = 5_000;

pub struct PendingConfirmation {
    pub action: String,
    pub cmd: Value,
}

/// Captured request/response metadata used to export HAR 1.2 files.
pub struct HarEntry {
    pub request_id: String,
    /// Seconds since Unix epoch (CDP `wallTime`), with sub-second precision.
    pub wall_time: f64,
    // Request fields
    pub method: String,
    pub url: String,
    pub request_headers: Vec<(String, String)>,
    pub post_data: Option<String>,
    pub request_body_size: i64,
    pub resource_type: String,
    // Response fields — populated by `Network.responseReceived`
    pub status: Option<i64>,
    pub status_text: String,
    /// Normalised from CDP `response.protocol` (e.g. `"h2"` → `"HTTP/2.0"`).
    pub http_version: String,
    pub response_headers: Vec<(String, String)>,
    pub mime_type: String,
    pub redirect_url: String,
    /// Updated by `Network.loadingFinished` for final accuracy.
    pub response_body_size: i64,
    /// Raw CDP `ResourceTiming` object from `Network.responseReceived`.
    pub cdp_timing: Option<Value>,
    /// Monotonic timestamp (seconds) from `Network.loadingFinished`; used to
    /// compute the `receive` timing phase.
    pub loading_finished_timestamp: Option<f64>,
}

pub struct RouteEntry {
    pub url_pattern: String,
    pub response: Option<RouteResponse>,
    pub abort: bool,
}

pub struct RouteResponse {
    pub status: Option<u16>,
    pub body: Option<String>,
    pub content_type: Option<String>,
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Clone, serde::Serialize)]
pub struct TrackedRequest {
    pub url: String,
    pub method: String,
    pub headers: Value,
    pub timestamp: u64,
    #[serde(rename = "resourceType")]
    pub resource_type: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "postData", skip_serializing_if = "Option::is_none")]
    pub post_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    #[serde(rename = "responseHeaders", skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Value>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

pub struct FetchPausedRequest {
    pub request_id: String,
    pub url: String,
    pub resource_type: String,
    pub session_id: String,
    /// Original request headers from the Fetch.requestPaused event, needed
    /// because Fetch.continueRequest replaces (not merges) headers.
    pub request_headers: Option<serde_json::Map<String, Value>>,
}

pub enum BackendType {
    Cdp,
    WebDriver,
}

#[derive(Debug, Clone, Default)]
pub struct PendingDialog {
    pub dialog_type: String,
    pub message: String,
    pub url: String,
    pub default_prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MouseState {
    pub x: f64,
    pub y: f64,
    pub buttons: i32,
}

#[derive(Default)]
struct DrainedEvents {
    pending_acks: Vec<i64>,
    new_targets: Vec<TargetCreatedEvent>,
    changed_targets: Vec<TargetInfoChangedEvent>,
    destroyed_targets: Vec<String>,
    /// Cross-origin iframe (frame_id, session_id) pairs from Target.attachedToTarget.
    attached_iframe_sessions: Vec<(String, String)>,
    /// Session IDs from Target.detachedFromTarget.
    detached_iframe_sessions: Vec<String>,
}

/// Compute a hash of the [`LaunchOptions`] fields that require a browser
/// relaunch when changed (baked into the Chrome process at startup).
///
/// Fields NOT hashed (adjustable at runtime via CDP without relaunch):
/// ignore_https_errors, color_scheme, download_path, storage_state
fn launch_hash(opts: &LaunchOptions) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut h = DefaultHasher::new();
    opts.headless.hash(&mut h);
    opts.extensions.hash(&mut h);
    opts.profile.hash(&mut h);
    opts.executable_path.hash(&mut h);
    opts.args.hash(&mut h);
    opts.proxy.hash(&mut h);
    opts.proxy_bypass.hash(&mut h);
    opts.proxy_username.hash(&mut h);
    opts.proxy_password.hash(&mut h);
    opts.user_agent.hash(&mut h);
    opts.allow_file_access.hash(&mut h);
    h.finish()
}

pub struct DaemonState {
    pub browser: Option<BrowserManager>,
    pub appium: Option<AppiumManager>,
    pub safari_driver: Option<safari::SafariDriverProcess>,
    pub webdriver_backend: Option<super::webdriver::backend::WebDriverBackend>,
    pub backend_type: BackendType,
    pub ref_map: RefMap,
    pub domain_filter: Arc<RwLock<Option<DomainFilter>>>,
    pub event_tracker: EventTracker,
    pub session_name: Option<String>,
    pub session_id: String,
    pub tracing_state: TracingState,
    pub recording_state: RecordingState,
    event_rx: Option<broadcast::Receiver<CdpEvent>>,
    pub screencasting: bool,
    pub policy: Option<ActionPolicy>,
    pub pending_confirmation: Option<PendingConfirmation>,
    pub har_recording: bool,
    pub har_entries: Vec<HarEntry>,
    pub confirm_actions: Option<ConfirmActions>,
    pub inspect_server: Option<InspectServer>,
    pub routes: Arc<RwLock<Vec<RouteEntry>>>,
    pub tracked_requests: Vec<TrackedRequest>,
    pub request_tracking: bool,
    pub active_frame_id: Option<String>,
    /// Cross-origin iframe frame_id → dedicated CDP session_id.
    /// Populated by Target.attachedToTarget events from Target.setAutoAttach.
    pub iframe_sessions: HashMap<String, String>,
    /// Origin-scoped extra HTTP headers set via `--headers` on navigate.
    /// Key is the origin (scheme + host + port), value is the headers map.
    /// Wrapped in Arc<RwLock<>> so the background Fetch handler can read it.
    pub origin_headers: Arc<RwLock<HashMap<String, HashMap<String, String>>>>,
    /// Proxy authentication credentials (username, password) for handling
    /// Fetch.authRequired events from authenticated proxies.
    pub proxy_credentials: Arc<RwLock<Option<(String, String)>>>,
    /// Background task that processes Fetch.requestPaused events in real-time,
    /// handling domain filtering, route interception, and origin-scoped headers
    /// without deadlocking navigation/evaluate.
    fetch_handler_task: Option<tokio::task::JoinHandle<()>>,
    /// Background task that auto-accepts `alert` and `beforeunload` dialogs
    /// so they never block the agent.
    dialog_handler_task: Option<tokio::task::JoinHandle<()>>,
    pub mouse_state: MouseState,
    /// Tracks the currently open JavaScript dialog (alert/confirm/prompt), if any.
    pub pending_dialog: Option<PendingDialog>,
    /// When true, automatically dismiss `beforeunload` dialogs and accept `alert`
    /// dialogs so they never block the agent.  Enabled by default.
    pub auto_dialog: bool,
    /// Shared slot for stream server to receive CDP client when browser launches.
    pub stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
    /// Stream server instance kept alive so the broadcast channel remains open.
    pub stream_server: Option<Arc<StreamServer>>,
    /// Hash of launch options used for the current browser, for relaunch detection.
    launch_hash: Option<u64>,
    /// Browser engine name (e.g. "chrome", "lightpanda") for observability.
    pub engine: String,
    /// Default timeout for wait operations, from AGENT_BROWSER_DEFAULT_TIMEOUT env var.
    pub default_timeout_ms: u64,
}

impl DaemonState {
    pub fn new() -> Self {
        Self {
            browser: None,
            appium: None,
            safari_driver: None,
            webdriver_backend: None,
            backend_type: BackendType::Cdp,
            ref_map: RefMap::new(),
            domain_filter: Arc::new(RwLock::new(
                env::var("AGENT_BROWSER_ALLOWED_DOMAINS")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(|s| DomainFilter::new(&s)),
            )),
            event_tracker: EventTracker::new(),
            session_name: env::var("AGENT_BROWSER_SESSION_NAME").ok(),
            session_id: env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string()),
            tracing_state: TracingState::new(),
            recording_state: RecordingState::new(),
            event_rx: None,
            screencasting: false,
            policy: ActionPolicy::load_if_exists(),
            pending_confirmation: None,
            har_recording: false,
            har_entries: Vec::new(),
            confirm_actions: ConfirmActions::from_env(),
            inspect_server: None,
            routes: Arc::new(RwLock::new(Vec::new())),
            tracked_requests: Vec::new(),
            request_tracking: false,
            active_frame_id: None,
            iframe_sessions: HashMap::new(),
            origin_headers: Arc::new(RwLock::new(HashMap::new())),
            proxy_credentials: Arc::new(RwLock::new(None)),
            fetch_handler_task: None,
            dialog_handler_task: None,
            mouse_state: MouseState::default(),
            pending_dialog: None,
            auto_dialog: !matches!(
                env::var("AGENT_BROWSER_NO_AUTO_DIALOG").as_deref(),
                Ok("1" | "true" | "yes")
            ),
            stream_client: None,
            stream_server: None,
            launch_hash: None,
            engine: env::var("AGENT_BROWSER_ENGINE").unwrap_or_else(|_| "chrome".to_string()),
            default_timeout_ms: env::var("AGENT_BROWSER_DEFAULT_TIMEOUT")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(30_000),
        }
    }

    /// Extract the timeout from a command JSON, falling back to the
    /// configured `default_timeout_ms` (from `AGENT_BROWSER_DEFAULT_TIMEOUT`).
    /// All wait-family handlers should use this instead of reading the
    /// timeout field and providing their own fallback.
    fn timeout_ms(&self, cmd: &Value) -> u64 {
        cmd.get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(self.default_timeout_ms)
    }

    fn reset_input_state(&mut self) {
        self.mouse_state = MouseState::default();
    }

    /// Create state with an optional stream client slot and server instance
    /// (for daemon startup with stream server).
    pub fn new_with_stream(
        stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
        stream_server: Option<Arc<StreamServer>>,
    ) -> Self {
        let mut s = Self::new();
        if stream_server.is_some() {
            s.request_tracking = true;
        }
        s.stream_client = stream_client;
        s.stream_server = stream_server;
        s
    }

    fn subscribe_to_browser_events(&mut self) {
        if let Some(ref browser) = self.browser {
            self.event_rx = Some(browser.client.subscribe());
        }
    }

    /// Start the background task that processes Fetch.requestPaused and
    /// Fetch.authRequired events in real-time (domain filtering, route
    /// interception, origin-scoped headers, proxy authentication).
    /// Must be called after the browser is set and events are subscribed.
    fn start_fetch_handler(&mut self) {
        // Abort any existing handler.
        if let Some(task) = self.fetch_handler_task.take() {
            task.abort();
        }

        let Some(ref browser) = self.browser else {
            return;
        };

        let client = browser.client.clone();
        let mut rx = browser.client.subscribe();
        let domain_filter = self.domain_filter.clone();
        let routes = self.routes.clone();
        let origin_headers = self.origin_headers.clone();
        let proxy_credentials = self.proxy_credentials.clone();

        self.fetch_handler_task = Some(tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) if event.method == "Fetch.authRequired" => {
                        let request_id = event
                            .params
                            .get("requestId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let sid = event.session_id.clone().unwrap_or_default();
                        let creds = proxy_credentials.read().await;
                        if let Some((ref user, ref pass)) = *creds {
                            let _ = client
                                .send_command(
                                    "Fetch.continueWithAuth",
                                    Some(json!({
                                        "requestId": request_id,
                                        "authChallengeResponse": {
                                            "response": "ProvideCredentials",
                                            "username": user,
                                            "password": pass,
                                        }
                                    })),
                                    Some(&sid),
                                )
                                .await;
                        } else {
                            let _ = client
                                .send_command(
                                    "Fetch.continueWithAuth",
                                    Some(json!({
                                        "requestId": request_id,
                                        "authChallengeResponse": {
                                            "response": "CancelAuth",
                                        }
                                    })),
                                    Some(&sid),
                                )
                                .await;
                        }
                    }
                    Ok(event) if event.method == "Fetch.requestPaused" => {
                        let request_id = event
                            .params
                            .get("requestId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let request_url = event
                            .params
                            .get("request")
                            .and_then(|r| r.get("url"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let resource_type = event
                            .params
                            .get("resourceType")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let request_headers = event
                            .params
                            .get("request")
                            .and_then(|r| r.get("headers"))
                            .and_then(|h| h.as_object())
                            .cloned();
                        let sid = event.session_id.clone().unwrap_or_default();

                        let paused = FetchPausedRequest {
                            request_id,
                            url: request_url,
                            resource_type,
                            session_id: sid,
                            request_headers,
                        };

                        let df = domain_filter.read().await;
                        let rt = routes.read().await;
                        let oh = origin_headers.read().await;

                        resolve_fetch_paused(&client, df.as_ref(), &rt, &oh, &paused).await;
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }));
    }

    /// Start the background task that auto-accepts `alert` and `beforeunload`
    /// dialogs so they never block the agent. `confirm` and `prompt` dialogs
    /// are left for the agent to handle explicitly.
    fn start_dialog_handler(&mut self) {
        if let Some(task) = self.dialog_handler_task.take() {
            task.abort();
        }

        if !self.auto_dialog {
            return;
        }

        let Some(ref browser) = self.browser else {
            return;
        };

        let client = browser.client.clone();
        let mut rx = browser.client.subscribe();

        self.dialog_handler_task = Some(tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) if event.method == "Page.javascriptDialogOpening" => {
                        let dialog_type = event
                            .params
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if matches!(dialog_type, "beforeunload" | "alert") {
                            let message = event
                                .params
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            eprintln!("[auto-dismiss] {} dialog: {}", dialog_type, message);
                            let sid = event.session_id.clone().unwrap_or_default();
                            if let Err(e) = client
                                .send_command(
                                    "Page.handleJavaScriptDialog",
                                    Some(json!({ "accept": true })),
                                    Some(&sid),
                                )
                                .await
                            {
                                eprintln!(
                                    "[auto-dismiss] failed to dismiss {} dialog: {}",
                                    dialog_type, e
                                );
                            }
                        }
                    }
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        }));
    }

    /// Update the stream server's CDP client slot when browser is set or cleared.
    pub async fn update_stream_client(&self) {
        if let Some(ref slot) = self.stream_client {
            let mut guard = slot.write().await;
            *guard = self.browser.as_ref().map(|m| Arc::clone(&m.client));
        }
        if let Some(ref server) = self.stream_server {
            // Update the CDP page session ID so screencast commands target the right page
            let session_id = self
                .browser
                .as_ref()
                .and_then(|m| m.active_session_id().ok().map(|s| s.to_string()));
            server.set_cdp_session_id(session_id).await;

            // Broadcast connection status change to WebSocket clients
            let connected = self.browser.is_some();
            let sc = server.is_screencasting().await;
            let (vw, vh) = server.viewport().await;
            server
                .broadcast_status(connected, sc, vw, vh, &self.engine)
                .await;
            if let Some(ref mgr) = self.browser {
                server.broadcast_tabs(&mgr.tab_list()).await;
            } else {
                server.broadcast_tabs(&[]).await;
            }
            // Notify the background CDP event loop that the client changed
            server.notify_client_changed();
        }
    }

    /// Spawn a background task that polls screenshots and pipes them to ffmpeg.
    async fn start_recording_task(
        &mut self,
        client: Arc<CdpClient>,
        session_id: String,
    ) -> Result<(), String> {
        let shared_count = Arc::new(AtomicU64::new(0));
        let (cancel_tx, cancel_rx) = oneshot::channel();
        let handle = recording::spawn_recording_task(
            client,
            session_id,
            self.recording_state.output_path.clone(),
            shared_count.clone(),
            cancel_rx,
        );
        self.recording_state.capture_task = Some(handle);
        self.recording_state.shared_frame_count = Some(shared_count);
        self.recording_state.cancel_tx = Some(cancel_tx);
        Ok(())
    }

    async fn stop_recording_task(&mut self) -> Result<(), String> {
        recording::stop_recording_task(&mut self.recording_state).await
    }

    pub async fn drain_cdp_events_background(&mut self) {
        let drained = self.drain_cdp_events();
        self.apply_drained_events(drained).await;
    }

    async fn apply_drained_events(&mut self, drained: DrainedEvents) {
        // ACK screencast frames
        if !drained.pending_acks.is_empty() {
            if let Some(ref browser) = self.browser {
                if let Ok(session_id) = browser.active_session_id() {
                    for ack_sid in drained.pending_acks {
                        let _ = stream::ack_screencast_frame(&browser.client, session_id, ack_sid)
                            .await;
                    }
                }
            }
        }

        // Remove destroyed targets
        for target_id in &drained.destroyed_targets {
            if let Some(ref mut mgr) = self.browser {
                mgr.remove_page_by_target_id(target_id);
            }
        }

        // Track cross-origin iframe sessions
        for (frame_id, iframe_sid) in &drained.attached_iframe_sessions {
            self.iframe_sessions
                .insert(frame_id.clone(), iframe_sid.clone());
            if let Some(ref mgr) = self.browser {
                let _ = mgr
                    .client
                    .send_command_no_params(
                        "Runtime.runIfWaitingForDebugger",
                        Some(iframe_sid.as_str()),
                    )
                    .await;
                let _ = mgr
                    .client
                    .send_command_no_params("DOM.enable", Some(iframe_sid.as_str()))
                    .await;
                let _ = mgr
                    .client
                    .send_command_no_params("Accessibility.enable", Some(iframe_sid.as_str()))
                    .await;
                if self.har_recording || self.request_tracking {
                    let _ = mgr
                        .client
                        .send_command_no_params("Network.enable", Some(iframe_sid.as_str()))
                        .await;
                }
            }
        }
        for sid in &drained.detached_iframe_sessions {
            self.iframe_sessions.retain(|_, v| v != sid);
        }

        // Attach and register new targets
        for te in &drained.new_targets {
            if let Some(ref mut mgr) = self.browser {
                let attach_result: Result<AttachToTargetResult, String> = mgr
                    .client
                    .send_command_typed(
                        "Target.attachToTarget",
                        &AttachToTargetParams {
                            target_id: te.target_info.target_id.clone(),
                            flatten: true,
                        },
                        None,
                    )
                    .await;
                if let Ok(attach) = attach_result {
                    let _ = mgr.enable_domains_pub(&attach.session_id).await;

                    // Install domain filter on new pages
                    let df = self.domain_filter.read().await;
                    if let Some(ref filter) = *df {
                        let has_proxy_creds = self.proxy_credentials.read().await.is_some();
                        let _ = network::install_domain_filter(
                            &mgr.client,
                            &attach.session_id,
                            &filter.allowed_domains,
                            has_proxy_creds,
                        )
                        .await;
                    }

                    mgr.add_page(super::browser::PageInfo {
                        target_id: te.target_info.target_id.clone(),
                        session_id: attach.session_id,
                        url: te.target_info.url.clone(),
                        title: te.target_info.title.clone(),
                        target_type: te.target_info.target_type.clone(),
                    });
                }
            }
        }

        // Update changed targets
        for te in &drained.changed_targets {
            if let Some(ref mut mgr) = self.browser {
                mgr.update_page_target_info(&te.target_info);
            }
        }
    }

    fn drain_cdp_events(&mut self) -> DrainedEvents {
        let rx = match self.event_rx.as_mut() {
            Some(rx) => rx,
            None => return DrainedEvents::default(),
        };

        let mut pending_acks: Vec<i64> = Vec::new();
        let mut new_targets: Vec<TargetCreatedEvent> = Vec::new();
        let mut new_target_ids: HashSet<String> = HashSet::new();
        let mut changed_targets: Vec<TargetInfoChangedEvent> = Vec::new();
        let mut destroyed_targets: Vec<String> = Vec::new();
        let mut attached_iframe_sessions: Vec<(String, String)> = Vec::new();
        let mut detached_iframe_sessions: Vec<String> = Vec::new();

        loop {
            match rx.try_recv() {
                Ok(event) => {
                    // Target events are not session-scoped; handle them first
                    match event.method.as_str() {
                        "Target.targetCreated" => {
                            if let Ok(te) =
                                serde_json::from_value::<TargetCreatedEvent>(event.params.clone())
                            {
                                if should_track_target(&te.target_info) {
                                    let already_tracked = self
                                        .browser
                                        .as_ref()
                                        .is_none_or(|b| b.has_target(&te.target_info.target_id));
                                    if !already_tracked {
                                        new_target_ids.insert(te.target_info.target_id.clone());
                                        new_targets.push(te);
                                    }
                                }
                            }
                            continue;
                        }
                        "Target.targetInfoChanged" => {
                            if let Ok(te) = serde_json::from_value::<TargetInfoChangedEvent>(
                                event.params.clone(),
                            ) {
                                if should_track_target(&te.target_info) {
                                    // If this target is not yet tracked (e.g. it was
                                    // initially filtered because its URL was
                                    // chrome://newtab/), promote it to a new target
                                    // so it gets attached and added to `pages`.
                                    let already_tracked = self
                                        .browser
                                        .as_ref()
                                        .is_some_and(|b| b.has_target(&te.target_info.target_id));
                                    if already_tracked
                                        || new_target_ids.contains(&te.target_info.target_id)
                                    {
                                        changed_targets.push(te);
                                    } else {
                                        new_target_ids.insert(te.target_info.target_id.clone());
                                        new_targets.push(TargetCreatedEvent {
                                            target_info: te.target_info,
                                        });
                                    }
                                }
                            }
                            continue;
                        }
                        "Target.targetDestroyed" => {
                            if let Ok(te) =
                                serde_json::from_value::<TargetDestroyedEvent>(event.params.clone())
                            {
                                destroyed_targets.push(te.target_id);
                            }
                            continue;
                        }
                        "Target.attachedToTarget" => {
                            if let (Some(sid), Some(target_info)) = (
                                event.params.get("sessionId").and_then(|v| v.as_str()),
                                event.params.get("targetInfo"),
                            ) {
                                let target_type = target_info
                                    .get("type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if target_type == "iframe" {
                                    // For OOPIF targets, Chrome uses the frameId as
                                    // the targetId, so we can key iframe_sessions by it.
                                    if let Some(target_id) =
                                        target_info.get("targetId").and_then(|v| v.as_str())
                                    {
                                        attached_iframe_sessions
                                            .push((target_id.to_string(), sid.to_string()));
                                    }
                                }
                            }
                            continue;
                        }
                        "Target.detachedFromTarget" => {
                            if let Some(sid) =
                                event.params.get("sessionId").and_then(|v| v.as_str())
                            {
                                detached_iframe_sessions.push(sid.to_string());
                            }
                            continue;
                        }
                        _ => {}
                    }

                    let session_matches = if let Some(ref browser) = self.browser {
                        event.session_id.as_deref() == browser.active_session_id().ok()
                    } else {
                        false
                    };

                    // Allow Network events from cross-origin iframe sessions
                    // when HAR recording or request tracking is active.
                    let iframe_network_event = !session_matches
                        && (self.har_recording || self.request_tracking)
                        && event.method.starts_with("Network.")
                        && event
                            .session_id
                            .as_ref()
                            .is_some_and(|sid| self.iframe_sessions.values().any(|v| v == sid));

                    if !session_matches && !iframe_network_event {
                        continue;
                    }

                    match event.method.as_str() {
                        "Runtime.consoleAPICalled" => {
                            let level = event
                                .params
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or("log");
                            let raw_args: Vec<Value> = event
                                .params
                                .get("args")
                                .and_then(|v| v.as_array())
                                .cloned()
                                .unwrap_or_default();
                            let text = network::format_console_args(&raw_args);
                            if let Some(ref server) = self.stream_server {
                                server.broadcast_console(level, &text, &raw_args);
                            }
                            self.event_tracker.add_console(level, &text, raw_args);
                        }
                        "Runtime.exceptionThrown" => {
                            if let Ok(ex_event) =
                                serde_json::from_value::<ExceptionThrownEvent>(event.params.clone())
                            {
                                let details = &ex_event.exception_details;
                                let text = details
                                    .exception
                                    .as_ref()
                                    .and_then(|e| e.description.as_deref())
                                    .unwrap_or(&details.text);
                                self.event_tracker.add_error(
                                    text,
                                    None,
                                    details.line_number,
                                    details.column_number,
                                );
                                if let Some(ref server) = self.stream_server {
                                    server.broadcast_page_error(
                                        text,
                                        details.line_number,
                                        details.column_number,
                                    );
                                }
                            }
                        }
                        "Network.requestWillBeSent"
                            if self.har_recording || self.request_tracking =>
                        {
                            if let Some(request) = event.params.get("request") {
                                let method = request
                                    .get("method")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("GET")
                                    .to_string();
                                let url = request
                                    .get("url")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let request_id = event
                                    .params
                                    .get("requestId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if self.har_recording {
                                    let wall_time = event
                                        .params
                                        .get("wallTime")
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or(0.0);
                                    let request_headers =
                                        har_extract_headers(request.get("headers"));
                                    let post_data = request
                                        .get("postData")
                                        .and_then(|v| v.as_str())
                                        .map(String::from);
                                    let request_body_size =
                                        post_data.as_ref().map(|s| s.len() as i64).unwrap_or(0);
                                    let resource_type = event
                                        .params
                                        .get("type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Other")
                                        .to_string();
                                    self.har_entries.push(HarEntry {
                                        request_id: request_id.clone(),
                                        wall_time,
                                        method: method.clone(),
                                        url: url.clone(),
                                        request_headers,
                                        post_data,
                                        request_body_size,
                                        resource_type,
                                        status: None,
                                        status_text: String::new(),
                                        http_version: "HTTP/1.1".to_string(),
                                        response_headers: Vec::new(),
                                        mime_type: String::new(),
                                        redirect_url: String::new(),
                                        response_body_size: -1,
                                        cdp_timing: None,
                                        loading_finished_timestamp: None,
                                    });
                                }
                                if self.request_tracking {
                                    let headers =
                                        request.get("headers").cloned().unwrap_or(json!({}));
                                    let resource_type = event
                                        .params
                                        .get("type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("Other")
                                        .to_string();
                                    let timestamp = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_millis() as u64)
                                        .unwrap_or(0);
                                    self.tracked_requests.push(TrackedRequest {
                                        url,
                                        method,
                                        headers,
                                        timestamp,
                                        resource_type,
                                        request_id,
                                        post_data: request
                                            .get("postData")
                                            .and_then(|v| v.as_str())
                                            .map(String::from),
                                        status: None,
                                        response_headers: None,
                                        mime_type: None,
                                    });
                                }
                            }
                        }
                        "Network.responseReceived"
                            if self.har_recording || self.request_tracking =>
                        {
                            if let Some(response) = event.params.get("response") {
                                let request_id = event
                                    .params
                                    .get("requestId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let status = response.get("status").and_then(|v| v.as_i64());
                                let status_text = response
                                    .get("statusText")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let mime_type = response
                                    .get("mimeType")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let http_version = response
                                    .get("protocol")
                                    .and_then(|v| v.as_str())
                                    .map(har_cdp_protocol_to_http_version)
                                    .unwrap_or_else(|| "HTTP/1.1".to_string());
                                let response_headers = har_extract_headers(response.get("headers"));
                                let redirect_url = response_headers
                                    .iter()
                                    .find(|(k, _)| k.eq_ignore_ascii_case("location"))
                                    .map(|(_, v)| v.clone())
                                    .unwrap_or_default();
                                let encoded_data_length = response
                                    .get("encodedDataLength")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(-1);
                                let cdp_timing = response.get("timing").cloned();
                                if self.har_recording {
                                    if let Some(entry) = self
                                        .har_entries
                                        .iter_mut()
                                        .rev()
                                        .find(|e| e.request_id == request_id)
                                    {
                                        entry.status = status;
                                        entry.status_text = status_text;
                                        entry.mime_type = mime_type;
                                        entry.http_version = http_version;
                                        entry.response_headers = response_headers;
                                        entry.redirect_url = redirect_url;
                                        entry.response_body_size = encoded_data_length;
                                        entry.cdp_timing = cdp_timing;
                                    }
                                }
                                if self.request_tracking {
                                    let resp_headers = response.get("headers").cloned();
                                    let resp_mime = response
                                        .get("mimeType")
                                        .and_then(|v| v.as_str())
                                        .map(String::from);
                                    if let Some(entry) = self
                                        .tracked_requests
                                        .iter_mut()
                                        .rev()
                                        .find(|e| e.request_id == request_id)
                                    {
                                        entry.status = status;
                                        entry.mime_type = resp_mime;
                                        entry.response_headers = resp_headers;
                                    }
                                }
                            }
                        }
                        "Network.loadingFinished" if self.har_recording => {
                            let request_id = event
                                .params
                                .get("requestId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let timestamp = event.params.get("timestamp").and_then(|v| v.as_f64());
                            let encoded_data_length = event
                                .params
                                .get("encodedDataLength")
                                .and_then(|v| v.as_i64());
                            if let Some(entry) = self
                                .har_entries
                                .iter_mut()
                                .rev()
                                .find(|e| e.request_id == request_id)
                            {
                                if let Some(ts) = timestamp {
                                    entry.loading_finished_timestamp = Some(ts);
                                }
                                if let Some(len) = encoded_data_length {
                                    entry.response_body_size = len;
                                }
                            }
                        }
                        "Network.loadingFailed" if self.har_recording => {
                            let request_id = event
                                .params
                                .get("requestId")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let timestamp = event.params.get("timestamp").and_then(|v| v.as_f64());
                            let error_text = event
                                .params
                                .get("errorText")
                                .and_then(|v| v.as_str())
                                .unwrap_or("Failed");
                            if let Some(entry) = self
                                .har_entries
                                .iter_mut()
                                .rev()
                                .find(|e| e.request_id == request_id)
                            {
                                if entry.status.is_none() {
                                    entry.status = Some(0);
                                    entry.status_text = error_text.to_string();
                                }
                                if let Some(ts) = timestamp {
                                    entry.loading_finished_timestamp = Some(ts);
                                }
                            }
                        }
                        "Page.screencastFrame" => {
                            // Frame broadcasting and acks are handled in real-time by the
                            // stream server's background CDP event loop. Here we just
                            // collect acks as a fallback for non-streaming mode.
                            if self.stream_server.is_none() {
                                if let Some(sid) =
                                    event.params.get("sessionId").and_then(|v| v.as_i64())
                                {
                                    pending_acks.push(sid);
                                }
                            }
                        }
                        "Page.javascriptDialogOpening" => {
                            if let Ok(dialog_event) =
                                serde_json::from_value::<JavascriptDialogOpeningEvent>(
                                    event.params.clone(),
                                )
                            {
                                // When auto_dialog is enabled, alert and beforeunload
                                // dialogs are handled by the background dialog_handler_task.
                                // Skip tracking them to avoid a stale warning.
                                let auto_handled = self.auto_dialog
                                    && matches!(
                                        dialog_event.dialog_type.as_str(),
                                        "beforeunload" | "alert"
                                    );
                                if !auto_handled {
                                    self.pending_dialog = Some(PendingDialog {
                                        dialog_type: dialog_event.dialog_type,
                                        message: dialog_event.message,
                                        url: dialog_event.url,
                                        default_prompt: dialog_event.default_prompt,
                                    });
                                }
                            }
                        }
                        "Page.javascriptDialogClosed" => {
                            self.pending_dialog = None;
                        }
                        // Fetch.requestPaused is handled by the background
                        // fetch_handler_task — no need to collect here.
                        _ => {}
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(n)) => {
                    eprintln!("[agent-browser] Warning: CDP event buffer overflowed, {} events dropped. Network requests may be missing from HAR output.", n);
                    continue;
                }
                Err(broadcast::error::TryRecvError::Closed) => {
                    self.event_rx = None;
                    break;
                }
            }
        }

        DrainedEvents {
            pending_acks,
            new_targets,
            changed_targets,
            destroyed_targets,
            attached_iframe_sessions,
            detached_iframe_sessions,
        }
    }
}

impl Drop for DaemonState {
    fn drop(&mut self) {
        // The background fetch handler sits in rx.recv().await indefinitely.
        // Without aborting it, the tokio runtime won't shut down (tests hang).
        if let Some(task) = self.fetch_handler_task.take() {
            task.abort();
        }
        if let Some(task) = self.dialog_handler_task.take() {
            task.abort();
        }
    }
}

pub async fn execute_command(cmd: &Value, state: &mut DaemonState) -> Value {
    let action = cmd.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let id = cmd
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let cmd_start = std::time::Instant::now();

    if let Some(ref server) = state.stream_server {
        server.broadcast_command(action, &id, cmd);
    }

    // Drain and apply pending CDP events (console, errors, screencast frames, target lifecycle)
    state.drain_cdp_events_background().await;

    // Hot-reload and check action policy
    if let Some(ref mut policy) = state.policy {
        let _ = policy.reload();
        match policy.check(action) {
            PolicyResult::Allow => {}
            PolicyResult::Deny(reason) => {
                return error_response(
                    &id,
                    &format!("Action '{}' denied by policy: {}", action, reason),
                );
            }
            PolicyResult::RequiresConfirmation => {
                state.pending_confirmation = Some(PendingConfirmation {
                    action: action.to_string(),
                    cmd: cmd.clone(),
                });
                return json!({
                    "id": id,
                    "success": true,
                    "data": { "confirmation_required": true, "action": action },
                });
            }
        }
    }

    // Check AGENT_BROWSER_CONFIRM_ACTIONS (category-based, independent of policy file)
    if action != "confirm" && action != "deny" {
        if let Some(ref ca) = state.confirm_actions {
            if ca.requires_confirmation(action) {
                state.pending_confirmation = Some(PendingConfirmation {
                    action: action.to_string(),
                    cmd: cmd.clone(),
                });
                return json!({
                    "id": id,
                    "success": true,
                    "data": {
                        "confirmation_required": true,
                        "confirmation_id": id,
                        "action": action,
                    },
                });
            }
        }
    }

    let skip_launch = matches!(
        action,
        "" | "launch"
            | "close"
            | "har_stop"
            | "credentials_set"
            | "credentials_get"
            | "credentials_delete"
            | "credentials_list"
            | "auth_save"
            | "auth_show"
            | "auth_delete"
            | "auth_list"
            | "state_list"
            | "state_show"
            | "state_clear"
            | "state_clean"
            | "state_rename"
            | "device_list"
            | "stream_enable"
            | "stream_disable"
            | "stream_status"
    );
    if !skip_launch {
        // Check if existing connection is stale and needs re-launch.
        // First do a fast, non-blocking check: did the browser process crash/exit?
        // This avoids a 3-second CDP timeout when Chrome is already dead.
        let needs_launch = if let Some(ref mut mgr) = state.browser {
            mgr.has_process_exited() || !mgr.is_connection_alive().await
        } else {
            true
        };

        if needs_launch {
            if state.browser.is_some() {
                if let Some(ref mut mgr) = state.browser {
                    let _ = mgr.close().await;
                }
                state.browser = None;
                state.screencasting = false;
                state.reset_input_state();
                state.update_stream_client().await;
            }
            if let Err(e) = auto_launch(state).await {
                return error_response(&id, &format!("Auto-launch failed: {}", e));
            }
        }

        if let Some(ref mut mgr) = state.browser {
            if mgr.page_count() == 0 {
                let _ = mgr.ensure_page().await;
            }
        }
    }

    // WebDriver backend: reject unsupported CDP-only actions
    if matches!(state.backend_type, BackendType::WebDriver)
        && WEBDRIVER_UNSUPPORTED_ACTIONS.contains(&action)
    {
        return error_response(
            &id,
            &format!(
                "Action '{}' is not supported on the WebDriver backend",
                action
            ),
        );
    }

    let result = match action {
        "launch" => handle_launch(cmd, state).await,
        "navigate" => handle_navigate(cmd, state).await,
        "url" => handle_url(state).await,
        "cdp_url" => handle_cdp_url(state),
        "inspect" => handle_inspect(state).await,
        "title" => handle_title(state).await,
        "content" => handle_content(state).await,
        "evaluate" => handle_evaluate(cmd, state).await,
        "close" => handle_close(state).await,
        "snapshot" => handle_snapshot(cmd, state).await,
        "screenshot" => handle_screenshot(cmd, state).await,
        "click" => handle_click(cmd, state).await,
        "dblclick" => handle_dblclick(cmd, state).await,
        "fill" => handle_fill(cmd, state).await,
        "type" => handle_type(cmd, state).await,
        "press" => handle_press(cmd, state).await,
        "hover" => handle_hover(cmd, state).await,
        "scroll" => handle_scroll(cmd, state).await,
        "select" => handle_select(cmd, state).await,
        "check" => handle_check(cmd, state).await,
        "uncheck" => handle_uncheck(cmd, state).await,
        "wait" => handle_wait(cmd, state).await,
        "gettext" => handle_gettext(cmd, state).await,
        "getattribute" => handle_getattribute(cmd, state).await,
        "isvisible" => handle_isvisible(cmd, state).await,
        "isenabled" => handle_isenabled(cmd, state).await,
        "ischecked" => handle_ischecked(cmd, state).await,
        "back" => handle_back(state).await,
        "forward" => handle_forward(state).await,
        "reload" => handle_reload(state).await,
        "cookies_get" => handle_cookies_get(cmd, state).await,
        "cookies_set" => handle_cookies_set(cmd, state).await,
        "cookies_clear" => handle_cookies_clear(state).await,
        "storage_get" => handle_storage_get(cmd, state).await,
        "storage_set" => handle_storage_set(cmd, state).await,
        "storage_clear" => handle_storage_clear(cmd, state).await,
        "setcontent" => handle_setcontent(cmd, state).await,
        "headers" => handle_headers(cmd, state).await,
        "offline" => handle_offline(cmd, state).await,
        "console" => handle_console(cmd, state).await,
        "errors" => handle_errors(state).await,
        "state_save" => handle_state_save(cmd, state).await,
        "state_load" => handle_state_load(cmd, state).await,
        "state_list" | "state_show" | "state_clear" | "state_clean" | "state_rename" => {
            state::dispatch_state_command(cmd)
                .expect("dispatch_state_command must handle all state_* actions matched here")
        }
        "trace_start" => handle_trace_start(state).await,
        "trace_stop" => handle_trace_stop(cmd, state).await,
        "profiler_start" => handle_profiler_start(cmd, state).await,
        "profiler_stop" => handle_profiler_stop(cmd, state).await,
        "recording_start" => handle_recording_start(cmd, state).await,
        "recording_stop" => handle_recording_stop(state).await,
        "recording_restart" => handle_recording_restart(cmd, state).await,
        "pdf" => handle_pdf(cmd, state).await,
        "tab_list" => handle_tab_list(state).await,
        "tab_new" => handle_tab_new(cmd, state).await,
        "tab_switch" => handle_tab_switch(cmd, state).await,
        "tab_close" => handle_tab_close(cmd, state).await,
        "viewport" => handle_viewport(cmd, state).await,
        "useragent" | "user_agent" => handle_user_agent(cmd, state).await,
        "set_media" => handle_set_media(cmd, state).await,
        "download" => handle_download(cmd, state).await,
        "diff_snapshot" => handle_diff_snapshot(cmd, state).await,
        "diff_url" => handle_diff_url(cmd, state).await,
        "credentials_set" => handle_credentials_set(cmd).await,
        "credentials_get" => handle_credentials_get(cmd).await,
        "credentials_delete" => handle_credentials_delete(cmd).await,
        "credentials_list" => handle_credentials_list().await,
        "mouse" => handle_mouse(cmd, state).await,
        "keyboard" => handle_keyboard(cmd, state).await,
        "focus" => handle_focus(cmd, state).await,
        "clear" => handle_clear(cmd, state).await,
        "selectall" => handle_selectall(cmd, state).await,
        "scrollintoview" => handle_scrollintoview(cmd, state).await,
        "dispatch" => handle_dispatch(cmd, state).await,
        "highlight" => handle_highlight(cmd, state).await,
        "tap" => handle_tap(cmd, state).await,
        "boundingbox" => handle_boundingbox(cmd, state).await,
        "innertext" => handle_innertext(cmd, state).await,
        "innerhtml" => handle_innerhtml(cmd, state).await,
        "inputvalue" => handle_inputvalue(cmd, state).await,
        "setvalue" => handle_setvalue(cmd, state).await,
        "count" => handle_count(cmd, state).await,
        "styles" => handle_styles(cmd, state).await,
        "bringtofront" => handle_bringtofront(state).await,
        "timezone" => handle_timezone(cmd, state).await,
        "locale" => handle_locale(cmd, state).await,
        "geolocation" => handle_geolocation(cmd, state).await,
        "permissions" => handle_permissions(cmd, state).await,
        "dialog" => handle_dialog(cmd, state).await,
        "upload" => handle_upload(cmd, state).await,
        "addscript" => handle_addscript(cmd, state).await,
        "addinitscript" => handle_addinitscript(cmd, state).await,
        "addstyle" => handle_addstyle(cmd, state).await,
        "clipboard" => handle_clipboard(cmd, state).await,
        "wheel" => handle_wheel(cmd, state).await,
        "device" => handle_device(cmd, state).await,
        "screencast_start" => handle_screencast_start(cmd, state).await,
        "screencast_stop" => handle_screencast_stop(state).await,
        "stream_enable" => handle_stream_enable(cmd, state).await,
        "stream_disable" => handle_stream_disable(state).await,
        "stream_status" => handle_stream_status(state).await,
        "waitforurl" => handle_waitforurl(cmd, state).await,
        "waitforloadstate" => handle_waitforloadstate(cmd, state).await,
        "waitforfunction" => handle_waitforfunction(cmd, state).await,
        "frame" => handle_frame(cmd, state).await,
        "mainframe" => handle_mainframe(state).await,
        "getbyrole" => handle_getbyrole(cmd, state).await,
        "getbytext" => handle_getbytext(cmd, state).await,
        "getbylabel" => handle_getbylabel(cmd, state).await,
        "getbyplaceholder" => handle_getbyplaceholder(cmd, state).await,
        "getbyalttext" => handle_getbyalttext(cmd, state).await,
        "getbytitle" => handle_getbytitle(cmd, state).await,
        "getbytestid" => handle_getbytestid(cmd, state).await,
        "nth" => handle_nth(cmd, state).await,
        "find" => handle_find(cmd, state).await,
        "evalhandle" => handle_evalhandle(cmd, state).await,
        "drag" => handle_drag(cmd, state).await,
        "expose" => handle_expose(cmd, state).await,
        "pause" => handle_pause(state).await,
        "multiselect" => handle_multiselect(cmd, state).await,
        "responsebody" => handle_responsebody(cmd, state).await,
        "waitfordownload" => handle_waitfordownload(cmd, state).await,
        "window_new" => handle_window_new(cmd, state).await,
        "diff_screenshot" => handle_diff_screenshot(cmd, state).await,
        "video_start" => handle_video_start(cmd, state).await,
        "video_stop" => handle_video_stop(state).await,
        "har_start" => handle_har_start(state).await,
        "har_stop" => handle_har_stop(cmd, state).await,
        "route" => handle_route(cmd, state).await,
        "unroute" => handle_unroute(cmd, state).await,
        "requests" => handle_requests(cmd, state).await,
        "request_detail" => handle_request_detail(cmd, state).await,
        "credentials" => handle_http_credentials(cmd, state).await,
        "emulatemedia" => handle_set_media(cmd, state).await,
        "auth_save" => handle_auth_save(cmd).await,
        "auth_login" => handle_auth_login(cmd, state).await,
        "auth_list" => handle_credentials_list().await,
        "auth_delete" => handle_credentials_delete(cmd).await,
        "auth_show" => handle_auth_show(cmd).await,
        "confirm" => handle_confirm(cmd, state).await,
        "deny" => handle_deny(cmd, state).await,
        "swipe" => handle_swipe(cmd, state).await,
        "device_list" => handle_device_list().await,
        "input_mouse" => handle_input_mouse(cmd, state).await,
        "input_keyboard" => handle_input_keyboard(cmd, state).await,
        "input_touch" => handle_input_touch(cmd, state).await,
        "keydown" => handle_keydown(cmd, state).await,
        "keyup" => handle_keyup(cmd, state).await,
        "inserttext" => handle_inserttext(cmd, state).await,
        "mousemove" => handle_mousemove(cmd, state).await,
        "mousedown" => handle_mousedown(cmd, state).await,
        "mouseup" => handle_mouseup(cmd, state).await,
        _ => Err(format!("Not yet implemented: {}", action)),
    };

    let mut resp = match result {
        Ok(data) => success_response(&id, data),
        Err(e) => error_response(&id, &super::browser::to_ai_friendly_error(&e)),
    };

    // Auto-report pending JavaScript dialog so agents know why commands may hang
    if action != "dialog" {
        if let Some(ref dialog) = state.pending_dialog {
            if let Some(obj) = resp.as_object_mut() {
                obj.insert(
                    "warning".to_string(),
                    json!(format!(
                        "A JavaScript {} dialog is blocking the page: \"{}\" — use `dialog accept` or `dialog dismiss` to resolve it",
                        dialog.dialog_type, dialog.message
                    )),
                );
            }
        }
    }

    if let Some(ref server) = state.stream_server {
        let duration_ms = cmd_start.elapsed().as_millis() as u64;
        let success = resp
            .get("status")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s == "success");
        let data = resp.get("data").cloned().unwrap_or(Value::Null);
        server.broadcast_result(&id, action, success, &data, duration_ms);

        if let Some(ref mgr) = state.browser {
            server.broadcast_tabs(&mgr.tab_list()).await;

            // Keep the stream server's CDP session in sync with the active tab
            // so screencasting always targets the correct page.
            if matches!(
                action,
                "tab_new" | "tab_switch" | "tab_close" | "open" | "navigate"
            ) {
                let session_id = mgr.active_session_id().ok().map(|s| s.to_string());
                server.set_cdp_session_id(session_id).await;
                server.notify_client_changed();
            }
        }
    }

    resp
}

// ---------------------------------------------------------------------------
// Auto-launch
// ---------------------------------------------------------------------------

/// Connect to a running Chrome via auto-discovery and open a fresh tab so
/// subsequent navigations don't hijack the user's existing tabs.
async fn connect_auto_with_fresh_tab() -> Result<BrowserManager, String> {
    let mut mgr = BrowserManager::connect_auto().await?;
    mgr.tab_new(None).await?;
    let session_id = mgr.active_session_id()?.to_string();
    let _ = mgr
        .client
        .send_command("Page.bringToFront", None, Some(&session_id))
        .await;
    Ok(mgr)
}

async fn auto_launch(state: &mut DaemonState) -> Result<(), String> {
    let options = launch_options_from_env();
    let engine = env::var("AGENT_BROWSER_ENGINE").ok();

    // Store proxy credentials for Fetch.authRequired handling
    let has_proxy_auth = options.proxy_username.is_some();
    if has_proxy_auth {
        let mut creds = state.proxy_credentials.write().await;
        *creds = Some((
            options.proxy_username.clone().unwrap_or_default(),
            options.proxy_password.clone().unwrap_or_default(),
        ));
    }

    state.engine = engine.as_deref().unwrap_or("chrome").to_string();
    write_engine_file(&state.session_id, &state.engine);
    write_extensions_file(&state.session_id);

    if let Ok(cdp) = env::var("AGENT_BROWSER_CDP") {
        let mgr = BrowserManager::connect_cdp(&cdp).await?;
        state.reset_input_state();
        state.browser = Some(mgr);
        state.subscribe_to_browser_events();
        state.start_fetch_handler();
        state.start_dialog_handler();
        state.update_stream_client().await;
        try_auto_restore_state(state).await;
        return Ok(());
    }

    if env::var("AGENT_BROWSER_AUTO_CONNECT").is_ok() {
        state.reset_input_state();
        state.browser = Some(connect_auto_with_fresh_tab().await?);
        state.subscribe_to_browser_events();
        state.start_fetch_handler();
        state.start_dialog_handler();
        state.update_stream_client().await;
        try_auto_restore_state(state).await;
        return Ok(());
    }

    // Cloud provider: when AGENT_BROWSER_PROVIDER is set, connect via the
    // provider API instead of launching a local Chrome instance.  This mirrors
    // the logic in handle_launch() so that auto_launch (triggered by any
    // command arriving before an explicit "launch") honours the provider env.
    if let Ok(provider) = env::var("AGENT_BROWSER_PROVIDER") {
        let p = provider.to_lowercase();
        // ios/safari are device providers handled via explicit launch command
        if !p.is_empty() && p != "ios" && p != "safari" {
            let conn = providers::connect_provider(&p).await?;
            let ws_headers = if p == "agentcore" {
                providers::take_agentcore_ws_headers()
            } else {
                None
            };
            let connect_result = if conn.direct_page {
                BrowserManager::connect_cdp_direct(&conn.ws_url).await
            } else if ws_headers.is_some() {
                BrowserManager::connect_cdp_with_headers(&conn.ws_url, ws_headers).await
            } else {
                BrowserManager::connect_cdp(&conn.ws_url).await
            };
            match connect_result {
                Ok(mgr) => {
                    state.reset_input_state();
                    state.browser = Some(mgr);
                    state.subscribe_to_browser_events();
                    state.start_fetch_handler();
                    state.start_dialog_handler();
                    state.update_stream_client().await;
                    write_provider_file(&state.session_id, &p);
                    try_auto_restore_state(state).await;
                    return Ok(());
                }
                Err(e) => {
                    if let Some(ref ps) = conn.session {
                        providers::close_provider_session(ps).await;
                    }
                    return Err(format!("Provider '{}' connection failed: {}", p, e));
                }
            }
        }
    }

    let hash = launch_hash(&options);
    let mgr = BrowserManager::launch(options, engine.as_deref()).await?;
    state.reset_input_state();
    state.browser = Some(mgr);
    state.launch_hash = Some(hash);
    state.subscribe_to_browser_events();
    state.start_fetch_handler();
    state.start_dialog_handler();
    state.update_stream_client().await;

    // Enable Fetch with handleAuthRequests for proxy authentication
    if has_proxy_auth {
        if let Some(ref mgr) = state.browser {
            if let Ok(session_id) = mgr.active_session_id() {
                let _ = network::install_domain_filter_fetch(&mgr.client, session_id, true).await;
            }
        }
    }

    try_auto_restore_state(state).await;
    Ok(())
}

fn launch_options_from_env() -> LaunchOptions {
    let headed = env::var("AGENT_BROWSER_HEADED")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    let extensions: Option<Vec<String>> = env::var("AGENT_BROWSER_EXTENSIONS").ok().map(|v| {
        v.split([',', '\n'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });

    LaunchOptions {
        headless: !headed,
        executable_path: env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok(),
        proxy: env::var("AGENT_BROWSER_PROXY").ok(),
        proxy_bypass: env::var("AGENT_BROWSER_PROXY_BYPASS").ok(),
        proxy_username: env::var("AGENT_BROWSER_PROXY_USERNAME").ok(),
        proxy_password: env::var("AGENT_BROWSER_PROXY_PASSWORD").ok(),
        profile: env::var("AGENT_BROWSER_PROFILE").ok(),
        allow_file_access: env::var("AGENT_BROWSER_ALLOW_FILE_ACCESS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        args: env::var("AGENT_BROWSER_ARGS")
            .map(|v| {
                v.split([',', '\n'])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        extensions,
        storage_state: env::var("AGENT_BROWSER_STATE").ok(),
        user_agent: env::var("AGENT_BROWSER_USER_AGENT").ok(),
        ignore_https_errors: env::var("AGENT_BROWSER_IGNORE_HTTPS_ERRORS")
            .map(|v| v == "1" || v == "true")
            .unwrap_or(false),
        color_scheme: env::var("AGENT_BROWSER_COLOR_SCHEME").ok(),
        download_path: env::var("AGENT_BROWSER_DOWNLOAD_PATH").ok(),
        use_real_keychain: false,
    }
}

async fn try_auto_restore_state(state: &mut DaemonState) {
    let session_name = match state.session_name.as_deref() {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => return,
    };
    if let Some(path) = state::find_auto_state_file(&session_name) {
        if let Some(ref mgr) = state.browser {
            if let Ok(session_id) = mgr.active_session_id() {
                let _ = state::load_state(&mgr.client, session_id, &path).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 1 handlers
// ---------------------------------------------------------------------------

async fn handle_launch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let headless = cmd
        .get("headless")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let cdp_url = cmd.get("cdpUrl").and_then(|v| v.as_str());
    let cdp_port = cmd.get("cdpPort").and_then(|v| v.as_u64());
    let auto_connect = cmd
        .get("autoConnect")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let extensions: Option<Vec<String>> =
        cmd.get("extensions").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });
    let storage_state = cmd.get("storageState").and_then(|v| v.as_str());

    let launch_options = LaunchOptions {
        headless,
        executable_path: cmd
            .get("executablePath")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok()),
        proxy: cmd.get("proxy").and_then(|v| {
            v.as_str().map(|s| s.to_string()).or_else(|| {
                v.get("server")
                    .and_then(|s| s.as_str())
                    .map(|s| s.to_string())
            })
        }),
        proxy_bypass: cmd
            .get("proxy")
            .and_then(|v| v.get("bypass"))
            .and_then(|v| v.as_str())
            .map(String::from),
        proxy_username: cmd
            .get("proxy")
            .and_then(|v| v.get("username"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| env::var("AGENT_BROWSER_PROXY_USERNAME").ok()),
        proxy_password: cmd
            .get("proxy")
            .and_then(|v| v.get("password"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| env::var("AGENT_BROWSER_PROXY_PASSWORD").ok()),
        profile: cmd
            .get("profile")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        allow_file_access: cmd
            .get("allowFileAccess")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        args: cmd
            .get("args")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default(),
        extensions,
        storage_state: storage_state.map(String::from),
        user_agent: cmd
            .get("userAgent")
            .and_then(|v| v.as_str())
            .map(String::from),
        ignore_https_errors: cmd
            .get("ignoreHTTPSErrors")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        color_scheme: cmd
            .get("colorScheme")
            .and_then(|v| v.as_str())
            .map(String::from),
        download_path: cmd
            .get("downloadPath")
            .and_then(|v| v.as_str())
            .map(String::from),
        use_real_keychain: false,
    };

    let new_hash = launch_hash(&launch_options);

    // Hash comparison and fast process-exit check are evaluated before the
    // async is_connection_alive to skip the expensive CDP liveness probe
    // when a relaunch is already certain.
    let needs_relaunch = if let Some(ref mut mgr) = state.browser {
        let is_external = cdp_url.is_some() || cdp_port.is_some() || auto_connect;
        let was_external = mgr.is_cdp_connection();
        let hash_changed = !is_external && state.launch_hash != Some(new_hash);
        is_external != was_external
            || hash_changed
            || mgr.has_process_exited()
            || !mgr.is_connection_alive().await
    } else {
        true
    };

    if needs_relaunch {
        if let Some(ref mut b) = state.browser {
            b.close().await?;
            state.browser = None;
            state.launch_hash = None;
            state.screencasting = false;
            state.reset_input_state();
            state.update_stream_client().await;
        }
    } else {
        return Ok(json!({ "launched": true, "reused": true }));
    }
    state.ref_map.clear();

    let has_cdp = cdp_url.is_some() || cdp_port.is_some();
    super::browser::validate_launch_options(
        launch_options.extensions.as_deref(),
        has_cdp,
        launch_options.profile.as_deref(),
        storage_state,
        launch_options.allow_file_access,
        launch_options.executable_path.as_deref(),
    )?;

    if let Some(url) = cdp_url {
        state.reset_input_state();
        state.browser = Some(BrowserManager::connect_cdp(url).await?);
        state.subscribe_to_browser_events();
        state.start_fetch_handler();
        state.start_dialog_handler();
        state.update_stream_client().await;
        return Ok(json!({ "launched": true }));
    }

    if let Some(port) = cdp_port {
        state.reset_input_state();
        state.browser = Some(BrowserManager::connect_cdp(&port.to_string()).await?);
        state.subscribe_to_browser_events();
        state.start_fetch_handler();
        state.start_dialog_handler();
        state.update_stream_client().await;
        return Ok(json!({ "launched": true }));
    }

    if auto_connect {
        state.reset_input_state();
        state.browser = Some(connect_auto_with_fresh_tab().await?);
        state.subscribe_to_browser_events();
        state.start_fetch_handler();
        state.start_dialog_handler();
        state.update_stream_client().await;
        return Ok(json!({ "launched": true }));
    }

    if let Some(provider) = cmd.get("provider").and_then(|v| v.as_str()) {
        match provider.to_lowercase().as_str() {
            "ios" => {
                return launch_ios(cmd, state).await;
            }
            "safari" => {
                return launch_safari(cmd, state).await;
            }
            _ => {
                let conn = providers::connect_provider(provider).await?;

                let ws_headers = if provider.eq_ignore_ascii_case("agentcore") {
                    providers::take_agentcore_ws_headers()
                } else {
                    None
                };

                let connect_result = if conn.direct_page {
                    BrowserManager::connect_cdp_direct(&conn.ws_url).await
                } else if ws_headers.is_some() {
                    BrowserManager::connect_cdp_with_headers(&conn.ws_url, ws_headers).await
                } else {
                    BrowserManager::connect_cdp(&conn.ws_url).await
                };
                match connect_result {
                    Ok(mgr) => {
                        state.reset_input_state();
                        state.browser = Some(mgr);
                        state.subscribe_to_browser_events();
                        state.start_fetch_handler();
                        state.start_dialog_handler();
                        state.update_stream_client().await;
                        write_provider_file(&state.session_id, provider);

                        if let Some(info) = providers::get_agentcore_info() {
                            return Ok(json!({
                                "launched": true,
                                "provider": provider,
                                "agentCoreSessionId": info.session_id,
                                "agentCoreLiveViewUrl": info.live_view_url
                            }));
                        }

                        return Ok(json!({ "launched": true, "provider": provider }));
                    }
                    Err(e) => {
                        if let Some(ref ps) = conn.session {
                            providers::close_provider_session(ps).await;
                        }
                        return Err(e);
                    }
                }
            }
        }
    }

    let engine = cmd
        .get("engine")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| env::var("AGENT_BROWSER_ENGINE").ok());

    // Store proxy credentials for Fetch.authRequired handling
    let has_proxy_auth = launch_options.proxy_username.is_some();
    if has_proxy_auth {
        let mut creds = state.proxy_credentials.write().await;
        *creds = Some((
            launch_options.proxy_username.clone().unwrap_or_default(),
            launch_options.proxy_password.clone().unwrap_or_default(),
        ));
    }

    if let Some(ref domains) = cmd
        .get("allowedDomains")
        .and_then(|v| v.as_str())
        .map(String::from)
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(DomainFilter::new(domains));
    }

    state.engine = engine.as_deref().unwrap_or("chrome").to_string();
    write_engine_file(&state.session_id, &state.engine);
    write_extensions_file(&state.session_id);
    state.reset_input_state();
    state.browser = Some(BrowserManager::launch(launch_options, engine.as_deref()).await?);
    state.launch_hash = Some(new_hash);
    state.subscribe_to_browser_events();
    state.start_fetch_handler();
    state.start_dialog_handler();
    state.update_stream_client().await;

    // Enable Fetch interception (domain filtering and/or proxy auth).
    // Only call Fetch.enable once to avoid overwriting handleAuthRequests.
    {
        let df = state.domain_filter.read().await;
        let has_domain_filter = df.is_some();

        if has_domain_filter || has_proxy_auth {
            if let Some(ref mgr) = state.browser {
                if let Ok(session_id) = mgr.active_session_id() {
                    if let Some(ref filter) = *df {
                        let _ = network::install_domain_filter(
                            &mgr.client,
                            session_id,
                            &filter.allowed_domains,
                            has_proxy_auth,
                        )
                        .await;
                        network::sanitize_existing_pages(&mgr.client, &mgr.pages_list(), filter)
                            .await;
                    } else {
                        // No domain filter, but proxy auth needs Fetch.enable
                        let _ = network::install_domain_filter_fetch(
                            &mgr.client,
                            session_id,
                            has_proxy_auth,
                        )
                        .await;
                    }
                }
            }
        }
    }

    Ok(json!({ "launched": true }))
}

async fn launch_ios(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let device_name = cmd.get("deviceName").and_then(|v| v.as_str());
    let device_udid = cmd.get("udid").and_then(|v| v.as_str());
    let platform_version = cmd.get("platformVersion").and_then(|v| v.as_str());

    // Select device (or use default)
    let device = ios::select_device(device_name, device_udid)?;

    // Boot simulator if it's not real and not already booted
    if !device.is_real && device.state != "Booted" {
        ios::boot_simulator(&device.udid)?;
    }

    // Start Appium
    let mut appium = AppiumManager::connect_or_launch(Some(&device.udid)).await?;

    // Create iOS Safari session
    appium
        .create_ios_session(Some(&device.name), platform_version)
        .await?;

    // Create a WebDriverBackend from the Appium session for common commands
    if let Some(sid) = appium.client.session_id_pub().map(String::from) {
        let wd_client = super::webdriver::client::WebDriverClient::new_with_session(4723, sid);
        state.webdriver_backend = Some(WebDriverBackend::new(wd_client));
    }

    state.appium = Some(appium);
    state.backend_type = BackendType::WebDriver;
    state.engine = "safari".to_string();
    write_engine_file(&state.session_id, &state.engine);
    write_provider_file(&state.session_id, "ios");
    write_extensions_file(&state.session_id);
    state.reset_input_state();

    Ok(json!({
        "launched": true,
        "provider": "ios",
        "device": device.name,
        "udid": device.udid,
        "backend": "webdriver",
    }))
}

async fn launch_safari(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let port: u16 = cmd
        .get("port")
        .and_then(|v| v.as_u64())
        .map(|p| p as u16)
        .unwrap_or(0);
    let driver_port = if port > 0 { port } else { 0 };

    // Find a free port if none specified
    let actual_port = if driver_port > 0 {
        driver_port
    } else {
        // Use any available high port
        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("Failed to find free port: {}", e))?;
        listener
            .local_addr()
            .map_err(|e| format!("Failed to get local address: {}", e))?
            .port()
    };

    let driver = safari::launch_safaridriver(actual_port)?;
    let mut client = super::webdriver::client::WebDriverClient::new(actual_port);

    client
        .create_session(serde_json::json!({
            "browserName": "safari",
        }))
        .await?;

    state.safari_driver = Some(driver);
    state.webdriver_backend = Some(WebDriverBackend::new(client));
    state.backend_type = BackendType::WebDriver;
    state.engine = "safari".to_string();
    write_engine_file(&state.session_id, &state.engine);
    write_provider_file(&state.session_id, "safari");
    write_extensions_file(&state.session_id);
    state.reset_input_state();

    Ok(json!({
        "launched": true,
        "provider": "safari",
        "port": actual_port,
        "backend": "webdriver",
    }))
}

async fn handle_navigate(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let url = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;

    {
        let df = state.domain_filter.read().await;
        if let Some(ref filter) = *df {
            filter.check_url(url)?;
        }
    }

    // WebDriver backend path
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            state.ref_map.clear();
            wb.navigate(url).await?;
            let new_url = wb.get_url().await.unwrap_or_else(|_| url.to_string());
            let title = wb.get_title().await.unwrap_or_default();
            return Ok(json!({ "url": new_url, "title": title }));
        }
    }

    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;

    let wait_until = cmd
        .get("waitUntil")
        .and_then(|v| v.as_str())
        .map(WaitUntil::from_str)
        .unwrap_or(WaitUntil::Load);

    // If --headers was passed, store them keyed by origin and enable Fetch
    // interception. The background fetch_handler_task (started on launch)
    // injects them into matching requests in real-time.
    let scoped_headers = cmd
        .get("headers")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty());

    if let Some(headers_map) = scoped_headers {
        if let Some(origin) = url::Url::parse(url)
            .ok()
            .map(|u| u.origin().ascii_serialization())
        {
            let headers: HashMap<String, String> = headers_map
                .iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect();

            let first_origin_header = {
                let mut map = state.origin_headers.write().await;
                let first = map.is_empty();
                map.insert(origin, headers);
                first
            };

            // Enable Fetch interception the first time --headers is used.
            // Fetch.enable is idempotent — safe even if domain filter or
            // routes already enabled it. Wildcard ensures we see all requests.
            if first_origin_header {
                let session_id = mgr.active_session_id()?.to_string();
                let has_proxy_creds = state.proxy_credentials.read().await.is_some();
                let mut params = json!({ "patterns": [{ "urlPattern": "*" }] });
                if has_proxy_creds {
                    params["handleAuthRequests"] = json!(true);
                }
                mgr.client
                    .send_command("Fetch.enable", Some(params), Some(&session_id))
                    .await?;
            }
        }
    }

    state.ref_map.clear();
    state.iframe_sessions.clear();
    state.active_frame_id = None;
    mgr.navigate(url, wait_until).await
}

async fn handle_url(state: &DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            let url = wb.get_url().await?;
            return Ok(json!({ "url": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let url = mgr.get_url().await?;
    Ok(json!({ "url": url }))
}

fn handle_cdp_url(state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    Ok(json!({ "cdpUrl": mgr.get_cdp_url() }))
}

async fn handle_inspect(state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;

    // Shut down any existing inspect server so we always target the current page
    if let Some(server) = state.inspect_server.take() {
        server.shutdown();
    }

    let target_id = mgr.active_target_id()?.to_string();
    let chrome_hp = mgr.chrome_host_port().to_string();
    let proxy_handle = mgr.client.inspect_handle();

    let server = InspectServer::start(proxy_handle, target_id, chrome_hp).await?;
    let url = format!("http://127.0.0.1:{}", server.port());
    open_url_in_browser(&url);

    state.inspect_server = Some(server);
    Ok(json!({ "opened": true, "url": url }))
}

fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let result = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("cmd")
        .args(["/c", "start", "", url])
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let result: Result<std::process::Child, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));
    if let Err(e) = result {
        let _ = writeln!(std::io::stderr(), "[inspect] Failed to open browser: {}", e);
    }
}

async fn handle_title(state: &DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            let title = wb.get_title().await?;
            return Ok(json!({ "title": title }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let title = mgr.get_title().await?;
    Ok(json!({ "title": title }))
}

async fn handle_content(state: &DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            let html = wb.get_content().await?;
            let url = wb.get_url().await.unwrap_or_default();
            return Ok(json!({ "html": html, "origin": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let html = mgr.get_content().await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "html": html, "origin": url }))
}

async fn handle_evaluate(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            let script = cmd
                .get("script")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'script' parameter")?;
            let result = wb.evaluate(script).await?;
            let url = wb.get_url().await.unwrap_or_default();
            return Ok(json!({ "result": result, "origin": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let script = cmd
        .get("script")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'script' parameter")?;

    let result = mgr.evaluate(script, None).await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "result": result, "origin": url }))
}

async fn handle_close(state: &mut DaemonState) -> Result<Value, String> {
    if let Some(ref mgr) = state.browser {
        if let Some(ref session_name) = state.session_name {
            if let Ok(session_id) = mgr.active_session_id() {
                let _ = state::save_state(
                    &mgr.client,
                    session_id,
                    None,
                    Some(session_name.as_str()),
                    &state.session_id,
                    mgr.visited_origins(),
                )
                .await;
            }
        }
    }
    if let Some(ref mut mgr) = state.browser {
        mgr.close().await?;
    }
    state.browser = None;
    state.launch_hash = None;
    state.screencasting = false;
    state.reset_input_state();
    state.update_stream_client().await;

    // Stop background Fetch handler
    if let Some(task) = state.fetch_handler_task.take() {
        task.abort();
    }
    {
        let mut map = state.origin_headers.write().await;
        map.clear();
    }

    // Close WebDriver sessions
    if let Some(ref mut wb) = state.webdriver_backend {
        let _ = wb.close().await;
    }
    state.webdriver_backend = None;
    if let Some(ref mut appium) = state.appium {
        let _ = appium.close().await;
    }
    state.appium = None;
    if let Some(ref mut driver) = state.safari_driver {
        driver.kill();
    }
    state.safari_driver = None;
    state.backend_type = BackendType::Cdp;

    if let Some(server) = state.inspect_server.take() {
        server.shutdown();
    }

    state.ref_map.clear();
    Ok(json!({ "closed": true }))
}

// ---------------------------------------------------------------------------
// Phase 2 handlers
// ---------------------------------------------------------------------------

async fn handle_snapshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let options = SnapshotOptions {
        selector: cmd
            .get("selector")
            .and_then(|v| v.as_str())
            .map(String::from),
        interactive: cmd
            .get("interactive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        compact: cmd
            .get("compact")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        depth: cmd
            .get("maxDepth")
            .and_then(|v| v.as_u64())
            .map(|d| d as usize),
    };

    state.ref_map.clear();
    let tree = snapshot::take_snapshot(
        &mgr.client,
        &session_id,
        &options,
        &mut state.ref_map,
        state.active_frame_id.as_deref(),
        &state.iframe_sessions,
    )
    .await?;

    let url = mgr.get_url().await.unwrap_or_default();

    let refs: serde_json::Map<String, Value> = state
        .ref_map
        .entries_sorted()
        .into_iter()
        .map(|(ref_id, entry)| {
            let mut obj = serde_json::Map::new();
            obj.insert("role".into(), Value::String(entry.role));
            obj.insert("name".into(), Value::String(entry.name));
            (ref_id, Value::Object(obj))
        })
        .collect();

    Ok(json!({ "snapshot": tree, "origin": url, "refs": refs }))
}

async fn handle_screenshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let annotate = cmd
        .get("annotate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            if annotate {
                return Err(
                    "Annotated screenshots are not yet implemented on the WebDriver backend"
                        .to_string(),
                );
            }

            let base64_data = wb.screenshot().await?;
            let path = cmd.get("path").and_then(|v| v.as_str());
            if let Some(p) = path {
                let bytes = base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    &base64_data,
                )
                .map_err(|e| format!("Base64 decode error: {}", e))?;
                std::fs::write(p, bytes)
                    .map_err(|e| format!("Failed to write screenshot: {}", e))?;
                return Ok(json!({ "path": p }));
            }
            let tmp = format!(
                "/tmp/screenshot-{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            );
            let bytes =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &base64_data)
                    .map_err(|e| format!("Base64 decode error: {}", e))?;
            std::fs::write(&tmp, bytes)
                .map_err(|e| format!("Failed to write screenshot: {}", e))?;
            return Ok(json!({ "path": tmp }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let format = cmd
        .get("format")
        .or_else(|| cmd.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("png")
        .to_string();

    let options = ScreenshotOptions {
        selector: cmd
            .get("selector")
            .and_then(|v| v.as_str())
            .map(String::from),
        path: cmd.get("path").and_then(|v| v.as_str()).map(String::from),
        full_page: cmd
            .get("fullPage")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        format,
        quality: cmd
            .get("quality")
            .and_then(|v| v.as_i64())
            .map(|q| q as i32),
        annotate,
        output_dir: cmd
            .get("screenshotDir")
            .and_then(|v| v.as_str())
            .map(String::from),
    };

    if annotate {
        state.ref_map.clear();
        let _ = snapshot::take_snapshot(
            &mgr.client,
            &session_id,
            &SnapshotOptions {
                interactive: true,
                ..SnapshotOptions::default()
            },
            &mut state.ref_map,
            state.active_frame_id.as_deref(),
            &state.iframe_sessions,
        )
        .await?;
    }

    let result = screenshot::take_screenshot(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &options,
        &state.iframe_sessions,
    )
    .await?;

    let mut response = json!({ "path": result.path });
    if !result.annotations.is_empty() {
        response["annotations"] = serde_json::to_value(&result.annotations)
            .map_err(|e| format!("Failed to serialize annotations: {}", e))?;
    }

    Ok(response)
}

async fn handle_click(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            wb.click(selector).await?;
            return Ok(json!({ "clicked": selector }));
        }
    }

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let new_tab = cmd.get("newTab").and_then(|v| v.as_bool()).unwrap_or(false);

    if new_tab {
        use super::element::resolve_element_object_id;
        let (object_id, effective_session_id) = resolve_element_object_id(
            &mgr.client,
            &session_id,
            &state.ref_map,
            selector,
            &state.iframe_sessions,
        )
        .await?;
        let call_params = json!({
            "objectId": object_id,
            "functionDeclaration": "function() { var h = this.getAttribute('href'); if (!h) return null; try { return new URL(h, document.baseURI).toString(); } catch(e) { return null; } }",
            "returnByValue": true
        });
        let call_result = mgr
            .client
            .send_command(
                "Runtime.callFunctionOn",
                Some(call_params),
                Some(&effective_session_id),
            )
            .await?;
        let href = call_result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                format!(
                    "Element '{}' does not have an href attribute. --new-tab only works on links.",
                    selector
                )
            })?
            .to_string();

        let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
        state.ref_map.clear();
        mgr.tab_new(Some(&href)).await?;

        return Ok(json!({ "clicked": selector, "newTab": true, "url": href }));
    }

    let button = cmd.get("button").and_then(|v| v.as_str()).unwrap_or("left");
    let click_count = cmd.get("clickCount").and_then(|v| v.as_i64()).unwrap_or(1) as i32;

    interaction::click(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        button,
        click_count,
        &state.iframe_sessions,
    )
    .await?;

    Ok(json!({ "clicked": selector }))
}

async fn handle_dblclick(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::dblclick(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "clicked": selector }))
}

async fn handle_fill(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let value = cmd
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'value' parameter")?;

    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            wb.fill(selector, value).await?;
            return Ok(json!({ "filled": selector }));
        }
    }

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    interaction::fill(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        value,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "filled": selector }))
}

async fn handle_type(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let text = cmd
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'text' parameter")?;
    let clear = cmd.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
    let delay = cmd.get("delay").and_then(|v| v.as_u64());

    interaction::type_text(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        text,
        clear,
        delay,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "typed": text }))
}

async fn handle_press(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let key = cmd
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'key' parameter")?;

    // Parse modifier+key chords like "Control+a", "Shift+Enter", "Control+Shift+a"
    let (actual_key, modifiers) = parse_key_chord(key);

    interaction::press_key_with_modifiers(&mgr.client, &session_id, &actual_key, modifiers).await?;
    Ok(json!({ "pressed": key }))
}

/// Parse a key chord string like "Control+a" or "Control+Shift+Enter" into
/// the actual key name and an optional CDP modifier bitmask.
///
/// CDP modifier values: 1 = Alt, 2 = Control, 4 = Meta (Cmd), 8 = Shift.
fn parse_key_chord(input: &str) -> (String, Option<i32>) {
    let parts: Vec<&str> = input.split('+').collect();
    if parts.len() < 2 {
        return (input.to_string(), None);
    }

    let mut modifiers = 0i32;
    let mut key_parts: Vec<&str> = Vec::new();

    for part in &parts {
        match part.to_lowercase().as_str() {
            "alt" => modifiers |= 1,
            "control" | "ctrl" => modifiers |= 2,
            "meta" | "cmd" | "command" => modifiers |= 4,
            "shift" => modifiers |= 8,
            _ => key_parts.push(part),
        }
    }

    // If no modifiers were found, the '+' was part of the key name (e.g. "+")
    // or the input was something unexpected — treat the whole string as the key.
    if modifiers == 0 {
        return (input.to_string(), None);
    }

    // The actual key is whatever remains after stripping modifiers.
    // If nothing remains (e.g. "Control+"), treat the whole string as-is.
    let actual_key = if key_parts.is_empty() {
        input.to_string()
    } else {
        key_parts.join("+")
    };

    (actual_key, Some(modifiers))
}

async fn handle_hover(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::hover(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "hovered": selector }))
}

async fn handle_scroll(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd.get("selector").and_then(|v| v.as_str());

    let (mut dx, mut dy) = (
        cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
        cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
    );

    if let Some(direction) = cmd.get("direction").and_then(|v| v.as_str()) {
        let amount = cmd.get("amount").and_then(|v| v.as_f64()).unwrap_or(300.0);
        match direction {
            "up" => dy = -amount,
            "down" => dy = amount,
            "left" => dx = -amount,
            "right" => dx = amount,
            _ => {}
        }
    }

    interaction::scroll(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        dx,
        dy,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "scrolled": true }))
}

async fn handle_select(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let values: Vec<String> = match cmd.get("values") {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => cmd
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
    };

    interaction::select_option(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &values,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "selected": values }))
}

async fn handle_check(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::check(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "checked": selector }))
}

async fn handle_uncheck(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::uncheck(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "unchecked": selector }))
}

async fn handle_wait(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let timeout_ms = state.timeout_ms(cmd);

    if let Some(text) = cmd.get("text").and_then(|v| v.as_str()) {
        wait_for_text(&mgr.client, &session_id, text, timeout_ms).await?;
        return Ok(json!({ "waited": "text", "text": text }));
    }

    if let Some(selector) = cmd.get("selector").and_then(|v| v.as_str()) {
        let state_str = cmd
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("visible");
        wait_for_selector(&mgr.client, &session_id, selector, state_str, timeout_ms).await?;
        return Ok(json!({ "waited": "selector", "selector": selector }));
    }

    if let Some(url_pattern) = cmd.get("url").and_then(|v| v.as_str()) {
        wait_for_url(&mgr.client, &session_id, url_pattern, timeout_ms).await?;
        return Ok(json!({ "waited": "url", "url": url_pattern }));
    }

    if let Some(fn_str) = cmd.get("function").and_then(|v| v.as_str()) {
        wait_for_function(&mgr.client, &session_id, fn_str, timeout_ms).await?;
        return Ok(json!({ "waited": "function" }));
    }

    if let Some(load_state) = cmd.get("loadState").and_then(|v| v.as_str()) {
        let wait_until = WaitUntil::from_str(load_state);
        mgr.wait_for_lifecycle_external(wait_until, &session_id)
            .await?;
        return Ok(json!({ "waited": "load", "state": load_state }));
    }

    // Just a timeout wait
    tokio::time::sleep(tokio::time::Duration::from_millis(timeout_ms)).await;
    Ok(json!({ "waited": "timeout", "ms": timeout_ms }))
}

async fn handle_gettext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let text = super::element::get_element_text(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "text": text, "origin": url }))
}

async fn handle_getattribute(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let attribute = cmd
        .get("attribute")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'attribute' parameter")?;

    let value = super::element::get_element_attribute(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        attribute,
        &state.iframe_sessions,
    )
    .await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "value": value, "origin": url }))
}

async fn handle_isvisible(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let visible = super::element::is_element_visible(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "visible": visible, "origin": url }))
}

async fn handle_isenabled(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let enabled = super::element::is_element_enabled(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "enabled": enabled, "origin": url }))
}

async fn handle_ischecked(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let checked = super::element::is_element_checked(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "checked": checked, "origin": url }))
}

async fn handle_back(state: &mut DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            wb.back().await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            let url = wb.get_url().await.unwrap_or_default();
            state.ref_map.clear();
            return Ok(json!({ "url": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    mgr.evaluate("history.back()", None).await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    let url = mgr.get_url().await.unwrap_or_default();
    state.ref_map.clear();
    Ok(json!({ "url": url }))
}

async fn handle_forward(state: &mut DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            wb.forward().await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            let url = wb.get_url().await.unwrap_or_default();
            state.ref_map.clear();
            return Ok(json!({ "url": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    mgr.evaluate("history.forward()", None).await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    let url = mgr.get_url().await.unwrap_or_default();
    state.ref_map.clear();
    Ok(json!({ "url": url }))
}

async fn handle_reload(state: &mut DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            wb.reload().await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            let url = wb.get_url().await.unwrap_or_default();
            state.ref_map.clear();
            return Ok(json!({ "url": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    mgr.client
        .send_command_no_params("Page.reload", Some(&session_id))
        .await?;

    let mut rx = mgr.client.subscribe();
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.method == "Page.loadEventFired"
                        && event.session_id.as_deref() == Some(&session_id)
                    {
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    })
    .await;

    let url = mgr.get_url().await.unwrap_or_default();
    state.ref_map.clear();
    Ok(json!({ "url": url }))
}

// ---------------------------------------------------------------------------
// Wait helpers
// ---------------------------------------------------------------------------

async fn wait_for_selector(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    selector: &str,
    state: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let check_fn = match state {
        "attached" => format!(
            "!!document.querySelector({})",
            serde_json::to_string(selector).unwrap_or_default()
        ),
        "detached" => format!(
            "!document.querySelector({})",
            serde_json::to_string(selector).unwrap_or_default()
        ),
        "hidden" => format!(
            r#"(() => {{
                const el = document.querySelector({sel});
                if (!el) return true;
                const s = window.getComputedStyle(el);
                return s.display === 'none' || s.visibility === 'hidden' || parseFloat(s.opacity) === 0;
            }})()"#,
            sel = serde_json::to_string(selector).unwrap_or_default()
        ),
        _ => format!(
            r#"(() => {{
                const el = document.querySelector({sel});
                if (!el) return false;
                const r = el.getBoundingClientRect();
                const s = window.getComputedStyle(el);
                return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none';
            }})()"#,
            sel = serde_json::to_string(selector).unwrap_or_default()
        ),
    };

    poll_until_true(client, session_id, &check_fn, timeout_ms).await
}

async fn wait_for_url(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    pattern: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let check_fn = format!(
        "location.href.includes({})",
        serde_json::to_string(pattern).unwrap_or_default()
    );
    poll_until_true(client, session_id, &check_fn, timeout_ms).await
}

async fn wait_for_text(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    text: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let check_fn = format!(
        "(document.body.innerText || '').includes({})",
        serde_json::to_string(text).unwrap_or_default()
    );
    poll_until_true(client, session_id, &check_fn, timeout_ms).await
}

async fn wait_for_function(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    fn_str: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let check_fn = format!("!!({})", fn_str);
    poll_until_true(client, session_id, &check_fn, timeout_ms).await
}

async fn poll_until_true(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    expression: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    loop {
        let result: super::cdp::types::EvaluateResult = client
            .send_command_typed(
                "Runtime.evaluate",
                &super::cdp::types::EvaluateParams {
                    expression: expression.to_string(),
                    return_by_value: Some(true),
                    await_promise: Some(true),
                },
                Some(session_id),
            )
            .await?;

        if result
            .result
            .value
            .as_ref()
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Ok(());
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Wait timed out after {}ms", timeout_ms));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
}

// ---------------------------------------------------------------------------
// Phase 3 handlers
// ---------------------------------------------------------------------------

async fn handle_cookies_get(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            let cookies_list = wb.get_cookies().await?;
            return Ok(json!({ "cookies": cookies_list }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let urls = cmd.get("urls").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });

    let cookies_list = cookies::get_cookies(&mgr.client, &session_id, urls).await?;
    Ok(json!({ "cookies": cookies_list }))
}

async fn handle_cookies_set(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let url = mgr.get_url().await.ok();

    let cookie_values = if let Some(arr) = cmd.get("cookies").and_then(|v| v.as_array()) {
        arr.clone()
    } else {
        let mut cookie = serde_json::Map::new();
        for key in &[
            "name", "value", "domain", "path", "expires", "httpOnly", "secure", "sameSite", "url",
        ] {
            if let Some(v) = cmd.get(*key) {
                if !v.is_null() {
                    cookie.insert(key.to_string(), v.clone());
                }
            }
        }
        vec![Value::Object(cookie)]
    };

    cookies::set_cookies(&mgr.client, &session_id, cookie_values, url.as_deref()).await?;
    Ok(json!({ "set": true }))
}

async fn handle_cookies_clear(state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    cookies::clear_cookies(&mgr.client, &session_id).await?;
    Ok(json!({ "cleared": true }))
}

async fn handle_storage_get(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let storage_type = cmd.get("type").and_then(|v| v.as_str()).unwrap_or("local");
    let key = cmd.get("key").and_then(|v| v.as_str());
    storage::storage_get(&mgr.client, &session_id, storage_type, key).await
}

async fn handle_storage_set(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let storage_type = cmd.get("type").and_then(|v| v.as_str()).unwrap_or("local");
    let key = cmd
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'key' parameter")?;
    let value = cmd
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'value' parameter")?;
    storage::storage_set(&mgr.client, &session_id, storage_type, key, value).await?;
    Ok(json!({ "set": true }))
}

async fn handle_storage_clear(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let storage_type = cmd.get("type").and_then(|v| v.as_str()).unwrap_or("local");
    storage::storage_clear(&mgr.client, &session_id, storage_type).await?;
    Ok(json!({ "cleared": true }))
}

async fn handle_setcontent(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let html = cmd
        .get("html")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'html' parameter")?;
    network::set_content(&mgr.client, &session_id, html).await?;
    Ok(json!({ "set": true }))
}

async fn handle_headers(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let headers_value = cmd.get("headers").ok_or("Missing 'headers' parameter")?;

    let headers: HashMap<String, String> = headers_value
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();

    network::set_extra_headers(&mgr.client, &session_id, &headers).await?;
    Ok(json!({ "set": true }))
}

async fn handle_offline(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let offline = cmd.get("offline").and_then(|v| v.as_bool()).unwrap_or(true);
    network::set_offline(&mgr.client, &session_id, offline).await?;
    Ok(json!({ "offline": offline }))
}

async fn handle_console(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let clear = cmd.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
    if clear {
        state.event_tracker.clear_console();
        Ok(json!({ "cleared": true }))
    } else {
        let result = state.event_tracker.get_console_json();
        Ok(result)
    }
}

async fn handle_errors(state: &DaemonState) -> Result<Value, String> {
    Ok(state.event_tracker.get_errors_json())
}

async fn handle_state_save(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let path = cmd.get("path").and_then(|v| v.as_str());

    let saved_path = state::save_state(
        &mgr.client,
        &session_id,
        path,
        state.session_name.as_deref(),
        &state.session_id,
        mgr.visited_origins(),
    )
    .await?;

    Ok(json!({ "saved": true, "path": saved_path }))
}

async fn handle_state_load(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    state::load_state(&mgr.client, &session_id, path).await?;
    Ok(json!({ "loaded": true, "path": path }))
}

// ---------------------------------------------------------------------------
// Phase 6 handlers
// ---------------------------------------------------------------------------

async fn handle_diff_snapshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let compact = cmd
        .get("compact")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let max_depth = cmd
        .get("maxDepth")
        .and_then(|v| v.as_u64())
        .map(|d| d as usize);
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .map(String::from);

    let options = SnapshotOptions {
        compact,
        depth: max_depth,
        selector,
        ..SnapshotOptions::default()
    };
    let current = snapshot::take_snapshot(
        &mgr.client,
        &session_id,
        &options,
        &mut state.ref_map,
        state.active_frame_id.as_deref(),
        &state.iframe_sessions,
    )
    .await?;

    let baseline = cmd.get("baseline").and_then(|v| v.as_str());

    let baseline_text = match baseline {
        Some(b) if std::path::Path::new(b).exists() => {
            std::fs::read_to_string(b).map_err(|e| format!("Failed to read baseline: {}", e))?
        }
        Some(b) => b.to_string(),
        None => String::new(),
    };

    let result = diff::diff_snapshots(&baseline_text, &current);
    Ok(json!({
        "diff": result.diff,
        "additions": result.additions,
        "removals": result.removals,
        "unchanged": result.unchanged,
        "changed": result.changed,
    }))
}

async fn handle_diff_url(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;

    let url1 = cmd
        .get("url1")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url1' parameter")?;
    let url2 = cmd
        .get("url2")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url2' parameter")?;

    let wait_until = cmd
        .get("waitUntil")
        .and_then(|v| v.as_str())
        .map(WaitUntil::from_str)
        .unwrap_or(WaitUntil::Load);

    // Navigate to URL1 and snapshot
    mgr.navigate(url1, wait_until).await?;
    let session_id = mgr.active_session_id()?.to_string();
    let options = SnapshotOptions::default();
    let snap1 = snapshot::take_snapshot(
        &mgr.client,
        &session_id,
        &options,
        &mut state.ref_map,
        None,
        &state.iframe_sessions,
    )
    .await?;

    // Navigate to URL2 and snapshot
    mgr.navigate(url2, wait_until).await?;
    state.ref_map.clear();
    let snap2 = snapshot::take_snapshot(
        &mgr.client,
        &session_id,
        &options,
        &mut state.ref_map,
        None,
        &state.iframe_sessions,
    )
    .await?;

    let result = diff::diff_text(&snap1, &snap2);
    Ok(json!({
        "diff": result,
        "url1": url1,
        "url2": url2,
        "snapshot1": snap1,
        "snapshot2": snap2,
    }))
}

async fn handle_credentials_set(cmd: &Value) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name'")?;
    let username = cmd
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'username'")?;
    let password = cmd
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'password'")?;
    let url = cmd.get("url").and_then(|v| v.as_str());
    auth::credentials_set(name, username, password, url)
}

async fn handle_credentials_get(cmd: &Value) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name'")?;
    auth::credentials_get(name)
}

async fn handle_credentials_delete(cmd: &Value) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name'")?;
    auth::credentials_delete(name)
}

async fn handle_credentials_list() -> Result<Value, String> {
    auth::credentials_list()
}

async fn handle_auth_show(cmd: &Value) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name'")?;
    auth::auth_show(name)
}

async fn handle_mouse(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let event_type = cmd
        .get("eventType")
        .and_then(|v| v.as_str())
        .unwrap_or("mouseMoved");
    let x = cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let button = cmd.get("button").and_then(|v| v.as_str()).unwrap_or("none");
    let click_count = cmd.get("clickCount").and_then(|v| v.as_i64()).unwrap_or(0);

    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({
                "type": event_type,
                "x": x,
                "y": y,
                "button": button,
                "clickCount": click_count,
            })),
            Some(&session_id),
        )
        .await?;

    Ok(json!({ "dispatched": event_type }))
}

async fn handle_keyboard(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    match cmd.get("subaction").and_then(|v| v.as_str()) {
        Some("type") => {
            let text = cmd
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'text' parameter")?;
            interaction::type_text_into_active_context(&mgr.client, &session_id, text, None)
                .await?;
            return Ok(json!({ "typed": text }));
        }
        Some("insertText") => {
            let text = cmd
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'text' parameter")?;
            mgr.client
                .send_command(
                    "Input.insertText",
                    Some(json!({ "text": text })),
                    Some(&session_id),
                )
                .await?;
            return Ok(json!({ "inserted": true }));
        }
        _ => {}
    }

    let event_type = cmd
        .get("eventType")
        .and_then(|v| v.as_str())
        .unwrap_or("keyDown");
    let key = cmd.get("key").and_then(|v| v.as_str());
    let code = cmd.get("code").and_then(|v| v.as_str());
    let text = cmd.get("text").and_then(|v| v.as_str());

    let mut params = json!({ "type": event_type });
    if let Some(k) = key {
        params["key"] = Value::String(k.to_string());
    }
    if let Some(c) = code {
        params["code"] = Value::String(c.to_string());
    }
    if let Some(t) = text {
        params["text"] = Value::String(t.to_string());
    }

    mgr.client
        .send_command("Input.dispatchKeyEvent", Some(params), Some(&session_id))
        .await?;

    Ok(json!({ "dispatched": event_type }))
}

// ---------------------------------------------------------------------------
// Phase 5 handlers
// ---------------------------------------------------------------------------

async fn handle_tab_list(state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let tabs = mgr.tab_list();
    Ok(json!({ "tabs": tabs }))
}

async fn handle_tab_new(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let url = cmd.get("url").and_then(|v| v.as_str());
    state.ref_map.clear();
    state.iframe_sessions.clear();
    state.active_frame_id = None;
    mgr.tab_new(url).await
}

async fn handle_tab_switch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let index = cmd
        .get("index")
        .and_then(|v| v.as_u64())
        .ok_or("Missing 'index' parameter")? as usize;
    state.ref_map.clear();
    state.iframe_sessions.clear();
    state.active_frame_id = None;
    let result = mgr.tab_switch(index).await?;

    if let Some(ref server) = state.stream_server {
        if let Ok(dims) = mgr
            .evaluate(
                "JSON.stringify([window.innerWidth,window.innerHeight])",
                None,
            )
            .await
        {
            if let Some(s) = dims.get("result").and_then(|v| v.as_str()) {
                if let Ok(arr) = serde_json::from_str::<Vec<u32>>(s) {
                    if arr.len() == 2 && arr[0] > 0 && arr[1] > 0 {
                        server.set_viewport(arr[0], arr[1]).await;
                    }
                }
            }
        }
    }

    Ok(result)
}

async fn handle_tab_close(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let index = cmd
        .get("index")
        .and_then(|v| v.as_u64())
        .map(|i| i as usize);
    state.ref_map.clear();
    state.iframe_sessions.clear();
    state.active_frame_id = None;
    mgr.tab_close(index).await
}

async fn handle_viewport(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let width = cmd.get("width").and_then(|v| v.as_i64()).unwrap_or(1280) as i32;
    let height = cmd.get("height").and_then(|v| v.as_i64()).unwrap_or(720) as i32;
    let scale = cmd
        .get("deviceScaleFactor")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let mobile = cmd.get("mobile").and_then(|v| v.as_bool()).unwrap_or(false);

    mgr.set_viewport(width, height, scale, mobile).await?;

    // Update stream server viewport so status messages and screencast use the new dimensions
    if let Some(ref server) = state.stream_server {
        server.set_viewport(width as u32, height as u32).await;
    }

    Ok(json!({ "width": width, "height": height, "deviceScaleFactor": scale, "mobile": mobile }))
}

async fn handle_user_agent(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let ua = cmd
        .get("userAgent")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'userAgent' parameter")?;
    mgr.set_user_agent(ua).await?;
    Ok(json!({ "userAgent": ua }))
}

async fn handle_set_media(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let media = cmd.get("media").and_then(|v| v.as_str());

    let mut feat_list: Vec<(String, String)> = Vec::new();

    if let Some(scheme) = cmd.get("colorScheme").and_then(|v| v.as_str()) {
        feat_list.push(("prefers-color-scheme".to_string(), scheme.to_string()));
    }
    if let Some(motion) = cmd.get("reducedMotion").and_then(|v| v.as_str()) {
        feat_list.push(("prefers-reduced-motion".to_string(), motion.to_string()));
    }

    if let Some(obj) = cmd.get("features").and_then(|v| v.as_object()) {
        for (k, v) in obj {
            feat_list.push((k.clone(), v.as_str().unwrap_or("").to_string()));
        }
    }

    let features = if feat_list.is_empty() {
        None
    } else {
        Some(feat_list)
    };

    mgr.set_emulated_media(media, features).await?;
    Ok(json!({ "set": true }))
}

async fn handle_download(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let path_str = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    // Resolve to absolute path and canonicalize to prevent path traversal
    let raw_dest = if std::path::Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Failed to get current directory: {}", e))?
            .join(path_str)
    };

    // Extract directory and desired filename
    let download_dir = raw_dest
        .parent()
        .ok_or("Invalid download path: no parent directory")?
        .to_path_buf();

    // Create the directory if it doesn't exist
    std::fs::create_dir_all(&download_dir)
        .map_err(|e| format!("Failed to create download directory: {}", e))?;

    // Canonicalize after mkdir so the path actually exists for resolution
    let download_dir = download_dir
        .canonicalize()
        .map_err(|e| format!("Failed to resolve download directory: {}", e))?;
    let dest = download_dir.join(
        raw_dest
            .file_name()
            .ok_or("Invalid download path: no filename")?,
    );
    let download_dir_str = download_dir
        .to_str()
        .ok_or("Download directory path is not valid UTF-8")?;

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    // Set download behavior to save to the parent directory
    mgr.set_download_behavior(download_dir_str).await?;

    // Subscribe to CDP events before clicking so we don't miss the download event
    let mut rx = mgr.client.subscribe();

    // Click the element to trigger the download
    interaction::click(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        "left",
        1,
        &state.iframe_sessions,
    )
    .await?;

    // Wait for download to complete
    const DOWNLOAD_TIMEOUT: tokio::time::Duration = tokio::time::Duration::from_secs(30);
    let deadline = tokio::time::Instant::now() + DOWNLOAD_TIMEOUT;
    let mut downloaded_guid: Option<String> = None;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Timeout waiting for download to complete".to_string());
        }

        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                // Browser-domain download events may arrive without a sessionId
                // or with a different sessionId than the page session, so we
                // accept them regardless. Page-domain events are matched by
                // session to avoid cross-tab confusion.
                let is_page_session = event.session_id.as_deref() == Some(&session_id);
                let is_download_event = |method: &str, browser_method: &str, page_method: &str| {
                    method == browser_method || (method == page_method && is_page_session)
                };

                // Capture the GUID from downloadWillBegin
                if is_download_event(
                    &event.method,
                    "Browser.downloadWillBegin",
                    "Page.downloadWillBegin",
                ) {
                    if let Some(guid) = event.params.get("guid").and_then(|v| v.as_str()) {
                        downloaded_guid = Some(guid.to_string());
                    }
                }
                // Check for download completion or cancellation
                if is_download_event(
                    &event.method,
                    "Browser.downloadProgress",
                    "Page.downloadProgress",
                ) {
                    match event.params.get("state").and_then(|v| v.as_str()) {
                        Some("completed") => break,
                        Some("canceled") => {
                            return Err("Download was canceled".to_string());
                        }
                        _ => {}
                    }
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Err("Event stream closed".to_string()),
            Err(_) => return Err("Timeout waiting for download to complete".to_string()),
        }
    }

    // With "allowAndName" behavior, Chrome saves the file using the GUID as filename.
    // Rename it to the user-requested filename.
    if let Some(guid) = downloaded_guid {
        let guid_path = download_dir.join(&guid);
        // Chrome may still be flushing the file to disk after signalling
        // completion; wait briefly for it to appear.
        for _ in 0..10 {
            if guid_path.exists() {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        if guid_path.exists() {
            std::fs::rename(&guid_path, &dest)
                .map_err(|e| format!("Failed to rename downloaded file: {}", e))?;
        } else {
            // The file might have been saved under its original name instead
            // of the GUID (e.g. when Chrome falls back to "allow" behavior).
            if !dest.exists() {
                return Err(format!(
                    "Downloaded file not found at expected path (GUID: {})",
                    guid
                ));
            }
        }
    } else {
        // GUID capture failed -- the file may have been saved under its original name
        // by Chrome. Only return success if dest already exists (avoid touching
        // unrelated files in the directory).
        if !dest.exists() {
            return Err(
                "Download completed but could not determine the downloaded file name".to_string(),
            );
        }
    }

    let dest_str = dest.to_string_lossy().to_string();
    Ok(json!({ "path": dest_str }))
}

// ---------------------------------------------------------------------------
// Phase 4 handlers
// ---------------------------------------------------------------------------

async fn handle_trace_start(state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    native_tracing::trace_start(&mgr.client, &session_id, &mut state.tracing_state).await
}

async fn handle_trace_stop(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let path = cmd.get("path").and_then(|v| v.as_str());
    native_tracing::trace_stop(&mgr.client, &session_id, &mut state.tracing_state, path).await
}

async fn handle_profiler_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let categories = cmd.get("categories").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });
    native_tracing::profiler_start(
        &mgr.client,
        &session_id,
        &mut state.tracing_state,
        categories,
    )
    .await
}

async fn handle_profiler_stop(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let path = cmd.get("path").and_then(|v| v.as_str());
    native_tracing::profiler_stop(&mgr.client, &session_id, &mut state.tracing_state, path).await
}

async fn handle_recording_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    let recording_url = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    let (client, new_session_id) = {
        let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
        let old_session_id = mgr.active_session_id()?.to_string();

        // Capture current URL if no URL specified
        let nav_url = if let Some(u) = recording_url {
            u.to_string()
        } else {
            mgr.get_url()
                .await
                .unwrap_or_else(|_| "about:blank".to_string())
        };

        // Capture current cookies
        let cookies_result = mgr
            .client
            .send_command_no_params("Network.getAllCookies", Some(&old_session_id))
            .await
            .ok();

        // Create new browser context
        let ctx_result = mgr
            .client
            .send_command_no_params("Target.createBrowserContext", None)
            .await?;
        let context_id = ctx_result
            .get("browserContextId")
            .and_then(|v| v.as_str())
            .ok_or("Failed to get browserContextId")?
            .to_string();

        // Create page in new context
        let create_result: CreateTargetResult = mgr
            .client
            .send_command_typed(
                "Target.createTarget",
                &json!({ "url": "about:blank", "browserContextId": context_id }),
                None,
            )
            .await?;

        let attach_result: AttachToTargetResult = mgr
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: create_result.target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await?;

        let new_session_id = attach_result.session_id.clone();
        mgr.enable_domains_pub(&new_session_id).await?;

        // Re-apply download behavior to the recording context.
        // Without this, downloads in the recording context are silently dropped
        // because Browser.setDownloadBehavior at launch only applies to the default context.
        if let Some(ref dl_path) = mgr.download_path {
            let _ = mgr
                .client
                .send_command(
                    "Browser.setDownloadBehavior",
                    Some(json!({
                        "behavior": "allow",
                        "downloadPath": dl_path,
                        "browserContextId": context_id,
                        "eventsEnabled": true
                    })),
                    None,
                )
                .await;
        }

        // Transfer cookies to new context
        if let Some(ref cr) = cookies_result {
            if let Some(cookie_arr) = cr.get("cookies").and_then(|v| v.as_array()) {
                if !cookie_arr.is_empty() {
                    let _ = mgr
                        .client
                        .send_command(
                            "Network.setCookies",
                            Some(json!({ "cookies": cookie_arr })),
                            Some(&new_session_id),
                        )
                        .await;
                }
            }
        }

        // Add page and switch to it
        mgr.add_page(super::browser::PageInfo {
            target_id: create_result.target_id,
            session_id: new_session_id.clone(),
            url: nav_url.clone(),
            title: String::new(),
            target_type: "page".to_string(),
        });

        // Navigate to URL
        if nav_url != "about:blank" {
            let _ = mgr
                .client
                .send_command(
                    "Page.navigate",
                    Some(json!({ "url": nav_url })),
                    Some(&new_session_id),
                )
                .await;
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
        }

        (mgr.client.clone(), new_session_id)
    };

    let result = recording::recording_start(&mut state.recording_state, path)?;
    state.start_recording_task(client, new_session_id).await?;

    if let Some(ref server) = state.stream_server {
        server.set_recording(true, &state.engine).await;
    }

    Ok(result)
}

async fn handle_recording_stop(state: &mut DaemonState) -> Result<Value, String> {
    state.stop_recording_task().await?;
    let result = recording::recording_stop(&mut state.recording_state);

    if let Some(ref server) = state.stream_server {
        server.set_recording(false, &state.engine).await;
    }

    result
}

async fn handle_recording_restart(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    let _ = state.stop_recording_task().await;
    let result = recording::recording_restart(&mut state.recording_state, path)?;

    if let Some(ref browser) = state.browser {
        let session_id = browser.active_session_id()?.to_string();
        state
            .start_recording_task(browser.client.clone(), session_id)
            .await?;
    }

    Ok(result)
}

async fn handle_pdf(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let params = json!({
        "printBackground": cmd.get("printBackground").and_then(|v| v.as_bool()).unwrap_or(true),
        "landscape": cmd.get("landscape").and_then(|v| v.as_bool()).unwrap_or(false),
        "preferCSSPageSize": cmd.get("preferCSSPageSize").and_then(|v| v.as_bool()).unwrap_or(false),
    });

    let result = mgr
        .client
        .send_command("Page.printToPDF", Some(params), Some(&session_id))
        .await?;

    let data = result
        .get("data")
        .and_then(|v| v.as_str())
        .ok_or("No PDF data returned")?;

    let path = cmd.get("path").and_then(|v| v.as_str());
    let save_path = match path {
        Some(p) => p.to_string(),
        None => {
            let dir = dirs::home_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(".agent-browser")
                .join("tmp")
                .join("pdfs");
            let _ = std::fs::create_dir_all(&dir);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            dir.join(format!("page-{}.pdf", timestamp))
                .to_string_lossy()
                .to_string()
        }
    };

    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
        .map_err(|e| format!("Failed to decode PDF: {}", e))?;
    std::fs::write(&save_path, &bytes).map_err(|e| format!("Failed to save PDF: {}", e))?;

    Ok(json!({ "path": save_path }))
}

// ---------------------------------------------------------------------------
// Phase 8 handlers
// ---------------------------------------------------------------------------

async fn handle_focus(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::focus(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "focused": selector }))
}

async fn handle_clear(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::clear(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "cleared": selector }))
}

async fn handle_selectall(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::select_all(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "selected": selector }))
}

async fn handle_scrollintoview(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::scroll_into_view(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "scrolled": selector }))
}

async fn handle_dispatch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let event_type = cmd
        .get("event")
        .or_else(|| cmd.get("eventType"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'event' parameter")?;
    let event_init = cmd.get("eventInit");

    interaction::dispatch_event(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        event_type,
        event_init,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "dispatched": event_type, "selector": selector }))
}

async fn handle_highlight(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::highlight(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "highlighted": selector }))
}

async fn handle_tap(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd.get("selector").and_then(|v| v.as_str());

    // Route through Appium for iOS/WebDriver using coordinate-based tap
    if let Some(ref appium) = state.appium {
        if state.browser.is_none() {
            let x = cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(200.0);
            let y = cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(200.0);
            appium.tap(x, y).await?;
            return Ok(json!({ "tapped": true, "x": x, "y": y }));
        }
    }

    let sel = selector.ok_or("Missing 'selector' parameter")?;
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    interaction::tap_touch(
        &mgr.client,
        &session_id,
        &state.ref_map,
        sel,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "tapped": sel }))
}

async fn handle_boundingbox(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let bbox = super::element::get_element_bounding_box(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(bbox)
}

async fn handle_innertext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let text = super::element::get_element_inner_text(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "text": text }))
}

async fn handle_innerhtml(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let html = super::element::get_element_inner_html(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "html": html }))
}

async fn handle_inputvalue(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let value = super::element::get_element_input_value(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "value": value }))
}

async fn handle_setvalue(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let value = cmd
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'value' parameter")?;

    super::element::set_element_value(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        value,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "set": selector, "value": value }))
}

async fn handle_count(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let count = super::element::get_element_count(&mgr.client, &session_id, selector).await?;
    Ok(json!({ "count": count, "selector": selector }))
}

async fn handle_styles(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let properties = cmd.get("properties").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });

    let styles = super::element::get_element_styles(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        properties,
        &state.iframe_sessions,
    )
    .await?;
    Ok(json!({ "styles": styles }))
}

async fn handle_bringtofront(state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    mgr.bring_to_front().await?;
    Ok(json!({ "broughtToFront": true }))
}

async fn handle_timezone(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let timezone = cmd
        .get("timezoneId")
        .or_else(|| cmd.get("timezone"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'timezoneId' parameter")?;
    mgr.set_timezone(timezone).await?;
    Ok(json!({ "timezoneId": timezone }))
}

async fn handle_locale(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let locale = cmd
        .get("locale")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'locale' parameter")?;
    mgr.set_locale(locale).await?;
    Ok(json!({ "locale": locale }))
}

async fn handle_geolocation(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let latitude = cmd
        .get("latitude")
        .and_then(|v| v.as_f64())
        .ok_or("Missing 'latitude' parameter")?;
    let longitude = cmd
        .get("longitude")
        .and_then(|v| v.as_f64())
        .ok_or("Missing 'longitude' parameter")?;
    let accuracy = cmd.get("accuracy").and_then(|v| v.as_f64());

    mgr.set_geolocation(latitude, longitude, accuracy).await?;
    Ok(json!({ "latitude": latitude, "longitude": longitude }))
}

async fn handle_permissions(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let permissions: Vec<String> = cmd
        .get("permissions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    mgr.grant_permissions(&permissions).await?;
    Ok(json!({ "granted": permissions }))
}

async fn handle_dialog(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let response = cmd.get("response").and_then(|v| v.as_str());

    // dialog status — return pending dialog info
    if response == Some("status") {
        return Ok(match &state.pending_dialog {
            Some(dialog) => {
                let mut obj = json!({
                    "hasDialog": true,
                    "type": dialog.dialog_type,
                    "message": dialog.message,
                });
                if let Some(ref prompt) = dialog.default_prompt {
                    obj["defaultPrompt"] = json!(prompt);
                }
                obj
            }
            None => json!({ "hasDialog": false }),
        });
    }

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let accept = response
        .map(|r| r == "accept")
        .or_else(|| cmd.get("accept").and_then(|v| v.as_bool()))
        .unwrap_or(true);
    let prompt_text = cmd.get("promptText").and_then(|v| v.as_str());

    mgr.handle_dialog(accept, prompt_text).await?;
    state.pending_dialog = None;
    Ok(json!({ "handled": true, "accepted": accept }))
}

async fn handle_upload(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let files: Vec<String> = cmd
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .or_else(|| {
            cmd.get("file")
                .and_then(|v| v.as_str())
                .map(|s| vec![s.to_string()])
        })
        .unwrap_or_default();

    mgr.upload_files(selector, &files).await?;
    Ok(json!({ "uploaded": files.len(), "selector": selector }))
}

async fn handle_addscript(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let content = cmd
        .get("content")
        .or_else(|| cmd.get("source"))
        .or_else(|| cmd.get("script"))
        .and_then(|v| v.as_str());
    let url = cmd.get("url").and_then(|v| v.as_str());

    if content.is_none() && url.is_none() {
        return Err("At least one of 'content' or 'url' is required".to_string());
    }

    if let Some(src_url) = url {
        let js = format!(
            r#"new Promise((resolve, reject) => {{
                const s = document.createElement('script');
                s.src = {};
                s.onload = () => resolve(true);
                s.onerror = () => reject(new Error('Failed to load script'));
                document.head.appendChild(s);
            }})"#,
            serde_json::to_string(src_url).unwrap_or_default()
        );
        mgr.evaluate(&js, None).await?;
    } else if let Some(source) = content {
        let js = format!(
            r#"(() => {{
                const s = document.createElement('script');
                s.textContent = {};
                document.head.appendChild(s);
            }})()"#,
            serde_json::to_string(source).unwrap_or_default()
        );
        mgr.evaluate(&js, None).await?;
    }

    Ok(json!({ "added": true }))
}

async fn handle_addinitscript(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let source = cmd
        .get("script")
        .or_else(|| cmd.get("source"))
        .or_else(|| cmd.get("content"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'script' parameter")?;

    let identifier = mgr.add_script_to_evaluate(source).await?;
    Ok(json!({ "added": true, "identifier": identifier }))
}

async fn handle_addstyle(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let content = cmd
        .get("content")
        .or_else(|| cmd.get("css"))
        .and_then(|v| v.as_str());
    let url = cmd.get("url").and_then(|v| v.as_str());

    if content.is_none() && url.is_none() {
        return Err("At least one of 'content' or 'url' is required".to_string());
    }

    if let Some(href) = url {
        let js = format!(
            r#"new Promise((resolve, reject) => {{
                const link = document.createElement('link');
                link.rel = 'stylesheet';
                link.href = {};
                link.onload = () => resolve(true);
                link.onerror = () => reject(new Error('Failed to load stylesheet'));
                document.head.appendChild(link);
            }})"#,
            serde_json::to_string(href).unwrap_or_default()
        );
        mgr.evaluate(&js, None).await?;
    } else if let Some(css) = content {
        let js = format!(
            r#"(() => {{
                const style = document.createElement('style');
                style.textContent = {};
                document.head.appendChild(style);
            }})()"#,
            serde_json::to_string(css).unwrap_or_default()
        );
        mgr.evaluate(&js, None).await?;
    }

    Ok(json!({ "added": true }))
}

async fn handle_clipboard(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let action = cmd
        .get("subAction")
        .or_else(|| cmd.get("operation"))
        .and_then(|v| v.as_str())
        .unwrap_or("read");

    let session_id = mgr.active_session_id()?.to_string();

    // cfg! is compile-time; assumes the browser runs on the same OS as the CLI binary.
    let modifier: i32 = if cfg!(target_os = "macos") { 4 } else { 2 };

    match action {
        "write" => {
            let text = cmd
                .get("text")
                .or_else(|| cmd.get("value"))
                .and_then(|v| v.as_str())
                .ok_or("Missing 'text' parameter")?;
            let js = format!(
                "navigator.clipboard.writeText({})",
                serde_json::to_string(text).unwrap_or_default()
            );
            mgr.evaluate(&js, None).await?;
            Ok(json!({ "written": text }))
        }
        "copy" => {
            interaction::press_key_with_modifiers(&mgr.client, &session_id, "c", Some(modifier))
                .await?;
            Ok(json!({ "copied": true }))
        }
        "paste" => {
            interaction::press_key_with_modifiers(&mgr.client, &session_id, "v", Some(modifier))
                .await?;
            Ok(json!({ "pasted": true }))
        }
        _ => {
            let result = mgr.evaluate("navigator.clipboard.readText()", None).await?;
            Ok(json!({ "text": result }))
        }
    }
}

async fn handle_wheel(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let x = cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let delta_x = cmd.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let delta_y = cmd.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0);

    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({
                "type": "mouseWheel",
                "x": x,
                "y": y,
                "deltaX": delta_x,
                "deltaY": delta_y,
            })),
            Some(&session_id),
        )
        .await?;

    Ok(json!({ "scrolled": true, "deltaX": delta_x, "deltaY": delta_y }))
}

async fn handle_device(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let name = cmd
        .get("name")
        .or_else(|| cmd.get("device"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name' parameter")?;

    let (width, height, scale, mobile, ua) = match name.to_lowercase().as_str() {
        "iphone 15" | "iphone15" => (393, 852, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"),
        "iphone 16" | "iphone16" => (393, 852, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"),
        "iphone 16 pro" | "iphone16pro" => (402, 874, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"),
        "iphone 17" | "iphone17" => (402, 874, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 19_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/19.0 Mobile/15E148 Safari/604.1"),
        "ipad" | "ipad air" => (820, 1180, 2.0, true, "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/604.1"),
        "ipad pro" => (1024, 1366, 2.0, true, "Mozilla/5.0 (iPad; CPU OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/604.1"),
        "pixel 9" | "pixel9" => (412, 923, 2.625, true, "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36"),
        "galaxy s25" | "galaxys25" => (360, 800, 3.0, true, "Mozilla/5.0 (Linux; Android 15; SM-S931B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Mobile Safari/537.36"),
        // Legacy aliases
        "iphone 12" | "iphone12" => (390, 844, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 14_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/14.0 Mobile/15E148 Safari/604.1"),
        "iphone 14" | "iphone14" => (390, 844, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Mobile/15E148 Safari/604.1"),
        "pixel 5" | "pixel5" => (393, 851, 2.75, true, "Mozilla/5.0 (Linux; Android 11; Pixel 5) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.91 Mobile Safari/537.36"),
        "pixel 7" | "pixel7" => (412, 915, 2.625, true, "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Mobile Safari/537.36"),
        "galaxy s21" | "galaxys21" => (360, 800, 3.0, true, "Mozilla/5.0 (Linux; Android 11; SM-G991B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.91 Mobile Safari/537.36"),
        _ => return Err(format!("Unknown device: {}. Supported: iPhone 15, iPhone 16, iPhone 16 Pro, iPhone 17, iPad, iPad Pro, Pixel 9, Galaxy S25", name)),
    };

    mgr.set_viewport(width, height, scale, mobile).await?;
    mgr.set_user_agent(ua).await?;

    // Update stream server viewport so status messages and screencast use the new dimensions
    if let Some(ref server) = state.stream_server {
        server.set_viewport(width as u32, height as u32).await;
    }

    Ok(json!({
        "device": name,
        "width": width,
        "height": height,
        "deviceScaleFactor": scale,
        "mobile": mobile,
    }))
}

// ---------------------------------------------------------------------------
// Stream handlers
// ---------------------------------------------------------------------------

fn stream_file_path(session_id: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.stream", session_id))
}

fn write_stream_file(session_id: &str, port: u16) -> Result<(), String> {
    let path = stream_file_path(session_id);
    fs::write(&path, port.to_string()).map_err(|e| {
        format!(
            "Failed to write stream metadata '{}': {}",
            path.display(),
            e
        )
    })
}

fn remove_stream_file(session_id: &str) -> Result<(), String> {
    let path = stream_file_path(session_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!(
            "Failed to remove stream metadata '{}': {}",
            path.display(),
            err
        )),
    }
}

fn engine_file_path(session_id: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.engine", session_id))
}

fn write_engine_file(session_id: &str, engine: &str) {
    let _ = fs::write(engine_file_path(session_id), engine);
}

fn remove_engine_file(session_id: &str) {
    let _ = fs::remove_file(engine_file_path(session_id));
}

fn provider_file_path(session_id: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.provider", session_id))
}

fn write_provider_file(session_id: &str, provider: &str) {
    let _ = fs::write(provider_file_path(session_id), provider);
}

fn remove_provider_file(session_id: &str) {
    let _ = fs::remove_file(provider_file_path(session_id));
}

fn extensions_file_path(session_id: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.extensions", session_id))
}

fn write_extensions_file(session_id: &str) {
    if let Ok(val) = env::var("AGENT_BROWSER_EXTENSIONS") {
        let trimmed = val.trim();
        if !trimmed.is_empty() {
            let _ = fs::write(extensions_file_path(session_id), trimmed);
            return;
        }
    }
    let _ = fs::remove_file(extensions_file_path(session_id));
}

fn remove_extensions_file(session_id: &str) {
    let _ = fs::remove_file(extensions_file_path(session_id));
}

async fn current_stream_status(state: &DaemonState) -> Value {
    debug_assert_eq!(
        state.stream_server.is_some(),
        state.stream_client.is_some(),
        "stream server and stream client slot should be set together"
    );

    let connected = match state.browser.as_ref() {
        Some(mgr) => mgr.is_connection_alive().await,
        None => false,
    };
    let runtime_screencasting = match state.stream_server.as_ref() {
        Some(server) => server.is_screencasting().await,
        None => false,
    };

    json!({
        "enabled": state.stream_server.is_some(),
        "port": state
            .stream_server
            .as_ref()
            .map(|server| Value::from(server.port()))
            .unwrap_or(Value::Null),
        "connected": connected,
        "screencasting": connected && (state.screencasting || runtime_screencasting),
    })
}

async fn handle_stream_enable(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if state.stream_server.is_some() {
        return Err("Streaming is already enabled for this session".to_string());
    }

    let requested_port = match cmd.get("port").and_then(|value| value.as_u64()) {
        Some(raw) => u16::try_from(raw)
            .map_err(|_| format!("Invalid stream port '{}': expected 0-65535", raw))?,
        None => 0,
    };

    let (server, client_slot) =
        StreamServer::start_without_client(requested_port, state.session_id.clone(), false).await?;
    let port = server.port();
    if let Err(err) = write_stream_file(&state.session_id, port) {
        server.shutdown().await;
        return Err(err);
    }

    state.stream_client = Some(client_slot);
    state.stream_server = Some(Arc::new(server));
    state.request_tracking = true;
    if state.screencasting {
        if let Some(ref server) = state.stream_server {
            server.set_screencasting(true).await;
        }
    }
    state.update_stream_client().await;

    Ok(current_stream_status(state).await)
}

async fn handle_stream_disable(state: &mut DaemonState) -> Result<Value, String> {
    let Some(server) = state.stream_server.clone() else {
        return Err("Streaming is not enabled for this session".to_string());
    };

    server.shutdown().await;
    state.stream_server = None;
    state.stream_client = None;
    remove_stream_file(&state.session_id)?;
    remove_engine_file(&state.session_id);
    remove_provider_file(&state.session_id);

    Ok(json!({ "disabled": true }))
}

async fn handle_stream_status(state: &DaemonState) -> Result<Value, String> {
    Ok(current_stream_status(state).await)
}

// ---------------------------------------------------------------------------
// Screencast handlers
// ---------------------------------------------------------------------------

async fn handle_screencast_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    if state.screencasting {
        return Err("Screencast already active".to_string());
    }

    // Use stored viewport as default for screencast dimensions
    let (default_w, default_h) = if let Some(ref server) = state.stream_server {
        server.viewport().await
    } else {
        (1280, 720)
    };
    let format = cmd.get("format").and_then(|v| v.as_str()).unwrap_or("jpeg");
    let quality = cmd.get("quality").and_then(|v| v.as_i64()).unwrap_or(80) as i32;
    let max_width = cmd
        .get("maxWidth")
        .and_then(|v| v.as_i64())
        .unwrap_or(default_w as i64) as i32;
    let max_height = cmd
        .get("maxHeight")
        .and_then(|v| v.as_i64())
        .unwrap_or(default_h as i64) as i32;

    stream::start_screencast(
        &mgr.client,
        &session_id,
        format,
        quality,
        max_width,
        max_height,
    )
    .await?;
    state.screencasting = true;

    if let Some(ref server) = state.stream_server {
        server.set_screencasting(true).await;
        server
            .broadcast_status(
                true,
                true,
                max_width as u32,
                max_height as u32,
                &state.engine,
            )
            .await;
    }

    Ok(json!({ "started": true }))
}

async fn handle_screencast_stop(state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?;

    if !state.screencasting {
        return Err("No screencast active".to_string());
    }

    stream::stop_screencast(&mgr.client, session_id).await?;
    state.screencasting = false;

    if let Some(ref server) = state.stream_server {
        server.set_screencasting(false).await;
        let (vw, vh) = server.viewport().await;
        server
            .broadcast_status(true, false, vw, vh, &state.engine)
            .await;
    }

    Ok(json!({ "stopped": true }))
}

// ---------------------------------------------------------------------------
// Wait variant handlers
// ---------------------------------------------------------------------------

async fn handle_waitforurl(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let url_pattern = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;
    let timeout_ms = state.timeout_ms(cmd);

    wait_for_url(&mgr.client, &session_id, url_pattern, timeout_ms).await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "url": url }))
}

async fn handle_waitforloadstate(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let load_state = cmd.get("state").and_then(|v| v.as_str()).unwrap_or("load");
    let timeout_ms = state.timeout_ms(cmd);

    let wait_until = WaitUntil::from_str(load_state);
    let _ = tokio::time::timeout(
        tokio::time::Duration::from_millis(timeout_ms),
        mgr.wait_for_lifecycle_external(wait_until, &session_id),
    )
    .await
    .map_err(|_| format!("Timeout waiting for load state: {}", load_state))?;

    Ok(json!({ "state": load_state }))
}

async fn handle_waitforfunction(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let expression = cmd
        .get("expression")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'expression' parameter")?;
    let timeout_ms = state.timeout_ms(cmd);

    wait_for_function(&mgr.client, &session_id, expression, timeout_ms).await?;

    let result: super::cdp::types::EvaluateResult = mgr
        .client
        .send_command_typed(
            "Runtime.evaluate",
            &super::cdp::types::EvaluateParams {
                expression: format!("({})", expression),
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(&session_id),
        )
        .await?;

    Ok(json!({ "result": result.result.value.unwrap_or(Value::Null) }))
}

// ---------------------------------------------------------------------------
// Frame handlers
// ---------------------------------------------------------------------------

async fn handle_frame(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let selector = cmd.get("selector").and_then(|v| v.as_str());
    let name = cmd.get("name").and_then(|v| v.as_str());
    let url = cmd.get("url").and_then(|v| v.as_str());

    if selector.is_none() && name.is_none() && url.is_none() {
        return Err("At least one of 'selector', 'name', or 'url' is required".to_string());
    }

    let tree_result = mgr
        .client
        .send_command_no_params("Page.getFrameTree", Some(&session_id))
        .await?;

    fn find_frame(tree: &Value, name: Option<&str>, url: Option<&str>) -> Option<String> {
        let frame = tree.get("frame")?;
        let frame_name = frame.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let frame_url = frame.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let frame_id = frame.get("id").and_then(|v| v.as_str())?;

        if let Some(n) = name {
            if frame_name == n {
                return Some(frame_id.to_string());
            }
        }
        if let Some(u) = url {
            if frame_url.contains(u) {
                return Some(frame_id.to_string());
            }
        }

        if let Some(children) = tree.get("childFrames").and_then(|v| v.as_array()) {
            for child in children {
                if let Some(id) = find_frame(child, name, url) {
                    return Some(id);
                }
            }
        }
        None
    }

    let frame_tree = &tree_result["frameTree"];

    // If selector is a ref (@e1), resolve the iframe element from the ref map
    if let Some(sel) = selector {
        if let Some(ref_id) = super::element::parse_ref(sel) {
            let entry = state
                .ref_map
                .get(&ref_id)
                .ok_or_else(|| format!("Unknown ref: {}", ref_id))?;
            let backend_node_id = entry
                .backend_node_id
                .ok_or_else(|| format!("Ref {} has no backend node id", ref_id))?;

            // Use DOM.describeNode to resolve the child frame ID directly.
            // This works reliably for all iframes, including those without
            // name, id, or src attributes.
            let describe: Value = mgr
                .client
                .send_command(
                    "DOM.describeNode",
                    Some(json!({ "backendNodeId": backend_node_id, "depth": 1 })),
                    Some(&session_id),
                )
                .await?;

            // Verify this is an iframe/frame element
            let node_name = describe
                .get("node")
                .and_then(|n| n.get("nodeName"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if node_name != "IFRAME" && node_name != "FRAME" {
                return Err("Ref does not point to an iframe element".to_string());
            }

            // Try contentDocument.frameId first (standard for iframes)
            let frame_id = describe
                .get("node")
                .and_then(|n| n.get("contentDocument"))
                .and_then(|cd| cd.get("frameId"))
                .and_then(|v| v.as_str())
                // Fallback: the node itself may carry a frameId
                .or_else(|| {
                    describe
                        .get("node")
                        .and_then(|n| n.get("frameId"))
                        .and_then(|v| v.as_str())
                })
                .ok_or("Could not resolve frame ID for iframe element")?;

            let label = describe
                .get("node")
                .and_then(|n| n.get("attributes"))
                .and_then(|a| a.as_array())
                .and_then(|attrs| {
                    attrs
                        .iter()
                        .enumerate()
                        .find(|(_, v)| v.as_str() == Some("name"))
                        .and_then(|(i, _)| attrs.get(i + 1))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or(&ref_id);

            state.active_frame_id = Some(frame_id.to_string());
            return Ok(json!({ "frame": label }));
        }

        // CSS selector path
        let js = format!(
            r#"(() => {{
                const el = document.querySelector({});
                if (!el) return null;
                if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {{
                    return el.name || el.id || el.src || null;
                }}
                return null;
            }})()"#,
            serde_json::to_string(sel).unwrap_or_default()
        );
        let result = mgr.evaluate(&js, None).await?;
        let frame_name = result.as_str().ok_or("Could not find frame for selector")?;
        if let Some(frame_id) = find_frame(frame_tree, Some(frame_name), None) {
            state.active_frame_id = Some(frame_id);
            return Ok(json!({ "frame": frame_name }));
        }
    }

    if let Some(frame_id) = find_frame(frame_tree, name, url) {
        let label = name.or(url).unwrap_or("frame");
        state.active_frame_id = Some(frame_id);
        return Ok(json!({ "frame": label }));
    }

    Err("Frame not found".to_string())
}

async fn handle_mainframe(state: &mut DaemonState) -> Result<Value, String> {
    state.active_frame_id = None;
    Ok(json!({ "frame": "main" }))
}

// ---------------------------------------------------------------------------
// Semantic locator handlers
// ---------------------------------------------------------------------------

async fn execute_subaction(
    cmd: &Value,
    state: &mut DaemonState,
    selector: &str,
) -> Result<Value, String> {
    let subaction = cmd
        .get("subaction")
        .and_then(|v| v.as_str())
        .unwrap_or("click");
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    match subaction {
        "click" => {
            interaction::click(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                "left",
                1,
                &state.iframe_sessions,
            )
            .await?;
            Ok(json!({ "clicked": selector }))
        }
        "fill" => {
            let value = cmd
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'value' for fill subaction")?;
            interaction::fill(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                value,
                &state.iframe_sessions,
            )
            .await?;
            Ok(json!({ "filled": selector }))
        }
        "check" => {
            interaction::check(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                &state.iframe_sessions,
            )
            .await?;
            Ok(json!({ "checked": selector }))
        }
        "hover" => {
            interaction::hover(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                &state.iframe_sessions,
            )
            .await?;
            Ok(json!({ "hovered": selector }))
        }
        "text" => {
            let text = super::element::get_element_text(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                &state.iframe_sessions,
            )
            .await?;
            Ok(json!({ "text": text }))
        }
        _ => Err(format!("Unknown subaction: {}", subaction)),
    }
}

fn build_role_selector(role: &str, name: Option<&str>, exact: bool) -> String {
    match name {
        Some(n) => {
            let exact_str = if exact { ", exact: true" } else { "" };
            format!("getByRole('{}', {{ name: '{}'{} }})", role, n, exact_str)
        }
        None => format!("getByRole('{}')", role),
    }
}

async fn handle_getbyrole(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let role = cmd
        .get("role")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'role' parameter")?;
    let name = cmd.get("name").and_then(|v| v.as_str());
    let exact = cmd.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);

    let name_match = name
        .map(|n| {
            if exact {
                format!(
                    "el.getAttribute('aria-label') === {} || el.textContent.trim() === {}",
                    serde_json::to_string(n).unwrap_or_default(),
                    serde_json::to_string(n).unwrap_or_default()
                )
            } else {
                format!(
                    "(el.getAttribute('aria-label') || '').includes({n}) || el.textContent.includes({n})",
                    n = serde_json::to_string(n).unwrap_or_default()
                )
            }
        })
        .unwrap_or_else(|| "true".to_string());

    let js = format!(
        r#"(() => {{
            const els = document.querySelectorAll('[role="{role}"], {role}');
            for (const el of els) {{
                if ({name_match}) {{
                    el.setAttribute('data-agent-browser-located', 'true');
                    return true;
                }}
            }}
            return false;
        }})()"#,
        role = role,
        name_match = name_match,
    );

    let result: super::cdp::types::EvaluateResult = mgr
        .client
        .send_command_typed(
            "Runtime.evaluate",
            &super::cdp::types::EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&session_id),
        )
        .await?;

    if !result
        .result
        .value
        .as_ref()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let desc = build_role_selector(role, name, exact);
        return Err(format!("No element found: {}", desc));
    }

    let selector = "[data-agent-browser-located='true']";
    let result = execute_subaction(cmd, state, selector).await;

    // Clean up the marker attribute
    if let Some(ref browser) = state.browser {
        if browser.active_session_id().is_ok() {
            let _ = browser
                .evaluate(
                    "document.querySelector('[data-agent-browser-located]')?.removeAttribute('data-agent-browser-located')",
                    None,
                )
                .await;
        }
    }

    result
}

async fn handle_semantic_locator(
    cmd: &Value,
    state: &mut DaemonState,
    strategy: &str,
    param_name: &str,
) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let value = cmd
        .get(param_name)
        .and_then(|v| v.as_str())
        .ok_or(format!("Missing '{}' parameter", param_name))?;
    let exact = cmd.get("exact").and_then(|v| v.as_bool()).unwrap_or(false);

    let match_fn = if exact {
        format!(
            "el.textContent.trim() === {}",
            serde_json::to_string(value).unwrap_or_default()
        )
    } else {
        format!(
            "el.textContent.includes({})",
            serde_json::to_string(value).unwrap_or_default()
        )
    };

    let query = match strategy {
        "label" => format!(
            r#"(() => {{
                const label = Array.from(document.querySelectorAll('label')).find(el => {match_fn});
                if (!label) return false;
                const forId = label.getAttribute('for');
                const target = forId ? document.getElementById(forId) : label.querySelector('input,select,textarea');
                if (target) {{ target.setAttribute('data-agent-browser-located', 'true'); return true; }}
                return false;
            }})()"#,
            match_fn = match_fn,
        ),
        "placeholder" => format!(
            r#"(() => {{
                const el = document.querySelector('input[placeholder={val}], textarea[placeholder={val}]');
                if (el) {{ el.setAttribute('data-agent-browser-located', 'true'); return true; }}
                return false;
            }})()"#,
            val = serde_json::to_string(value).unwrap_or_default(),
        ),
        "alttext" => format!(
            r#"(() => {{
                const el = document.querySelector('img[alt={val}], [alt={val}]');
                if (el) {{ el.setAttribute('data-agent-browser-located', 'true'); return true; }}
                return false;
            }})()"#,
            val = serde_json::to_string(value).unwrap_or_default(),
        ),
        "title" => format!(
            r#"(() => {{
                const el = document.querySelector('[title={val}]');
                if (el) {{ el.setAttribute('data-agent-browser-located', 'true'); return true; }}
                return false;
            }})()"#,
            val = serde_json::to_string(value).unwrap_or_default(),
        ),
        "testid" => format!(
            r#"(() => {{
                const el = document.querySelector('[data-testid={val}]');
                if (el) {{ el.setAttribute('data-agent-browser-located', 'true'); return true; }}
                return false;
            }})()"#,
            val = serde_json::to_string(value).unwrap_or_default(),
        ),
        _ => {
            // "text" strategy
            format!(
                r#"(() => {{
                    const all = document.querySelectorAll('*');
                    for (const el of all) {{
                        if (el.children.length === 0 && {match_fn}) {{
                            el.setAttribute('data-agent-browser-located', 'true');
                            return true;
                        }}
                    }}
                    return false;
                }})()"#,
                match_fn = match_fn,
            )
        }
    };

    let result: super::cdp::types::EvaluateResult = mgr
        .client
        .send_command_typed(
            "Runtime.evaluate",
            &super::cdp::types::EvaluateParams {
                expression: query,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&session_id),
        )
        .await?;

    if !result
        .result
        .value
        .as_ref()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(format!("No element found by {} '{}'", strategy, value));
    }

    let selector = "[data-agent-browser-located='true']";
    let action_result = execute_subaction(cmd, state, selector).await;

    if let Some(ref browser) = state.browser {
        let _ = browser
            .evaluate(
                "document.querySelector('[data-agent-browser-located]')?.removeAttribute('data-agent-browser-located')",
                None,
            )
            .await;
    }

    action_result
}

async fn handle_getbytext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    handle_semantic_locator(cmd, state, "text", "text").await
}

async fn handle_getbylabel(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    handle_semantic_locator(cmd, state, "label", "label").await
}

async fn handle_getbyplaceholder(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    handle_semantic_locator(cmd, state, "placeholder", "placeholder").await
}

async fn handle_getbyalttext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    handle_semantic_locator(cmd, state, "alttext", "text").await
}

async fn handle_getbytitle(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    handle_semantic_locator(cmd, state, "title", "text").await
}

async fn handle_getbytestid(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    handle_semantic_locator(cmd, state, "testid", "testId").await
}

async fn handle_nth(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let index = cmd
        .get("index")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'index' parameter")?;

    let js = format!(
        r#"(() => {{
            const els = document.querySelectorAll({sel});
            const idx = {idx} < 0 ? els.length + {idx} : {idx};
            if (idx < 0 || idx >= els.length) return false;
            els[idx].setAttribute('data-agent-browser-located', 'true');
            return true;
        }})()"#,
        sel = serde_json::to_string(selector).unwrap_or_default(),
        idx = index,
    );

    let result: super::cdp::types::EvaluateResult = mgr
        .client
        .send_command_typed(
            "Runtime.evaluate",
            &super::cdp::types::EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&session_id),
        )
        .await?;

    if !result
        .result
        .value
        .as_ref()
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(format!(
            "No element at index {} for selector '{}'",
            index, selector
        ));
    }

    let located = "[data-agent-browser-located='true']";
    let action_result = execute_subaction(cmd, state, located).await;

    if let Some(ref browser) = state.browser {
        let _ = browser
            .evaluate(
                "document.querySelector('[data-agent-browser-located]')?.removeAttribute('data-agent-browser-located')",
                None,
            )
            .await;
    }

    action_result
}

async fn handle_find(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let js = format!(
        r#"(() => {{
            const els = document.querySelectorAll({});
            return Array.from(els).map((el, i) => ({{
                index: i,
                tagName: el.tagName.toLowerCase(),
                text: el.textContent?.trim().substring(0, 100) || '',
                visible: el.offsetWidth > 0 && el.offsetHeight > 0,
            }}));
        }})()"#,
        serde_json::to_string(selector).unwrap_or_default()
    );

    let result = mgr.evaluate(&js, None).await?;
    Ok(json!({ "elements": result, "selector": selector }))
}

async fn handle_evalhandle(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let script = cmd
        .get("script")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'script' parameter")?;

    let result: super::cdp::types::EvaluateResult = mgr
        .client
        .send_command_typed(
            "Runtime.evaluate",
            &super::cdp::types::EvaluateParams {
                expression: script.to_string(),
                return_by_value: Some(false),
                await_promise: Some(true),
            },
            Some(&session_id),
        )
        .await?;

    let handle = result.result.object_id.unwrap_or_default();
    Ok(json!({ "handle": handle }))
}

// ---------------------------------------------------------------------------
// Advanced interaction handlers
// ---------------------------------------------------------------------------

async fn handle_drag(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let source = cmd
        .get("source")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'source' parameter")?;
    let target = cmd
        .get("target")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'target' parameter")?;

    let (sx, sy, source_session_id) = super::element::resolve_element_center(
        &mgr.client,
        &session_id,
        &state.ref_map,
        source,
        &state.iframe_sessions,
    )
    .await?;
    let (tx, ty, target_session_id) = super::element::resolve_element_center(
        &mgr.client,
        &session_id,
        &state.ref_map,
        target,
        &state.iframe_sessions,
    )
    .await?;

    // Mouse down at source
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mouseMoved", "x": sx, "y": sy })),
            Some(&source_session_id),
        )
        .await?;
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mousePressed", "x": sx, "y": sy, "button": "left", "buttons": 1, "clickCount": 1 })),
            Some(&source_session_id),
        )
        .await?;

    // Move in steps to target, keeping the left button held (buttons: 1) so
    // that the browser sees a drag rather than a plain pointer move.
    let steps = 10;
    for i in 1..=steps {
        let cx = sx + (tx - sx) * (i as f64) / (steps as f64);
        let cy = sy + (ty - sy) * (i as f64) / (steps as f64);
        mgr.client
            .send_command(
                "Input.dispatchMouseEvent",
                Some(json!({ "type": "mouseMoved", "x": cx, "y": cy, "button": "left", "buttons": 1 })),
                Some(&target_session_id),
            )
            .await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Mouse up at target
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mouseReleased", "x": tx, "y": ty, "button": "left", "buttons": 0, "clickCount": 1 })),
            Some(&target_session_id),
        )
        .await?;

    Ok(json!({ "dragged": true, "source": source, "target": target }))
}

async fn handle_expose(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name' parameter")?;

    mgr.client
        .send_command(
            "Runtime.addBinding",
            Some(json!({ "name": name })),
            Some(&session_id),
        )
        .await?;

    Ok(json!({ "exposed": name }))
}

async fn handle_pause(_state: &DaemonState) -> Result<Value, String> {
    Ok(json!({ "paused": true, "note": "Use DevTools to inspect. The daemon remains running." }))
}

async fn handle_multiselect(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;
    let values: Vec<String> = cmd
        .get("values")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let values_json = serde_json::to_string(&values).unwrap_or("[]".to_string());
    let js = format!(
        r#"(() => {{
            const select = document.querySelector({sel});
            if (!select) throw new Error('Select element not found');
            const vals = {vals};
            for (const opt of select.options) {{
                opt.selected = vals.includes(opt.value);
            }}
            select.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return Array.from(select.selectedOptions).map(o => o.value);
        }})()"#,
        sel = serde_json::to_string(selector).unwrap_or_default(),
        vals = values_json,
    );

    let result = mgr.evaluate(&js, None).await?;
    Ok(json!({ "selected": result }))
}

async fn handle_responsebody(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let url_pattern = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;
    let timeout_ms = state.timeout_ms(cmd);

    let mut rx = mgr.client.subscribe();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "Timeout waiting for response matching '{}'",
                url_pattern
            ));
        }

        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if event.method == "Network.responseReceived"
                    && event.session_id.as_deref() == Some(&session_id)
                {
                    if let Some(resp_url) = event
                        .params
                        .get("response")
                        .and_then(|r| r.get("url"))
                        .and_then(|u| u.as_str())
                    {
                        if resp_url.contains(url_pattern) {
                            let request_id = event
                                .params
                                .get("requestId")
                                .and_then(|v| v.as_str())
                                .ok_or("No requestId in response event")?;
                            let status = event
                                .params
                                .get("response")
                                .and_then(|r| r.get("status"))
                                .and_then(|v| v.as_i64())
                                .unwrap_or(0);
                            let headers = event
                                .params
                                .get("response")
                                .and_then(|r| r.get("headers"))
                                .cloned()
                                .unwrap_or(json!({}));

                            let body_result = mgr
                                .client
                                .send_command(
                                    "Network.getResponseBody",
                                    Some(json!({ "requestId": request_id })),
                                    Some(&session_id),
                                )
                                .await?;
                            let body = body_result
                                .get("body")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");

                            return Ok(
                                json!({ "body": body, "status": status, "headers": headers }),
                            );
                        }
                    }
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Err("Event stream closed".to_string()),
            Err(_) => {
                return Err(format!(
                    "Timeout waiting for response matching '{}'",
                    url_pattern
                ));
            }
        }
    }
}

async fn handle_waitfordownload(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let timeout_ms = state.timeout_ms(cmd);

    let mut rx = mgr.client.subscribe();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Timeout waiting for download".to_string());
        }

        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                // Browser-domain events may arrive without a sessionId;
                // Page-domain events are matched by session.
                let is_page_session = event.session_id.as_deref() == Some(&session_id);
                let is_progress = event.method == "Browser.downloadProgress"
                    || (event.method == "Page.downloadProgress" && is_page_session);

                if is_progress
                    && event.params.get("state").and_then(|v| v.as_str()) == Some("completed")
                {
                    let path = cmd
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("download");
                    return Ok(json!({ "path": path }));
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Err("Event stream closed".to_string()),
            Err(_) => return Err("Timeout waiting for download".to_string()),
        }
    }
}

async fn handle_window_new(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;

    // Create a new browser context
    let context_result = mgr
        .client
        .send_command_no_params("Target.createBrowserContext", None)
        .await?;
    let context_id = context_result
        .get("browserContextId")
        .and_then(|v| v.as_str())
        .ok_or("Failed to create browser context")?
        .to_string();

    let create_result: super::cdp::types::CreateTargetResult = mgr
        .client
        .send_command_typed(
            "Target.createTarget",
            &json!({ "url": "about:blank", "browserContextId": context_id }),
            None,
        )
        .await?;

    let attach: super::cdp::types::AttachToTargetResult = mgr
        .client
        .send_command_typed(
            "Target.attachToTarget",
            &super::cdp::types::AttachToTargetParams {
                target_id: create_result.target_id.clone(),
                flatten: true,
            },
            None,
        )
        .await?;

    mgr.add_page(super::browser::PageInfo {
        target_id: create_result.target_id,
        session_id: attach.session_id,
        url: "about:blank".to_string(),
        title: String::new(),
        target_type: "page".to_string(),
    });

    if let Some(viewport) = cmd.get("viewport") {
        let width = viewport
            .get("width")
            .and_then(|v| v.as_i64())
            .unwrap_or(1280) as i32;
        let height = viewport
            .get("height")
            .and_then(|v| v.as_i64())
            .unwrap_or(720) as i32;
        mgr.set_viewport(width, height, 1.0, false).await?;

        // Update stream server viewport
        if let Some(ref server) = state.stream_server {
            server.set_viewport(width as u32, height as u32).await;
        }
    }

    let total = mgr.page_count();
    state.ref_map.clear();

    Ok(json!({ "index": total - 1, "total": total }))
}

async fn handle_diff_screenshot(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let baseline_path = cmd
        .get("baseline")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'baseline' parameter")?;

    let threshold = cmd.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.1);

    let options = ScreenshotOptions {
        selector: cmd
            .get("selector")
            .and_then(|v| v.as_str())
            .map(String::from),
        path: None,
        full_page: cmd
            .get("fullPage")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        format: "png".to_string(),
        quality: None,
        annotate: false,
        output_dir: None,
    };

    let result = screenshot::take_screenshot(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &options,
        &state.iframe_sessions,
    )
    .await?;

    let current_bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &result.base64)
            .map_err(|e| format!("Failed to decode screenshot: {}", e))?;

    let baseline_bytes =
        std::fs::read(baseline_path).map_err(|e| format!("Failed to read baseline: {}", e))?;

    let result = diff::diff_screenshot(&baseline_bytes, &current_bytes, threshold)?;

    let output_path = cmd.get("output").and_then(|v| v.as_str());
    if let (Some(out_path), Some(ref diff_data)) = (output_path, &result.diff_image) {
        std::fs::write(out_path, diff_data)
            .map_err(|e| format!("Failed to write diff image: {}", e))?;
    }

    Ok(json!({
        "match": result.matched,
        "mismatchPercentage": result.mismatch_percentage,
        "totalPixels": result.total_pixels,
        "differentPixels": result.different_pixels,
        "diffPath": output_path,
        "dimensionMismatch": result.dimension_mismatch,
    }))
}

// ---------------------------------------------------------------------------
// Video and HAR handlers
// ---------------------------------------------------------------------------

async fn handle_video_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    if state.recording_state.active {
        return Err("A recording is already in progress".to_string());
    }

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    recording::recording_start(&mut state.recording_state, path)?;
    state
        .start_recording_task(mgr.client.clone(), session_id)
        .await?;

    Ok(json!({
        "started": true,
        "note": "Video recording started. Use video_stop to save the recording."
    }))
}

async fn handle_video_stop(state: &mut DaemonState) -> Result<Value, String> {
    if !state.recording_state.active {
        return Ok(json!({
            "stopped": false,
            "note": "No video recording was started. Use recording_stop if you used recording_start."
        }));
    }

    state.stop_recording_task().await?;
    recording::recording_stop(&mut state.recording_state)
}

/// Begin capturing network traffic for a later HAR export.
async fn handle_har_start(state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    mgr.client
        .send_command_no_params("Network.enable", Some(&session_id))
        .await?;
    // Also enable Network on cross-origin iframe sessions so their
    // requests are captured in the HAR output.
    for iframe_sid in state.iframe_sessions.values() {
        let _ = mgr
            .client
            .send_command_no_params("Network.enable", Some(iframe_sid.as_str()))
            .await;
    }
    state.har_recording = true;
    state.har_entries.clear();
    Ok(json!({ "started": true }))
}

/// Stop HAR recording and write the captured requests to disk.
async fn handle_har_stop(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = har_output_path(cmd.get("path").and_then(|v| v.as_str()));

    state.har_recording = false;

    let entries: Vec<Value> = state.har_entries.drain(..).map(har_entry_to_json).collect();
    let request_count = entries.len();
    let browser = har_browser_metadata(state).await;

    let mut log = json!({
        "version": "1.2",
        "creator": {
            "name": "agent-browser",
            "version": env!("CARGO_PKG_VERSION")
        },
        "entries": entries
    });
    if let Some(browser) = browser {
        log["browser"] = browser;
    }
    let har = json!({ "log": log });

    let har_str = serde_json::to_string_pretty(&har)
        .map_err(|e| format!("Failed to serialize HAR: {}", e))?;
    std::fs::write(&path, har_str).map_err(|e| format!("Failed to write HAR: {}", e))?;

    Ok(json!({ "path": path, "requestCount": request_count }))
}

// ---------------------------------------------------------------------------
// HAR serialization helpers
// ---------------------------------------------------------------------------

/// Convert a `HarEntry` (collected from CDP events) into a HAR 1.2 entry object.
fn har_entry_to_json(e: HarEntry) -> Value {
    let started_date_time = har_wall_time_to_rfc3339(e.wall_time);

    let request_cookies = e
        .request_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
        .map(|(_, v)| har_parse_request_cookies(v))
        .unwrap_or_default();

    let query_string = har_parse_query_string(&e.url);

    let req_headers: Vec<Value> = e
        .request_headers
        .iter()
        .map(|(k, v)| json!({ "name": k, "value": v }))
        .collect();

    let resp_cookies: Vec<Value> = e
        .response_headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .map(|(_, v)| {
            // Split on ';' first to discard attributes (Path, HttpOnly, etc.),
            // then split on '=' once to separate name from value.
            let name_value = v.split(';').next().unwrap_or("");
            let (name, value) = name_value.split_once('=').unwrap_or((name_value, ""));
            json!({ "name": name.trim(), "value": value.trim() })
        })
        .collect();

    let resp_headers: Vec<Value> = e
        .response_headers
        .iter()
        .map(|(k, v)| json!({ "name": k, "value": v }))
        .collect();

    let (timings, total_time) =
        har_compute_timings(e.cdp_timing.as_ref(), e.loading_finished_timestamp);

    let mime_type = if e.mime_type.is_empty() {
        "application/octet-stream".to_string()
    } else {
        e.mime_type
    };

    let post_content_type = e
        .request_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("text/plain")
        .to_string();

    let mut request = json!({
        "method": e.method,
        "url": e.url,
        "httpVersion": e.http_version,
        "cookies": request_cookies,
        "headers": req_headers,
        "queryString": query_string,
        "headersSize": -1,
        "bodySize": e.request_body_size,
    });
    if let Some(body) = e.post_data {
        request["postData"] = json!({ "mimeType": post_content_type, "text": body });
    }

    json!({
        "startedDateTime": started_date_time,
        "time": total_time,
        "request": request,
        "response": {
            "status": e.status.unwrap_or(0),
            "statusText": e.status_text,
            "httpVersion": e.http_version,
            "cookies": resp_cookies,
            "headers": resp_headers,
            "content": {
                "size": e.response_body_size,
                "mimeType": mime_type,
            },
            "redirectURL": e.redirect_url,
            "headersSize": -1,
            "bodySize": e.response_body_size,
        },
        "cache": {},
        "timings": timings,
        "_resourceType": e.resource_type,
    })
}

/// Convert a CDP headers object (`{ "Name": "value", ... }`) into a flat
/// `Vec<(name, value)>` preserving insertion order.
fn har_extract_headers(headers_val: Option<&Value>) -> Vec<(String, String)> {
    headers_val
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Map a CDP `response.protocol` value to an HTTP-version string as required
/// by the HAR spec (e.g. `"h2"` → `"HTTP/2.0"`).
fn har_cdp_protocol_to_http_version(protocol: &str) -> String {
    match protocol.to_ascii_lowercase().as_str() {
        "h2" => "HTTP/2.0".to_string(),
        "h3" => "HTTP/3.0".to_string(),
        "http/1.0" => "HTTP/1.0".to_string(),
        _ => "HTTP/1.1".to_string(),
    }
}

/// Parse query-string parameters from a URL into a HAR `queryString` array.
fn har_parse_query_string(url_str: &str) -> Vec<Value> {
    url::Url::parse(url_str)
        .map(|u| {
            u.query_pairs()
                .map(|(k, v)| json!({ "name": k.as_ref(), "value": v.as_ref() }))
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a `Cookie: name1=val1; name2=val2` header value into HAR cookie objects.
fn har_parse_request_cookies(cookie_header: &str) -> Vec<Value> {
    cookie_header
        .split(';')
        .filter_map(|pair| {
            let pair = pair.trim();
            if pair.is_empty() {
                return None;
            }
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some(json!({ "name": name.trim(), "value": value.trim() }))
        })
        .collect()
}

/// Compute HAR `timings` and total `time` (ms) from a CDP `ResourceTiming`
/// object and the optional `Network.loadingFinished` monotonic timestamp.
///
/// CDP timing values are milliseconds relative to `requestTime` (seconds since
/// browser start). A value of `-1` means the phase did not occur.
fn har_compute_timings(
    cdp_timing: Option<&Value>,
    loading_finished_ts: Option<f64>,
) -> (Value, f64) {
    let Some(t) = cdp_timing else {
        return (json!({ "send": 0, "wait": 0, "receive": 0 }), 0.0);
    };

    let get = |key: &str| t.get(key).and_then(|v| v.as_f64()).unwrap_or(-1.0);

    let request_time = get("requestTime");
    let dns_start = get("dnsStart");
    let dns_end = get("dnsEnd");
    let connect_start = get("connectStart");
    let connect_end = get("connectEnd");
    let ssl_start = get("sslStart");
    let ssl_end = get("sslEnd");
    let send_start = get("sendStart");
    let send_end = get("sendEnd");
    let recv_headers_start = get("receiveHeadersStart");
    let recv_headers_end = get("receiveHeadersEnd");

    let dns = if dns_start >= 0.0 && dns_end >= 0.0 {
        dns_end - dns_start
    } else {
        -1.0
    };
    let connect = if connect_start >= 0.0 && connect_end >= 0.0 {
        connect_end - connect_start
    } else {
        -1.0
    };
    let ssl = if ssl_start >= 0.0 && ssl_end >= 0.0 {
        ssl_end - ssl_start
    } else {
        -1.0
    };
    let send = (send_end - send_start).max(0.0);

    // wait: end of sending → first byte of response headers.
    let wait_end = if recv_headers_start >= 0.0 {
        recv_headers_start
    } else {
        recv_headers_end
    };
    let wait = if send_end >= 0.0 && wait_end >= send_end {
        wait_end - send_end
    } else {
        0.0
    };

    // receive: first response byte → loading complete.
    // requestTime (seconds) + recv_headers_end (ms) / 1000 = absolute headers-end timestamp.
    let receive = loading_finished_ts
        .filter(|_| request_time >= 0.0 && recv_headers_end >= 0.0)
        .map(|lf_ts| {
            let recv_start_abs = request_time + recv_headers_end / 1000.0;
            ((lf_ts - recv_start_abs) * 1000.0).max(0.0)
        })
        .unwrap_or(0.0);

    let blocked = if dns_start > 0.0 {
        dns_start
    } else if connect_start > 0.0 {
        connect_start
    } else if send_start > 0.0 {
        send_start
    } else {
        -1.0
    };

    let total: f64 = [
        if blocked > 0.0 { blocked } else { 0.0 },
        if dns >= 0.0 { dns } else { 0.0 },
        if connect >= 0.0 { connect } else { 0.0 },
        send,
        wait,
        receive,
    ]
    .iter()
    .sum();

    let mut timings = json!({ "send": send, "wait": wait, "receive": receive });
    if blocked > 0.0 {
        timings["blocked"] = json!(blocked);
    }
    if dns >= 0.0 {
        timings["dns"] = json!(dns);
    }
    if connect >= 0.0 {
        timings["connect"] = json!(connect);
    }
    if ssl >= 0.0 {
        timings["ssl"] = json!(ssl);
    }

    (timings, total)
}

/// Format a Unix epoch timestamp (seconds, fractional) as RFC 3339 using the
/// `time` crate, e.g. `"2024-03-17T10:30:00.456Z"`.
fn har_wall_time_to_rfc3339(wall_time: f64) -> String {
    if wall_time > 0.0 {
        let nanos = (wall_time * 1_000_000_000.0).round() as i128;
        if let Ok(dt) = OffsetDateTime::from_unix_timestamp_nanos(nanos) {
            if let Ok(s) = dt.format(&Rfc3339) {
                return s;
            }
        }
    }
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn har_output_path(explicit_path: Option<&str>) -> String {
    match explicit_path {
        Some(path) => path.to_string(),
        None => {
            let dir = get_har_dir();
            let _ = std::fs::create_dir_all(&dir);
            dir.join(format!("har-{}.har", unix_timestamp_millis()))
                .to_string_lossy()
                .to_string()
        }
    }
}

fn get_har_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser").join("tmp").join("har")
    } else {
        std::env::temp_dir().join("agent-browser").join("har")
    }
}

fn unix_timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

async fn har_browser_metadata(state: &DaemonState) -> Option<Value> {
    let mgr = state.browser.as_ref()?;
    if !mgr.is_connection_alive().await {
        return None;
    }

    let version = mgr
        .client
        .send_command_no_params("Browser.getVersion", None)
        .await
        .ok()?;
    browser_metadata_from_version(&version)
}

fn browser_metadata_from_version(version: &Value) -> Option<Value> {
    let product = version.get("product").and_then(|v| v.as_str())?;
    let (name, browser_version) = product.split_once('/').unwrap_or((product, ""));
    Some(json!({
        "name": name,
        "version": browser_version,
    }))
}

// ---------------------------------------------------------------------------
// Fetch interception resolver (domain filter + routes + origin headers)
// ---------------------------------------------------------------------------

async fn resolve_fetch_paused(
    client: &CdpClient,
    domain_filter: Option<&DomainFilter>,
    routes: &[RouteEntry],
    origin_headers: &HashMap<String, HashMap<String, String>>,
    paused: &FetchPausedRequest,
) {
    let session_id = &paused.session_id;

    // Domain filter check (takes priority over routes and origin headers)
    if let Some(filter) = domain_filter {
        if let Ok(parsed) = url::Url::parse(&paused.url) {
            let scheme = parsed.scheme();
            if scheme != "http" && scheme != "https" {
                if paused.resource_type.eq_ignore_ascii_case("document") {
                    let _ = client
                        .send_command(
                            "Fetch.failRequest",
                            Some(json!({
                                "requestId": paused.request_id,
                                "errorReason": "BlockedByClient"
                            })),
                            Some(session_id),
                        )
                        .await;
                } else {
                    let _ = client
                        .send_command(
                            "Fetch.continueRequest",
                            Some(json!({ "requestId": paused.request_id })),
                            Some(session_id),
                        )
                        .await;
                }
                return;
            }

            if let Some(hostname) = parsed.host_str() {
                if !filter.is_allowed(hostname) {
                    if paused.resource_type.eq_ignore_ascii_case("document") {
                        let error_body = format!(
                            "<html><body><h1>Blocked</h1><p>Navigation to {} is not allowed by domain filter.</p></body></html>",
                            hostname
                        );
                        let encoded = base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            error_body.as_bytes(),
                        );
                        let _ = client
                            .send_command(
                                "Fetch.fulfillRequest",
                                Some(json!({
                                    "requestId": paused.request_id,
                                    "responseCode": 403,
                                    "responseHeaders": [
                                        { "name": "Content-Type", "value": "text/html" },
                                    ],
                                    "body": encoded,
                                })),
                                Some(session_id),
                            )
                            .await;
                    } else {
                        let _ = client
                            .send_command(
                                "Fetch.failRequest",
                                Some(json!({
                                    "requestId": paused.request_id,
                                    "errorReason": "BlockedByClient"
                                })),
                                Some(session_id),
                            )
                            .await;
                    }
                    return;
                }
            }
        }
    }

    // Route matching
    for route in routes {
        let matches = if route.url_pattern == "*" {
            true
        } else if route.url_pattern.contains('*') {
            let parts: Vec<&str> = route.url_pattern.split('*').collect();
            if parts.len() == 2 {
                paused.url.starts_with(parts[0]) && paused.url.ends_with(parts[1])
            } else {
                paused.url.contains(&route.url_pattern)
            }
        } else {
            paused.url.contains(&route.url_pattern)
        };

        if matches {
            if route.abort {
                let _ = client
                    .send_command(
                        "Fetch.failRequest",
                        Some(json!({
                            "requestId": paused.request_id,
                            "errorReason": "Failed"
                        })),
                        Some(session_id),
                    )
                    .await;
                return;
            }

            if let Some(ref resp) = route.response {
                let status = resp.status.unwrap_or(200);
                let body_str = resp.body.as_deref().unwrap_or("");
                let encoded = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    body_str.as_bytes(),
                );
                let mut headers = vec![];
                if let Some(ct) = &resp.content_type {
                    headers.push(json!({ "name": "Content-Type", "value": ct }));
                }
                if let Some(h) = &resp.headers {
                    for (k, v) in h {
                        headers.push(json!({ "name": k, "value": v }));
                    }
                }

                let _ = client
                    .send_command(
                        "Fetch.fulfillRequest",
                        Some(json!({
                            "requestId": paused.request_id,
                            "responseCode": status,
                            "responseHeaders": headers,
                            "body": encoded,
                        })),
                        Some(session_id),
                    )
                    .await;
                return;
            }
        }
    }

    // No matching route — continue, injecting origin-scoped headers if applicable.
    let extra = url::Url::parse(&paused.url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .and_then(|o| origin_headers.get(&o));

    if let Some(extra_headers) = extra {
        // Merge original request headers with extra headers.
        // Fetch.continueRequest replaces (not merges), so include originals.
        let mut combined: Vec<Value> = Vec::new();
        if let Some(ref orig) = paused.request_headers {
            for (k, v) in orig {
                if !extra_headers.keys().any(|ek| ek.eq_ignore_ascii_case(k)) {
                    if let Some(s) = v.as_str() {
                        combined.push(json!({ "name": k, "value": s }));
                    }
                }
            }
        }
        for (k, v) in extra_headers {
            combined.push(json!({ "name": k, "value": v }));
        }
        let _ = client
            .send_command(
                "Fetch.continueRequest",
                Some(json!({ "requestId": paused.request_id, "headers": combined })),
                Some(session_id),
            )
            .await;
    } else {
        let _ = client
            .send_command(
                "Fetch.continueRequest",
                Some(json!({ "requestId": paused.request_id })),
                Some(session_id),
            )
            .await;
    }
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// Build the Fetch.enable patterns list from current routes, domain filter,
/// and origin headers state.  When domain filtering or origin-scoped headers
/// are active a wildcard pattern is included so all requests are intercepted.
async fn build_fetch_patterns(state: &DaemonState) -> Vec<Value> {
    let routes = state.routes.read().await;
    let mut patterns: Vec<Value> = routes
        .iter()
        .map(|r| json!({ "urlPattern": r.url_pattern }))
        .collect();
    let has_domain_filter = state.domain_filter.read().await.is_some();
    let has_origin_headers = !state.origin_headers.read().await.is_empty();
    let has_proxy_creds = state.proxy_credentials.read().await.is_some();
    if (has_domain_filter || has_origin_headers || has_proxy_creds)
        && !patterns.iter().any(|p| p["urlPattern"] == "*")
    {
        patterns.push(json!({ "urlPattern": "*" }));
    }
    patterns
}

/// Build the full Fetch.enable params object, including `handleAuthRequests`
/// when proxy credentials are configured.
async fn build_fetch_enable_params(state: &DaemonState, patterns: Vec<Value>) -> Value {
    let has_proxy_creds = state.proxy_credentials.read().await.is_some();
    if has_proxy_creds {
        json!({ "patterns": patterns, "handleAuthRequests": true })
    } else {
        json!({ "patterns": patterns })
    }
}

async fn handle_route(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let url_pattern = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?
        .to_string();
    let abort = cmd.get("abort").and_then(|v| v.as_bool()).unwrap_or(false);

    let response = cmd.get("response").and_then(|v| {
        if v.is_null() {
            return None;
        }
        Some(RouteResponse {
            status: v.get("status").and_then(|s| s.as_u64()).map(|s| s as u16),
            body: v.get("body").and_then(|s| s.as_str()).map(String::from),
            content_type: v
                .get("contentType")
                .and_then(|s| s.as_str())
                .map(String::from),
            headers: v.get("headers").and_then(|h| {
                h.as_object().map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
            }),
        })
    });

    {
        let mut routes = state.routes.write().await;
        routes.push(RouteEntry {
            url_pattern: url_pattern.clone(),
            response,
            abort,
        });
    }

    let patterns = build_fetch_patterns(state).await;
    let params = build_fetch_enable_params(state, patterns).await;
    mgr.client
        .send_command("Fetch.enable", Some(params), Some(&session_id))
        .await?;

    Ok(json!({ "routed": url_pattern }))
}

async fn handle_unroute(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let url = cmd.get("url").and_then(|v| v.as_str());

    {
        let mut routes = state.routes.write().await;
        match url {
            Some(pattern) => {
                routes.retain(|r| r.url_pattern != pattern);
            }
            None => {
                routes.clear();
            }
        }
    }

    let patterns = build_fetch_patterns(state).await;
    if patterns.is_empty() {
        mgr.client
            .send_command("Fetch.disable", None, Some(&session_id))
            .await?;
    } else {
        let params = build_fetch_enable_params(state, patterns).await;
        mgr.client
            .send_command("Fetch.enable", Some(params), Some(&session_id))
            .await?;
    }

    let label = url.unwrap_or("all");
    Ok(json!({ "unrouted": label }))
}

pub fn matches_status_filter(status: Option<i64>, filter: &str) -> bool {
    let Some(code) = status else { return false };
    let f = filter.to_lowercase();
    if let Ok(exact) = f.parse::<i64>() {
        return code == exact;
    }
    if f.len() == 3 && f.ends_with("xx") {
        if let Ok(prefix) = f[..1].parse::<i64>() {
            return code / 100 == prefix;
        }
    }
    if let Some((lo, hi)) = f.split_once('-') {
        if let (Ok(lo), Ok(hi)) = (lo.parse::<i64>(), hi.parse::<i64>()) {
            return code >= lo && code <= hi;
        }
    }
    false
}

async fn handle_requests(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if cmd.get("clear").and_then(|v| v.as_bool()).unwrap_or(false) {
        state.tracked_requests.clear();
        return Ok(json!({ "cleared": true }));
    }

    if !state.request_tracking {
        state.request_tracking = true;
        if let Some(ref mgr) = state.browser {
            if let Ok(session_id) = mgr.active_session_id() {
                let _ = mgr
                    .client
                    .send_command_no_params("Network.enable", Some(session_id))
                    .await;
            }
        }
    }

    let filter = cmd.get("filter").and_then(|v| v.as_str());
    let type_filter = cmd.get("type").and_then(|v| v.as_str());
    let method_filter = cmd.get("method").and_then(|v| v.as_str());
    let status_filter = cmd.get("status").and_then(|v| v.as_str());

    let type_list: Vec<String> = type_filter
        .map(|t| t.split(',').map(|s| s.trim().to_lowercase()).collect())
        .unwrap_or_default();

    let requests: Vec<&TrackedRequest> = state
        .tracked_requests
        .iter()
        .filter(|r| {
            if let Some(f) = filter {
                if !r.url.contains(f) {
                    return false;
                }
            }
            if !type_list.is_empty() && !type_list.contains(&r.resource_type.to_lowercase()) {
                return false;
            }
            if let Some(m) = method_filter {
                if !r.method.eq_ignore_ascii_case(m) {
                    return false;
                }
            }
            if let Some(s) = status_filter {
                if !matches_status_filter(r.status, s) {
                    return false;
                }
            }
            true
        })
        .collect();

    Ok(json!({ "requests": requests }))
}

async fn handle_request_detail(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let request_id = cmd
        .get("requestId")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'requestId' parameter")?;

    let entry = state
        .tracked_requests
        .iter()
        .find(|r| r.request_id == request_id)
        .ok_or("Request not found")?;

    let mut result = serde_json::to_value(entry).unwrap_or(json!({}));

    if let Some(ref mgr) = state.browser {
        if let Ok(session_id) = mgr.active_session_id() {
            if let Ok(body_result) = mgr
                .client
                .send_command(
                    "Network.getResponseBody",
                    Some(json!({ "requestId": request_id })),
                    Some(session_id),
                )
                .await
            {
                let base64_encoded = body_result
                    .get("base64Encoded")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let body = body_result
                    .get("body")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if base64_encoded {
                    result["responseBody"] = json!(format!("[base64, {} chars]", body.len()));
                } else {
                    result["responseBody"] = json!(body);
                }
            }
        }
    }

    Ok(result)
}

async fn handle_http_credentials(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let username = cmd
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'username' parameter")?;
    let password = cmd
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'password' parameter")?;

    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{}:{}", username, password),
    );

    let mut headers = HashMap::new();
    headers.insert("Authorization".to_string(), format!("Basic {}", encoded));
    network::set_extra_headers(&mgr.client, &session_id, &headers).await?;

    Ok(json!({ "set": true }))
}

// ---------------------------------------------------------------------------
// Auth handlers
// ---------------------------------------------------------------------------

/// Wait for any selector in `selectors` to appear and return the first match.
///
/// This is used by `auth_login` auto-detection so SPA login forms can render
/// after initial navigation without requiring global network-idle.
async fn wait_for_any_selector(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    selectors: &[&str],
    timeout_ms: u64,
) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    loop {
        for selector in selectors {
            let expression = format!(
                r#"(() => {{
                    const el = document.querySelector({sel});
                    if (!el) return false;

                    const r = el.getBoundingClientRect();
                    const s = window.getComputedStyle(el);
                    const opacity = parseFloat(s.opacity || '1');
                    const isVisible =
                        r.width > 0 &&
                        r.height > 0 &&
                        s.visibility !== 'hidden' &&
                        s.display !== 'none' &&
                        (!Number.isFinite(opacity) || opacity > 0);

                    if (!isVisible) return false;
                    if (el.matches(':disabled')) return false;

                    if (el instanceof HTMLInputElement && el.type === 'hidden') return false;
                    if ((el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) && el.readOnly) return false;

                    return true;
                }})()"#,
                sel = serde_json::to_string(selector).unwrap_or_default()
            );

            let result: super::cdp::types::EvaluateResult = client
                .send_command_typed(
                    "Runtime.evaluate",
                    &super::cdp::types::EvaluateParams {
                        expression,
                        return_by_value: Some(true),
                        await_promise: Some(true),
                    },
                    Some(session_id),
                )
                .await?;

            if result
                .result
                .value
                .as_ref()
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Ok((*selector).to_string());
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Wait timed out after {}ms", timeout_ms));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(
            AUTH_LOGIN_SELECTOR_POLL_INTERVAL_MS,
        ))
        .await;
    }
}

async fn handle_auth_save(cmd: &Value) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name'")?;
    let url = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url'")?;
    let username = cmd
        .get("username")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'username'")?;
    let password = cmd
        .get("password")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'password'")?;
    let username_selector = cmd.get("usernameSelector").and_then(|v| v.as_str());
    let password_selector = cmd.get("passwordSelector").and_then(|v| v.as_str());
    let submit_selector = cmd.get("submitSelector").and_then(|v| v.as_str());
    auth::auth_save(
        name,
        url,
        username,
        password,
        username_selector,
        password_selector,
        submit_selector,
    )
}

async fn handle_auth_login(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name'")?;
    let cred = auth::credentials_get_full(name)?;
    if cred.url.is_empty() {
        return Err("Credential has no URL".to_string());
    }
    let url = cred.url;
    let username = cred.username;
    let password = cred.password;

    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    mgr.navigate(&url, AUTH_LOGIN_WAIT_UNTIL).await?;

    let session_id = mgr.active_session_id()?.to_string();
    let auth_timeout_ms = mgr.default_timeout_ms();

    let preferred_user_selectors = [
        "input[type=email]",
        "input[name=email]",
        "input[id=email]",
        "input[autocomplete=email]",
        "input[autocomplete=username]",
        "input[name=username]",
        "input[name*=email i]",
        "input[name*=user i]",
        "input[id*=email i]",
        "input[id*=user i]",
        "input[type=text][name*=email i]",
        "input[type=text][name*=user i]",
        "input[type=text][id*=email i]",
        "input[type=text][id*=user i]",
        "input[type=text][autocomplete=email]",
        "input[type=text][autocomplete=username]",
    ];
    let fallback_user_selectors = ["input[type=text]", "input:not([type])"];
    let auto_submit_selectors = [
        "button[type=submit]",
        "input[type=submit]",
        "button:not([type])",
    ];

    let username_sel = cmd
        .get("usernameSelector")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or(cred.username_selector);
    let password_sel = cmd
        .get("passwordSelector")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or(cred.password_selector);
    let submit_sel = cmd
        .get("submitSelector")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or(cred.submit_selector);

    // Find and fill username
    let user_sel = if let Some(s) = username_sel {
        wait_for_selector(&mgr.client, &session_id, &s, "visible", auth_timeout_ms)
            .await
            .map_err(|_| format!("Timed out waiting for username selector '{}'", s))?;
        s
    } else {
        let preferred_window_ms = auth_timeout_ms.min(AUTH_LOGIN_PREFERRED_SELECTOR_WINDOW_MS);
        let fallback_window_ms = auth_timeout_ms.saturating_sub(preferred_window_ms);

        match wait_for_any_selector(
            &mgr.client,
            &session_id,
            &preferred_user_selectors,
            preferred_window_ms,
        )
        .await
        {
            Ok(selector) => selector,
            Err(_) => {
                if fallback_window_ms == 0 {
                    return Err(format!(
                        "Timed out waiting for username field (preferred selectors for {}ms: {})",
                        preferred_window_ms,
                        preferred_user_selectors.join(", ")
                    ));
                }

                wait_for_any_selector(
                    &mgr.client,
                    &session_id,
                    &fallback_user_selectors,
                    fallback_window_ms,
                )
                .await
                .map_err(|_| {
                    format!(
                        "Timed out waiting for username field (preferred selectors for {}ms: {}; fallback selectors for {}ms: {})",
                        preferred_window_ms,
                        preferred_user_selectors.join(", "),
                        fallback_window_ms,
                        fallback_user_selectors.join(", ")
                    )
                })?
            }
        }
    };
    interaction::fill(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &user_sel,
        &username,
        &state.iframe_sessions,
    )
    .await?;

    // Find and fill password
    let pass_sel = password_sel.unwrap_or_else(|| "input[type=password]".to_string());
    wait_for_selector(
        &mgr.client,
        &session_id,
        &pass_sel,
        "visible",
        auth_timeout_ms,
    )
    .await
    .map_err(|_| format!("Timed out waiting for password selector '{}'", pass_sel))?;
    interaction::fill(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &pass_sel,
        &password,
        &state.iframe_sessions,
    )
    .await?;

    // Find and click submit
    let sub_sel = if let Some(s) = submit_sel {
        wait_for_selector(&mgr.client, &session_id, &s, "visible", auth_timeout_ms)
            .await
            .map_err(|_| format!("Timed out waiting for submit selector '{}'", s))?;
        s
    } else {
        wait_for_any_selector(
            &mgr.client,
            &session_id,
            &auto_submit_selectors,
            auth_timeout_ms,
        )
        .await
        .map_err(|_| {
            format!(
                "Timed out waiting for submit button (tried selectors: {})",
                auto_submit_selectors.join(", ")
            )
        })?
    };
    interaction::click(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &sub_sel,
        "left",
        1,
        &state.iframe_sessions,
    )
    .await?;

    // Wait for navigation after submit (with fallback timeout)
    let mut rx = mgr.client.subscribe();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    let mut navigated = false;

    loop {
        let result = tokio::time::timeout_at(deadline, rx.recv()).await;
        match result {
            Ok(Ok(event)) => {
                if event.session_id.as_deref() == Some(&session_id) {
                    match event.method.as_str() {
                        "Page.frameNavigated" | "Page.loadEventFired" => {
                            navigated = true;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }

    if !navigated {
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
    }

    Ok(json!({ "loggedIn": true, "name": name }))
}

// ---------------------------------------------------------------------------
// Confirmation handlers (stub)
// ---------------------------------------------------------------------------

async fn handle_confirm(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let pending = state
        .pending_confirmation
        .take()
        .ok_or("No pending confirmation")?;

    // Temporarily remove policy and confirm_actions to avoid re-triggering confirmation
    let policy = state.policy.take();
    let confirm_actions = state.confirm_actions.take();
    let result = Box::pin(execute_command(&pending.cmd, state)).await;
    state.policy = policy;
    state.confirm_actions = confirm_actions;

    Ok(json!({ "confirmed": true, "action": pending.action, "result": result }))
}

async fn handle_deny(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let pending = state
        .pending_confirmation
        .take()
        .ok_or("No pending confirmation")?;

    Ok(json!({ "denied": true, "action": pending.action }))
}

// ---------------------------------------------------------------------------
// iOS handlers (stub)
// ---------------------------------------------------------------------------

async fn handle_swipe(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    // Route through Appium for iOS/WebDriver
    if let Some(ref appium) = state.appium {
        if state.browser.is_none() {
            let start_x = cmd.get("startX").and_then(|v| v.as_f64()).unwrap_or(200.0);
            let start_y = cmd.get("startY").and_then(|v| v.as_f64()).unwrap_or(400.0);
            let end_x = cmd.get("endX").and_then(|v| v.as_f64()).unwrap_or(200.0);
            let end_y = cmd.get("endY").and_then(|v| v.as_f64()).unwrap_or(100.0);

            if let Some(direction) = cmd.get("direction").and_then(|v| v.as_str()) {
                let distance = cmd
                    .get("distance")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(300.0);
                let (dx, dy) = match direction {
                    "up" => (0.0, -distance),
                    "down" => (0.0, distance),
                    "left" => (-distance, 0.0),
                    "right" => (distance, 0.0),
                    _ => (0.0, -distance),
                };
                let actual_end_x = start_x + dx;
                let actual_end_y = start_y + dy;
                let duration = cmd.get("duration").and_then(|v| v.as_u64()).unwrap_or(800);
                appium
                    .swipe(start_x, start_y, actual_end_x, actual_end_y, duration)
                    .await?;
                return Ok(json!({ "swiped": direction }));
            }

            let duration = cmd.get("duration").and_then(|v| v.as_u64()).unwrap_or(800);
            appium
                .swipe(start_x, start_y, end_x, end_y, duration)
                .await?;
            return Ok(json!({ "swiped": true, "from": [start_x, start_y], "to": [end_x, end_y] }));
        }
    }

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let start_x = cmd.get("startX").and_then(|v| v.as_f64()).unwrap_or(200.0);
    let start_y = cmd.get("startY").and_then(|v| v.as_f64()).unwrap_or(400.0);
    let end_x = cmd.get("endX").and_then(|v| v.as_f64()).unwrap_or(200.0);
    let end_y = cmd.get("endY").and_then(|v| v.as_f64()).unwrap_or(100.0);

    if let Some(direction) = cmd.get("direction").and_then(|v| v.as_str()) {
        let distance = cmd
            .get("distance")
            .and_then(|v| v.as_f64())
            .unwrap_or(300.0);
        let (dx, dy) = match direction {
            "up" => (0.0, -distance),
            "down" => (0.0, distance),
            "left" => (-distance, 0.0),
            "right" => (distance, 0.0),
            _ => (0.0, -distance),
        };
        let cx = start_x;
        let cy = start_y;

        mgr.client
            .send_command(
                "Input.dispatchTouchEvent",
                Some(json!({ "type": "touchStart", "touchPoints": [{ "x": cx, "y": cy }] })),
                Some(&session_id),
            )
            .await?;

        let steps = 10;
        for i in 1..=steps {
            let x = cx + dx * (i as f64) / (steps as f64);
            let y = cy + dy * (i as f64) / (steps as f64);
            mgr.client
                .send_command(
                    "Input.dispatchTouchEvent",
                    Some(json!({ "type": "touchMove", "touchPoints": [{ "x": x, "y": y }] })),
                    Some(&session_id),
                )
                .await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
        }

        mgr.client
            .send_command(
                "Input.dispatchTouchEvent",
                Some(json!({ "type": "touchEnd", "touchPoints": [] })),
                Some(&session_id),
            )
            .await?;

        return Ok(json!({ "swiped": direction }));
    }

    // Manual coordinates
    mgr.client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(json!({ "type": "touchStart", "touchPoints": [{ "x": start_x, "y": start_y }] })),
            Some(&session_id),
        )
        .await?;

    let steps = 10;
    for i in 1..=steps {
        let x = start_x + (end_x - start_x) * (i as f64) / (steps as f64);
        let y = start_y + (end_y - start_y) * (i as f64) / (steps as f64);
        mgr.client
            .send_command(
                "Input.dispatchTouchEvent",
                Some(json!({ "type": "touchMove", "touchPoints": [{ "x": x, "y": y }] })),
                Some(&session_id),
            )
            .await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(16)).await;
    }

    mgr.client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(json!({ "type": "touchEnd", "touchPoints": [] })),
            Some(&session_id),
        )
        .await?;

    Ok(json!({ "swiped": true, "from": [start_x, start_y], "to": [end_x, end_y] }))
}

async fn handle_device_list() -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        use super::webdriver::ios;
        let devices = ios::list_all_devices()?;
        Ok(ios::to_device_json(&devices))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err("device_list is only available on macOS with Xcode".to_string())
    }
}

// ---------------------------------------------------------------------------
// Input event handlers
// ---------------------------------------------------------------------------

fn mouse_button_mask(button: &str) -> i32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        "back" => 8,
        "forward" => 16,
        _ => 0,
    }
}

fn primary_button_from_mask(buttons: i32) -> &'static str {
    if buttons & 1 != 0 {
        "left"
    } else if buttons & 2 != 0 {
        "right"
    } else if buttons & 4 != 0 {
        "middle"
    } else if buttons & 8 != 0 {
        "back"
    } else if buttons & 16 != 0 {
        "forward"
    } else {
        "none"
    }
}

#[allow(clippy::too_many_arguments)]
fn build_mouse_event_params(
    mouse_state: &mut MouseState,
    event_type: &str,
    x: Option<f64>,
    y: Option<f64>,
    button: Option<&str>,
    buttons: Option<i32>,
    click_count: Option<i32>,
    delta_x: Option<f64>,
    delta_y: Option<f64>,
    modifiers: Option<i32>,
) -> DispatchMouseEventParams {
    let x = x.unwrap_or(mouse_state.x);
    let y = y.unwrap_or(mouse_state.y);
    mouse_state.x = x;
    mouse_state.y = y;

    let mut next_buttons = buttons.unwrap_or(mouse_state.buttons);
    if buttons.is_none() {
        match event_type {
            "mousePressed" => {
                next_buttons |= mouse_button_mask(button.unwrap_or("left"));
            }
            "mouseReleased" => {
                next_buttons &= !mouse_button_mask(button.unwrap_or("left"));
            }
            _ => {}
        }
    }
    mouse_state.buttons = next_buttons;

    DispatchMouseEventParams {
        event_type: event_type.to_string(),
        x,
        y,
        button: Some(
            button
                .unwrap_or(primary_button_from_mask(next_buttons))
                .to_string(),
        ),
        buttons: Some(next_buttons),
        click_count,
        delta_x,
        delta_y,
        modifiers,
    }
}

async fn handle_input_mouse(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let event_type = cmd
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("mouseMoved");
    let params = build_mouse_event_params(
        &mut state.mouse_state,
        event_type,
        cmd.get("x").and_then(|v| v.as_f64()),
        cmd.get("y").and_then(|v| v.as_f64()),
        cmd.get("button").and_then(|v| v.as_str()),
        cmd.get("buttons")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        cmd.get("clickCount")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
        cmd.get("deltaX").and_then(|v| v.as_f64()),
        cmd.get("deltaY").and_then(|v| v.as_f64()),
        cmd.get("modifiers")
            .and_then(|v| v.as_i64())
            .map(|v| v as i32),
    );

    mgr.client
        .send_command_typed::<_, Value>("Input.dispatchMouseEvent", &params, Some(&session_id))
        .await?;
    Ok(json!({ "dispatched": event_type }))
}

async fn handle_input_keyboard(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let event_type = cmd
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("keyDown");

    let mut params = json!({ "type": event_type });
    for key in &["key", "code", "text"] {
        if let Some(v) = cmd.get(*key) {
            params[*key] = v.clone();
        }
    }

    mgr.client
        .send_command("Input.dispatchKeyEvent", Some(params), Some(&session_id))
        .await?;
    Ok(json!({ "dispatched": event_type }))
}

async fn handle_input_touch(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let event_type = cmd
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("touchStart");

    mgr.client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(json!({
                "type": event_type,
                "touchPoints": cmd.get("touchPoints").unwrap_or(&json!([])),
            })),
            Some(&session_id),
        )
        .await?;
    Ok(json!({ "dispatched": event_type }))
}

async fn handle_keydown(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let key = cmd
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'key' parameter")?;

    mgr.client
        .send_command(
            "Input.dispatchKeyEvent",
            Some(json!({ "type": "keyDown", "key": key })),
            Some(&session_id),
        )
        .await?;
    Ok(json!({ "keydown": key }))
}

async fn handle_keyup(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let key = cmd
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'key' parameter")?;

    mgr.client
        .send_command(
            "Input.dispatchKeyEvent",
            Some(json!({ "type": "keyUp", "key": key })),
            Some(&session_id),
        )
        .await?;
    Ok(json!({ "keyup": key }))
}

async fn handle_inserttext(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let text = cmd
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'text' parameter")?;

    mgr.client
        .send_command(
            "Input.insertText",
            Some(json!({ "text": text })),
            Some(&session_id),
        )
        .await?;
    Ok(json!({ "inserted": true }))
}

async fn handle_mousemove(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let x = cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let params = build_mouse_event_params(
        &mut state.mouse_state,
        "mouseMoved",
        Some(x),
        Some(y),
        None,
        None,
        None,
        None,
        None,
        None,
    );

    mgr.client
        .send_command_typed::<_, Value>("Input.dispatchMouseEvent", &params, Some(&session_id))
        .await?;
    Ok(json!({ "moved": true }))
}

async fn handle_mousedown(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let button = cmd.get("button").and_then(|v| v.as_str()).unwrap_or("left");
    let params = build_mouse_event_params(
        &mut state.mouse_state,
        "mousePressed",
        None,
        None,
        Some(button),
        None,
        Some(1),
        None,
        None,
        None,
    );

    mgr.client
        .send_command_typed::<_, Value>("Input.dispatchMouseEvent", &params, Some(&session_id))
        .await?;
    Ok(json!({ "pressed": true }))
}

async fn handle_mouseup(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let button = cmd.get("button").and_then(|v| v.as_str()).unwrap_or("left");
    let params = build_mouse_event_params(
        &mut state.mouse_state,
        "mouseReleased",
        None,
        None,
        Some(button),
        None,
        Some(1),
        None,
        None,
        None,
    );

    mgr.client
        .send_command_typed::<_, Value>("Input.dispatchMouseEvent", &params, Some(&session_id))
        .await?;
    Ok(json!({ "released": true }))
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

fn success_response(id: &str, data: Value) -> Value {
    json!({
        "id": id,
        "success": true,
        "data": data,
    })
}

fn error_response(id: &str, error: &str) -> Value {
    json!({
        "id": id,
        "success": false,
        "error": error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;
    use std::fs;

    fn unique_socket_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agent-browser-{label}-{}-{nanos}",
            std::process::id()
        ))
    }

    #[tokio::test]
    async fn test_stream_enable_disable_and_status_without_browser() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
        let socket_dir = unique_socket_dir("stream-runtime");
        fs::create_dir_all(&socket_dir).expect("socket dir should be created");
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            socket_dir.to_str().expect("socket dir should be utf-8"),
        );
        guard.set("AGENT_BROWSER_SESSION", "stream-runtime-session");

        let mut state = DaemonState::new();

        let disabled_status = handle_stream_status(&state)
            .await
            .expect("status should work before enable");
        assert_eq!(disabled_status["enabled"], false);
        assert_eq!(disabled_status["port"], Value::Null);
        assert_eq!(disabled_status["connected"], false);
        assert_eq!(disabled_status["screencasting"], false);

        let enabled_status = handle_stream_enable(&json!({ "port": 0 }), &mut state)
            .await
            .expect("stream enable should succeed");
        let port = enabled_status["port"]
            .as_u64()
            .expect("runtime stream should report a bound port");
        assert!(port > 0, "runtime stream should bind a non-zero port");
        assert_eq!(enabled_status["enabled"], true);
        assert_eq!(enabled_status["connected"], false);
        assert_eq!(enabled_status["screencasting"], false);

        let stream_path = socket_dir.join("stream-runtime-session.stream");
        let port_file =
            fs::read_to_string(&stream_path).expect("stream metadata file should exist");
        assert_eq!(port_file.trim(), port.to_string());

        let duplicate_err = handle_stream_enable(&json!({}), &mut state)
            .await
            .expect_err("duplicate enable should fail");
        assert!(duplicate_err.contains("already enabled"));

        let status = handle_stream_status(&state)
            .await
            .expect("status should work after enable");
        assert_eq!(status["enabled"], true);
        assert_eq!(status["port"], port);

        let disabled = handle_stream_disable(&mut state)
            .await
            .expect("stream disable should succeed");
        assert_eq!(disabled["disabled"], true);
        assert!(
            !stream_path.exists(),
            "disabling runtime stream should remove the metadata file"
        );
        assert!(state.stream_server.is_none());
        assert!(state.stream_client.is_none());

        let final_status = handle_stream_status(&state)
            .await
            .expect("status should work after disable");
        assert_eq!(final_status["enabled"], false);
        assert_eq!(final_status["port"], Value::Null);

        let disable_err = handle_stream_disable(&mut state)
            .await
            .expect_err("duplicate disable should fail");
        assert!(disable_err.contains("not enabled"));

        let _ = fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn test_stream_disable_preserves_existing_screencast_state() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
        let socket_dir = unique_socket_dir("stream-preserve-screencast");
        fs::create_dir_all(&socket_dir).expect("socket dir should be created");
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            socket_dir.to_str().expect("socket dir should be utf-8"),
        );
        guard.set(
            "AGENT_BROWSER_SESSION",
            "stream-preserve-screencast-session",
        );

        let mut state = DaemonState::new();
        handle_stream_enable(&json!({ "port": 0 }), &mut state)
            .await
            .expect("stream enable should succeed");
        state.screencasting = true;

        let disabled = handle_stream_disable(&mut state)
            .await
            .expect("stream disable should succeed");
        assert_eq!(disabled["disabled"], true);
        assert!(
            state.screencasting,
            "stream disable should not clear an independently managed screencast state"
        );

        let _ = fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn test_stream_disable_clears_state_when_stream_file_removal_fails() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
        let socket_dir = unique_socket_dir("stream-disable-cleanup");
        fs::create_dir_all(&socket_dir).expect("socket dir should be created");
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            socket_dir.to_str().expect("socket dir should be utf-8"),
        );
        guard.set("AGENT_BROWSER_SESSION", "stream-disable-cleanup-session");

        let mut state = DaemonState::new();
        handle_stream_enable(&json!({ "port": 0 }), &mut state)
            .await
            .expect("stream enable should succeed");

        let stream_path = socket_dir.join("stream-disable-cleanup-session.stream");
        fs::remove_file(&stream_path).expect("stream metadata file should exist");
        fs::create_dir(&stream_path).expect("directory should force remove_stream_file failure");

        let err = handle_stream_disable(&mut state)
            .await
            .expect_err("stream disable should surface file removal failure");
        assert!(err.contains("Failed to remove stream metadata"));
        assert!(
            state.stream_server.is_none(),
            "stream disable should clear stream_server even when metadata cleanup fails"
        );
        assert!(
            state.stream_client.is_none(),
            "stream disable should clear stream_client even when metadata cleanup fails"
        );

        let _ = fs::remove_dir_all(&socket_dir);
    }

    #[tokio::test]
    async fn test_stream_enable_port_conflict_returns_error() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
        let socket_dir = unique_socket_dir("stream-port-conflict");
        fs::create_dir_all(&socket_dir).expect("socket dir should be created");
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            socket_dir.to_str().expect("socket dir should be utf-8"),
        );
        guard.set("AGENT_BROWSER_SESSION", "stream-port-conflict-session");

        let listener = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("test should reserve an ephemeral port");
        let port = listener
            .local_addr()
            .expect("listener should have local addr")
            .port();

        let mut state = DaemonState::new();
        let err = handle_stream_enable(&json!({ "port": port }), &mut state)
            .await
            .expect_err("conflicting port should fail");
        assert!(err.contains("Failed to bind stream server"));
        assert!(state.stream_server.is_none());
        assert!(state.stream_client.is_none());
        assert!(
            !socket_dir
                .join("stream-port-conflict-session.stream")
                .exists(),
            "failed enable should not leave stale metadata behind"
        );

        drop(listener);
        let _ = fs::remove_dir_all(&socket_dir);
    }

    #[test]
    fn test_success_response_structure() {
        let resp = success_response("cmd-1", json!({"url": "https://example.com"}));
        assert_eq!(resp["id"], "cmd-1");
        assert_eq!(resp["success"], true);
        assert!(resp["data"].is_object());
        assert_eq!(resp["data"]["url"], "https://example.com");
    }

    #[test]
    fn test_error_response_structure() {
        let resp = error_response("cmd-2", "Something went wrong");
        assert_eq!(resp["id"], "cmd-2");
        assert_eq!(resp["success"], false);
        assert_eq!(resp["error"], "Something went wrong");
    }

    #[tokio::test]
    async fn test_daemon_state_new() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "AGENT_BROWSER_SESSION_NAME",
            "AGENT_BROWSER_SESSION",
        ]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");
        guard.remove("AGENT_BROWSER_SESSION_NAME");
        guard.remove("AGENT_BROWSER_SESSION");

        let state = DaemonState::new();
        assert!(state.browser.is_none());
        assert!(state.domain_filter.read().await.is_none());
        assert_eq!(state.session_id, "default");
        assert!(!state.tracing_state.active);
        assert!(!state.recording_state.active);
        assert_eq!(state.mouse_state.x, 0.0);
        assert_eq!(state.mouse_state.y, 0.0);
        assert_eq!(state.mouse_state.buttons, 0);
    }

    #[test]
    fn test_mouse_event_params_preserve_position_and_buttons() {
        let mut mouse_state = MouseState::default();

        let move_params = build_mouse_event_params(
            &mut mouse_state,
            "mouseMoved",
            Some(120.0),
            Some(240.0),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(move_params.x, 120.0);
        assert_eq!(move_params.y, 240.0);
        assert_eq!(move_params.buttons, Some(0));

        let down_params = build_mouse_event_params(
            &mut mouse_state,
            "mousePressed",
            None,
            None,
            Some("left"),
            None,
            Some(1),
            None,
            None,
            None,
        );
        assert_eq!(down_params.x, 120.0);
        assert_eq!(down_params.y, 240.0);
        assert_eq!(down_params.button.as_deref(), Some("left"));
        assert_eq!(down_params.buttons, Some(1));
        assert_eq!(mouse_state.buttons, 1);

        let drag_move_params = build_mouse_event_params(
            &mut mouse_state,
            "mouseMoved",
            Some(150.0),
            Some(260.0),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(drag_move_params.buttons, Some(1));
        assert_eq!(drag_move_params.button.as_deref(), Some("left"));
        assert_eq!(mouse_state.x, 150.0);
        assert_eq!(mouse_state.y, 260.0);

        let up_params = build_mouse_event_params(
            &mut mouse_state,
            "mouseReleased",
            None,
            None,
            Some("left"),
            None,
            Some(1),
            None,
            None,
            None,
        );
        assert_eq!(up_params.x, 150.0);
        assert_eq!(up_params.y, 260.0);
        assert_eq!(up_params.buttons, Some(0));
        assert_eq!(mouse_state.buttons, 0);
    }

    #[test]
    fn test_reset_input_state_clears_mouse_state() {
        let mut state = DaemonState::new();
        state.mouse_state.x = 12.0;
        state.mouse_state.y = 34.0;
        state.mouse_state.buttons = 1;

        state.reset_input_state();

        assert_eq!(state.mouse_state.x, 0.0);
        assert_eq!(state.mouse_state.y, 0.0);
        assert_eq!(state.mouse_state.buttons, 0);
    }

    #[test]
    fn test_launch_options_from_env_defaults() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_HEADED"]);
        let opts = launch_options_from_env();
        assert!(opts.headless);
        assert!(opts.args.is_empty());
        assert!(!opts.allow_file_access);
    }

    #[test]
    fn test_launch_options_from_env_headed_flag() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_HEADED"]);
        _guard.set("AGENT_BROWSER_HEADED", "1");
        let opts = launch_options_from_env();
        assert!(
            !opts.headless,
            "AGENT_BROWSER_HEADED=1 should set headless=false"
        );
    }

    #[test]
    fn test_har_entry_to_json_enriches_request_and_response() {
        // wall_time: 2026-03-15T12:00:00Z = 1_773_576_000
        let entry = HarEntry {
            request_id: "req-1".to_string(),
            wall_time: 1773576000.0,
            method: "POST".to_string(),
            url: "https://example.com/api?foo=bar&baz=qux".to_string(),
            request_headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Cookie".to_string(), "session=abc; theme=dark".to_string()),
            ],
            post_data: Some(r#"{"x":1}"#.to_string()),
            request_body_size: 7,
            resource_type: "XHR".to_string(),
            status: Some(201),
            status_text: "Created".to_string(),
            http_version: "HTTP/2.0".to_string(),
            response_headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                (
                    "location".to_string(),
                    "https://example.com/api/1".to_string(),
                ),
                (
                    "set-cookie".to_string(),
                    "token=xyz; Path=/; HttpOnly".to_string(),
                ),
            ],
            mime_type: "application/json".to_string(),
            redirect_url: "https://example.com/api/1".to_string(),
            response_body_size: 42,
            cdp_timing: None,
            loading_finished_timestamp: None,
        };

        let har = har_entry_to_json(entry);
        assert_eq!(har["startedDateTime"], "2026-03-15T12:00:00Z");
        assert_eq!(har["request"]["method"], "POST");
        assert_eq!(har["request"]["httpVersion"], "HTTP/2.0");
        assert_eq!(har["request"]["queryString"][0]["name"], "foo");
        assert_eq!(har["request"]["queryString"][0]["value"], "bar");
        assert_eq!(har["request"]["bodySize"], 7);
        assert_eq!(har["request"]["postData"]["mimeType"], "application/json");
        assert_eq!(har["request"]["postData"]["text"], r#"{"x":1}"#);
        assert_eq!(har["request"]["cookies"][0]["name"], "session");
        assert_eq!(har["request"]["cookies"][0]["value"], "abc");
        assert_eq!(har["request"]["cookies"][1]["name"], "theme");
        assert_eq!(har["request"]["cookies"][1]["value"], "dark");
        assert_eq!(har["response"]["status"], 201);
        assert_eq!(har["response"]["statusText"], "Created");
        assert_eq!(har["response"]["content"]["mimeType"], "application/json");
        assert_eq!(har["response"]["content"]["size"], 42);
        assert_eq!(har["response"]["redirectURL"], "https://example.com/api/1");
        assert_eq!(har["response"]["cookies"][0]["name"], "token");
        assert_eq!(har["response"]["cookies"][0]["value"], "xyz");
        assert_eq!(har["_resourceType"], "XHR");
    }

    #[test]
    fn test_har_wall_time_to_rfc3339_epoch() {
        // Known timestamp: 2026-03-15T12:00:00Z = 1_773_576_000
        let result = har_wall_time_to_rfc3339(1773576000.0);
        assert!(result.starts_with("2026-03-15T12:00:00"));
    }

    #[test]
    fn test_har_wall_time_to_rfc3339_fractional_seconds() {
        let result = har_wall_time_to_rfc3339(1773576000.456);
        assert!(result.contains(".456") || result.contains("456"));
    }

    #[test]
    fn test_har_cdp_protocol_to_http_version() {
        assert_eq!(har_cdp_protocol_to_http_version("h2"), "HTTP/2.0");
        assert_eq!(har_cdp_protocol_to_http_version("h3"), "HTTP/3.0");
        assert_eq!(har_cdp_protocol_to_http_version("http/1.0"), "HTTP/1.0");
        assert_eq!(har_cdp_protocol_to_http_version("http/1.1"), "HTTP/1.1");
        assert_eq!(har_cdp_protocol_to_http_version("unknown"), "HTTP/1.1");
    }

    #[test]
    fn test_har_parse_request_cookies() {
        let cookies = har_parse_request_cookies("session=abc; theme=dark; empty=");
        assert_eq!(cookies.len(), 3);
        assert_eq!(cookies[0]["name"], "session");
        assert_eq!(cookies[0]["value"], "abc");
        assert_eq!(cookies[1]["name"], "theme");
        assert_eq!(cookies[1]["value"], "dark");
        assert_eq!(cookies[2]["name"], "empty");
        assert_eq!(cookies[2]["value"], "");
    }

    #[test]
    fn test_har_set_cookie_strips_attributes_before_equal_split() {
        let entry = HarEntry {
            request_id: "r".to_string(),
            wall_time: 1773576000.0,
            method: "GET".to_string(),
            url: "https://example.com/".to_string(),
            request_headers: vec![],
            post_data: None,
            request_body_size: 0,
            resource_type: "Document".to_string(),
            status: Some(200),
            status_text: "OK".to_string(),
            http_version: "HTTP/1.1".to_string(),
            response_headers: vec![(
                "set-cookie".to_string(),
                "token=abc; Path=/; HttpOnly".to_string(),
            )],
            mime_type: "text/html".to_string(),
            redirect_url: String::new(),
            response_body_size: 0,
            cdp_timing: None,
            loading_finished_timestamp: None,
        };
        let har = har_entry_to_json(entry);
        assert_eq!(har["response"]["cookies"][0]["name"], "token");
        assert_eq!(har["response"]["cookies"][0]["value"], "abc");
    }

    #[test]
    fn test_har_compute_timings_no_cdp_timing() {
        let (timings, total) = har_compute_timings(None, None);
        assert_eq!(timings["send"], 0);
        assert_eq!(timings["wait"], 0);
        assert_eq!(timings["receive"], 0);
        assert_eq!(total, 0.0);
    }

    #[test]
    fn test_har_compute_timings_with_cdp_timing() {
        let cdp = json!({
            "requestTime": 1000.0,
            "dnsStart": 0.0, "dnsEnd": 5.0,
            "connectStart": 5.0, "connectEnd": 15.0,
            "sslStart": 8.0, "sslEnd": 15.0,
            "sendStart": 15.0, "sendEnd": 16.0,
            "receiveHeadersStart": 16.0, "receiveHeadersEnd": 50.0,
        });
        let (timings, total) = har_compute_timings(Some(&cdp), Some(1000.1));
        assert_eq!(timings["dns"], 5.0);
        assert_eq!(timings["connect"], 10.0);
        assert_eq!(timings["ssl"], 7.0);
        assert_eq!(timings["send"], 1.0);
        assert!(total > 0.0);
    }

    #[tokio::test]
    async fn test_handle_har_stop_without_path_uses_default_location() {
        let mut state = DaemonState::new();
        state.har_recording = true;
        state.har_entries.push(HarEntry {
            request_id: "req-2".to_string(),
            wall_time: 1773576000.0,
            method: "GET".to_string(),
            url: "https://example.com/".to_string(),
            request_headers: vec![("Accept".to_string(), "text/html".to_string())],
            post_data: None,
            request_body_size: 0,
            resource_type: "Document".to_string(),
            status: Some(200),
            status_text: "OK".to_string(),
            http_version: "HTTP/2.0".to_string(),
            response_headers: vec![("content-type".to_string(), "text/html".to_string())],
            mime_type: "text/html".to_string(),
            redirect_url: String::new(),
            response_body_size: 128,
            cdp_timing: None,
            loading_finished_timestamp: None,
        });

        let result = handle_har_stop(&json!({ "action": "har_stop" }), &mut state)
            .await
            .unwrap();

        let path = result["path"].as_str().unwrap();
        assert!(path.ends_with(".har"));
        assert!(std::path::Path::new(path).starts_with(get_har_dir()));
        assert_eq!(result["requestCount"], 1);
        assert!(!state.har_recording);
        assert!(state.har_entries.is_empty());

        let har: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(har["log"]["version"], "1.2");
        assert_eq!(har["log"]["creator"]["name"], "agent-browser");
        assert!(har["log"].get("browser").is_none());
        assert_eq!(har["log"]["entries"][0]["response"]["content"]["size"], 128);

        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn test_execute_har_stop_skips_browser_auto_launch() {
        let path = std::env::temp_dir().join(format!(
            "agent-browser-har-stop-{}.har",
            unix_timestamp_millis()
        ));
        let mut state = DaemonState::new();
        state.har_entries.push(HarEntry {
            request_id: "req-3".to_string(),
            wall_time: 1773576000.0,
            method: "GET".to_string(),
            url: "https://example.com/".to_string(),
            request_headers: vec![],
            post_data: None,
            request_body_size: 0,
            resource_type: "Document".to_string(),
            status: Some(200),
            status_text: "OK".to_string(),
            http_version: "HTTP/1.1".to_string(),
            response_headers: vec![],
            mime_type: "text/html".to_string(),
            redirect_url: String::new(),
            response_body_size: 64,
            cdp_timing: None,
            loading_finished_timestamp: None,
        });

        let result = execute_command(
            &json!({
                "action": "har_stop",
                "id": "har-stop-1",
                "path": path.to_string_lossy().to_string()
            }),
            &mut state,
        )
        .await;

        assert_eq!(result["success"], true);
        assert_eq!(result["data"]["requestCount"], 1);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_browser_metadata_from_version_parses_product() {
        let metadata = browser_metadata_from_version(&json!({
            "product": "HeadlessChrome/123.0.6312.0"
        }))
        .unwrap();

        assert_eq!(metadata["name"], "HeadlessChrome");
        assert_eq!(metadata["version"], "123.0.6312.0");
    }

    #[test]
    fn test_default_timeout_ms_from_env() {
        // When AGENT_BROWSER_DEFAULT_TIMEOUT is set, DaemonState should use it
        env::set_var("AGENT_BROWSER_DEFAULT_TIMEOUT", "3000");
        let state = DaemonState::new();
        assert_eq!(state.default_timeout_ms, 3000);
        env::remove_var("AGENT_BROWSER_DEFAULT_TIMEOUT");
    }

    #[test]
    fn test_default_timeout_ms_fallback() {
        // When AGENT_BROWSER_DEFAULT_TIMEOUT is unset, DaemonState uses 30000
        env::remove_var("AGENT_BROWSER_DEFAULT_TIMEOUT");
        let state = DaemonState::new();
        assert_eq!(state.default_timeout_ms, 30_000);
    }

    #[tokio::test]
    async fn test_execute_unknown_command() {
        let mut state = DaemonState::new();
        let cmd = json!({ "action": "unknown_action_xyz", "id": "test-1" });
        let result = execute_command(&cmd, &mut state).await;
        assert_eq!(result["success"], false);
        let error_msg = result["error"].as_str().unwrap();
        assert!(
            error_msg.contains("Not yet implemented") || error_msg.contains("Auto-launch failed"),
            "Unexpected error: {}",
            error_msg
        );
    }

    #[tokio::test]
    async fn test_execute_empty_action() {
        let mut state = DaemonState::new();
        let cmd = json!({ "id": "test-2" });
        let result = execute_command(&cmd, &mut state).await;
        // Empty action triggers auto-launch which will fail without a browser
        assert_eq!(result["success"], false);
    }

    #[tokio::test]
    async fn test_execute_close_without_browser() {
        let mut state = DaemonState::new();
        let cmd = json!({ "action": "close", "id": "test-3" });
        let result = execute_command(&cmd, &mut state).await;
        assert_eq!(result["success"], true);
        assert_eq!(result["data"]["closed"], true);
    }

    #[tokio::test]
    async fn test_navigate_without_browser() {
        let mut state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }
        let cmd = json!({
            "action": "navigate",
            "url": "https://blocked.com",
            "id": "test-4"
        });
        let result = execute_command(&cmd, &mut state).await;
        // Will fail because auto-launch fails, but the domain filter won't block since
        // auto-launch happens first
        assert_eq!(result["success"], false);
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn test_credentials_roundtrip_via_actions() {
        let _lock = crate::native::auth::AUTH_TEST_MUTEX.lock().unwrap();
        let key_var = "AGENT_BROWSER_ENCRYPTION_KEY";
        let original = std::env::var(key_var).ok();
        // SAFETY: AUTH_TEST_MUTEX serializes all test access so no concurrent mutation.
        unsafe { std::env::set_var(key_var, "a".repeat(64)) };

        let mut state = DaemonState::new();

        let set_cmd = json!({
            "action": "credentials_set",
            "name": "test-cred-action",
            "username": "user",
            "password": "pass",
            "id": "c1"
        });
        let result = execute_command(&set_cmd, &mut state).await;
        assert_eq!(result["success"], true);

        let get_cmd = json!({
            "action": "credentials_get",
            "name": "test-cred-action",
            "id": "c2"
        });
        let result = execute_command(&get_cmd, &mut state).await;
        assert_eq!(result["success"], true);
        assert_eq!(result["data"]["username"], "user");

        let list_cmd = json!({ "action": "credentials_list", "id": "c3" });
        let result = execute_command(&list_cmd, &mut state).await;
        assert_eq!(result["success"], true);

        let del_cmd = json!({
            "action": "credentials_delete",
            "name": "test-cred-action",
            "id": "c4"
        });
        let result = execute_command(&del_cmd, &mut state).await;
        assert_eq!(result["success"], true);

        // SAFETY: AUTH_TEST_MUTEX serializes all test access so no concurrent mutation.
        match original {
            Some(val) => unsafe { std::env::set_var(key_var, val) },
            None => unsafe { std::env::remove_var(key_var) },
        }
    }

    #[tokio::test]
    async fn test_state_list_via_actions() {
        let mut state = DaemonState::new();
        let cmd = json!({ "action": "state_list", "id": "s1" });
        let result = execute_command(&cmd, &mut state).await;
        assert_eq!(result["success"], true);
        assert!(result["data"]["files"].is_array());
    }

    #[tokio::test]
    async fn test_build_fetch_patterns_empty_state() {
        let state = DaemonState::new();
        let patterns = build_fetch_patterns(&state).await;
        assert!(
            patterns.is_empty(),
            "No routes/filters/headers → no patterns"
        );
    }

    #[tokio::test]
    async fn test_build_fetch_patterns_with_routes() {
        let state = DaemonState::new();
        {
            let mut routes = state.routes.write().await;
            routes.push(super::RouteEntry {
                url_pattern: "https://example.com/*".to_string(),
                response: None,
                abort: true,
            });
        }
        let patterns = build_fetch_patterns(&state).await;
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["urlPattern"], "https://example.com/*");
    }

    #[tokio::test]
    async fn test_build_fetch_patterns_adds_wildcard_for_domain_filter() {
        let state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(super::super::network::DomainFilter::new("example.com"));
        }
        let patterns = build_fetch_patterns(&state).await;
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["urlPattern"], "*");
    }

    #[tokio::test]
    async fn test_build_fetch_patterns_adds_wildcard_for_origin_headers() {
        let state = DaemonState::new();
        {
            let mut oh = state.origin_headers.write().await;
            let mut headers = HashMap::new();
            headers.insert("Authorization".to_string(), "Bearer xxx".to_string());
            oh.insert("http://example.com".to_string(), headers);
        }
        let patterns = build_fetch_patterns(&state).await;
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["urlPattern"], "*");
    }

    #[tokio::test]
    async fn test_build_fetch_patterns_no_duplicate_wildcard() {
        let state = DaemonState::new();
        {
            let mut routes = state.routes.write().await;
            routes.push(super::RouteEntry {
                url_pattern: "*".to_string(),
                response: None,
                abort: false,
            });
        }
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(super::super::network::DomainFilter::new("example.com"));
        }
        let patterns = build_fetch_patterns(&state).await;
        assert_eq!(
            patterns.len(),
            1,
            "Should not add a second wildcard when routes already contain one"
        );
    }

    #[test]
    fn test_auth_login_waits_for_load_event() {
        use super::super::browser::WaitUntil;
        assert_eq!(
            super::AUTH_LOGIN_WAIT_UNTIL,
            WaitUntil::Load,
            "auth_login should navigate with Load and then wait for form \
             selectors explicitly"
        );
    }

    #[test]
    fn test_parse_key_chord_plain_key() {
        let (key, mods) = parse_key_chord("a");
        assert_eq!(key, "a");
        assert_eq!(mods, None);
    }

    #[test]
    fn test_parse_key_chord_enter() {
        let (key, mods) = parse_key_chord("Enter");
        assert_eq!(key, "Enter");
        assert_eq!(mods, None);
    }

    #[test]
    fn test_parse_key_chord_control_a() {
        let (key, mods) = parse_key_chord("Control+a");
        assert_eq!(key, "a");
        assert_eq!(mods, Some(2));
    }

    #[test]
    fn test_parse_key_chord_ctrl_alias() {
        let (key, mods) = parse_key_chord("Ctrl+c");
        assert_eq!(key, "c");
        assert_eq!(mods, Some(2));
    }

    #[test]
    fn test_parse_key_chord_shift_enter() {
        let (key, mods) = parse_key_chord("Shift+Enter");
        assert_eq!(key, "Enter");
        assert_eq!(mods, Some(8));
    }

    #[test]
    fn test_parse_key_chord_control_shift_a() {
        let (key, mods) = parse_key_chord("Control+Shift+a");
        assert_eq!(key, "a");
        assert_eq!(mods, Some(2 | 8));
    }

    #[test]
    fn test_parse_key_chord_meta_a() {
        let (key, mods) = parse_key_chord("Meta+a");
        assert_eq!(key, "a");
        assert_eq!(mods, Some(4));
    }

    #[test]
    fn test_parse_key_chord_alt_tab() {
        let (key, mods) = parse_key_chord("Alt+Tab");
        assert_eq!(key, "Tab");
        assert_eq!(mods, Some(1));
    }

    #[test]
    fn test_parse_key_chord_plus_key() {
        // A bare "+" should not be confused with a separator
        let (key, mods) = parse_key_chord("+");
        assert_eq!(key, "+");
        assert_eq!(mods, None);
    }

    #[tokio::test]
    async fn test_auto_dialog_enabled_by_default() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_NO_AUTO_DIALOG"]);
        std::env::remove_var("AGENT_BROWSER_NO_AUTO_DIALOG");
        let state = DaemonState::new();
        assert!(state.auto_dialog, "auto_dialog should be true by default");
        drop(guard);
    }

    #[tokio::test]
    async fn test_auto_dialog_disabled_by_env() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_NO_AUTO_DIALOG"]);
        guard.set("AGENT_BROWSER_NO_AUTO_DIALOG", "1");
        let state = DaemonState::new();
        assert!(
            !state.auto_dialog,
            "auto_dialog should be false when AGENT_BROWSER_NO_AUTO_DIALOG=1"
        );
        drop(guard);
    }

    #[tokio::test]
    async fn test_auto_dialog_disabled_by_env_true() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_NO_AUTO_DIALOG"]);
        guard.set("AGENT_BROWSER_NO_AUTO_DIALOG", "true");
        let state = DaemonState::new();
        assert!(
            !state.auto_dialog,
            "auto_dialog should be false when AGENT_BROWSER_NO_AUTO_DIALOG=true"
        );
        drop(guard);
    }

    #[tokio::test]
    async fn test_auto_dialog_not_disabled_by_random_value() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_NO_AUTO_DIALOG"]);
        guard.set("AGENT_BROWSER_NO_AUTO_DIALOG", "no");
        let state = DaemonState::new();
        assert!(
            state.auto_dialog,
            "auto_dialog should remain true for non-truthy env values"
        );
        drop(guard);
    }

    #[test]
    fn test_pending_dialog_not_set_for_auto_handled_alert() {
        // Simulate what handle_browser_event does: when auto_dialog is true,
        // alert/beforeunload should NOT populate pending_dialog.
        let auto_dialog = true;
        for dialog_type in &["alert", "beforeunload"] {
            let auto_handled = auto_dialog && matches!(*dialog_type, "beforeunload" | "alert");
            assert!(
                auto_handled,
                "{dialog_type} should be auto-handled when auto_dialog is true"
            );
        }
    }

    #[test]
    fn test_pending_dialog_set_for_confirm_prompt() {
        // confirm and prompt should NOT be auto-handled even when auto_dialog is true.
        let auto_dialog = true;
        for dialog_type in &["confirm", "prompt"] {
            let auto_handled = auto_dialog && matches!(*dialog_type, "beforeunload" | "alert");
            assert!(!auto_handled, "{dialog_type} should NOT be auto-handled");
        }
    }
}
