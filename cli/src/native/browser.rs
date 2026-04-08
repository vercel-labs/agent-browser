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
        page.browser_context_id = target.browser_context_id.clone();
        return true;
    }
    false
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
    pub target_id: String,
    pub session_id: String,
    pub url: String,
    pub title: String,
    pub target_type: String, // "page" or "webview"
    pub browser_context_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitUntil {
    Load,
    DomContentLoaded,
    NetworkIdle,
    None,
}

pub enum NavigationOutcome {
    Page(Value),
    Download(Value),
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
    /// Whether the agent owns the default browser context and may safely mutate
    /// its browser-wide download behavior.
    manages_default_download_context: bool,
    /// Browser contexts created and owned by the agent. These are safe to
    /// update with browser-wide download behavior.
    managed_browser_context_ids: HashSet<String>,
    /// Whether to ignore HTTPS certificate errors, re-applied to new contexts (e.g., recording)
    pub ignore_https_errors: bool,
    /// Origins visited during this session, used by save_state to collect cross-origin localStorage.
    visited_origins: HashSet<String>,
}

const LIGHTPANDA_CDP_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const LIGHTPANDA_CDP_CONNECT_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LIGHTPANDA_TARGET_INIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Default)]
struct DownloadState {
    guid: Option<String>,
    suggested_filename: Option<String>,
    /// The configured download directory; used to construct the file path for
    /// Page-domain download events which do not include `filePath`.
    download_path: Option<String>,
}

