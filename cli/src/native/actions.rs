use serde_json::{json, Value};
use std::env;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use super::auth;
use super::browser::{BrowserManager, WaitUntil};
use super::cdp::chrome::LaunchOptions;
use super::cdp::client::CdpClient;
use super::cdp::types::{
    AttachToTargetParams, AttachToTargetResult, CdpEvent, ConsoleApiCalledEvent,
    CreateTargetResult, ExceptionThrownEvent, TargetCreatedEvent, TargetDestroyedEvent,
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
use super::stream;
use super::tracing::{self as native_tracing, TracingState};
use super::webdriver::appium::AppiumManager;
use super::webdriver::backend::{BrowserBackend, WebDriverBackend, WEBDRIVER_UNSUPPORTED_ACTIONS};
use super::webdriver::ios;
use super::webdriver::safari;

pub struct PendingConfirmation {
    pub action: String,
    pub cmd: Value,
}

pub struct HarEntry {
    pub method: String,
    pub url: String,
    pub status: Option<i64>,
    pub mime_type: Option<String>,
    pub request_id: String,
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
    pub headers: Option<std::collections::HashMap<String, String>>,
}

#[derive(Clone, serde::Serialize)]
pub struct TrackedRequest {
    pub url: String,
    pub method: String,
    pub headers: Value,
    pub timestamp: u64,
    #[serde(rename = "resourceType")]
    pub resource_type: String,
}

pub struct FetchPausedRequest {
    pub request_id: String,
    pub url: String,
    pub resource_type: String,
    pub session_id: String,
}

pub enum BackendType {
    Cdp,
    WebDriver,
}

pub struct DaemonState {
    pub browser: Option<BrowserManager>,
    pub appium: Option<AppiumManager>,
    pub safari_driver: Option<safari::SafariDriverProcess>,
    pub webdriver_backend: Option<super::webdriver::backend::WebDriverBackend>,
    pub backend_type: BackendType,
    pub ref_map: RefMap,
    pub domain_filter: Option<DomainFilter>,
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
    pub routes: Vec<RouteEntry>,
    pub tracked_requests: Vec<TrackedRequest>,
    pub request_tracking: bool,
    pub active_frame_id: Option<String>,
    /// Shared slot for stream server to receive CDP client when browser launches.
    pub stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
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
            domain_filter: env::var("AGENT_BROWSER_ALLOWED_DOMAINS")
                .ok()
                .filter(|s| !s.is_empty())
                .map(|s| DomainFilter::new(&s)),
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
            routes: Vec::new(),
            tracked_requests: Vec::new(),
            request_tracking: false,
            active_frame_id: None,
            stream_client: None,
        }
    }

    /// Create state with an optional stream client slot (for daemon startup with stream server).
    pub fn new_with_stream_client(
        stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
    ) -> Self {
        let mut s = Self::new();
        s.stream_client = stream_client;
        s
    }

    fn subscribe_to_browser_events(&mut self) {
        if let Some(ref browser) = self.browser {
            self.event_rx = Some(browser.client.subscribe());
        }
    }

    /// Update the stream server's CDP client slot when browser is set or cleared.
    pub async fn update_stream_client(&self) {
        if let Some(ref slot) = self.stream_client {
            let mut guard = slot.write().await;
            *guard = self.browser.as_ref().map(|m| Arc::clone(&m.client));
        }
    }

    fn drain_cdp_events(
        &mut self,
    ) -> (
        Vec<i64>,
        Vec<TargetCreatedEvent>,
        Vec<String>,
        Vec<FetchPausedRequest>,
    ) {
        let rx = match self.event_rx.as_mut() {
            Some(rx) => rx,
            None => return (Vec::new(), Vec::new(), Vec::new(), Vec::new()),
        };

        let mut pending_acks: Vec<i64> = Vec::new();
        let mut new_targets: Vec<TargetCreatedEvent> = Vec::new();
        let mut destroyed_targets: Vec<String> = Vec::new();
        let mut fetch_paused: Vec<FetchPausedRequest> = Vec::new();

        loop {
            match rx.try_recv() {
                Ok(event) => {
                    // Target events are not session-scoped; handle them first
                    match event.method.as_str() {
                        "Target.targetCreated" => {
                            if let Ok(te) =
                                serde_json::from_value::<TargetCreatedEvent>(event.params.clone())
                            {
                                if (te.target_info.target_type == "page"
                                    || te.target_info.target_type == "webview")
                                    && !te.target_info.url.is_empty()
                                {
                                    let already_tracked = self
                                        .browser
                                        .as_ref()
                                        .is_none_or(|b| b.has_target(&te.target_info.target_id));
                                    if !already_tracked {
                                        new_targets.push(te);
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
                        _ => {}
                    }

                    let session_matches = if let Some(ref browser) = self.browser {
                        event.session_id.as_deref() == browser.active_session_id().ok()
                    } else {
                        false
                    };

                    if !session_matches {
                        continue;
                    }

                    match event.method.as_str() {
                        "Runtime.consoleAPICalled" => {
                            if let Ok(console_event) = serde_json::from_value::<ConsoleApiCalledEvent>(
                                event.params.clone(),
                            ) {
                                let text: String = console_event
                                    .args
                                    .iter()
                                    .filter_map(|arg| {
                                        arg.value
                                            .as_ref()
                                            .map(|v| match v {
                                                Value::String(s) => s.clone(),
                                                other => other.to_string(),
                                            })
                                            .or_else(|| arg.description.clone())
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                self.event_tracker
                                    .add_console(&console_event.call_type, &text);
                            }
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
                                    self.har_entries.push(HarEntry {
                                        method: method.clone(),
                                        url: url.clone(),
                                        status: None,
                                        mime_type: None,
                                        request_id,
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
                                    });
                                }
                            }
                        }
                        "Network.responseReceived" if self.har_recording => {
                            if let Some(response) = event.params.get("response") {
                                let request_id = event
                                    .params
                                    .get("requestId")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let status = response.get("status").and_then(|v| v.as_i64());
                                let mime_type = response
                                    .get("mimeType")
                                    .and_then(|v| v.as_str())
                                    .map(String::from);
                                if let Some(entry) = self
                                    .har_entries
                                    .iter_mut()
                                    .rev()
                                    .find(|e| e.request_id == request_id)
                                {
                                    entry.status = status;
                                    entry.mime_type = mime_type;
                                }
                            }
                        }
                        "Page.screencastFrame" => {
                            if self.recording_state.active {
                                if let Some(data) =
                                    event.params.get("data").and_then(|v| v.as_str())
                                {
                                    if let Ok(bytes) = base64::Engine::decode(
                                        &base64::engine::general_purpose::STANDARD,
                                        data,
                                    ) {
                                        recording::recording_add_frame(
                                            &mut self.recording_state,
                                            &bytes,
                                        );
                                    }
                                }
                            }
                            if let Some(sid) =
                                event.params.get("sessionId").and_then(|v| v.as_i64())
                            {
                                pending_acks.push(sid);
                            }
                        }
                        "Fetch.requestPaused" => {
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
                            let sid = event.session_id.clone().unwrap_or_default();

                            fetch_paused.push(FetchPausedRequest {
                                request_id,
                                url: request_url,
                                resource_type,
                                session_id: sid,
                            });
                        }
                        _ => {}
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
                Err(broadcast::error::TryRecvError::Closed) => {
                    self.event_rx = None;
                    break;
                }
            }
        }

        (pending_acks, new_targets, destroyed_targets, fetch_paused)
    }
}

pub async fn execute_command(cmd: &Value, state: &mut DaemonState) -> Value {
    let action = cmd.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let id = cmd
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Drain pending CDP events (console, errors, screencast frames, target lifecycle, fetch)
    let (pending_acks, new_targets, destroyed_targets, fetch_paused) = state.drain_cdp_events();
    if !pending_acks.is_empty() {
        if let Some(ref browser) = state.browser {
            if let Ok(session_id) = browser.active_session_id() {
                for ack_sid in pending_acks {
                    let _ =
                        stream::ack_screencast_frame(&browser.client, session_id, ack_sid).await;
                }
            }
        }
    }

    for target_id in &destroyed_targets {
        if let Some(ref mut mgr) = state.browser {
            mgr.remove_page_by_target_id(target_id);
        }
    }

    for te in &new_targets {
        if let Some(ref mut mgr) = state.browser {
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
                if let Some(ref filter) = state.domain_filter {
                    let _ = network::install_domain_filter(
                        &mgr.client,
                        &attach.session_id,
                        &filter.allowed_domains,
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

    // Handle Fetch.requestPaused events (route interception + domain filter)
    for paused in &fetch_paused {
        if let Some(ref browser) = state.browser {
            resolve_fetch_paused(browser, state.domain_filter.as_ref(), &state.routes, paused)
                .await;
        }
    }

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
    );
    if !skip_launch {
        // Check if existing connection is stale and needs re-launch
        let needs_launch = if let Some(ref mgr) = state.browser {
            !mgr.is_connection_alive().await
        } else {
            true
        };

        if needs_launch {
            if state.browser.is_some() {
                if let Some(ref mut mgr) = state.browser {
                    let _ = mgr.close().await;
                }
                state.browser = None;
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
        "console" => handle_console(state).await,
        "errors" => handle_errors(state).await,
        "state_save" => handle_state_save(cmd, state).await,
        "state_load" => handle_state_load(cmd, state).await,
        "state_list" => handle_state_list().await,
        "state_show" => handle_state_show(cmd).await,
        "state_clear" => handle_state_clear(cmd).await,
        "state_clean" => handle_state_clean(cmd).await,
        "state_rename" => handle_state_rename(cmd).await,
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

    match result {
        Ok(data) => success_response(&id, data),
        Err(e) => error_response(&id, &super::browser::to_ai_friendly_error(&e)),
    }
}

// ---------------------------------------------------------------------------
// Auto-launch
// ---------------------------------------------------------------------------

async fn auto_launch(state: &mut DaemonState) -> Result<(), String> {
    let options = launch_options_from_env();
    let engine = env::var("AGENT_BROWSER_ENGINE").ok();

    if let Ok(cdp) = env::var("AGENT_BROWSER_CDP") {
        let mgr = BrowserManager::connect_cdp(&cdp).await?;
        state.browser = Some(mgr);
        state.subscribe_to_browser_events();
        state.update_stream_client().await;
        try_auto_restore_state(state).await;
        return Ok(());
    }

    if env::var("AGENT_BROWSER_AUTO_CONNECT").is_ok() {
        let mgr = BrowserManager::connect_auto().await?;
        state.browser = Some(mgr);
        state.subscribe_to_browser_events();
        state.update_stream_client().await;
        try_auto_restore_state(state).await;
        return Ok(());
    }

    let mgr = BrowserManager::launch(options, engine.as_deref()).await?;
    state.browser = Some(mgr);
    state.subscribe_to_browser_events();
    state.update_stream_client().await;
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
    }
}

fn daemon_state_from_env(state: &mut DaemonState) {
    if let Ok(name) = env::var("AGENT_BROWSER_SESSION_NAME") {
        if !name.is_empty() {
            state.session_name = Some(name);
        }
    }
    if let Ok(domains) = env::var("AGENT_BROWSER_ALLOWED_DOMAINS") {
        if !domains.is_empty() {
            state.domain_filter = Some(DomainFilter::new(&domains));
        }
    }
    if state.policy.is_none() {
        state.policy = ActionPolicy::load_if_exists();
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

    // Relaunch logic: check if we can reuse the existing connection
    let needs_relaunch = if let Some(ref mgr) = state.browser {
        let has_cdp_arg = cdp_url.is_some() || cdp_port.is_some();
        let was_cdp = mgr.is_cdp_connection();
        has_cdp_arg != was_cdp || !mgr.is_connection_alive().await
    } else {
        true
    };

    if needs_relaunch {
        if let Some(ref mut b) = state.browser {
            b.close().await?;
            state.browser = None;
            state.update_stream_client().await;
        }
    } else {
        return Ok(json!({ "launched": true, "reused": true }));
    }
    state.ref_map.clear();
    let extensions: Option<Vec<String>> =
        cmd.get("extensions").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });

    let profile = cmd.get("profile").and_then(|v| v.as_str());
    let storage_state = cmd.get("storageState").and_then(|v| v.as_str());
    let allow_file_access = cmd
        .get("allowFileAccess")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let executable_path: Option<String> = cmd
        .get("executablePath")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok());

    let has_cdp = cdp_url.is_some() || cdp_port.is_some();
    super::browser::validate_launch_options(
        extensions.as_deref(),
        has_cdp,
        profile,
        storage_state,
        allow_file_access,
        executable_path.as_deref(),
    )?;

    if let Some(url) = cdp_url {
        state.browser = Some(BrowserManager::connect_cdp(url).await?);
        state.subscribe_to_browser_events();
        state.update_stream_client().await;
        return Ok(json!({ "launched": true }));
    }

    if let Some(port) = cdp_port {
        state.browser = Some(BrowserManager::connect_cdp(&port.to_string()).await?);
        state.subscribe_to_browser_events();
        state.update_stream_client().await;
        return Ok(json!({ "launched": true }));
    }

    if auto_connect {
        state.browser = Some(BrowserManager::connect_auto().await?);
        state.subscribe_to_browser_events();
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
                let (ws_url, provider_session) = providers::connect_provider(provider).await?;
                match BrowserManager::connect_cdp(&ws_url).await {
                    Ok(mgr) => {
                        state.browser = Some(mgr);
                        state.subscribe_to_browser_events();
                        state.update_stream_client().await;
                        return Ok(json!({ "launched": true, "provider": provider }));
                    }
                    Err(e) => {
                        if let Some(ref ps) = provider_session {
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

    let options = LaunchOptions {
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
        proxy_bypass: cmd
            .get("proxy")
            .and_then(|v| v.get("bypass"))
            .and_then(|v| v.as_str())
            .map(String::from),
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
    };

    if let Some(ref domains) = cmd
        .get("allowedDomains")
        .and_then(|v| v.as_str())
        .map(String::from)
    {
        state.domain_filter = Some(DomainFilter::new(domains));
    }

    state.browser = Some(BrowserManager::launch(options, engine.as_deref()).await?);
    state.subscribe_to_browser_events();
    state.update_stream_client().await;

    if let Some(ref filter) = state.domain_filter {
        if let Some(ref mgr) = state.browser {
            if let Ok(session_id) = mgr.active_session_id() {
                let _ = network::install_domain_filter(
                    &mgr.client,
                    session_id,
                    &filter.allowed_domains,
                )
                .await;
                network::sanitize_existing_pages(&mgr.client, &mgr.pages_list(), filter).await;
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

    if let Some(ref filter) = state.domain_filter {
        filter.check_url(url)?;
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

    let scoped_headers = cmd
        .get("headers")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty());

    if let Some(headers_map) = scoped_headers {
        let session_id = mgr.active_session_id()?.to_string();
        let headers: std::collections::HashMap<String, String> = headers_map
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect();
        network::set_extra_headers(&mgr.client, &session_id, &headers).await?;
    }

    state.ref_map.clear();
    let result = mgr.navigate(url, wait_until).await;

    if scoped_headers.is_some() {
        if let Ok(session_id) = mgr.active_session_id() {
            let empty: std::collections::HashMap<String, String> = std::collections::HashMap::new();
            let _ = network::set_extra_headers(&mgr.client, session_id, &empty).await;
        }
    }

    result
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
        eprintln!("[inspect] Failed to open browser: {}", e);
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
                )
                .await;
            }
        }
    }
    if let Some(ref mut mgr) = state.browser {
        mgr.close().await?;
    }
    state.browser = None;
    state.update_stream_client().await;

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
            .get("depth")
            .and_then(|v| v.as_u64())
            .map(|d| d as usize),
        cursor: cmd.get("cursor").and_then(|v| v.as_bool()).unwrap_or(false),
    };

    state.ref_map.clear();
    let tree =
        snapshot::take_snapshot(&mgr.client, &session_id, &options, &mut state.ref_map).await?;

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
        )
        .await?;
    }

    let result =
        screenshot::take_screenshot(&mgr.client, &session_id, &state.ref_map, &options).await?;

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
        let object_id =
            resolve_element_object_id(&mgr.client, &session_id, &state.ref_map, selector).await?;
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
                Some(&session_id),
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

    interaction::dblclick(&mgr.client, &session_id, &state.ref_map, selector).await?;
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

    interaction::fill(&mgr.client, &session_id, &state.ref_map, selector, value).await?;
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

    interaction::press_key(&mgr.client, &session_id, key).await?;
    Ok(json!({ "pressed": key }))
}

async fn handle_hover(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::hover(&mgr.client, &session_id, &state.ref_map, selector).await?;
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

    interaction::scroll(&mgr.client, &session_id, &state.ref_map, selector, dx, dy).await?;
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

    interaction::select_option(&mgr.client, &session_id, &state.ref_map, selector, &values).await?;
    Ok(json!({ "selected": values }))
}

async fn handle_check(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::check(&mgr.client, &session_id, &state.ref_map, selector).await?;
    Ok(json!({ "checked": selector }))
}

async fn handle_uncheck(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::uncheck(&mgr.client, &session_id, &state.ref_map, selector).await?;
    Ok(json!({ "unchecked": selector }))
}

async fn handle_wait(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let timeout_ms = cmd.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30000);

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

    let text = super::element::get_element_text(&mgr.client, &session_id, &state.ref_map, selector)
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

    let visible =
        super::element::is_element_visible(&mgr.client, &session_id, &state.ref_map, selector)
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

    let enabled =
        super::element::is_element_enabled(&mgr.client, &session_id, &state.ref_map, selector)
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

    let checked =
        super::element::is_element_checked(&mgr.client, &session_id, &state.ref_map, selector)
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
        while let Ok(event) = rx.recv().await {
            if event.method == "Page.loadEventFired"
                && event.session_id.as_deref() == Some(&session_id)
            {
                return;
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

    let headers: std::collections::HashMap<String, String> = headers_value
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

async fn handle_console(state: &DaemonState) -> Result<Value, String> {
    Ok(state.event_tracker.get_console_json())
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

async fn handle_state_list() -> Result<Value, String> {
    state::state_list()
}

async fn handle_state_show(cmd: &Value) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;
    state::state_show(path)
}

async fn handle_state_clear(cmd: &Value) -> Result<Value, String> {
    let path = cmd.get("path").and_then(|v| v.as_str());
    state::state_clear(path)
}

async fn handle_state_clean(cmd: &Value) -> Result<Value, String> {
    let days = cmd.get("days").and_then(|v| v.as_u64()).unwrap_or(30);
    state::state_clean(days)
}

async fn handle_state_rename(cmd: &Value) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;
    let name = cmd
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'name' parameter")?;
    state::state_rename(path, name)
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
    let current =
        snapshot::take_snapshot(&mgr.client, &session_id, &options, &mut state.ref_map).await?;

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
    let snap1 =
        snapshot::take_snapshot(&mgr.client, &session_id, &options, &mut state.ref_map).await?;

    // Navigate to URL2 and snapshot
    mgr.navigate(url2, wait_until).await?;
    state.ref_map.clear();
    let snap2 =
        snapshot::take_snapshot(&mgr.client, &session_id, &options, &mut state.ref_map).await?;

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
    mgr.tab_new(url).await
}

async fn handle_tab_switch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let index = cmd
        .get("index")
        .and_then(|v| v.as_u64())
        .ok_or("Missing 'index' parameter")? as usize;
    state.ref_map.clear();
    mgr.tab_switch(index).await
}

async fn handle_tab_close(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_mut().ok_or("Browser not launched")?;
    let index = cmd
        .get("index")
        .and_then(|v| v.as_u64())
        .map(|i| i as usize);
    state.ref_map.clear();
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

    let features = cmd.get("features").and_then(|v| v.as_object()).map(|m| {
        m.iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
            .collect::<Vec<(String, String)>>()
    });

    mgr.set_emulated_media(media, features).await?;
    Ok(json!({ "set": true }))
}

async fn handle_download(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;
    mgr.set_download_behavior(path).await?;
    Ok(json!({ "downloadPath": path }))
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

    let result = recording::recording_start(&mut state.recording_state, path)?;

    // Start screencast on new page
    stream::start_screencast(&mgr.client, &new_session_id, "jpeg", 80, 1280, 720).await?;
    state.screencasting = true;

    Ok(result)
}

async fn handle_recording_stop(state: &mut DaemonState) -> Result<Value, String> {
    // Stop screencast
    if state.screencasting {
        if let Some(ref browser) = state.browser {
            if let Ok(session_id) = browser.active_session_id() {
                let _ = stream::stop_screencast(&browser.client, session_id).await;
            }
        }
        state.screencasting = false;
    }

    // Drain remaining frames before stopping
    let (ack_ids, _, _, _) = state.drain_cdp_events();
    if !ack_ids.is_empty() {
        if let Some(ref browser) = state.browser {
            if let Ok(session_id) = browser.active_session_id() {
                for ack_sid in ack_ids {
                    let _ =
                        stream::ack_screencast_frame(&browser.client, session_id, ack_sid).await;
                }
            }
        }
    }

    recording::recording_stop(&mut state.recording_state)
}

async fn handle_recording_restart(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    // Stop screencast, restart recording, start screencast again
    if state.screencasting {
        if let Some(ref browser) = state.browser {
            if let Ok(session_id) = browser.active_session_id() {
                let _ = stream::stop_screencast(&browser.client, session_id).await;
            }
        }
        state.screencasting = false;
    }

    let result = recording::recording_restart(&mut state.recording_state, path)?;

    if let Some(ref browser) = state.browser {
        let session_id = browser.active_session_id()?.to_string();
        stream::start_screencast(&browser.client, &session_id, "jpeg", 80, 1280, 720).await?;
        state.screencasting = true;
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

    interaction::focus(&mgr.client, &session_id, &state.ref_map, selector).await?;
    Ok(json!({ "focused": selector }))
}

async fn handle_clear(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::clear(&mgr.client, &session_id, &state.ref_map, selector).await?;
    Ok(json!({ "cleared": selector }))
}

async fn handle_selectall(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::select_all(&mgr.client, &session_id, &state.ref_map, selector).await?;
    Ok(json!({ "selected": selector }))
}

async fn handle_scrollintoview(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let selector = cmd
        .get("selector")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'selector' parameter")?;

    interaction::scroll_into_view(&mgr.client, &session_id, &state.ref_map, selector).await?;
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

    interaction::highlight(&mgr.client, &session_id, &state.ref_map, selector).await?;
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

    interaction::tap_touch(&mgr.client, &session_id, &state.ref_map, sel).await?;
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

    let text =
        super::element::get_element_inner_text(&mgr.client, &session_id, &state.ref_map, selector)
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

    let html =
        super::element::get_element_inner_html(&mgr.client, &session_id, &state.ref_map, selector)
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

    let value =
        super::element::get_element_input_value(&mgr.client, &session_id, &state.ref_map, selector)
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

    super::element::set_element_value(&mgr.client, &session_id, &state.ref_map, selector, value)
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

async fn handle_dialog(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let accept = cmd
        .get("response")
        .and_then(|v| v.as_str())
        .map(|r| r == "accept")
        .or_else(|| cmd.get("accept").and_then(|v| v.as_bool()))
        .unwrap_or(true);
    let prompt_text = cmd.get("promptText").and_then(|v| v.as_str());

    mgr.handle_dialog(accept, prompt_text).await?;
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
        "iphone 12" | "iphone12" => (390, 844, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 14_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/14.0 Mobile/15E148 Safari/604.1"),
        "iphone 14" | "iphone14" => (390, 844, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 16_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Mobile/15E148 Safari/604.1"),
        "iphone 15" | "iphone15" => (393, 852, 3.0, true, "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1"),
        "ipad" | "ipad air" => (820, 1180, 2.0, true, "Mozilla/5.0 (iPad; CPU OS 14_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/14.0 Safari/604.1"),
        "ipad pro" => (1024, 1366, 2.0, true, "Mozilla/5.0 (iPad; CPU OS 14_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/14.0 Safari/604.1"),
        "pixel 5" | "pixel5" => (393, 851, 2.75, true, "Mozilla/5.0 (Linux; Android 11; Pixel 5) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.91 Mobile Safari/537.36"),
        "pixel 7" | "pixel7" => (412, 915, 2.625, true, "Mozilla/5.0 (Linux; Android 13; Pixel 7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/116.0.0.0 Mobile Safari/537.36"),
        "galaxy s21" | "galaxys21" => (360, 800, 3.0, true, "Mozilla/5.0 (Linux; Android 11; SM-G991B) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/90.0.4430.91 Mobile Safari/537.36"),
        _ => return Err(format!("Unknown device: {}. Supported: iPhone 12, iPhone 14, iPhone 15, iPad, iPad Pro, Pixel 5, Pixel 7, Galaxy S21", name)),
    };

    mgr.set_viewport(width, height, scale, mobile).await?;
    mgr.set_user_agent(ua).await?;

    Ok(json!({
        "device": name,
        "width": width,
        "height": height,
        "deviceScaleFactor": scale,
        "mobile": mobile,
    }))
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

    let format = cmd.get("format").and_then(|v| v.as_str()).unwrap_or("jpeg");
    let quality = cmd.get("quality").and_then(|v| v.as_i64()).unwrap_or(80) as i32;
    let max_width = cmd.get("maxWidth").and_then(|v| v.as_i64()).unwrap_or(1280) as i32;
    let max_height = cmd.get("maxHeight").and_then(|v| v.as_i64()).unwrap_or(720) as i32;

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
    let timeout_ms = cmd.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30000);

    wait_for_url(&mgr.client, &session_id, url_pattern, timeout_ms).await?;
    let url = mgr.get_url().await.unwrap_or_default();
    Ok(json!({ "url": url }))
}

async fn handle_waitforloadstate(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let load_state = cmd.get("state").and_then(|v| v.as_str()).unwrap_or("load");
    let timeout_ms = cmd.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30000);

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
    let timeout_ms = cmd.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30000);

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

    // If selector, resolve via JS to find the iframe's contentWindow
    if let Some(sel) = selector {
        let js = format!(
            r#"(() => {{
                const el = document.querySelector({});
                if (!el) return null;
                if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {{
                    return el.name || el.id || 'frame';
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
            )
            .await?;
            Ok(json!({ "clicked": selector }))
        }
        "fill" => {
            let value = cmd
                .get("value")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'value' for fill subaction")?;
            interaction::fill(&mgr.client, &session_id, &state.ref_map, selector, value).await?;
            Ok(json!({ "filled": selector }))
        }
        "check" => {
            interaction::check(&mgr.client, &session_id, &state.ref_map, selector).await?;
            Ok(json!({ "checked": selector }))
        }
        "hover" => {
            interaction::hover(&mgr.client, &session_id, &state.ref_map, selector).await?;
            Ok(json!({ "hovered": selector }))
        }
        "text" => {
            let text = super::element::get_element_text(
                &mgr.client,
                &session_id,
                &state.ref_map,
                selector,
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

async fn resolve_semantic_locator(
    client: &super::cdp::client::CdpClient,
    session_id: &str,
    strategy: &str,
    value: &str,
    exact: bool,
) -> Result<String, String> {
    let js = match strategy {
        "role" => {
            format!(
                r#"(() => {{
                    const els = document.querySelectorAll('[role="{}"]');
                    if (els.length === 0) return null;
                    return 'found';
                }})()"#,
                value
            )
        }
        "text" => {
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
            format!(
                r#"(() => {{
                    const all = document.querySelectorAll('*');
                    for (const el of all) {{
                        if (el.children.length === 0 && {}) return 'found';
                    }}
                    return null;
                }})()"#,
                match_fn
            )
        }
        _ => return Err(format!("Unknown semantic strategy: {}", strategy)),
    };

    let result: super::cdp::types::EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &super::cdp::types::EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    if result
        .result
        .value
        .as_ref()
        .map(|v| v.is_null())
        .unwrap_or(true)
    {
        return Err(format!("No element found for {} '{}'", strategy, value));
    }

    Ok(value.to_string())
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
        if let Ok(sid) = browser.active_session_id() {
            let _ = browser
                .evaluate(
                    "document.querySelector('[data-agent-browser-located]')?.removeAttribute('data-agent-browser-located')",
                    None,
                )
                .await;
            let _ = sid;
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
    let _session_id = mgr.active_session_id()?.to_string();
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

    let (sx, sy) =
        super::element::resolve_element_center(&mgr.client, &session_id, &state.ref_map, source)
            .await?;
    let (tx, ty) =
        super::element::resolve_element_center(&mgr.client, &session_id, &state.ref_map, target)
            .await?;

    // Mouse down at source
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mouseMoved", "x": sx, "y": sy })),
            Some(&session_id),
        )
        .await?;
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mousePressed", "x": sx, "y": sy, "button": "left", "clickCount": 1 })),
            Some(&session_id),
        )
        .await?;

    // Move in steps to target
    let steps = 10;
    for i in 1..=steps {
        let cx = sx + (tx - sx) * (i as f64) / (steps as f64);
        let cy = sy + (ty - sy) * (i as f64) / (steps as f64);
        mgr.client
            .send_command(
                "Input.dispatchMouseEvent",
                Some(json!({ "type": "mouseMoved", "x": cx, "y": cy })),
                Some(&session_id),
            )
            .await?;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Mouse up at target
    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mouseReleased", "x": tx, "y": ty, "button": "left", "clickCount": 1 })),
            Some(&session_id),
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
    let _session_id = mgr.active_session_id()?.to_string();
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
    let timeout_ms = cmd.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30000);

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
    let timeout_ms = cmd.get("timeout").and_then(|v| v.as_u64()).unwrap_or(30000);

    let mut rx = mgr.client.subscribe();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("Timeout waiting for download".to_string());
        }

        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(event)) => {
                if event.method == "Page.downloadProgress"
                    && event.session_id.as_deref() == Some(&session_id)
                    && event.params.get("state").and_then(|v| v.as_str()) == Some("completed")
                {
                    let path = cmd
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("download");
                    return Ok(json!({ "path": path }));
                }
            }
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

    let result =
        screenshot::take_screenshot(&mgr.client, &session_id, &state.ref_map, &options).await?;

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
    stream::start_screencast(&mgr.client, &session_id, "jpeg", 80, 1280, 720).await?;
    state.screencasting = true;

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

    if state.screencasting {
        if let Some(ref browser) = state.browser {
            if let Ok(session_id) = browser.active_session_id() {
                let _ = stream::stop_screencast(&browser.client, session_id).await;
            }
        }
        state.screencasting = false;
    }

    let (ack_ids, _, _, _) = state.drain_cdp_events();
    if !ack_ids.is_empty() {
        if let Some(ref browser) = state.browser {
            if let Ok(session_id) = browser.active_session_id() {
                for ack_sid in ack_ids {
                    let _ =
                        stream::ack_screencast_frame(&browser.client, session_id, ack_sid).await;
                }
            }
        }
    }

    recording::recording_stop(&mut state.recording_state)
}

async fn handle_har_start(state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    mgr.client
        .send_command_no_params("Network.enable", Some(&session_id))
        .await?;
    state.har_recording = true;
    state.har_entries.clear();
    Ok(json!({ "started": true }))
}

async fn handle_har_stop(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or("Missing 'path' parameter")?;

    state.har_recording = false;

    let entries: Vec<Value> = state
        .har_entries
        .drain(..)
        .map(|e| {
            json!({
                "request": {
                    "method": e.method,
                    "url": e.url,
                },
                "response": {
                    "status": e.status.unwrap_or(0),
                    "content": {
                        "mimeType": e.mime_type.unwrap_or_default(),
                    }
                }
            })
        })
        .collect();
    let request_count = entries.len();

    let har = json!({
        "log": {
            "version": "1.2",
            "entries": entries
        }
    });

    let har_str = serde_json::to_string_pretty(&har)
        .map_err(|e| format!("Failed to serialize HAR: {}", e))?;
    std::fs::write(path, har_str).map_err(|e| format!("Failed to write HAR: {}", e))?;

    Ok(json!({ "path": path, "requestCount": request_count }))
}

// ---------------------------------------------------------------------------
// Fetch interception resolver (routes + domain filter)
// ---------------------------------------------------------------------------

async fn resolve_fetch_paused(
    browser: &BrowserManager,
    domain_filter: Option<&DomainFilter>,
    routes: &[RouteEntry],
    paused: &FetchPausedRequest,
) {
    let session_id = &paused.session_id;

    // Domain filter check (takes priority over routes)
    if let Some(filter) = domain_filter {
        if let Ok(parsed) = url::Url::parse(&paused.url) {
            let scheme = parsed.scheme();
            if scheme != "http" && scheme != "https" {
                if paused.resource_type.eq_ignore_ascii_case("document") {
                    let _ = browser
                        .client
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
                    let _ = browser
                        .client
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
                        let _ = browser
                            .client
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
                        let _ = browser
                            .client
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
                let _ = browser
                    .client
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

                let _ = browser
                    .client
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

    // No matching route -- continue the request
    let _ = browser
        .client
        .send_command(
            "Fetch.continueRequest",
            Some(json!({ "requestId": paused.request_id })),
            Some(session_id),
        )
        .await;
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

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

    state.routes.push(RouteEntry {
        url_pattern: url_pattern.clone(),
        response,
        abort,
    });

    // Re-enable Fetch with all route patterns combined.
    // When domain filtering is active, include a wildcard so all requests
    // continue to be intercepted for domain checks.
    let mut patterns: Vec<Value> = state
        .routes
        .iter()
        .map(|r| json!({ "urlPattern": r.url_pattern }))
        .collect();
    if state.domain_filter.is_some() && !patterns.iter().any(|p| p["urlPattern"] == "*") {
        patterns.push(json!({ "urlPattern": "*" }));
    }

    mgr.client
        .send_command(
            "Fetch.enable",
            Some(json!({ "patterns": patterns })),
            Some(&session_id),
        )
        .await?;

    Ok(json!({ "routed": url_pattern }))
}

async fn handle_unroute(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();

    let url = cmd.get("url").and_then(|v| v.as_str());

    match url {
        Some(pattern) => {
            state.routes.retain(|r| r.url_pattern != pattern);
        }
        None => {
            state.routes.clear();
        }
    }

    if state.routes.is_empty() {
        if state.domain_filter.is_some() {
            // Domain filtering still needs Fetch interception; reset to wildcard
            mgr.client
                .send_command(
                    "Fetch.enable",
                    Some(json!({ "patterns": [{ "urlPattern": "*" }] })),
                    Some(&session_id),
                )
                .await?;
        } else {
            mgr.client
                .send_command("Fetch.disable", None, Some(&session_id))
                .await?;
        }
    } else {
        let patterns: Vec<Value> = state
            .routes
            .iter()
            .map(|r| json!({ "urlPattern": r.url_pattern }))
            .collect();
        mgr.client
            .send_command(
                "Fetch.enable",
                Some(json!({ "patterns": patterns })),
                Some(&session_id),
            )
            .await?;
    }

    let label = url.unwrap_or("all");
    Ok(json!({ "unrouted": label }))
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
    let requests: Vec<&TrackedRequest> = if let Some(f) = filter {
        state
            .tracked_requests
            .iter()
            .filter(|r| r.url.contains(f))
            .collect()
    } else {
        state.tracked_requests.iter().collect()
    };

    Ok(json!({ "requests": requests }))
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

    let mut headers = std::collections::HashMap::new();
    headers.insert("Authorization".to_string(), format!("Basic {}", encoded));
    network::set_extra_headers(&mgr.client, &session_id, &headers).await?;

    Ok(json!({ "set": true }))
}

// ---------------------------------------------------------------------------
// Auth handlers
// ---------------------------------------------------------------------------

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
    mgr.navigate(&url, WaitUntil::Load).await?;

    let session_id = mgr.active_session_id()?.to_string();

    let auto_user_selectors = [
        "input[type=email]",
        "input[name=email]",
        "input[type=text][name*=user]",
        "input[id*=user]",
        "input[type=text]",
    ];
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
        s
    } else {
        let mut found = None;
        for sel in &auto_user_selectors {
            let js = format!(
                "!!document.querySelector({})",
                serde_json::to_string(sel).unwrap_or_default()
            );
            if let Ok(val) = mgr.evaluate(&js, None).await {
                if val.as_bool().unwrap_or(false) {
                    found = Some(sel.to_string());
                    break;
                }
            }
        }
        found.ok_or("Could not find username field")?
    };
    interaction::fill(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &user_sel,
        &username,
    )
    .await?;

    // Find and fill password
    let pass_sel = password_sel.unwrap_or_else(|| "input[type=password]".to_string());
    interaction::fill(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &pass_sel,
        &password,
    )
    .await?;

    // Find and click submit
    let sub_sel = if let Some(s) = submit_sel {
        s
    } else {
        let mut found = None;
        for sel in &auto_submit_selectors {
            let js = format!(
                "!!document.querySelector({})",
                serde_json::to_string(sel).unwrap_or_default()
            );
            if let Ok(val) = mgr.evaluate(&js, None).await {
                if val.as_bool().unwrap_or(false) {
                    found = Some(sel.to_string());
                    break;
                }
            }
        }
        found.ok_or("Could not find submit button")?
    };
    interaction::click(
        &mgr.client,
        &session_id,
        &state.ref_map,
        &sub_sel,
        "left",
        1,
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

async fn handle_input_mouse(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let event_type = cmd
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("mouseMoved");
    let x = cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);

    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({
                "type": event_type, "x": x, "y": y,
                "button": cmd.get("button").and_then(|v| v.as_str()).unwrap_or("none"),
                "clickCount": cmd.get("clickCount").and_then(|v| v.as_i64()).unwrap_or(0),
                "deltaX": cmd.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0),
                "deltaY": cmd.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0),
            })),
            Some(&session_id),
        )
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

async fn handle_mousemove(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let x = cmd.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let y = cmd.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);

    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mouseMoved", "x": x, "y": y })),
            Some(&session_id),
        )
        .await?;
    Ok(json!({ "moved": true }))
}

async fn handle_mousedown(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let button = cmd.get("button").and_then(|v| v.as_str()).unwrap_or("left");

    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mousePressed", "x": 0, "y": 0, "button": button, "clickCount": 1 })),
            Some(&session_id),
        )
        .await?;
    Ok(json!({ "pressed": true }))
}

async fn handle_mouseup(cmd: &Value, state: &DaemonState) -> Result<Value, String> {
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let session_id = mgr.active_session_id()?.to_string();
    let button = cmd.get("button").and_then(|v| v.as_str()).unwrap_or("left");

    mgr.client
        .send_command(
            "Input.dispatchMouseEvent",
            Some(json!({ "type": "mouseReleased", "x": 0, "y": 0, "button": button, "clickCount": 1 })),
            Some(&session_id),
        )
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

    #[test]
    fn test_daemon_state_new() {
        let state = DaemonState::new();
        assert!(state.browser.is_none());
        assert!(state.domain_filter.is_none());
        assert_eq!(state.session_id, "default");
        assert!(!state.tracing_state.active);
        assert!(!state.recording_state.active);
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
        state.domain_filter = Some(DomainFilter::new("example.com"));
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
}
