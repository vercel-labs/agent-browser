use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex};

use super::cdp::chrome::{auto_connect_cdp, launch_chrome, ChromeProcess, LaunchOptions};
use super::cdp::client::CdpClient;
use super::cdp::discovery::discover_cdp_url;
use super::cdp::lightpanda::{launch_lightpanda, LightpandaLaunchOptions, LightpandaProcess};
use super::cdp::types::*;
use super::element::{resolve_element_object_id, RefMap};

// ---------------------------------------------------------------------------
// Launch validation
// ---------------------------------------------------------------------------

/// Validates launch/connect options for incompatible combinations.
/// Returns `Ok(())` if valid, or `Err(msg)` with a user-friendly error.
pub fn validate_launch_options(
    extensions: Option<&[String]>,
    has_cdp: bool,
    profile: Option<&str>,
    storage_state: Option<&str>,
    allow_file_access: bool,
    executable_path: Option<&str>,
) -> Result<(), String> {
    let has_extensions = extensions.map(|e| !e.is_empty()).unwrap_or(false);

    if has_extensions && has_cdp {
        return Err(
            "Cannot use extensions with cdp_url (extensions require local browser launch)"
                .to_string(),
        );
    }
    if profile.is_some() && has_cdp {
        return Err(
            "Cannot use profile with cdp_url (profile requires local browser launch)".to_string(),
        );
    }
    if storage_state.is_some() && profile.is_some() {
        return Err("Cannot use storage_state with profile".to_string());
    }
    if storage_state.is_some() && has_extensions {
        return Err("Cannot use storage_state with extensions".to_string());
    }
    if allow_file_access {
        if let Some(path) = executable_path {
            let lower = path.to_lowercase();
            if lower.contains("firefox") || lower.contains("webkit") || lower.contains("safari") {
                return Err(
                    "allow_file_access is not supported with non-Chromium browsers".to_string(),
                );
            }
        }
    }
    Ok(())
}

/// Validates that Chrome-only options are not used with Lightpanda.
fn validate_lightpanda_options(options: &LaunchOptions) -> Result<(), String> {
    if options
        .extensions
        .as_ref()
        .map(|e| !e.is_empty())
        .unwrap_or(false)
    {
        return Err("Extensions are not supported with Lightpanda".to_string());
    }
    if options.profile.is_some() {
        return Err("Profiles are not supported with Lightpanda".to_string());
    }
    if options.storage_state.is_some() {
        return Err("Storage state is not supported with Lightpanda".to_string());
    }
    if options.allow_file_access {
        return Err("File access is not supported with Lightpanda".to_string());
    }
    if !options.headless {
        return Err("Headed mode is not supported with Lightpanda (headless only)".to_string());
    }
    if options.webgpu {
        return Err("WebGPU (--webgpu) is not supported with Lightpanda".to_string());
    }
    if !options.args.is_empty() {
        return Err(
            "Custom Chrome arguments (--args) are not supported with Lightpanda".to_string(),
        );
    }
    Ok(())
}

/// Returns true for Chrome internal targets that should not be selected
/// during auto-connect (e.g. chrome://, chrome-extension://, devtools://).
fn is_internal_chrome_target(url: &str) -> bool {
    url.starts_with("chrome://")
        || url.starts_with("chrome-extension://")
        || url.starts_with("devtools://")
}

pub(crate) fn should_track_target(target: &TargetInfo) -> bool {
    (target.target_type == "page" || target.target_type == "webview")
        && (target.url.is_empty() || !is_internal_chrome_target(&target.url))
}

fn update_page_target_info_in_pages(pages: &mut [PageInfo], target: &TargetInfo) -> bool {
    if let Some(page) = pages.iter_mut().find(|p| p.target_id == target.target_id) {
        page.url = target.url.clone();
        page.title = target.title.clone();
        page.target_type = target.target_type.clone();
        return true;
    }
    false
}

fn active_page_index_after_removal(
    active_page_index: usize,
    removed_index: usize,
    remaining_pages: usize,
) -> usize {
    if remaining_pages == 0 {
        return 0;
    }

    if removed_index < active_page_index {
        return active_page_index - 1;
    }

    if active_page_index >= remaining_pages {
        return remaining_pages - 1;
    }

    active_page_index
}

/// Decides the active page index after a page is appended.
///
/// A freshly appended page becomes active only when the caller explicitly
/// activates it (an explicit user command such as `tab new` / `window new`) or
/// when it is the first page (so a non-empty manager always has a valid active
/// index). Event-discovered / human-opened tabs are appended with
/// `activate = false` and must not steal the active pointer.
fn active_page_index_after_add(
    active_page_index: usize,
    new_index: usize,
    was_empty: bool,
    activate: bool,
) -> usize {
    if was_empty || activate {
        new_index
    } else {
        active_page_index
    }
}

/// Converts common error messages into AI-friendly, actionable descriptions.
pub fn to_ai_friendly_error(error: &str) -> String {
    let lower = error.to_lowercase();
    if lower.contains("strict mode violation") {
        return "Element matched multiple results. Use a more specific selector.".to_string();
    }
    if lower.contains("element is not visible") {
        return "Element exists but is not visible. Wait for it to become visible or scroll it into view."
            .to_string();
    }
    if lower.contains("intercept") {
        return "Another element is covering the target element. Try scrolling or closing overlays."
            .to_string();
    }
    if lower.contains("timeout") {
        return "Operation timed out. The page may still be loading or the element may not exist."
            .to_string();
    }
    if lower.contains("element not found") || lower.contains("no element") {
        return "Element not found. Verify the selector is correct and the element exists in the DOM."
            .to_string();
    }
    error.to_string()
}

#[derive(Debug, Clone)]
pub struct PageInfo {
    pub tab_id: u32,
    /// Optional user-assigned label (e.g. "docs", "app"). Set via
    /// `tab new --label <name>`. Labels are agent-assigned and never
    /// auto-generated, never rewritten on navigation, and unique within a
    /// session. Agents use labels instead of `t<N>` for readable multi-tab
    /// workflows.
    pub label: Option<String>,
    pub target_id: String,
    pub session_id: String,
    pub url: String,
    pub title: String,
    pub target_type: String, // "page" or "webview"
}

/// Canonical string form of a stable tab id: `t1`, `t2`, ... The `t` prefix
/// disambiguates stable ids from positional indices (which the CLI no longer
/// accepts) and matches the `@e<N>` convention used for element refs.
pub fn format_tab_id(tab_id: u32) -> String {
    format!("t{}", tab_id)
}

/// A tab reference as parsed from CLI/JSON input. Either a stable id like
/// `t2`, a user-assigned label like `docs`, or a CDP target id like
/// `4A0B...C3`. Target ids are stable across daemon restarts, unlike `t<N>`
/// ids which are per-daemon counters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabRef {
    Id(u32),
    Label(String),
    Target(String),
}

/// Heuristic for CDP target ids: long hex strings (Chrome uses 32 uppercase
/// hex chars). Only used for inputs that are not valid labels; label-shaped
/// hex strings resolve label-first with a target-id fallback.
fn looks_like_target_id(s: &str) -> bool {
    s.len() >= 16 && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl TabRef {
    /// Parse a user-supplied string tab reference. Rejects bare integers
    /// with a teaching error so agents and scripts don't silently confuse
    /// stable ids with positional indices.
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("Empty tab reference; expected `t<N>` (e.g. `t2`) or a label".to_string());
        }
        if let Some(digits) = input.strip_prefix('t').or_else(|| input.strip_prefix('T')) {
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let id: u32 = digits.parse().map_err(|_| {
                    format!(
                        "Tab id `{}` out of range; ids are incrementing positive integers",
                        input
                    )
                })?;
                if id == 0 {
                    return Err(format!(
                        "Tab id `{}` is invalid; tab ids start at t1",
                        input
                    ));
                }
                return Ok(TabRef::Id(id));
            }
        }
        if looks_like_target_id(input) && !is_valid_label(input) {
            return Ok(TabRef::Target(input.to_string()));
        }
        if input.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!(
                "Expected a tab id like `t{}` or a label; positional integers are not accepted \
                 (run `agent-browser tab` to list stable tab ids)",
                input
            ));
        }
        if !is_valid_label(input) {
            return Err(format!(
                "Invalid tab label `{}`; labels must start with a letter and contain only \
                 letters, digits, `-`, and `_`",
                input
            ));
        }
        Ok(TabRef::Label(input.to_string()))
    }
}

/// Labels must look like identifiers: start with a letter, contain only
/// letters/digits/dashes/underscores. This keeps them distinguishable from
/// `t<N>` ids at a glance and safe to pass through shells without quoting.
pub fn is_valid_label(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    Load,
    DomContentLoaded,
    NetworkIdle,
    None,
}

impl WaitUntil {
    pub fn from_str(s: &str) -> Self {
        match s {
            "domcontentloaded" => Self::DomContentLoaded,
            "networkidle" => Self::NetworkIdle,
            "none" => Self::None,
            _ => Self::Load,
        }
    }
}

pub enum BrowserProcess {
    Chrome(ChromeProcess),
    Lightpanda(LightpandaProcess),
}

impl BrowserProcess {
    pub fn kill(&mut self) {
        match self {
            BrowserProcess::Chrome(p) => p.kill(),
            BrowserProcess::Lightpanda(p) => p.kill(),
        }
    }

    pub fn wait_or_kill(&mut self, timeout: std::time::Duration) {
        match self {
            BrowserProcess::Chrome(p) => p.wait_or_kill(timeout),
            BrowserProcess::Lightpanda(p) => p.kill(),
        }
    }

    /// Non-blocking check whether the browser process has exited.
    pub fn has_exited(&mut self) -> bool {
        match self {
            BrowserProcess::Chrome(p) => p.has_exited(),
            BrowserProcess::Lightpanda(_) => false,
        }
    }
}