enum BufferedDownload {
    InProgress(DownloadState),
    Completed(Value),
}

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
            initialize_lightpanda_manager(ws_url, process, download_path.clone()).await?
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
                manages_default_download_context: true,
                managed_browser_context_ids: HashSet::new(),
                ignore_https_errors,
                visited_origins: HashSet::new(),
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

        Ok(manager)
    }

    pub async fn connect_cdp(url: &str, download_path: Option<String>) -> Result<Self, String> {
        Self::connect_cdp_inner(url, false, None, download_path).await
    }

    /// Connect to a provider CDP proxy where the WebSocket IS the page session.
    /// Skips browser-level Target.* commands that most proxies don't support.
    pub async fn connect_cdp_direct(
        url: &str,
        download_path: Option<String>,
    ) -> Result<Self, String> {
        Self::connect_cdp_inner(url, true, None, download_path).await
    }

    pub async fn connect_cdp_with_headers(
        url: &str,
        headers: Option<Vec<(String, String)>>,
        download_path: Option<String>,
    ) -> Result<Self, String> {
        Self::connect_cdp_inner(url, false, headers, download_path).await
    }

    async fn connect_cdp_inner(
        url: &str,
        direct_page: bool,
        headers: Option<Vec<(String, String)>>,
        download_path: Option<String>,
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
            download_path: download_path.clone(),
            manages_default_download_context: false,
            managed_browser_context_ids: HashSet::new(),
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
        };

        if direct_page {
            manager.pages.push(PageInfo {
                target_id: "provider-page".to_string(),
                session_id: String::new(),
                url: String::new(),
                title: String::new(),
                target_type: "page".to_string(),
                browser_context_id: None,
            });
            manager.active_page_index = 0;
            manager.enable_domains_direct().await?;
        } else {
            manager.discover_and_attach_targets().await?;
        }

        Ok(manager)
    }

    pub async fn connect_auto(download_path: Option<String>) -> Result<Self, String> {
        let ws_url = auto_connect_cdp().await?;
        Self::connect_cdp(&ws_url, download_path).await
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

            self.pages.push(PageInfo {
                target_id: result.target_id,
                session_id: attach_result.session_id.clone(),
                url: "about:blank".to_string(),
                title: String::new(),
                target_type: "page".to_string(),
                browser_context_id: None,
            });
            self.active_page_index = 0;
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

                self.pages.push(PageInfo {
                    target_id: target.target_id.clone(),
                    session_id: attach_result.session_id.clone(),
                    url: target.url.clone(),
                    title: target.title.clone(),
                    target_type: target.target_type.clone(),
                    browser_context_id: target.browser_context_id.clone(),
                });
            }

            self.active_page_index = 0;
            let session_id = self.pages[0].session_id.clone();
            self.enable_domains(&session_id).await?;
        }

        Ok(())
    }

    pub async fn enable_domains_pub(
        &self,
        session_id: &str,
        browser_context_id: Option<&str>,
    ) -> Result<(), String> {
        self.enable_domains_with_context(session_id, browser_context_id)
            .await
    }

    async fn enable_domains(&self, session_id: &str) -> Result<(), String> {
        let browser_context_id = self.browser_context_id_for_session(session_id);
        self.enable_domains_with_context(session_id, browser_context_id)
            .await
    }

    async fn enable_domains_with_context(
        &self,
        session_id: &str,
        browser_context_id: Option<&str>,
    ) -> Result<(), String> {
        self.client
            .send_command_no_params("Page.enable", Some(session_id))
            .await?;
        self.client
            .send_command_no_params("Runtime.enable", Some(session_id))
            .await?;
        // Resume the target if it is paused waiting for the debugger.
        // This is needed for real browser sessions (Chrome 144+) where targets
        // are paused after attach until explicitly resumed. No-op otherwise.
        let _ = self
            .client
            .send_command_no_params("Runtime.runIfWaitingForDebugger", Some(session_id))
            .await;
        self.client
            .send_command_no_params("Network.enable", Some(session_id))
            .await?;
        // Enable auto-attach for cross-origin iframe support.
        // flatten: true gives each iframe its own session_id.
        // Ignored on engines that don't support it (e.g. Lightpanda).
        let _ = self
            .client
            .send_command(
                "Target.setAutoAttach",
                Some(json!({
                    "autoAttach": true,
                    "waitForDebuggerOnStart": false,
                    "flatten": true
                })),
                Some(session_id),
            )
            .await;
        if self.download_path.is_some() {
            self.apply_page_download_behavior(Some(session_id)).await;
            if self.should_apply_browser_download_behavior(browser_context_id) {
                self.apply_browser_download_behavior(browser_context_id)
                    .await;
            }
        }
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
        if self.download_path.is_some() {
            self.apply_page_download_behavior(None).await;
            if self.should_apply_browser_download_behavior(None) {
                self.apply_browser_download_behavior(None).await;
            }
        }
        Ok(())
    }

    pub fn active_session_id(&self) -> Result<&str, String> {
        self.pages
            .get(self.active_page_index)
            .map(|p| p.session_id.as_str())
            .ok_or_else(|| "No active page".to_string())
    }

    pub async fn navigate(
        &mut self,
        url: &str,
        wait_until: WaitUntil,
    ) -> Result<NavigationOutcome, String> {
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

        if navigation_has_explicit_download(&nav_result) {
            if let Some(ref dl_path) = self.download_path {
                let download_result = self
                    .wait_for_download(&session_id, &nav_result.frame_id, &mut lifecycle_rx)
                    .await;
                return match download_result {
                    Ok(info) => Ok(NavigationOutcome::Download(info)),
                    Err(e) => Err(format!(
                        "Navigation failed: {} (download_path={}, download_error={})",
                        nav_result.error_text.as_deref().unwrap_or("download"),
                        dl_path,
                        e
                    )),
                };
            }
            return Err("Navigation aborted: the URL triggered a file download. \
                 Use --download-path <dir> to save the file."
                .to_string());
        }

        if nav_result.error_text.as_deref() == Some("net::ERR_ABORTED") {
            if let Some(ref dl_path) = self.download_path {
                return match self
                    .buffered_download_after_abort(
                        &session_id,
                        &nav_result.frame_id,
                        &mut lifecycle_rx,
                    )
                    .await
                {
                    Ok(Some(info)) => Ok(NavigationOutcome::Download(info)),
                    Ok(None) => Err("Navigation failed: net::ERR_ABORTED".to_string()),
                    Err(e) => Err(format!(
                        "Navigation failed: {} (download_path={}, download_error={})",
                        "net::ERR_ABORTED", dl_path, e
                    )),
                };
            }
            return Err("Navigation failed: net::ERR_ABORTED".to_string());
        }

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

        Ok(NavigationOutcome::Page(
            json!({ "url": page_url, "title": title }),
        ))
    }

    /// Wait for a CDP download to complete after a navigation triggered a file
    /// download (net::ERR_ABORTED).  Returns a JSON value with the download
    /// path and suggested filename on success.
    async fn wait_for_download(
        &self,
        session_id: &str,
        frame_id: &str,
        rx: &mut broadcast::Receiver<CdpEvent>,
    ) -> Result<Value, String> {
        wait_for_download_completion(
            session_id,
            frame_id,
            rx,
            tokio::time::Duration::from_millis(self.default_timeout_ms),
            DownloadState {
                download_path: self.download_path.clone(),
                ..Default::default()
            },
        )
        .await
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
        let mut rx = self.client.subscribe();
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
        self.pages
            .get(self.active_page_index)
            .map(|p| p.target_id.as_str())
            .ok_or_else(|| "No active page".to_string())
    }

    /// Returns true if this manager was connected via CDP (as opposed to local launch).
    pub fn is_cdp_connection(&self) -> bool {
        self.browser_process.is_none()
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

        self.pages.push(PageInfo {
            target_id: result.target_id,
            session_id: attach_result.session_id.clone(),
            url: "about:blank".to_string(),
            title: String::new(),
            target_type: "page".to_string(),
            browser_context_id: None,
        });
        self.active_page_index = 0;
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

    pub fn tab_list(&self) -> Vec<Value> {
        self.pages
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!({
                    "index": i,
                    "title": p.title,
                    "url": p.url,
                    "type": p.target_type,
                    "active": i == self.active_page_index,
                })
            })
            .collect()
    }

    pub async fn tab_new(&mut self, url: Option<&str>) -> Result<Value, String> {
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

        let index = self.pages.len();
        self.pages.push(PageInfo {
            target_id: result.target_id,
            session_id: attach.session_id,
            url: target_url.to_string(),
            title: String::new(),
            target_type: "page".to_string(),
            browser_context_id: None,
        });
        self.active_page_index = index;

        Ok(json!({ "index": index, "url": target_url }))
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

        Ok(json!({ "index": index, "url": url, "title": title }))
    }

    pub async fn tab_close(&mut self, index: Option<usize>) -> Result<Value, String> {
        let target_index = index.unwrap_or(self.active_page_index);

        if target_index >= self.pages.len() {
            return Err(format!("Tab index {} out of range", target_index));
        }

        if self.pages.len() <= 1 {
            return Err("Cannot close the last tab".to_string());
        }

        let page = self.pages.remove(target_index);
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

        if self.active_page_index >= self.pages.len() {
            self.active_page_index = self.pages.len() - 1;
        }

        let session_id = self.pages[self.active_page_index].session_id.clone();
        self.enable_domains(&session_id).await?;

        Ok(json!({ "closed": target_index, "activeIndex": self.active_page_index }))
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

    pub fn add_page(&mut self, page: PageInfo) {
        let index = self.pages.len();
        self.pages.push(page);
        self.active_page_index = index;
    }

    pub fn update_page_target_info(&mut self, target: &TargetInfo) -> bool {
        update_page_target_info_in_pages(&mut self.pages, target)
    }

    pub fn remove_page_by_target_id(&mut self, target_id: &str) {
        if let Some(pos) = self.pages.iter().position(|p| p.target_id == target_id) {
            self.pages.remove(pos);
            self.update_active_page_if_needed();
        }
    }

    pub fn has_target(&self, target_id: &str) -> bool {
        self.pages.iter().any(|p| p.target_id == target_id)
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn pages_list(&self) -> Vec<PageInfo> {
        self.pages.clone()
    }

    pub fn visited_origins(&self) -> &HashSet<String> {
        &self.visited_origins
    }

    pub fn register_managed_browser_context(&mut self, browser_context_id: impl Into<String>) {
        self.managed_browser_context_ids
            .insert(browser_context_id.into());
    }

    pub async fn update_download_path(
        &mut self,
        download_path: Option<String>,
    ) -> Result<(), String> {
        self.download_path = download_path;

        let (has_direct_page, session_ids) = collect_page_download_behavior_targets(&self.pages);

        if has_direct_page {
            self.apply_page_download_behavior(None).await;
        }
        for session_id in session_ids {
            self.apply_page_download_behavior(Some(&session_id)).await;
        }

        if self.manages_default_download_context {
            self.apply_browser_download_behavior(None).await;
        }
        let mut browser_context_ids: Vec<_> =
            self.managed_browser_context_ids.iter().cloned().collect();
        browser_context_ids.sort();
        for browser_context_id in browser_context_ids {
            self.apply_browser_download_behavior(Some(&browser_context_id))
                .await;
        }
        Ok(())
    }

    fn browser_context_id_for_session(&self, session_id: &str) -> Option<&str> {
        self.pages
            .iter()
            .find(|page| page.session_id == session_id)
            .and_then(|page| page.browser_context_id.as_deref())
    }

    pub async fn set_download_behavior(&self, download_path: &str) -> Result<(), String> {
        let session_id = self.active_session_id()?;
        let browser_context_id = self.browser_context_id_for_session(session_id);
        self.client
            .send_command(
                "Browser.setDownloadBehavior",
                Some(browser_download_behavior_params(
                    Some(download_path),
                    browser_context_id,
                    "allowAndName",
                )),
                None,
            )
            .await?;
        Ok(())
    }

    fn should_apply_browser_download_behavior(&self, browser_context_id: Option<&str>) -> bool {
        should_apply_browser_download_behavior(
            browser_context_id,
            self.manages_default_download_context,
            &self.managed_browser_context_ids,
        )
    }

    async fn apply_page_download_behavior(&self, session_id: Option<&str>) {
        let _ = self
            .client
            .send_command(
                "Page.setDownloadBehavior",
                Some(page_download_behavior_params(self.download_path.as_deref())),
                session_id,
            )
            .await;
    }

    async fn apply_browser_download_behavior(&self, browser_context_id: Option<&str>) {
        let _ = self
            .client
            .send_command(
                "Browser.setDownloadBehavior",
                Some(browser_download_behavior_params(
                    self.download_path.as_deref(),
                    browser_context_id,
                    "allow",
                )),
                None,
            )
            .await;
    }

    async fn buffered_download_after_abort(
        &self,
        session_id: &str,
        frame_id: &str,
        rx: &mut broadcast::Receiver<CdpEvent>,
    ) -> Result<Option<Value>, String> {
        match drain_buffered_download_events(session_id, frame_id, rx, &self.download_path)? {
            Some(BufferedDownload::Completed(info)) => Ok(Some(info)),
            Some(BufferedDownload::InProgress(state)) => self
                .wait_for_download_from_state(session_id, frame_id, rx, state)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn wait_for_download_from_state(
        &self,
        session_id: &str,
        frame_id: &str,
        rx: &mut broadcast::Receiver<CdpEvent>,
        state: DownloadState,
    ) -> Result<Value, String> {
        wait_for_download_completion(
            session_id,
            frame_id,
            rx,
            tokio::time::Duration::from_millis(self.default_timeout_ms),
            state,
        )
        .await
    }
}

fn navigation_has_explicit_download(result: &PageNavigateResult) -> bool {
    result.is_download == Some(true)
}

fn page_download_behavior_params(download_path: Option<&str>) -> Value {
    match download_path {
        Some(download_path) => json!({
            "behavior": "allow",
            "downloadPath": download_path,
        }),
        None => json!({
            "behavior": "default",
        }),
    }
}

fn browser_download_behavior_params(
    download_path: Option<&str>,
    browser_context_id: Option<&str>,
    allow_behavior: &str,
) -> Value {
    let mut params = match download_path {
        Some(download_path) => json!({
            "behavior": allow_behavior,
            "downloadPath": download_path,
            "eventsEnabled": true,
        }),
        None => json!({
            "behavior": "default",
            "eventsEnabled": false,
        }),
    };
    if let Some(browser_context_id) = browser_context_id {
        params["browserContextId"] = json!(browser_context_id);
    }
    params
}

fn should_apply_browser_download_behavior(
    browser_context_id: Option<&str>,
    manages_default_download_context: bool,
    managed_browser_context_ids: &HashSet<String>,
) -> bool {
    match browser_context_id {
        Some(browser_context_id) => managed_browser_context_ids.contains(browser_context_id),
        None => manages_default_download_context,
    }
}

fn collect_page_download_behavior_targets(pages: &[PageInfo]) -> (bool, Vec<String>) {
    let mut session_ids = HashSet::new();
    let mut has_direct_page = false;

    for page in pages {
        if page.session_id.is_empty() {
            has_direct_page = true;
        } else {
            session_ids.insert(page.session_id.clone());
        }
    }

    let mut session_ids: Vec<_> = session_ids.into_iter().collect();
    session_ids.sort();

    (has_direct_page, session_ids)
}

async fn wait_for_download_completion(
    session_id: &str,
    frame_id: &str,
    rx: &mut broadcast::Receiver<CdpEvent>,
    timeout: tokio::time::Duration,
    mut state: DownloadState,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Timeout waiting for download to complete".to_string());
        }

        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if let Some(info) =
                    process_download_event(session_id, frame_id, &event, &mut state)?
                {
                    return Ok(info);
                }
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Err("Event stream closed".to_string()),
            Err(_) => return Err("Timeout waiting for download to complete".to_string()),
        }
    }
}

fn drain_buffered_download_events(
    session_id: &str,
    frame_id: &str,
    rx: &mut broadcast::Receiver<CdpEvent>,
    download_path: &Option<String>,
) -> Result<Option<BufferedDownload>, String> {
    let mut state = DownloadState {
        download_path: download_path.clone(),
        ..Default::default()
    };

    loop {
        match rx.try_recv() {
            Ok(event) => {
                if let Some(info) =
                    process_download_event(session_id, frame_id, &event, &mut state)?
                {
                    return Ok(Some(BufferedDownload::Completed(info)));
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                return Err("Event stream closed".to_string());
            }
        }
    }

    if state.guid.is_some() {
        Ok(Some(BufferedDownload::InProgress(state)))
    } else {
        Ok(None)
    }
}

fn process_download_event(
    session_id: &str,
    frame_id: &str,
    event: &CdpEvent,
    state: &mut DownloadState,
) -> Result<Option<Value>, String> {
    let is_page_session = event.session_id.as_deref() == Some(session_id);

    match event.method.as_str() {
        "Browser.downloadWillBegin" => {
            if event.params.get("frameId").and_then(|v| v.as_str()) != Some(frame_id) {
                return Ok(None);
            }
            if let Some(guid) = event.params.get("guid").and_then(|v| v.as_str()) {
                state.guid = Some(guid.to_string());
            }
            if let Some(filename) = event
                .params
                .get("suggestedFilename")
                .and_then(|v| v.as_str())
            {
                state.suggested_filename = Some(filename.to_string());
            }
        }
        "Page.downloadWillBegin" if is_page_session => {
            if let Some(event_frame_id) = event.params.get("frameId").and_then(|v| v.as_str()) {
                if event_frame_id != frame_id {
                    return Ok(None);
                }
            }
            if let Some(guid) = event.params.get("guid").and_then(|v| v.as_str()) {
                state.guid = Some(guid.to_string());
            }
            if let Some(filename) = event
                .params
                .get("suggestedFilename")
                .and_then(|v| v.as_str())
            {
                state.suggested_filename = Some(filename.to_string());
            }
        }
        "Browser.downloadProgress" => {
            return process_download_progress_event(event, state);
        }
        "Page.downloadProgress" if is_page_session => {
            return process_download_progress_event(event, state);
        }
        _ => {}
    }

    Ok(None)
}

fn process_download_progress_event(
    event: &CdpEvent,
    state: &DownloadState,
) -> Result<Option<Value>, String> {
    let Some(expected_guid) = state.guid.as_deref() else {
        return Ok(None);
    };
    if event.params.get("guid").and_then(|v| v.as_str()) != Some(expected_guid) {
        return Ok(None);
    }

    match event.params.get("state").and_then(|v| v.as_str()) {
        Some("completed") => {
            let file_path = event
                .params
                .get("filePath")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| {
                    // Page.downloadProgress does not include filePath;
                    // reconstruct it from the configured download directory
                    // and the suggested filename from downloadWillBegin.
                    let dir = state.download_path.as_deref()?;
                    let name = state.suggested_filename.as_deref()?;
                    Some(format!("{}/{}", dir.trim_end_matches('/'), name))
                })
                .unwrap_or_default();
            Ok(Some(json!({
                "download": true,
                "path": file_path,
                "filePath": file_path,
                "suggestedFilename": state.suggested_filename.as_deref().unwrap_or(""),
            })))
        }
        Some("canceled") => Err("Download was canceled".to_string()),
        _ => Ok(None),
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
                        "Page.loadEventFired" => {
                            if p.is_empty() {
                                idle_start = Some(tokio::time::Instant::now());
                            }
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
    download_path: Option<String>,
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
            download_path: download_path.clone(),
            manages_default_download_context: true,
            managed_browser_context_ids: HashSet::new(),
            ignore_https_errors: false,
            visited_origins: HashSet::new(),
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
            target_id: "popup-1".to_string(),
            session_id: "session-1".to_string(),
            url: String::new(),
            title: String::new(),
            target_type: "page".to_string(),
            browser_context_id: None,
        }];
        let target = TargetInfo {
            target_id: "popup-1".to_string(),
            target_type: "page".to_string(),
            title: "Popup".to_string(),
            url: "https://example.com/popup".to_string(),
            attached: None,
            browser_context_id: Some("context-1".to_string()),
        };

        assert!(update_page_target_info_in_pages(&mut pages, &target));
        assert_eq!(pages[0].url, "https://example.com/popup");
        assert_eq!(pages[0].title, "Popup");
        assert_eq!(pages[0].browser_context_id.as_deref(), Some("context-1"));
    }

    #[test]
    fn test_navigation_has_explicit_download_for_is_download() {
        let result = PageNavigateResult {
            frame_id: "frame-1".to_string(),
            loader_id: None,
            error_text: None,
            is_download: Some(true),
        };

        assert!(navigation_has_explicit_download(&result));
    }

    #[test]
    fn test_navigation_has_explicit_download_ignores_err_aborted() {
        let result = PageNavigateResult {
            frame_id: "frame-1".to_string(),
            loader_id: None,
            error_text: Some("net::ERR_ABORTED".to_string()),
            is_download: None,
        };

        assert!(!navigation_has_explicit_download(&result));
    }

    #[test]
    fn test_navigation_has_explicit_download_ignores_other_errors() {
        let result = PageNavigateResult {
            frame_id: "frame-1".to_string(),
            loader_id: None,
            error_text: Some("net::ERR_CONNECTION_RESET".to_string()),
            is_download: None,
        };

        assert!(!navigation_has_explicit_download(&result));
    }

    #[test]
    fn test_page_download_behavior_params_reset_to_default() {
        assert_eq!(
            page_download_behavior_params(None),
            json!({
                "behavior": "default",
            })
        );
    }

    #[test]
    fn test_browser_download_behavior_params_reset_to_default_for_context() {
        assert_eq!(
            browser_download_behavior_params(None, Some("context-1"), "allow"),
            json!({
                "behavior": "default",
                "eventsEnabled": false,
                "browserContextId": "context-1",
            })
        );
    }

    #[test]
    fn test_collect_page_download_behavior_targets_covers_direct_and_sessions() {
        let pages = vec![
            PageInfo {
                target_id: "page-1".to_string(),
                session_id: "session-1".to_string(),
                url: String::new(),
                title: String::new(),
                target_type: "page".to_string(),
                browser_context_id: None,
            },
            PageInfo {
                target_id: "page-2".to_string(),
                session_id: "session-2".to_string(),
                url: String::new(),
                title: String::new(),
                target_type: "page".to_string(),
                browser_context_id: Some("context-1".to_string()),
            },
            PageInfo {
                target_id: "page-3".to_string(),
                session_id: "session-3".to_string(),
                url: String::new(),
                title: String::new(),
                target_type: "page".to_string(),
                browser_context_id: Some("context-2".to_string()),
            },
            PageInfo {
                target_id: "direct-page".to_string(),
                session_id: String::new(),
                url: String::new(),
                title: String::new(),
                target_type: "page".to_string(),
                browser_context_id: None,
            },
        ];

        assert_eq!(
            collect_page_download_behavior_targets(&pages),
            (
                true,
                vec![
                    "session-1".to_string(),
                    "session-2".to_string(),
                    "session-3".to_string()
                ],
            )
        );
    }

    #[test]
    fn test_should_apply_browser_download_behavior_only_for_managed_contexts() {
        let managed_contexts = HashSet::from(["context-1".to_string()]);

        assert!(should_apply_browser_download_behavior(
            None,
            true,
            &managed_contexts
        ));
        assert!(!should_apply_browser_download_behavior(
            None,
            false,
            &managed_contexts
        ));
        assert!(should_apply_browser_download_behavior(
            Some("context-1"),
            false,
            &managed_contexts
        ));
        assert!(!should_apply_browser_download_behavior(
            Some("context-2"),
            true,
            &managed_contexts
        ));
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

    fn cdp_browser_event(method: &str, params: Value) -> CdpEvent {
        CdpEvent {
            method: method.to_string(),
            params,
            session_id: None,
        }
    }

    #[tokio::test]
    async fn test_wait_for_download_completion_ignores_other_browser_frames() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);

        tokio::spawn(async move {
            let _ = tx.send(cdp_browser_event(
                "Browser.downloadWillBegin",
                json!({
                    "frameId": "other-frame",
                    "guid": "other-guid",
                    "suggestedFilename": "other.csv",
                }),
            ));
            let _ = tx.send(cdp_browser_event(
                "Browser.downloadProgress",
                json!({
                    "guid": "other-guid",
                    "state": "completed",
                    "filePath": "/tmp/other.csv",
                }),
            ));
            sleep(Duration::from_millis(25)).await;
            let _ = tx.send(cdp_browser_event(
                "Browser.downloadWillBegin",
                json!({
                    "frameId": "frame-1",
                    "guid": "expected-guid",
                    "suggestedFilename": "report.csv",
                }),
            ));
            let _ = tx.send(cdp_browser_event(
                "Browser.downloadProgress",
                json!({
                    "guid": "expected-guid",
                    "state": "completed",
                    "filePath": "/tmp/report.csv",
                }),
            ));
        });

        let result = wait_for_download_completion(
            "session-1",
            "frame-1",
            &mut rx,
            Duration::from_secs(1),
            DownloadState::default(),
        )
        .await
        .expect("download should complete for the matching frame");

        assert_eq!(
            result.get("path").and_then(|v| v.as_str()),
            Some("/tmp/report.csv")
        );
        assert_eq!(
            result.get("filePath").and_then(|v| v.as_str()),
            Some("/tmp/report.csv")
        );
        assert_eq!(
            result.get("suggestedFilename").and_then(|v| v.as_str()),
            Some("report.csv")
        );
    }

    #[tokio::test]
    async fn test_wait_for_download_completion_requires_matching_guid() {
        let (tx, mut rx) = broadcast::channel::<CdpEvent>(16);

        tokio::spawn(async move {
            let _ = tx.send(cdp_event(
                "Page.downloadWillBegin",
                "session-1",
                json!({
                    "frameId": "frame-1",
                    "guid": "expected-guid",
                    "suggestedFilename": "report.csv",
                }),
            ));
            let _ = tx.send(cdp_browser_event(
                "Browser.downloadProgress",
                json!({
                    "guid": "other-guid",
                    "state": "completed",
                    "filePath": "/tmp/other.csv",
                }),
            ));
            sleep(Duration::from_millis(25)).await;
            let _ = tx.send(cdp_event(
                "Page.downloadProgress",
                "session-1",
                json!({
                    "guid": "expected-guid",
                    "state": "completed",
                    "filePath": "/tmp/report.csv",
                }),
            ));
        });

        let result = wait_for_download_completion(
            "session-1",
            "frame-1",
            &mut rx,
            Duration::from_secs(1),
            DownloadState::default(),
        )
        .await
        .expect("download should complete for the matching guid");

        assert_eq!(
            result.get("path").and_then(|v| v.as_str()),
            Some("/tmp/report.csv")
        );
        assert_eq!(
            result.get("filePath").and_then(|v| v.as_str()),
            Some("/tmp/report.csv")
        );
        assert_eq!(
            result.get("suggestedFilename").and_then(|v| v.as_str()),
            Some("report.csv")
        );
    }

    #[tokio::test]
    async fn test_drain_buffered_download_events_returns_none_without_signal() {
        let (_tx, mut rx) = broadcast::channel::<CdpEvent>(16);

        let result = drain_buffered_download_events("session-1", "frame-1", &mut rx, &None)
            .expect("buffered drain should not error without events");

        assert!(result.is_none());
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