pub struct BrowserManager {
    pub client: Arc<CdpClient>,
    browser_process: Option<BrowserProcess>,
    ws_url: String,
    pages: Vec<PageInfo>,
    active_page_index: usize,
    default_timeout_ms: u64,
    /// Stored download path from launch options, re-applied to new contexts (e.g., recording)
    pub download_path: Option<String>,
    /// Whether to ignore HTTPS certificate errors, re-applied to new contexts (e.g., recording)
    pub ignore_https_errors: bool,
    /// Origins visited during this session, used by save_state to collect cross-origin localStorage.
    visited_origins: HashSet<String>,
    next_tab_id: u32,
    /// True when the CDP WebSocket is already scoped to a page target and
    /// browser-level Target.* commands are not available.
    direct_page: bool,
    /// Strict session-to-tab binding (`--pin-tab`). When enabled, the session
    /// never silently adopts another tab: if the bound tab goes away, page
    /// commands fail with a `tab_gone` error until the agent re-binds via
    /// `tab new` or `tab <ref>`.
    pin_tab: bool,
    /// CDP target id of the tab this session is bound to. Updated whenever
    /// the session creates a tab or explicitly switches tabs; persisted by
    /// the daemon so re-attach selects the same tab instead of index 0.
    bound_target_id: Option<String>,
    /// Set when `pin_tab` is enabled and the bound tab was destroyed
    /// (externally, or found missing at attach time): `(target_id, last_url)`.
    /// While set, commands that need the active page fail with `tab_gone`.
    bound_target_gone: Option<(String, String)>,
}

/// Stable machine-readable prefix for "the bound tab no longer exists"
/// errors, so scripts using `--json` can match on it.
pub const TAB_GONE_PREFIX: &str = "tab_gone:";

fn tab_gone_error(target_id: &str, last_url: &str) -> String {
    let url_part = if last_url.is_empty() {
        String::new()
    } else {
        format!(", last url {}", last_url)
    };
    format!(
        "{} bound tab is gone (target {}{}). Run `agent-browser tab new <url>` to bind a new \
         tab, or `agent-browser tab list` to pick an existing one",
        TAB_GONE_PREFIX, target_id, url_part
    )
}

const LIGHTPANDA_CDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LIGHTPANDA_TARGET_INIT_TIMEOUT: Duration = Duration::from_secs(10);

impl BrowserManager {
    pub async fn launch(options: LaunchOptions, engine: Option<&str>) -> Result<Self, String> {
        let engine = engine.unwrap_or("chrome");

        match engine {
            "chrome" => {
                validate_launch_options(
                    options.extensions.as_deref(),
                    false,
                    options.profile.as_deref(),
                    options.storage_state.as_deref(),
                    options.allow_file_access,
                    options.executable_path.as_deref(),
                )?;
            }
            "lightpanda" => {
                validate_lightpanda_options(&options)?;
            }
            _ => {
                return Err(format!(
                    "Unknown engine '{}'. Supported engines: chrome, lightpanda",
                    engine
                ));
            }
        }

        let ignore_https_errors = options.ignore_https_errors;
        let user_agent = options.user_agent.clone();
        let color_scheme = options.color_scheme.clone();
        let download_path = options.download_path.clone();

        let (ws_url, process) = match engine {
            "lightpanda" => {
                let lp_options = LightpandaLaunchOptions {
                    executable_path: options.executable_path.clone(),
                    proxy: options.proxy.clone(),
                    port: None,
                };
                let lp = launch_lightpanda(&lp_options).await?;
                let url = lp.ws_url.clone();
                (url, BrowserProcess::Lightpanda(lp))
            }
            _ => {
                let chrome = tokio::task::spawn_blocking(move || launch_chrome(&options))
                    .await
                    .map_err(|e| format!("Chrome launch task failed: {}", e))??;
                let url = chrome.ws_url.clone();
                (url, BrowserProcess::Chrome(chrome))
            }
        };

        let manager = if engine == "lightpanda" {
            initialize_lightpanda_manager(ws_url, process).await?
        } else {
            let client = Arc::new(CdpClient::connect(&ws_url).await?);
            let mut manager = Self {
                client,
                browser_process: Some(process),
                ws_url,
                pages: Vec::new(),
                active_page_index: 0,
                default_timeout_ms: 25_000,
                download_path: download_path.clone(),
                ignore_https_errors,
                visited_origins: HashSet::new(),
                next_tab_id: 1,
                direct_page: false,
                pin_tab: false,
                bound_target_id: None,
                bound_target_gone: None,
            };
            manager.discover_and_attach_targets().await?;
            manager
        };

        let session_id = manager.active_session_id()?.to_string();

        if ignore_https_errors {
            let _ = manager
                .client
                .send_command(
                    "Security.setIgnoreCertificateErrors",
                    Some(json!({ "ignore": true })),
                    Some(&session_id),
                )
                .await;
        }

        if let Some(ref ua) = user_agent {
            let _ = manager
                .client
                .send_command(
                    "Emulation.setUserAgentOverride",
                    Some(json!({ "userAgent": ua })),
                    Some(&session_id),
                )
                .await;
        }

        if let Some(ref scheme) = color_scheme {
            let _ = manager
                .client
                .send_command(
                    "Emulation.setEmulatedMedia",
                    Some(json!({ "features": [{ "name": "prefers-color-scheme", "value": scheme }] })),
                    Some(&session_id),
                )
                .await;
        }

        if let Some(ref path) = download_path {
            let _ = manager
                .client
                .send_command(
                    "Browser.setDownloadBehavior",
                    Some(json!({ "behavior": "allow", "downloadPath": path })),
                    None,
                )
                .await;
        }

        Ok(manager)
    }

    pub async fn connect_cdp(url: &str) -> Result<Self, String> {
        Self::connect_cdp_inner(url, false, None).await
    }

    /// Connect to a provider CDP proxy where the WebSocket IS the page session.
    /// Skips browser-level Target.* commands that most proxies don't support.
    pub async fn connect_cdp_direct(url: &str) -> Result<Self, String> {
        Self::connect_cdp_inner(url, true, None).await
    }

    pub async fn connect_cdp_with_headers(
        url: &str,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        Self::connect_cdp_inner(url, false, headers).await
    }

    async fn connect_cdp_inner(
        url: &str,
        direct_page: bool,
        headers: Option<Vec<(String, String)>>,
    ) -> Result<Self, String> {
        let ws_url = resolve_cdp_url(url).await?;
        let client = Arc::new(CdpClient::connect_with_headers(&ws_url, headers).await?);
        let mut manager = Self {
            client,
            browser_process: None,
            ws_url,
            pages: Vec::new(),
            active_page_index: 0,
            default_timeout_ms: 25_000,
            download_path: None,
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
            next_tab_id: 1,
            direct_page,
            pin_tab: false,
            bound_target_id: None,
            bound_target_gone: None,
        };

        if direct_page {
            let tab_id = manager.assign_tab_id();
            manager.pages.push(PageInfo {
                tab_id,
                label: None,
                target_id: "provider-page".to_string(),
                session_id: String::new(),
                url: String::new(),
                title: String::new(),
                target_type: "page".to_string(),
            });
            manager.active_page_index = 0;
            manager.enable_domains_direct().await?;
        } else {
            manager.discover_and_attach_targets().await?;
        }
        Ok(manager)
    }

    pub async fn connect_auto() -> Result<Self, String> {
        let ws_url = auto_connect_cdp().await?;
        Self::connect_cdp(&ws_url).await
    }

    async fn discover_and_attach_targets(&mut self) -> Result<(), String> {
        self.client
            .send_command_typed::<_, Value>(
                "Target.setDiscoverTargets",
                &SetDiscoverTargetsParams { discover: true },
                None,
            )
            .await?;

        let result: GetTargetsResult = self
            .client
            .send_command_typed("Target.getTargets", &json!({}), None)
            .await?;

        let page_targets: Vec<TargetInfo> = result
            .target_infos
            .into_iter()
            .filter(should_track_target)
            .collect();

        if page_targets.is_empty() {
            // Create a new tab
            let result: CreateTargetResult = self
                .client
                .send_command_typed(
                    "Target.createTarget",
                    &CreateTargetParams {
                        url: "about:blank".to_string(),
                    },
                    None,
                )
                .await?;

            let attach_result: AttachToTargetResult = self
                .client
                .send_command_typed(
                    "Target.attachToTarget",
                    &AttachToTargetParams {
                        target_id: result.target_id.clone(),
                        flatten: true,
                    },
                    None,
                )
                .await?;

            let tab_id = self.next_tab_id;
            self.next_tab_id += 1;
            self.pages.push(PageInfo {
                tab_id,
                label: None,
                target_id: result.target_id,
                session_id: attach_result.session_id.clone(),
                url: "about:blank".to_string(),
                title: String::new(),
                target_type: "page".to_string(),
            });
            self.active_page_index = 0;
            self.bind_active_target();
            self.enable_domains(&attach_result.session_id).await?;
        } else {
            for target in &page_targets {
                let attach_result: AttachToTargetResult = self
                    .client
                    .send_command_typed(
                        "Target.attachToTarget",
                        &AttachToTargetParams {
                            target_id: target.target_id.clone(),
                            flatten: true,
                        },
                        None,
                    )
                    .await?;

                let tab_id = self.next_tab_id;
                self.next_tab_id += 1;
                self.pages.push(PageInfo {
                    tab_id,
                    label: None,
                    target_id: target.target_id.clone(),
                    session_id: attach_result.session_id.clone(),
                    url: target.url.clone(),
                    title: target.title.clone(),
                    target_type: target.target_type.clone(),
                });
            }

            self.active_page_index = 0;
            let session_id = self.pages[0].session_id.clone();
            self.enable_domains(&session_id).await?;
        }

        Ok(())
    }

    pub async fn enable_domains_pub(&self, session_id: &str) -> Result<(), String> {
        self.enable_domains(session_id).await
    }

    pub async fn prepare_domains_pub(&self, session_id: &str) -> Result<(), String> {
        self.prepare_domains(session_id).await
    }

    pub async fn resume_if_waiting_pub(&self, session_id: &str) -> Result<(), String> {
        self.resume_if_waiting(session_id).await
    }

    pub async fn enable_browser_auto_attach_pub(&self) -> Result<(), String> {
        self.client
            .send_command(
                "Target.setAutoAttach",
                Some(json!({
                    "autoAttach": true,
                    "waitForDebuggerOnStart": true,
                    "flatten": true
                })),
                None,
            )
            .await?;
        Ok(())
    }

    async fn enable_domains(&self, session_id: &str) -> Result<(), String> {
        self.prepare_domains(session_id).await?;
        self.resume_if_waiting(session_id).await?;
        Ok(())
    }

    async fn prepare_domains(&self, session_id: &str) -> Result<(), String> {
        self.client
            .send_command_no_params("Page.enable", Some(session_id))
            .await?;
        self.client
            .send_command_no_params("Runtime.enable", Some(session_id))
            .await?;
        self.client
            .send_command_no_params("Network.enable", Some(session_id))
            .await?;
        // Enable auto-attach for cross-origin iframe support.
        // flatten: true gives each iframe its own session_id.
        // waitForDebuggerOnStart keeps child targets paused until the daemon
        // installs any required network controls and explicitly resumes them.
        // Ignored on engines that don't support it (e.g. Lightpanda).
        let _ = self
            .client
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

    async fn resume_if_waiting(&self, session_id: &str) -> Result<(), String> {
        // Needed for real browser sessions (Chrome 144+) where targets are
        // paused after attach until explicitly resumed. No-op otherwise.
        let _ = self
            .client
            .send_command_no_wait("Runtime.runIfWaitingForDebugger", None, Some(session_id))
            .await;
        Ok(())
    }

    /// Enable domains on a direct page connection (no session_id needed).
    async fn enable_domains_direct(&self) -> Result<(), String> {
        self.client
            .send_command_no_params("Page.enable", None)
            .await?;
        self.client
            .send_command_no_params("Runtime.enable", None)
            .await?;
        let _ = self
            .client
            .send_command_no_params("Runtime.runIfWaitingForDebugger", None)
            .await;
        self.client
            .send_command_no_params("Network.enable", None)
            .await?;
        Ok(())
    }

    pub fn active_session_id(&self) -> Result<&str, String> {
        self.check_bound()?;
        self.pages
            .get(self.active_page_index)
            .map(|p| p.session_id.as_str())
            .ok_or_else(|| "No active page".to_string())
    }

    // -----------------------------------------------------------------------
    // Session-to-tab binding
    // -----------------------------------------------------------------------

    /// Enable or disable strict pin-tab semantics for this session.
    /// Disabling also clears a pending `tab_gone` state (which only exists
    /// under pin semantics) so the session falls back to legacy selection
    /// instead of staying stuck on errors for a pin it no longer has.
    pub fn set_pin_tab(&mut self, pin: bool) {
        self.pin_tab = pin;
        if !pin {
            self.bound_target_gone = None;
        }
    }

    pub fn pin_tab(&self) -> bool {
        self.pin_tab
    }

    /// The target id this session is bound to, if any.
    pub fn bound_target_id(&self) -> Option<&str> {
        self.bound_target_id.as_deref()
    }

    /// Returns the `tab_gone` error when the bound tab no longer exists.
    /// Commands that operate on the active page call this (via
    /// `active_session_id` / `active_target_id`) so they fail loudly instead
    /// of acting on a neighboring tab. Recovery commands (`tab list`,
    /// `tab new`, `tab <ref>`) do not.
    fn check_bound(&self) -> Result<(), String> {
        match self.bound_target_gone {
            Some((ref target_id, ref last_url)) => Err(tab_gone_error(target_id, last_url)),
            None => Ok(()),
        }
    }

    /// True when the bound tab is gone and the session needs an explicit
    /// re-bind (`tab new` / `tab <ref>`) before page commands can proceed.
    pub fn bound_target_is_gone(&self) -> bool {
        self.bound_target_gone.is_some()
    }

    /// Bind this session to the currently active tab. Called whenever the
    /// session creates a tab or explicitly switches tabs; clears any
    /// `tab_gone` state.
    fn bind_active_target(&mut self) {
        self.bound_target_id = self
            .pages
            .get(self.active_page_index)
            .map(|p| p.target_id.clone());
        self.bound_target_gone = None;
    }

    /// Restore a persisted binding after (re)attach. If the bound target is
    /// still alive, make it the active page and return `true`. If it is
    /// gone, return `false`: with `pin_tab` the manager enters the `tab_gone`
    /// state, otherwise the legacy selection (index 0) is kept unchanged.
    pub fn restore_target_binding(&mut self, target_id: &str, last_url: &str) -> bool {
        if let Some(index) = self.pages.iter().position(|p| p.target_id == target_id) {
            self.active_page_index = index;
            self.bound_target_id = Some(target_id.to_string());
            self.bound_target_gone = None;
            return true;
        }
        self.bound_target_id = None;
        if self.pin_tab {
            self.bound_target_gone = Some((target_id.to_string(), last_url.to_string()));
        }
        false
    }

    /// React to the bound tab disappearing (closed externally, or closed by
    /// this session). With `pin_tab`, enter the `tab_gone` state so nothing
    /// silently retargets; otherwise just drop the stale binding.
    fn handle_bound_target_removed(&mut self, target_id: &str, last_url: &str) {
        if self.bound_target_id.as_deref() != Some(target_id) {
            return;
        }
        self.bound_target_id = None;
        if self.pin_tab {
            self.bound_target_gone = Some((target_id.to_string(), last_url.to_string()));
        }
    }

    pub async fn navigate(&mut self, url: &str, wait_until: WaitUntil) -> Result<Value, String> {
        let session_id = self.active_session_id()?.to_string();
        let mut lifecycle_rx = self.client.subscribe();

        let nav_result: PageNavigateResult = self
            .client
            .send_command_typed(
                "Page.navigate",
                &PageNavigateParams {
                    url: url.to_string(),
                    referrer: None,
                },
                Some(&session_id),
            )
            .await?;

        if let Some(ref error_text) = nav_result.error_text {
            return Err(format!("Navigation failed: {}", error_text));
        }

        // Only wait for lifecycle events if Chrome created a new loader (full navigation).
        // If loader_id is None, it was a same-document navigation (e.g., hash routing)
        // which does not fire Page.loadEventFired or Page.domContentEventFired.
        if nav_result.loader_id.is_some() && wait_until != WaitUntil::None {
            self.wait_for_lifecycle(wait_until, &session_id, &mut lifecycle_rx)
                .await?;
        }

        let page_url = self.get_url().await.unwrap_or_else(|_| url.to_string());
        let title = self.get_title().await.unwrap_or_default();

        // Track visited origin for cross-origin localStorage collection in save_state
        if let Ok(parsed) = url::Url::parse(&page_url) {
            let origin = parsed.origin().ascii_serialization();
            if origin != "null" {
                self.visited_origins.insert(origin);
            }
        }

        if let Some(page) = self.pages.get_mut(self.active_page_index) {
            page.url = page_url.clone();
            page.title = title.clone();
        }

        let target_id = self
            .pages
            .get(self.active_page_index)
            .map(|p| p.target_id.clone());
        Ok(json!({ "url": page_url, "title": title, "targetId": target_id }))
    }

    async fn wait_for_lifecycle(
        &self,
        wait_until: WaitUntil,
        session_id: &str,
        rx: &mut broadcast::Receiver<CdpEvent>,
    ) -> Result<(), String> {
        let event_name = match wait_until {
            WaitUntil::Load => "Page.loadEventFired",
            WaitUntil::DomContentLoaded => "Page.domContentEventFired",
            WaitUntil::NetworkIdle => return self.wait_for_network_idle(session_id, rx).await,
            WaitUntil::None => return Ok(()),
        };

        let timeout = tokio::time::Duration::from_millis(self.default_timeout_ms);

        tokio::time::timeout(timeout, async {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if event.method == event_name
                            && event.session_id.as_deref() == Some(session_id)
                        {
                            return Ok(());
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            Err("Event stream closed".to_string())
        })
        .await
        .map_err(|_| format!("Timeout waiting for {}", event_name))?
    }

    async fn wait_for_network_idle(
        &self,
        session_id: &str,
        rx: &mut broadcast::Receiver<CdpEvent>,
    ) -> Result<(), String> {
        let timeout = tokio::time::Duration::from_millis(self.default_timeout_ms);
        poll_network_idle(session_id, rx, timeout).await
    }

    pub async fn get_url(&self) -> Result<String, String> {
        let result = self.evaluate_simple("location.href").await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    pub async fn get_title(&self) -> Result<String, String> {
        let result = self.evaluate_simple("document.title").await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    pub async fn get_content(&self) -> Result<String, String> {
        let result = self
            .evaluate_simple("document.documentElement.outerHTML")
            .await?;
        Ok(result.as_str().unwrap_or("").to_string())
    }

    pub async fn evaluate(&self, script: &str, _args: Option<Value>) -> Result<Value, String> {
        let session_id = self.active_session_id()?.to_string();

        let result: EvaluateResult = self
            .client
            .send_command_typed(
                "Runtime.evaluate",
                &EvaluateParams {
                    expression: script.to_string(),
                    return_by_value: Some(true),
                    await_promise: Some(true),
                },
                Some(&session_id),
            )
            .await?;

        if let Some(ref details) = result.exception_details {
            let msg = details
                .exception
                .as_ref()
                .and_then(|e| e.description.as_deref())
                .unwrap_or(&details.text);
            return Err(format!("Evaluation error: {}", msg));
        }

        Ok(result.result.value.unwrap_or(Value::Null))
    }

    async fn evaluate_simple(&self, expression: &str) -> Result<Value, String> {
        self.evaluate(expression, None).await
    }

    pub async fn wait_for_lifecycle_external(
        &self,
        wait_until: WaitUntil,
        session_id: &str,
    ) -> Result<(), String> {
        // Subscribe before probing so a lifecycle event that fires between
        // the probe and the wait cannot be missed.
        let mut rx = self.client.subscribe();

        // `wait_for_lifecycle` waits for the NEXT lifecycle event, which is
        // right mid-navigation but wrong for a standalone `wait --load`: a
        // page that already finished loading (or navigated client-side,
        // which fires no new load event) never emits another one, so the
        // wait would burn its entire timeout. Resolve immediately when the
        // document is already in the requested state.
        let already_reached = match wait_until {
            WaitUntil::Load => Some("document.readyState === 'complete'"),
            // readyState leaves 'loading' when DOMContentLoaded fires.
            WaitUntil::DomContentLoaded => Some("document.readyState !== 'loading'"),
            // Network idle is tracked from live network events; its poller
            // already treats a quiet stream as idle.
            WaitUntil::NetworkIdle | WaitUntil::None => None,
        };
        if let Some(expression) = already_reached {
            let probe: Result<EvaluateResult, String> = self
                .client
                .send_command_typed(
                    "Runtime.evaluate",
                    &EvaluateParams {
                        expression: expression.to_string(),
                        return_by_value: Some(true),
                        await_promise: Some(false),
                    },
                    Some(session_id),
                )
                .await;
            // A failed probe is not fatal; fall back to waiting for the event.
            if let Ok(result) = probe {
                if result.result.value.as_ref().and_then(|v| v.as_bool()) == Some(true) {
                    return Ok(());
                }
            }
        }

        self.wait_for_lifecycle(wait_until, session_id, &mut rx)
            .await
    }

    pub async fn close(&mut self) -> Result<(), String> {
        if self.browser_process.is_some() {
            // Only send Browser.close when we launched the browser ourselves.
            // For external connections (--auto-connect, --cdp) we just disconnect
            // without shutting down the user's browser.
            let _ = self
                .client
                .send_command_no_params("Browser.close", None)
                .await;
        }

        if let Some(mut process) = self.browser_process.take() {
            let timeout = std::time::Duration::from_secs(5);
            let _ = tokio::task::spawn_blocking(move || {
                process.wait_or_kill(timeout);
            })
            .await;
        }

        Ok(())
    }

    pub fn has_pages(&self) -> bool {
        !self.pages.is_empty()
    }

    pub fn default_timeout_ms(&self) -> u64 {
        self.default_timeout_ms
    }

    /// Checks if the CDP connection is alive by sending a simple command.
    /// Returns false if the command times out or fails.
    pub async fn is_connection_alive(&self) -> bool {
        let timeout = tokio::time::Duration::from_secs(3);
        let result = tokio::time::timeout(
            timeout,
            self.client
                .send_command_no_params("Browser.getVersion", None),
        )
        .await;

        match result {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        }
    }

    /// Non-blocking check whether the locally-launched browser process has exited
    /// (crashed or terminated). Also reaps the zombie if it has exited.
    /// Returns false for external CDP connections (no child process to monitor).
    pub fn has_process_exited(&mut self) -> bool {
        if let Some(ref mut process) = self.browser_process {
            process.has_exited()
        } else {
            false
        }
    }

    pub fn get_cdp_url(&self) -> &str {
        &self.ws_url
    }

    /// Returns the Chrome debug server address as "host:port".
    pub fn chrome_host_port(&self) -> &str {
        let stripped = self
            .ws_url
            .strip_prefix("ws://")
            .or_else(|| self.ws_url.strip_prefix("wss://"))
            .unwrap_or(&self.ws_url);
        stripped.split('/').next().unwrap_or(stripped)
    }

    pub fn active_target_id(&self) -> Result<&str, String> {
        self.check_bound()?;
        self.pages
            .get(self.active_page_index)
            .map(|p| p.target_id.as_str())
            .ok_or_else(|| "No active page".to_string())
    }

    /// Returns true if this manager was connected via CDP (as opposed to local launch).
    pub fn is_cdp_connection(&self) -> bool {
        self.browser_process.is_none()
    }

    pub fn is_direct_page_connection(&self) -> bool {
        self.direct_page
    }

    /// Ensures the browser has at least one page. If `pages` is empty, creates a new
    /// about:blank page and attaches to it.
    pub async fn ensure_page(&mut self) -> Result<(), String> {
        if !self.pages.is_empty() {
            return Ok(());
        }

        let result: CreateTargetResult = self
            .client
            .send_command_typed(
                "Target.createTarget",
                &CreateTargetParams {
                    url: "about:blank".to_string(),
                },
                None,
            )
            .await?;

        let attach_result: AttachToTargetResult = self
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: result.target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await?;

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        self.pages.push(PageInfo {
            tab_id,
            label: None,
            target_id: result.target_id,
            session_id: attach_result.session_id.clone(),
            url: "about:blank".to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        });
        self.active_page_index = 0;
        self.bind_active_target();
        self.enable_domains(&attach_result.session_id).await?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Tab management
    // -----------------------------------------------------------------------

    /// Checks if `active_page_index` is still valid and adjusts it if not
    /// (e.g., after a tab was closed).
    pub fn update_active_page_if_needed(&mut self) {
        if self.pages.is_empty() {
            self.active_page_index = 0;
            return;
        }
        if self.active_page_index >= self.pages.len() {
            self.active_page_index = self.pages.len() - 1;
        }
    }

    fn update_active_page_after_removal(&mut self, removed_index: usize) {
        self.active_page_index = active_page_index_after_removal(
            self.active_page_index,
            removed_index,
            self.pages.len(),
        );
    }

    pub fn tab_list(&self) -> Vec<Value> {
        self.pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!({
                    "tabId": format_tab_id(p.tab_id),
                    "targetId": p.target_id,
                    "label": p.label,
                    "title": p.title,
                    "url": p.url,
                    "type": p.target_type,
                    "active": i == self.active_page_index && !self.bound_target_is_gone(),
                })
            })
            .collect()
    }

    /// Resolve a user-supplied `TabRef` (either `t<N>` or a label) to the
    /// stable numeric `tab_id`. Returns a teaching error for unknown tabs.
    pub fn resolve_tab_ref(&self, tab_ref: &TabRef) -> Result<u32, String> {
        match tab_ref {
            TabRef::Id(id) => {
                if self.has_tab_id(*id) {
                    Ok(*id)
                } else {
                    Err(format!(
                        "Tab {} not found; run `agent-browser tab` to list open tabs",
                        format_tab_id(*id)
                    ))
                }
            }
            TabRef::Label(name) => self
                .pages
                .iter()
                .find(|p| p.label.as_deref() == Some(name.as_str()))
                .map(|p| p.tab_id)
                // A label-shaped hex string may actually be a CDP target id
                // (target ids can start with a letter); fall back to an
                // exact target-id match before giving up.
                .or_else(|| {
                    if looks_like_target_id(name) {
                        self.find_tab_id_by_target(name)
                    } else {
                        None
                    }
                })
                .ok_or_else(|| {
                    format!(
                        "No tab with label `{}`; run `agent-browser tab` to list open tabs",
                        name
                    )
                }),
            TabRef::Target(target_id) => self.find_tab_id_by_target(target_id).ok_or_else(|| {
                format!(
                    "No tab with target id `{}`; run `agent-browser tab list --json` to \
                         list open tabs with their target ids",
                    target_id
                )
            }),
        }
    }

    /// Exact, case-insensitive match of a CDP target id to a stable tab id.
    fn find_tab_id_by_target(&self, target_id: &str) -> Option<u32> {
        self.pages
            .iter()
            .find(|p| p.target_id.eq_ignore_ascii_case(target_id))
            .map(|p| p.tab_id)
    }

    /// Returns true iff a tab already carries the given label.
    pub fn has_label(&self, label: &str) -> bool {
        self.pages.iter().any(|p| p.label.as_deref() == Some(label))
    }

    pub async fn tab_new(
        &mut self,
        url: Option<&str>,
        label: Option<&str>,
    ) -> Result<Value, String> {
        if let Some(label) = label {
            if !is_valid_label(label) {
                return Err(format!(
                    "Invalid tab label `{}`; labels must start with a letter and contain only \
                     letters, digits, `-`, and `_`",
                    label
                ));
            }
            if self.has_label(label) {
                return Err(format!(
                    "Label `{}` is already used by another tab; labels must be unique within a \
                     session",
                    label
                ));
            }
        }

        let target_url = url.unwrap_or("about:blank");

        let result: CreateTargetResult = self
            .client
            .send_command_typed(
                "Target.createTarget",
                &CreateTargetParams {
                    url: target_url.to_string(),
                },
                None,
            )
            .await?;

        let attach: AttachToTargetResult = self
            .client
            .send_command_typed(
                "Target.attachToTarget",
                &AttachToTargetParams {
                    target_id: result.target_id.clone(),
                    flatten: true,
                },
                None,
            )
            .await?;

        self.enable_domains(&attach.session_id).await?;

        let tab_id = self.next_tab_id;
        self.next_tab_id += 1;
        let index = self.pages.len();
        let label = label.map(|s| s.to_string());
        let target_id = result.target_id.clone();
        self.pages.push(PageInfo {
            tab_id,
            label: label.clone(),
            target_id: result.target_id,
            session_id: attach.session_id,
            url: target_url.to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        });
        self.active_page_index = index;
        self.bind_active_target();

        Ok(json!({
            "tabId": format_tab_id(tab_id),
            "targetId": target_id,
            "label": label,
            "url": target_url,
            "total": self.pages.len(),
        }))
    }

    pub async fn tab_switch(&mut self, index: usize) -> Result<Value, String> {
        if index >= self.pages.len() {
            return Err(format!(
                "Tab index {} out of range (0-{})",
                index,
                self.pages.len().saturating_sub(1)
            ));
        }

        self.active_page_index = index;
        self.bind_active_target();
        let session_id = self.pages[index].session_id.clone();
        self.enable_domains(&session_id).await?;

        // Bring tab to front
        let _ = self
            .client
            .send_command("Page.bringToFront", None, Some(&session_id))
            .await;

        let url = self.get_url().await.unwrap_or_default();
        let title = self.get_title().await.unwrap_or_default();

        if let Some(page) = self.pages.get_mut(index) {
            page.url = url.clone();
            page.title = title.clone();
        }

        let page = &self.pages[index];
        Ok(json!({
            "tabId": format_tab_id(page.tab_id),
            "targetId": page.target_id,
            "label": page.label,
            "url": url,
            "title": title,
        }))
    }

    pub async fn tab_close(&mut self, index: Option<usize>) -> Result<Value, String> {
        if index.is_none() {
            // "Close the current tab" must not silently close a fallback tab
            // when the bound tab is already gone.
            self.check_bound()?;
        }
        let target_index = index.unwrap_or(self.active_page_index);

        if target_index >= self.pages.len() {
            return Err(format!("Tab index {} out of range", target_index));
        }

        if self.pages.len() <= 1 {
            return Err("Cannot close the last tab".to_string());
        }

        let page = self.pages.remove(target_index);
        self.update_active_page_after_removal(target_index);
        let closed_tab_id = page.tab_id;
        let closed_label = page.label.clone();
        let closed_target_id = page.target_id.clone();
        self.handle_bound_target_removed(&page.target_id, &page.url);
        let _ = self
            .client
            .send_command_typed::<_, Value>(
                "Target.closeTarget",
                &CloseTargetParams {
                    target_id: page.target_id,
                },
                None,
            )
            .await;

        // With pin-tab, closing the bound tab leaves the session unbound
        // (page commands return `tab_gone` until an explicit re-bind), so
        // don't touch the neighboring tab that inherited the active slot.
        if !self.bound_target_is_gone() {
            let session_id = self.pages[self.active_page_index].session_id.clone();
            self.enable_domains(&session_id).await?;
        }

        Ok(json!({
            "tabId": format_tab_id(closed_tab_id),
            "targetId": closed_target_id,
            "label": closed_label,
            "closed": true,
        }))
    }

    // -----------------------------------------------------------------------
    // Emulation
    // -----------------------------------------------------------------------

    pub async fn set_viewport(
        &self,
        width: i32,
        height: i32,
        device_scale_factor: f64,
        mobile: bool,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setDeviceMetricsOverride",
                Some(json!({
                    "width": width,
                    "height": height,
                    "deviceScaleFactor": device_scale_factor,
                    "mobile": mobile,
                })),
                Some(session_id),
            )
            .await?;

        // Screencast captures the actual content area, not the emulated CSS
        // viewport, so resize the content area to match.
        if let Ok(target_id) = self.active_target_id() {
            if let Ok(window_info) = self
                .client
                .send_command(
                    "Browser.getWindowForTarget",
                    Some(json!({ "targetId": target_id })),
                    None,
                )
                .await
            {
                if let Some(window_id) = window_info.get("windowId").and_then(|v| v.as_i64()) {
                    if let Err(e) = self
                        .client
                        .send_command(
                            "Browser.setContentsSize",
                            Some(json!({
                                "windowId": window_id,
                                "width": width,
                                "height": height,
                            })),
                            None,
                        )
                        .await
                    {
                        eprintln!("Browser.setContentsSize failed (experimental CDP): {e}");
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn set_user_agent(&self, user_agent: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setUserAgentOverride",
                Some(json!({ "userAgent": user_agent })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn set_emulated_media(
        &self,
        media: Option<&str>,
        features: Option<Vec<(String, String)>>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        let mut params = json!({});
        if let Some(m) = media {
            params["media"] = Value::String(m.to_string());
        }
        if let Some(feats) = features {
            let features_arr: Vec<Value> = feats
                .iter()
                .map(|(name, value)| json!({ "name": name, "value": value }))
                .collect();
            params["features"] = Value::Array(features_arr);
        }
        self.client
            .send_command("Emulation.setEmulatedMedia", Some(params), Some(session_id))
            .await?;
        Ok(())
    }

    pub async fn bring_to_front(&self) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command("Page.bringToFront", None, Some(session_id))
            .await?;
        Ok(())
    }

    pub async fn set_timezone(&self, timezone_id: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setTimezoneOverride",
                Some(json!({ "timezoneId": timezone_id })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn set_locale(&self, locale: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setLocaleOverride",
                Some(json!({ "locale": locale })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn set_geolocation(
        &self,
        latitude: f64,
        longitude: f64,
        accuracy: Option<f64>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Emulation.setGeolocationOverride",
                Some(json!({
                    "latitude": latitude,
                    "longitude": longitude,
                    "accuracy": accuracy.unwrap_or(1.0),
                })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn grant_permissions(&self, permissions: &[String]) -> Result<(), String> {
        self.client
            .send_command(
                "Browser.grantPermissions",
                Some(json!({ "permissions": permissions })),
                None,
            )
            .await?;
        Ok(())
    }

    pub async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        let mut params = json!({ "accept": accept });
        if let Some(text) = prompt_text {
            params["promptText"] = Value::String(text.to_string());
        }
        self.client
            .send_command(
                "Page.handleJavaScriptDialog",
                Some(params),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn upload_files(
        &self,
        selector: &str,
        files: &[String],
        ref_map: &RefMap,
        iframe_sessions: &HashMap<String, String>,
    ) -> Result<(), String> {
        let session_id = self.active_session_id()?;

        let (object_id, effective_session_id) =
            resolve_element_object_id(&self.client, session_id, ref_map, selector, iframe_sessions)
                .await?;

        let describe: Value = self
            .client
            .send_command(
                "DOM.describeNode",
                Some(json!({ "objectId": object_id })),
                Some(&effective_session_id),
            )
            .await?;

        let backend_node_id = describe
            .get("node")
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64())
            .ok_or("Could not get backendNodeId for file input")?;

        self.client
            .send_command(
                "DOM.setFileInputFiles",
                Some(json!({
                    "files": files,
                    "backendNodeId": backend_node_id,
                })),
                Some(&effective_session_id),
            )
            .await?;

        Ok(())
    }

    pub async fn add_script_to_evaluate(&self, source: &str) -> Result<String, String> {
        let session_id = self.active_session_id()?;
        let result = self
            .client
            .send_command(
                "Page.addScriptToEvaluateOnNewDocument",
                Some(json!({ "source": source })),
                Some(session_id),
            )
            .await?;
        Ok(result
            .get("identifier")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn remove_script_to_evaluate(&self, identifier: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Page.removeScriptToEvaluateOnNewDocument",
                Some(json!({ "identifier": identifier })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }

    pub async fn tab_switch_by_id(&mut self, tab_id: u32) -> Result<Value, String> {
        let index = self
            .pages
            .iter()
            .position(|p| p.tab_id == tab_id)
            .ok_or_else(|| format!("Tab ID {} not found", tab_id))?;
        self.tab_switch(index).await
    }

    pub async fn tab_close_by_id(&mut self, tab_id: Option<u32>) -> Result<Value, String> {
        let index = match tab_id {
            Some(id) => Some(
                self.pages
                    .iter()
                    .position(|p| p.tab_id == id)
                    .ok_or_else(|| format!("Tab ID {} not found", id))?,
            ),
            None => None,
        };
        self.tab_close(index).await
    }

    pub fn assign_tab_id(&mut self) -> u32 {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    /// Append a page and make it active (and bound). Use for explicit user
    /// commands (`window new`, recording setup) where the new page should
    /// become active.
    pub fn add_page(&mut self, page: PageInfo) {
        self.add_page_with_activation(page, true);
    }

    /// Append a page WITHOUT activating it. Use for event-discovered targets
    /// (`Target.targetCreated` drained from the shared Chrome) so a tab the
    /// human opens never steals the agent's active tab or its binding.
    pub fn add_page_without_activation(&mut self, page: PageInfo) {
        self.add_page_with_activation(page, false);
    }

    fn add_page_with_activation(&mut self, page: PageInfo, activate: bool) {
        let was_empty = self.pages.is_empty();
        let new_index = self.pages.len();
        self.pages.push(page);
        self.active_page_index =
            active_page_index_after_add(self.active_page_index, new_index, was_empty, activate);
        // Only pages that actually became active update the session binding;
        // background-registered pages must never overwrite it.
        if self.active_page_index == new_index {
            self.bind_active_target();
        }
    }

    /// The active page's `(target_id, url)` for binding persistence, or
    /// `None` when there is no page or the bound tab is gone (a stale
    /// binding must not be overwritten by the fallback tab).
    pub fn binding_snapshot(&self) -> Option<(String, String)> {
        if self.bound_target_is_gone() {
            return None;
        }
        self.pages
            .get(self.active_page_index)
            .map(|p| (p.target_id.clone(), p.url.clone()))
    }

    pub fn update_page_target_info(&mut self, target: &TargetInfo) -> bool {
        update_page_target_info_in_pages(&mut self.pages, target)
    }

    pub fn remove_page_by_target_id(&mut self, target_id: &str) {
        if let Some(pos) = self.pages.iter().position(|p| p.target_id == target_id) {
            let page = self.pages.remove(pos);
            self.update_active_page_after_removal(pos);
            // If the destroyed target was the bound tab (closed externally,
            // e.g. by another session sharing this browser), fail loudly
            // under pin-tab instead of silently pointing at a neighbor.
            self.handle_bound_target_removed(&page.target_id, &page.url);
        }
    }

    pub fn has_target(&self, target_id: &str) -> bool {
        self.pages.iter().any(|p| p.target_id == target_id)
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Returns the stable `tab_id` of the currently active page, if any.
    pub fn active_tab_id(&self) -> Option<u32> {
        self.pages.get(self.active_page_index).map(|p| p.tab_id)
    }

    /// Returns true if a tab with the given stable `tab_id` is still open.
    pub fn has_tab_id(&self, tab_id: u32) -> bool {
        self.pages.iter().any(|p| p.tab_id == tab_id)
    }

    pub fn pages_list(&self) -> Vec<PageInfo> {
        self.pages.clone()
    }

    pub fn visited_origins(&self) -> &HashSet<String> {
        &self.visited_origins
    }

    pub async fn set_download_behavior(&self, download_path: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        self.client
            .send_command(
                "Browser.setDownloadBehavior",
                Some(json!({
                    "behavior": "allowAndName",
                    "downloadPath": download_path,
                    "eventsEnabled": true,
                })),
                Some(session_id),
            )
            .await?;
        Ok(())
    }
}

/// Core network-idle polling loop, extracted so it can be unit-tested without a
/// full `BrowserManager` / CDP connection.
///
/// Returns `Ok(())` once no network requests have been in-flight for at least
/// 500 ms, or `Err` if `overall_timeout` elapses first.
async fn poll_network_idle(
    session_id: &str,
    rx: &mut broadcast::Receiver<CdpEvent>,
    overall_timeout: tokio::time::Duration,
) -> Result<(), String> {
    let pending = Arc::new(Mutex::new(HashSet::<String>::new()));

    tokio::time::timeout(overall_timeout, async {
        let mut idle_start: Option<tokio::time::Instant> = None;

        loop {
            let recv_result =
                tokio::time::timeout(tokio::time::Duration::from_millis(600), rx.recv()).await;

            match recv_result {
                Ok(Ok(event)) if event.session_id.as_deref() == Some(session_id) => {
                    let mut p = pending.lock().await;
                    match event.method.as_str() {
                        "Network.requestWillBeSent" => {
                            if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str())
                            {
                                p.insert(id.to_string());
                                idle_start = None;
                            }
                        }
                        "Network.loadingFinished" | "Network.loadingFailed" => {
                            if let Some(id) = event.params.get("requestId").and_then(|v| v.as_str())
                            {
                                p.remove(id);
                                if p.is_empty() {
                                    idle_start = Some(tokio::time::Instant::now());
                                }
                            }
                        }
                        "Page.loadEventFired" if p.is_empty() => {
                            idle_start = Some(tokio::time::Instant::now());
                        }
                        _ => {}
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(_)) => break,
                Err(_) => {
                    // Timeout on recv -- if no pending requests, start (or
                    // continue) the idle timer instead of returning
                    // immediately.  This prevents false-positive idle
                    // detection when the subscription starts after the page
                    // has already loaded (e.g. cached pages).
                    let p = pending.lock().await;
                    if p.is_empty() && idle_start.is_none() {
                        idle_start = Some(tokio::time::Instant::now());
                    }
                }
            }

            if let Some(start) = idle_start {
                if start.elapsed() >= tokio::time::Duration::from_millis(500) {
                    return Ok(());
                }
            }
        }

        Ok(())
    })
    .await
    .map_err(|_| "Timeout waiting for networkidle".to_string())?
}

async fn connect_cdp_with_retry(
    ws_url: &str,
    total_timeout: Duration,
    poll_interval: Duration,
) -> Result<CdpClient, String> {
    let deadline = Instant::now() + total_timeout;

    loop {
        match CdpClient::connect(ws_url).await {
            Ok(client) => return Ok(client),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

async fn initialize_lightpanda_manager(
    ws_url: String,
    process: BrowserProcess,
) -> Result<BrowserManager, String> {
    let deadline = Instant::now() + LIGHTPANDA_TARGET_INIT_TIMEOUT;
    let mut process = Some(process);

    loop {
        let client = match connect_cdp_with_retry(
            &ws_url,
            LIGHTPANDA_CDP_CONNECT_TIMEOUT,
            LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL,
        )
        .await
        {
            Ok(client) => client,
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(lightpanda_target_init_timeout(Some(&err)));
                }
                tokio::time::sleep(LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL).await;
                continue;
            }
        };

        let mut manager = BrowserManager {
            client: Arc::new(client),
            browser_process: None,
            ws_url: ws_url.clone(),
            pages: Vec::new(),
            active_page_index: 0,
            default_timeout_ms: 25_000,
            download_path: None,
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
            next_tab_id: 1,
            direct_page: false,
            pin_tab: false,
            bound_target_id: None,
            bound_target_gone: None,
        };

        match discover_and_attach_lightpanda_targets(&mut manager, deadline).await {
            Ok(()) => {
                manager.browser_process = process.take();
                return Ok(manager);
            }
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(lightpanda_target_init_timeout(Some(&err)));
                }
                tokio::time::sleep(LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL).await;
            }
        }
    }
}

async fn discover_and_attach_lightpanda_targets(
    manager: &mut BrowserManager,
    deadline: Instant,
) -> Result<(), String> {
    run_with_lightpanda_deadline(
        deadline,
        manager.discover_and_attach_targets(),
        "Target domain initialization attempt exceeded the remaining startup deadline",
    )
    .await
}

fn remaining_until(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now())
}

async fn run_with_lightpanda_deadline<F, T>(
    deadline: Instant,
    operation: F,
    timeout_context: &'static str,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    let remaining = remaining_until(deadline)
        .ok_or_else(|| lightpanda_target_init_timeout(Some("deadline expired before retry")))?;

    match tokio::time::timeout(remaining, operation).await {
        Ok(result) => result,
        Err(_) => Err(lightpanda_target_init_timeout(Some(timeout_context))),
    }
}

fn lightpanda_target_init_timeout(last_error: Option<&str>) -> String {
    let mut message = format!(
        "Timed out after {}ms waiting for Lightpanda Target domain to initialize",
        LIGHTPANDA_TARGET_INIT_TIMEOUT.as_millis(),
    );
    if let Some(last_error) = last_error {
        message.push_str(&format!("\nLast error: {}", last_error));
    }
    message
}

async fn resolve_cdp_url(input: &str) -> Result<String, String> {
    if input.starts_with("ws://") || input.starts_with("wss://") {
        return Ok(input.to_string());
    }

    if input.starts_with("http://") || input.starts_with("https://") {
        let parsed = url::Url::parse(input).map_err(|e| format!("Invalid CDP URL: {}", e))?;
        // If no explicit port and path is empty/root, this is likely a provider
        // WebSocket endpoint (e.g. https://xxx.cdp0.browser-use.com). Convert
        // the scheme to ws/wss and connect directly instead of probing :9222.
        if parsed.port().is_none() && (parsed.path().is_empty() || parsed.path() == "/") {
            let ws_scheme = if input.starts_with("https://") {
                "wss"
            } else {
                "ws"
            };
            let mut ws_url = parsed.clone();
            let _ = ws_url.set_scheme(ws_scheme);
            return Ok(ws_url.to_string());
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| format!("No host in CDP URL: {}", input))?;
        let port = parsed.port().unwrap_or(9222);
        let query = parsed.query().map(|q| q.to_string());
        return discover_cdp_url(host, port, query.as_deref()).await;
    }

    // Try as numeric port
    if let Ok(port) = input.parse::<u16>() {
        return discover_cdp_url("127.0.0.1", port, None).await;
    }

    Err(format!(
        "Invalid CDP target: {}. Use ws://, http://, or a port number.",
        input
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[test]
    fn test_format_tab_id() {
        assert_eq!(format_tab_id(1), "t1");
        assert_eq!(format_tab_id(42), "t42");
    }

    #[test]
    fn test_parse_tab_ref_id() {
        assert_eq!(TabRef::parse("t1"), Ok(TabRef::Id(1)));
        assert_eq!(TabRef::parse("t42"), Ok(TabRef::Id(42)));
        assert_eq!(TabRef::parse("T7"), Ok(TabRef::Id(7)));
    }

    #[test]
    fn test_parse_tab_ref_label() {
        assert_eq!(TabRef::parse("docs"), Ok(TabRef::Label("docs".to_string())));
        assert_eq!(
            TabRef::parse("app-2"),
            Ok(TabRef::Label("app-2".to_string()))
        );
        assert_eq!(
            TabRef::parse("my_tab"),
            Ok(TabRef::Label("my_tab".to_string()))
        );
    }

    #[test]
    fn test_parse_tab_ref_rejects_bare_integer() {
        let err = TabRef::parse("2").unwrap_err();
        assert!(
            err.contains("positional integers are not accepted"),
            "error should teach the user to use `t<N>`: {}",
            err
        );
        assert!(err.contains("t2"));
    }

    #[test]
    fn test_parse_tab_ref_rejects_empty() {
        assert!(TabRef::parse("").is_err());
        assert!(TabRef::parse("   ").is_err());
    }

    #[test]
    fn test_parse_tab_ref_rejects_zero() {
        let err = TabRef::parse("t0").unwrap_err();
        assert!(err.contains("start at t1"));
    }

    #[test]
    fn test_parse_tab_ref_rejects_invalid_label() {
        assert!(TabRef::parse("2docs").is_err());
        assert!(TabRef::parse("-docs").is_err());
        assert!(TabRef::parse("docs!").is_err());
        assert!(TabRef::parse("docs space").is_err());
    }

    #[test]
    fn test_is_valid_label() {
        assert!(is_valid_label("docs"));
        assert!(is_valid_label("Docs"));
        assert!(is_valid_label("app-2"));
        assert!(is_valid_label("my_tab"));
        assert!(!is_valid_label(""));
        assert!(!is_valid_label("2docs"));
        assert!(!is_valid_label("-docs"));
        assert!(!is_valid_label("docs!"));
    }

    #[test]
    fn test_should_track_popup_target_with_empty_url() {
        let target = TargetInfo {
            target_id: "popup-1".to_string(),
            target_type: "page".to_string(),
            title: String::new(),
            url: String::new(),
            attached: None,
            browser_context_id: None,
        };

        assert!(should_track_target(&target));
    }

    #[test]
    fn test_should_not_track_internal_chrome_target() {
        let target = TargetInfo {
            target_id: "chrome-tab".to_string(),
            target_type: "page".to_string(),
            title: "New Tab".to_string(),
            url: "chrome://newtab/".to_string(),
            attached: None,
            browser_context_id: None,
        };

        assert!(!should_track_target(&target));
    }

    #[test]
    fn test_update_page_target_info_in_pages_updates_existing_page() {
        let mut pages = vec![PageInfo {
            tab_id: 1,
            label: None,
            target_id: "popup-1".to_string(),
            session_id: "session-1".to_string(),
            url: String::new(),
            title: String::new(),
            target_type: "page".to_string(),
        }];
        let target = TargetInfo {
            target_id: "popup-1".to_string(),
            target_type: "page".to_string(),
            title: "Popup".to_string(),
            url: "https://example.com/popup".to_string(),
            attached: None,
            browser_context_id: None,
        };

        assert!(update_page_target_info_in_pages(&mut pages, &target));
        assert_eq!(pages[0].url, "https://example.com/popup");
        assert_eq!(pages[0].title, "Popup");
    }

    #[test]
    fn test_active_page_index_after_removal_shifts_when_earlier_tab_is_removed() {
        assert_eq!(active_page_index_after_removal(2, 0, 3), 1);
    }

    #[test]
    fn test_active_page_index_after_removal_keeps_same_slot_when_later_tab_is_removed() {
        assert_eq!(active_page_index_after_removal(1, 2, 3), 1);
    }

    #[test]
    fn test_active_page_index_after_removal_clamps_when_active_last_tab_is_removed() {
        assert_eq!(active_page_index_after_removal(3, 3, 3), 2);
    }

    #[test]
    fn test_active_page_index_after_removal_resets_when_last_page_disappears() {
        assert_eq!(active_page_index_after_removal(0, 0, 0), 0);
    }

    #[test]
    fn test_active_page_index_after_add_first_page_always_activates() {
        // The first page must become active even without explicit activation,
        // so a manager that has pages always has a valid active index.
        assert_eq!(active_page_index_after_add(0, 0, true, false), 0);
        assert_eq!(active_page_index_after_add(0, 0, true, true), 0);
    }

    #[test]
    fn test_active_page_index_after_add_discovered_tab_does_not_steal_active() {
        // Event-discovered / human-opened tabs are appended without activation and
        // must NOT move the active pointer (the active-tab steal fix).
        assert_eq!(active_page_index_after_add(2, 5, false, false), 2);
    }

    #[test]
    fn test_active_page_index_after_add_explicit_activation_moves_active() {
        // Explicit user commands (`tab new`, `window new`) activate the new page.
        assert_eq!(active_page_index_after_add(2, 5, false, true), 5);
    }

    #[test]
    fn test_validate_launch_options_extensions_and_cdp() {
        let ext = vec!["/path/to/ext".to_string()];
        assert!(validate_launch_options(Some(&ext), true, None, None, false, None,).is_err());
    }

    #[test]
    fn test_validate_launch_options_profile_and_cdp() {
        assert!(validate_launch_options(None, true, Some("/path"), None, false, None,).is_err());
    }

    #[test]
    fn test_validate_launch_options_storage_state_and_profile() {
        assert!(validate_launch_options(
            None,
            false,
            Some("/profile"),
            Some("/state.json"),
            false,
            None,
        )
        .is_err());
    }

    #[test]
    fn test_validate_launch_options_storage_state_and_extensions() {
        let ext = vec!["/ext".to_string()];
        assert!(
            validate_launch_options(Some(&ext), false, None, Some("/state.json"), false, None,)
                .is_err()
        );
    }

    #[test]
    fn test_validate_launch_options_allow_file_access_firefox() {
        assert!(
            validate_launch_options(None, false, None, None, true, Some("/usr/bin/firefox"),)
                .is_err()
        );
    }

    #[test]
    fn test_validate_launch_options_valid() {
        assert!(validate_launch_options(None, false, None, None, false, None,).is_ok());
    }

    #[test]
    fn test_to_ai_friendly_error_strict_mode() {
        assert_eq!(
            to_ai_friendly_error("Strict mode violation: multiple elements"),
            "Element matched multiple results. Use a more specific selector."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_not_visible() {
        assert_eq!(
            to_ai_friendly_error("element is not visible"),
            "Element exists but is not visible. Wait for it to become visible or scroll it into view."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_intercept() {
        assert_eq!(
            to_ai_friendly_error("element intercepted by another element"),
            "Another element is covering the target element. Try scrolling or closing overlays."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_timeout() {
        assert_eq!(
            to_ai_friendly_error("Timeout waiting for element"),
            "Operation timed out. The page may still be loading or the element may not exist."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_not_found() {
        assert_eq!(
            to_ai_friendly_error("Element not found"),
            "Element not found. Verify the selector is correct and the element exists in the DOM."
        );
    }

    #[test]
    fn test_to_ai_friendly_error_unknown() {
        let msg = "Some custom error message";
        assert_eq!(to_ai_friendly_error(msg), msg);
    }

    /// Errors containing "not found" but NOT "element" should pass through unchanged.
    #[test]
    fn test_to_ai_friendly_error_ignores_non_element_not_found() {
        let err = "Chrome not found. Install Chrome or use --executable-path.";
        assert_eq!(to_ai_friendly_error(err), err);
    }

    #[test]
    fn test_to_ai_friendly_error_catches_no_element() {
        let mapped =
            "Element not found. Verify the selector is correct and the element exists in the DOM.";
        assert_eq!(to_ai_friendly_error("No element found for css 'x'"), mapped);
    }

    #[test]
    fn test_remaining_until_returns_none_for_past_deadline() {
        let deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("past instant should be representable");
        assert!(remaining_until(deadline).is_none());
    }

    #[tokio::test]
    async fn test_run_with_lightpanda_deadline_enforces_timeout() {
        let deadline = Instant::now() + Duration::from_millis(25);
        let err = tokio::time::timeout(
            Duration::from_secs(1),
            run_with_lightpanda_deadline(
                deadline,
                async {
                    sleep(Duration::from_millis(100)).await;
                    Ok::<(), String>(())
                },
                "Target domain initialization attempt exceeded the remaining startup deadline",
            ),
        )
        .await
        .expect("outer timeout should not fire")
        .unwrap_err();

        assert!(err.contains(
            "Timed out after 10000ms waiting for Lightpanda Target domain to initialize"
        ));
        assert!(err.contains("remaining startup deadline"));
    }

    #[tokio::test]
    async fn test_run_with_lightpanda_deadline_returns_operation_error() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let err = run_with_lightpanda_deadline(
            deadline,
            async { Err::<(), String>("Target.getTargets failed".to_string()) },
            "unused timeout context",
        )
        .await
        .unwrap_err();

        assert_eq!(err, "Target.getTargets failed");
    }

    #[test]
    fn test_lightpanda_target_init_timeout_includes_last_error() {
        let err = lightpanda_target_init_timeout(Some("Target.setDiscoverTargets failed"));
        assert!(err.contains(
            "Timed out after 10000ms waiting for Lightpanda Target domain to initialize"
        ));
        assert!(err.contains("Target.setDiscoverTargets failed"));
    }

    #[test]
    fn test_validate_lightpanda_rejects_webgpu() {
        let options = LaunchOptions {
            webgpu: true,
            ..Default::default()
        };
        let err = validate_lightpanda_options(&options).unwrap_err();
        assert!(err.contains("WebGPU"));
        assert!(validate_lightpanda_options(&LaunchOptions::default()).is_ok());
    }

    #[test]
    fn test_is_internal_chrome_target() {
        assert!(is_internal_chrome_target("chrome://newtab/"));
        assert!(is_internal_chrome_target(
            "chrome://omnibox-popup.top-chrome/"
        ));
        assert!(is_internal_chrome_target(
            "chrome-extension://abc123/popup.html"
        ));
        assert!(is_internal_chrome_target(
            "devtools://devtools/bundled/inspector.html"
        ));
        assert!(!is_internal_chrome_target("https://example.com"));
        assert!(!is_internal_chrome_target("http://localhost:3000"));
        assert!(!is_internal_chrome_target("about:blank"));
    }

    // -----------------------------------------------------------------------
    // poll_network_idle tests
    // -----------------------------------------------------------------------

    fn cdp_event(method: &str, session_id: &str, params: Value) -> CdpEvent {
        CdpEvent {
            method: method.to_string(),
            params,
            session_id: Some(session_id.to_string()),
        }
    }

    /// Regression test for #846: when no network events arrive at all (e.g.
    /// page fully served from cache), poll_network_idle must NOT return
    /// instantly.  It should observe at least 500 ms of idle before resolving.
    #[tokio::test]
    async fn test_network_idle_no_events_does_not_return_instantly() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            poll_network_idle(session, &mut rx, Duration::from_secs(5)),
        )
        .await
        .expect("outer timeout should not fire");

        assert!(result.is_ok());
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500),
            "network idle returned in {:?}, expected >= 500ms",
            elapsed
        );

        drop(tx);
    }

    /// Normal flow: requests start and finish, idle is detected after the last
    /// request completes and 500 ms of silence passes.
    #[tokio::test]
    async fn test_network_idle_after_requests_complete() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        let _keep_alive = tx.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            let _ = tx.send(cdp_event(
                "Network.requestWillBeSent",
                session,
                json!({ "requestId": "r1" }),
            ));
            sleep(Duration::from_millis(100)).await;
            let _ = tx.send(cdp_event(
                "Network.loadingFinished",
                session,
                json!({ "requestId": "r1" }),
            ));
        });

        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            poll_network_idle(session, &mut rx, Duration::from_secs(5)),
        )
        .await
        .expect("outer timeout should not fire");

        assert!(result.is_ok());
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(500),
            "should wait >= 500ms after last request finishes, got {:?}",
            elapsed
        );
    }

    /// A new request arriving during the idle window resets the timer.
    #[tokio::test]
    async fn test_network_idle_resets_on_new_request() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        let _keep_alive = tx.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(50)).await;
            let _ = tx.send(cdp_event(
                "Network.requestWillBeSent",
                session,
                json!({ "requestId": "r1" }),
            ));
            sleep(Duration::from_millis(50)).await;
            let _ = tx.send(cdp_event(
                "Network.loadingFinished",
                session,
                json!({ "requestId": "r1" }),
            ));
            // Wait 200ms (< 500ms idle window), then fire another request
            sleep(Duration::from_millis(200)).await;
            let _ = tx.send(cdp_event(
                "Network.requestWillBeSent",
                session,
                json!({ "requestId": "r2" }),
            ));
            sleep(Duration::from_millis(100)).await;
            let _ = tx.send(cdp_event(
                "Network.loadingFinished",
                session,
                json!({ "requestId": "r2" }),
            ));
        });

        let start = tokio::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            poll_network_idle(session, &mut rx, Duration::from_secs(5)),
        )
        .await
        .expect("outer timeout should not fire");

        assert!(result.is_ok());
        let elapsed = start.elapsed();
        // r2 finishes at ~400ms; idle should be detected at ~900ms
        assert!(
            elapsed >= Duration::from_millis(800),
            "should wait for idle after second request, got {:?}",
            elapsed
        );
    }

    // -----------------------------------------------------------------------
    // Session-to-tab binding tests
    // -----------------------------------------------------------------------

    fn page(tab_id: u32, target_id: &str, url: &str) -> PageInfo {
        PageInfo {
            tab_id,
            label: None,
            target_id: target_id.to_string(),
            session_id: format!("session-{}", tab_id),
            url: url.to_string(),
            title: String::new(),
            target_type: "page".to_string(),
        }
    }

    /// Build a `BrowserManager` backed by a dummy WebSocket server that
    /// accepts the connection and then stays silent. Enough for the binding
    /// logic, which never awaits a CDP response in these tests.
    async fn test_manager(pages: Vec<PageInfo>) -> BrowserManager {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                if let Ok(_ws) = tokio_tungstenite::accept_async(stream).await {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        });
        let client = CdpClient::connect(&format!("ws://{}", addr)).await.unwrap();
        BrowserManager {
            client: Arc::new(client),
            browser_process: None,
            ws_url: format!("ws://{}", addr),
            pages,
            active_page_index: 0,
            default_timeout_ms: 25_000,
            download_path: None,
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
            next_tab_id: 100,
            direct_page: false,
            pin_tab: false,
            bound_target_id: None,
            bound_target_gone: None,
        }
    }

    const TARGET_A: &str = "AAAA0000BBBB1111CCCC2222DDDD3333";
    const TARGET_B: &str = "4F0A1111BBBB2222CCCC3333DDDD4444";

    #[tokio::test]
    async fn test_restore_target_binding_selects_bound_target() {
        let mut mgr = test_manager(vec![
            page(1, TARGET_B, "https://other.example"),
            page(2, TARGET_A, "https://mine.example"),
        ])
        .await;

        assert!(mgr.restore_target_binding(TARGET_A, "https://mine.example"));
        assert_eq!(mgr.active_target_id().unwrap(), TARGET_A);
        assert_eq!(mgr.bound_target_id(), Some(TARGET_A));
        assert!(!mgr.bound_target_is_gone());
    }

    #[tokio::test]
    async fn test_restore_target_binding_missing_with_pin_enters_tab_gone() {
        let mut mgr = test_manager(vec![page(1, TARGET_B, "https://other.example")]).await;
        mgr.set_pin_tab(true);

        assert!(!mgr.restore_target_binding(TARGET_A, "https://mine.example/checkout"));
        assert!(mgr.bound_target_is_gone());

        let err = mgr.active_session_id().unwrap_err();
        assert!(
            err.starts_with(TAB_GONE_PREFIX),
            "unexpected error: {}",
            err
        );
        assert!(err.contains(TARGET_A));
        assert!(err.contains("https://mine.example/checkout"));
        assert!(err.contains("tab new"));

        // active_target_id is guarded the same way
        assert!(mgr
            .active_target_id()
            .unwrap_err()
            .starts_with(TAB_GONE_PREFIX));
        // No tab is reported active while the binding is unresolved.
        assert!(mgr.tab_list().iter().all(|t| t["active"] == false));
    }

    #[tokio::test]
    async fn test_restore_target_binding_missing_without_pin_keeps_legacy_selection() {
        let mut mgr = test_manager(vec![page(1, TARGET_B, "https://other.example")]).await;

        assert!(!mgr.restore_target_binding(TARGET_A, "https://mine.example"));
        assert!(!mgr.bound_target_is_gone());
        // Legacy behavior: first target stays selected and commands work.
        assert_eq!(mgr.active_target_id().unwrap(), TARGET_B);
    }

    #[tokio::test]
    async fn test_external_removal_of_bound_tab_with_pin_enters_tab_gone() {
        let mut mgr = test_manager(vec![
            page(1, TARGET_A, "https://mine.example"),
            page(2, TARGET_B, "https://other.example"),
        ])
        .await;
        mgr.set_pin_tab(true);
        assert!(mgr.restore_target_binding(TARGET_A, "https://mine.example"));

        mgr.remove_page_by_target_id(TARGET_A);

        assert!(mgr.bound_target_is_gone());
        let err = mgr.active_session_id().unwrap_err();
        assert!(err.starts_with(TAB_GONE_PREFIX));
        // Recovery data is intact: the other tab is still listed.
        assert_eq!(mgr.tab_list().len(), 1);
    }

    #[tokio::test]
    async fn test_external_removal_of_bound_tab_without_pin_falls_back() {
        let mut mgr = test_manager(vec![
            page(1, TARGET_A, "https://mine.example"),
            page(2, TARGET_B, "https://other.example"),
        ])
        .await;
        assert!(mgr.restore_target_binding(TARGET_A, "https://mine.example"));

        mgr.remove_page_by_target_id(TARGET_A);

        assert!(!mgr.bound_target_is_gone());
        assert_eq!(mgr.bound_target_id(), None);
        assert_eq!(mgr.active_target_id().unwrap(), TARGET_B);
    }

    #[tokio::test]
    async fn test_removal_of_other_tab_does_not_affect_binding() {
        let mut mgr = test_manager(vec![
            page(1, TARGET_B, "https://other.example"),
            page(2, TARGET_A, "https://mine.example"),
        ])
        .await;
        mgr.set_pin_tab(true);
        assert!(mgr.restore_target_binding(TARGET_A, "https://mine.example"));

        mgr.remove_page_by_target_id(TARGET_B);

        assert!(!mgr.bound_target_is_gone());
        assert_eq!(mgr.active_target_id().unwrap(), TARGET_A);
        assert_eq!(mgr.bound_target_id(), Some(TARGET_A));
    }

    #[tokio::test]
    async fn test_tab_close_current_in_tab_gone_state_errors() {
        let mut mgr = test_manager(vec![
            page(1, TARGET_A, "https://mine.example"),
            page(2, TARGET_B, "https://other.example"),
        ])
        .await;
        mgr.set_pin_tab(true);
        assert!(mgr.restore_target_binding(TARGET_A, "https://mine.example"));
        mgr.remove_page_by_target_id(TARGET_A);

        // "Close the current tab" must not close the fallback neighbor.
        let err = mgr.tab_close(None).await.unwrap_err();
        assert!(err.starts_with(TAB_GONE_PREFIX));
        assert_eq!(mgr.tab_list().len(), 1);
    }

    #[tokio::test]
    async fn test_binding_snapshot_reflects_active_page_and_gone_state() {
        let mut mgr = test_manager(vec![
            page(1, TARGET_A, "https://mine.example"),
            page(2, TARGET_B, "https://other.example"),
        ])
        .await;
        mgr.set_pin_tab(true);
        assert!(mgr.restore_target_binding(TARGET_A, "https://mine.example"));
        assert_eq!(
            mgr.binding_snapshot(),
            Some((TARGET_A.to_string(), "https://mine.example".to_string()))
        );

        mgr.remove_page_by_target_id(TARGET_A);
        // A stale binding must not be overwritten by the fallback tab.
        assert_eq!(mgr.binding_snapshot(), None);
    }

    #[tokio::test]
    async fn test_add_page_without_activation_does_not_steal_active_slot_or_binding() {
        let mut mgr = test_manager(vec![page(1, TARGET_A, "https://mine.example")]).await;
        mgr.set_pin_tab(true);
        assert!(mgr.restore_target_binding(TARGET_A, "https://mine.example"));

        mgr.add_page_without_activation(page(2, TARGET_B, "https://other.example"));

        assert_eq!(mgr.active_target_id().unwrap(), TARGET_A);
        assert_eq!(mgr.bound_target_id(), Some(TARGET_A));
        assert_eq!(mgr.tab_list().len(), 2);
    }

    #[tokio::test]
    async fn test_add_page_activates_and_binds() {
        let mut mgr = test_manager(vec![page(1, TARGET_A, "https://mine.example")]).await;
        mgr.add_page(page(2, TARGET_B, "https://new.example"));

        assert_eq!(mgr.active_target_id().unwrap(), TARGET_B);
        assert_eq!(mgr.bound_target_id(), Some(TARGET_B));
    }

    #[tokio::test]
    async fn test_resolve_tab_ref_by_target_id() {
        let mgr = test_manager(vec![
            page(1, TARGET_A, "https://mine.example"),
            page(2, TARGET_B, "https://other.example"),
        ])
        .await;

        // Digit-leading target ids parse as TabRef::Target.
        let tab_ref = TabRef::parse(TARGET_B).unwrap();
        assert_eq!(tab_ref, TabRef::Target(TARGET_B.to_string()));
        assert_eq!(mgr.resolve_tab_ref(&tab_ref).unwrap(), 2);

        // Letter-leading target ids parse as labels and fall back to a
        // target-id match.
        let tab_ref = TabRef::parse(TARGET_A).unwrap();
        assert_eq!(mgr.resolve_tab_ref(&tab_ref).unwrap(), 1);

        // Case-insensitive.
        let lower = TARGET_B.to_lowercase();
        assert_eq!(
            mgr.resolve_tab_ref(&TabRef::parse(&lower).unwrap())
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn test_resolve_tab_ref_unknown_target_id_errors() {
        let mgr = test_manager(vec![page(1, TARGET_A, "https://mine.example")]).await;
        let err = mgr
            .resolve_tab_ref(&TabRef::Target("0123456789ABCDEF".to_string()))
            .unwrap_err();
        assert!(err.contains("target id"));
    }

    #[tokio::test]
    async fn test_tab_list_includes_target_id() {
        let mgr = test_manager(vec![page(1, TARGET_A, "https://mine.example")]).await;
        let tabs = mgr.tab_list();
        assert_eq!(tabs[0]["targetId"], TARGET_A);
    }

    #[test]
    fn test_parse_tab_ref_short_hex_is_label_not_target() {
        // Short hex strings stay labels (or errors), not target ids.
        assert_eq!(
            TabRef::parse("deadbeef"),
            Ok(TabRef::Label("deadbeef".to_string()))
        );
        assert!(TabRef::parse("1234").is_err());
    }

    #[test]
    fn test_tab_gone_error_without_url_omits_url_part() {
        let err = tab_gone_error("ABCD", "");
        assert!(err.starts_with(TAB_GONE_PREFIX));
        assert!(err.contains("(target ABCD)"));
        assert!(!err.contains("last url"));
    }

    /// When the overall timeout expires before idle is reached, the function
    /// returns an error.
    #[tokio::test]
    async fn test_network_idle_overall_timeout() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);
        let session = "s1";

        // Keep sending requests so idle is never reached
        tokio::spawn(async move {
            for i in 0u64.. {
                let _ = tx.send(cdp_event(
                    "Network.requestWillBeSent",
                    session,
                    json!({ "requestId": format!("r{}", i) }),
                ));
                sleep(Duration::from_millis(100)).await;
            }
        });

        let result = poll_network_idle(session, &mut rx, Duration::from_millis(800)).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("Timeout waiting for networkidle"));
    }
}
