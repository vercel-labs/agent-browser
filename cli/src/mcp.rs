//! Stdio MCP server for exposing agent-browser to MCP clients.
//!
//! The server keeps stdout exclusively for newline-delimited JSON-RPC
//! messages. Tool calls are delegated to the current binary in `--json` mode
//! so MCP behavior stays aligned with the normal CLI command surface.

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const TOOL_LIST_PAGE_SIZE: usize = 64;
const TOOL_OPEN: &str = "agent_browser_open";
const TOOL_BACK: &str = "agent_browser_back";
const TOOL_FORWARD: &str = "agent_browser_forward";
const TOOL_RELOAD: &str = "agent_browser_reload";
const TOOL_SNAPSHOT: &str = "agent_browser_snapshot";
const TOOL_CLICK: &str = "agent_browser_click";
const TOOL_DBLCLICK: &str = "agent_browser_dblclick";
const TOOL_FILL: &str = "agent_browser_fill";
const TOOL_TYPE: &str = "agent_browser_type";
const TOOL_PRESS: &str = "agent_browser_press";
const TOOL_KEYDOWN: &str = "agent_browser_keydown";
const TOOL_KEYUP: &str = "agent_browser_keyup";
const TOOL_KEYBOARD_TYPE: &str = "agent_browser_keyboard_type";
const TOOL_KEYBOARD_INSERT_TEXT: &str = "agent_browser_keyboard_insert_text";
const TOOL_HOVER: &str = "agent_browser_hover";
const TOOL_FOCUS: &str = "agent_browser_focus";
const TOOL_CHECK: &str = "agent_browser_check";
const TOOL_UNCHECK: &str = "agent_browser_uncheck";
const TOOL_SELECT: &str = "agent_browser_select";
const TOOL_DRAG: &str = "agent_browser_drag";
const TOOL_UPLOAD: &str = "agent_browser_upload";
const TOOL_DOWNLOAD: &str = "agent_browser_download";
const TOOL_SCROLL: &str = "agent_browser_scroll";
const TOOL_SCROLL_INTO_VIEW: &str = "agent_browser_scroll_into_view";
const TOOL_WAIT_MS: &str = "agent_browser_wait_ms";
const TOOL_WAIT_FOR_SELECTOR: &str = "agent_browser_wait_for_selector";
const TOOL_WAIT_FOR_TEXT: &str = "agent_browser_wait_for_text";
const TOOL_WAIT_FOR_URL: &str = "agent_browser_wait_for_url";
const TOOL_WAIT_FOR_LOAD: &str = "agent_browser_wait_for_load";
const TOOL_WAIT_FOR_FUNCTION: &str = "agent_browser_wait_for_function";
const TOOL_WAIT_FOR_DOWNLOAD: &str = "agent_browser_wait_for_download";
const TOOL_SCREENSHOT: &str = "agent_browser_screenshot";
const TOOL_PDF: &str = "agent_browser_pdf";
const TOOL_GET_TEXT: &str = "agent_browser_get_text";
const TOOL_GET_HTML: &str = "agent_browser_get_html";
const TOOL_GET_VALUE: &str = "agent_browser_get_value";
const TOOL_GET_ATTR: &str = "agent_browser_get_attr";
const TOOL_GET_COUNT: &str = "agent_browser_get_count";
const TOOL_GET_BOX: &str = "agent_browser_get_box";
const TOOL_GET_STYLES: &str = "agent_browser_get_styles";
const TOOL_GET_URL: &str = "agent_browser_get_url";
const TOOL_GET_TITLE: &str = "agent_browser_get_title";
const TOOL_GET_CDP_URL: &str = "agent_browser_get_cdp_url";
const TOOL_IS_VISIBLE: &str = "agent_browser_is_visible";
const TOOL_IS_ENABLED: &str = "agent_browser_is_enabled";
const TOOL_IS_CHECKED: &str = "agent_browser_is_checked";
const TOOL_FIND: &str = "agent_browser_find";
const TOOL_MOUSE_MOVE: &str = "agent_browser_mouse_move";
const TOOL_MOUSE_DOWN: &str = "agent_browser_mouse_down";
const TOOL_MOUSE_UP: &str = "agent_browser_mouse_up";
const TOOL_MOUSE_WHEEL: &str = "agent_browser_mouse_wheel";
const TOOL_SET_VIEWPORT: &str = "agent_browser_set_viewport";
const TOOL_SET_DEVICE: &str = "agent_browser_set_device";
const TOOL_SET_GEO: &str = "agent_browser_set_geo";
const TOOL_SET_OFFLINE: &str = "agent_browser_set_offline";
const TOOL_SET_HEADERS: &str = "agent_browser_set_headers";
const TOOL_SET_CREDENTIALS: &str = "agent_browser_set_credentials";
const TOOL_SET_MEDIA: &str = "agent_browser_set_media";
const TOOL_NETWORK_ROUTE: &str = "agent_browser_network_route";
const TOOL_NETWORK_UNROUTE: &str = "agent_browser_network_unroute";
const TOOL_NETWORK_REQUESTS: &str = "agent_browser_network_requests";
const TOOL_NETWORK_REQUEST: &str = "agent_browser_network_request";
const TOOL_NETWORK_HAR_START: &str = "agent_browser_network_har_start";
const TOOL_NETWORK_HAR_STOP: &str = "agent_browser_network_har_stop";
const TOOL_STORAGE_GET: &str = "agent_browser_storage_get";
const TOOL_STORAGE_SET: &str = "agent_browser_storage_set";
const TOOL_STORAGE_CLEAR: &str = "agent_browser_storage_clear";
const TOOL_COOKIES_GET: &str = "agent_browser_cookies_get";
const TOOL_COOKIES_SET: &str = "agent_browser_cookies_set";
const TOOL_COOKIES_SET_CURL: &str = "agent_browser_cookies_set_curl";
const TOOL_COOKIES_CLEAR: &str = "agent_browser_cookies_clear";
const TOOL_TAB_NEW: &str = "agent_browser_tab_new";
const TOOL_TAB_LIST: &str = "agent_browser_tab_list";
const TOOL_TAB_SWITCH: &str = "agent_browser_tab_switch";
const TOOL_TAB_CLOSE: &str = "agent_browser_tab_close";
const TOOL_WINDOW_NEW: &str = "agent_browser_window_new";
const TOOL_FRAME_SWITCH: &str = "agent_browser_frame_switch";
const TOOL_FRAME_MAIN: &str = "agent_browser_frame_main";
const TOOL_DIALOG_STATUS: &str = "agent_browser_dialog_status";
const TOOL_DIALOG_ACCEPT: &str = "agent_browser_dialog_accept";
const TOOL_DIALOG_DISMISS: &str = "agent_browser_dialog_dismiss";
const TOOL_TRACE_START: &str = "agent_browser_trace_start";
const TOOL_TRACE_STOP: &str = "agent_browser_trace_stop";
const TOOL_PROFILER_START: &str = "agent_browser_profiler_start";
const TOOL_PROFILER_STOP: &str = "agent_browser_profiler_stop";
const TOOL_RECORD_START: &str = "agent_browser_record_start";
const TOOL_RECORD_STOP: &str = "agent_browser_record_stop";
const TOOL_RECORD_RESTART: &str = "agent_browser_record_restart";
const TOOL_CONSOLE: &str = "agent_browser_console";
const TOOL_ERRORS: &str = "agent_browser_errors";
const TOOL_HIGHLIGHT: &str = "agent_browser_highlight";
const TOOL_INSPECT: &str = "agent_browser_inspect";
const TOOL_CLIPBOARD_READ: &str = "agent_browser_clipboard_read";
const TOOL_CLIPBOARD_WRITE: &str = "agent_browser_clipboard_write";
const TOOL_CLIPBOARD_COPY: &str = "agent_browser_clipboard_copy";
const TOOL_CLIPBOARD_PASTE: &str = "agent_browser_clipboard_paste";
const TOOL_AUTH_SAVE: &str = "agent_browser_auth_save";
const TOOL_AUTH_LOGIN: &str = "agent_browser_auth_login";
const TOOL_AUTH_LIST: &str = "agent_browser_auth_list";
const TOOL_AUTH_SHOW: &str = "agent_browser_auth_show";
const TOOL_AUTH_DELETE: &str = "agent_browser_auth_delete";
const TOOL_STATE_SAVE: &str = "agent_browser_state_save";
const TOOL_STATE_LOAD: &str = "agent_browser_state_load";
const TOOL_STATE_LIST: &str = "agent_browser_state_list";
const TOOL_STATE_CLEAR: &str = "agent_browser_state_clear";
const TOOL_STATE_SHOW: &str = "agent_browser_state_show";
const TOOL_STATE_CLEAN: &str = "agent_browser_state_clean";
const TOOL_STATE_RENAME: &str = "agent_browser_state_rename";
const TOOL_TAP: &str = "agent_browser_tap";
const TOOL_SWIPE: &str = "agent_browser_swipe";
const TOOL_DEVICE: &str = "agent_browser_device";
const TOOL_DIFF_SNAPSHOT: &str = "agent_browser_diff_snapshot";
const TOOL_DIFF_SCREENSHOT: &str = "agent_browser_diff_screenshot";
const TOOL_DIFF_URL: &str = "agent_browser_diff_url";
const TOOL_BATCH: &str = "agent_browser_batch";
const TOOL_REACT_TREE: &str = "agent_browser_react_tree";
const TOOL_REACT_INSPECT: &str = "agent_browser_react_inspect";
const TOOL_REACT_RENDERS_START: &str = "agent_browser_react_renders_start";
const TOOL_REACT_RENDERS_STOP: &str = "agent_browser_react_renders_stop";
const TOOL_REACT_SUSPENSE: &str = "agent_browser_react_suspense";
const TOOL_VITALS: &str = "agent_browser_vitals";
const TOOL_PUSHSTATE: &str = "agent_browser_pushstate";
const TOOL_REMOVE_INIT_SCRIPT: &str = "agent_browser_remove_init_script";
const TOOL_CONFIRM: &str = "agent_browser_confirm";
const TOOL_DENY: &str = "agent_browser_deny";
const TOOL_CONNECT: &str = "agent_browser_connect";
const TOOL_STREAM_ENABLE: &str = "agent_browser_stream_enable";
const TOOL_STREAM_DISABLE: &str = "agent_browser_stream_disable";
const TOOL_STREAM_STATUS: &str = "agent_browser_stream_status";
const TOOL_SESSION: &str = "agent_browser_session";
const TOOL_SESSION_LIST: &str = "agent_browser_session_list";
const TOOL_PROFILES: &str = "agent_browser_profiles";
const TOOL_SKILLS_LIST: &str = "agent_browser_skills_list";
const TOOL_SKILLS_GET: &str = "agent_browser_skills_get";
const TOOL_SKILLS_PATH: &str = "agent_browser_skills_path";
const TOOL_PLUGIN_ADD: &str = "agent_browser_plugin_add";
const TOOL_PLUGIN_LIST: &str = "agent_browser_plugin_list";
const TOOL_PLUGIN_SHOW: &str = "agent_browser_plugin_show";
const TOOL_PLUGIN_RUN: &str = "agent_browser_plugin_run";
const TOOL_DOCTOR: &str = "agent_browser_doctor";
const TOOL_DASHBOARD_START: &str = "agent_browser_dashboard_start";
const TOOL_DASHBOARD_STOP: &str = "agent_browser_dashboard_stop";
const TOOL_INSTALL: &str = "agent_browser_install";
const TOOL_UPGRADE: &str = "agent_browser_upgrade";
const TOOL_CHAT: &str = "agent_browser_chat";
const TOOL_EVAL: &str = "agent_browser_eval";
const TOOL_CLOSE: &str = "agent_browser_close";
const TOOL_TOOLS_PROFILES: &str = "agent_browser_tools_profiles";
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const RAW_JSON_ARG: &str = "--raw-json";

#[derive(Debug)]
struct ProtocolError {
    code: i64,
    message: String,
}

impl ProtocolError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {}", method),
        }
    }
}

#[derive(Debug)]
struct CliRun {
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone)]
struct McpConfig {
    profiles: Vec<ToolProfile>,
    enabled_tools: Option<BTreeSet<&'static str>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolProfile {
    Core,
    Network,
    State,
    Debug,
    Tabs,
    React,
    Mobile,
    All,
}

impl ToolProfile {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "core" | "default" => Some(Self::Core),
            "network" => Some(Self::Network),
            "state" | "storage" | "auth" => Some(Self::State),
            "debug" | "diagnostics" => Some(Self::Debug),
            "tabs" | "frames" => Some(Self::Tabs),
            "react" | "web" => Some(Self::React),
            "mobile" | "ios" => Some(Self::Mobile),
            "all" | "full" => Some(Self::All),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Network => "network",
            Self::State => "state",
            Self::Debug => "debug",
            Self::Tabs => "tabs",
            Self::React => "react",
            Self::Mobile => "mobile",
            Self::All => "all",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Core => "Everyday browser automation with navigation, snapshots, common interaction, waits, screenshots, basic reads, tab basics, JavaScript eval, close, and profile discovery.",
            Self::Network => "Network interception, request inspection, HAR capture, headers, credentials, and offline mode.",
            Self::State => "Cookies, storage, auth profiles, saved browser state, sessions, Chrome profiles, and bundled skills.",
            Self::Debug => "Console/errors, highlighting, DevTools, tracing, profiling, PDF, downloads/uploads, recording, clipboard, plugin registry and plugin command.run, doctor, dashboard, install, upgrade, and chat.",
            Self::Tabs => "Tab, window, frame, and JavaScript dialog management.",
            Self::React => "React tree inspection, render recording, Suspense inspection, Web Vitals, SPA pushstate, and init-script removal.",
            Self::Mobile => "Viewport/device/geolocation/media emulation plus touch, swipe, and lower-level mouse tools.",
            Self::All => "Every MCP tool, including the full typed CLI parity surface.",
        }
    }

    fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Core => CORE_PROFILE_TOOLS,
            Self::Network => NETWORK_PROFILE_TOOLS,
            Self::State => STATE_PROFILE_TOOLS,
            Self::Debug => DEBUG_PROFILE_TOOLS,
            Self::Tabs => TABS_PROFILE_TOOLS,
            Self::React => REACT_PROFILE_TOOLS,
            Self::Mobile => MOBILE_PROFILE_TOOLS,
            Self::All => &[],
        }
    }
}

impl McpConfig {
    fn from_profiles(profiles: Vec<ToolProfile>) -> Self {
        if profiles.contains(&ToolProfile::All) {
            return Self {
                profiles: vec![ToolProfile::All],
                enabled_tools: None,
            };
        }

        let mut enabled_tools = BTreeSet::new();
        for profile in &profiles {
            enabled_tools.extend(profile.tools().iter().copied());
        }

        Self {
            profiles,
            enabled_tools: Some(enabled_tools),
        }
    }

    fn core() -> Self {
        Self::from_profiles(vec![ToolProfile::Core])
    }

    #[cfg(test)]
    fn all() -> Self {
        Self::from_profiles(vec![ToolProfile::All])
    }

    fn allows(&self, name: &str) -> bool {
        match &self.enabled_tools {
            Some(enabled_tools) => enabled_tools.contains(name),
            None => true,
        }
    }

    fn profile_names(&self) -> Vec<&'static str> {
        self.profiles.iter().map(|profile| profile.name()).collect()
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self::core()
    }
}

const CORE_PROFILE_TOOLS: &[&str] = &[
    TOOL_TOOLS_PROFILES,
    TOOL_OPEN,
    TOOL_SNAPSHOT,
    TOOL_BACK,
    TOOL_FORWARD,
    TOOL_RELOAD,
    TOOL_CLICK,
    TOOL_FILL,
    TOOL_TYPE,
    TOOL_PRESS,
    TOOL_CHECK,
    TOOL_UNCHECK,
    TOOL_SELECT,
    TOOL_SCROLL,
    TOOL_WAIT_MS,
    TOOL_WAIT_FOR_SELECTOR,
    TOOL_WAIT_FOR_TEXT,
    TOOL_WAIT_FOR_LOAD,
    TOOL_SCREENSHOT,
    TOOL_GET_TEXT,
    TOOL_GET_URL,
    TOOL_GET_TITLE,
    TOOL_TAB_NEW,
    TOOL_TAB_LIST,
    TOOL_TAB_SWITCH,
    TOOL_TAB_CLOSE,
    TOOL_EVAL,
    TOOL_CLOSE,
];

const NETWORK_PROFILE_TOOLS: &[&str] = &[
    TOOL_SET_HEADERS,
    TOOL_SET_CREDENTIALS,
    TOOL_SET_OFFLINE,
    TOOL_NETWORK_ROUTE,
    TOOL_NETWORK_UNROUTE,
    TOOL_NETWORK_REQUESTS,
    TOOL_NETWORK_REQUEST,
    TOOL_NETWORK_HAR_START,
    TOOL_NETWORK_HAR_STOP,
];

const STATE_PROFILE_TOOLS: &[&str] = &[
    TOOL_STORAGE_GET,
    TOOL_STORAGE_SET,
    TOOL_STORAGE_CLEAR,
    TOOL_COOKIES_GET,
    TOOL_COOKIES_SET,
    TOOL_COOKIES_SET_CURL,
    TOOL_COOKIES_CLEAR,
    TOOL_AUTH_SAVE,
    TOOL_AUTH_LOGIN,
    TOOL_AUTH_LIST,
    TOOL_AUTH_SHOW,
    TOOL_AUTH_DELETE,
    TOOL_STATE_SAVE,
    TOOL_STATE_LOAD,
    TOOL_STATE_LIST,
    TOOL_STATE_CLEAR,
    TOOL_STATE_SHOW,
    TOOL_STATE_CLEAN,
    TOOL_STATE_RENAME,
    TOOL_SESSION,
    TOOL_SESSION_LIST,
    TOOL_PROFILES,
    TOOL_SKILLS_LIST,
    TOOL_SKILLS_GET,
    TOOL_SKILLS_PATH,
];

const DEBUG_PROFILE_TOOLS: &[&str] = &[
    TOOL_WAIT_FOR_DOWNLOAD,
    TOOL_PDF,
    TOOL_UPLOAD,
    TOOL_DOWNLOAD,
    TOOL_TRACE_START,
    TOOL_TRACE_STOP,
    TOOL_PROFILER_START,
    TOOL_PROFILER_STOP,
    TOOL_RECORD_START,
    TOOL_RECORD_STOP,
    TOOL_RECORD_RESTART,
    TOOL_CONSOLE,
    TOOL_ERRORS,
    TOOL_HIGHLIGHT,
    TOOL_INSPECT,
    TOOL_CLIPBOARD_READ,
    TOOL_CLIPBOARD_WRITE,
    TOOL_CLIPBOARD_COPY,
    TOOL_CLIPBOARD_PASTE,
    TOOL_DIFF_SNAPSHOT,
    TOOL_DIFF_SCREENSHOT,
    TOOL_DIFF_URL,
    TOOL_BATCH,
    TOOL_CONFIRM,
    TOOL_DENY,
    TOOL_CONNECT,
    TOOL_STREAM_ENABLE,
    TOOL_STREAM_DISABLE,
    TOOL_STREAM_STATUS,
    TOOL_PLUGIN_ADD,
    TOOL_PLUGIN_LIST,
    TOOL_PLUGIN_SHOW,
    TOOL_PLUGIN_RUN,
    TOOL_DOCTOR,
    TOOL_DASHBOARD_START,
    TOOL_DASHBOARD_STOP,
    TOOL_INSTALL,
    TOOL_UPGRADE,
    TOOL_CHAT,
];

const TABS_PROFILE_TOOLS: &[&str] = &[
    TOOL_BACK,
    TOOL_FORWARD,
    TOOL_RELOAD,
    TOOL_TAB_NEW,
    TOOL_TAB_LIST,
    TOOL_TAB_SWITCH,
    TOOL_TAB_CLOSE,
    TOOL_WINDOW_NEW,
    TOOL_FRAME_SWITCH,
    TOOL_FRAME_MAIN,
    TOOL_DIALOG_STATUS,
    TOOL_DIALOG_ACCEPT,
    TOOL_DIALOG_DISMISS,
];

const REACT_PROFILE_TOOLS: &[&str] = &[
    TOOL_REACT_TREE,
    TOOL_REACT_INSPECT,
    TOOL_REACT_RENDERS_START,
    TOOL_REACT_RENDERS_STOP,
    TOOL_REACT_SUSPENSE,
    TOOL_VITALS,
    TOOL_PUSHSTATE,
    TOOL_REMOVE_INIT_SCRIPT,
];

const MOBILE_PROFILE_TOOLS: &[&str] = &[
    TOOL_KEYDOWN,
    TOOL_KEYUP,
    TOOL_KEYBOARD_TYPE,
    TOOL_KEYBOARD_INSERT_TEXT,
    TOOL_MOUSE_MOVE,
    TOOL_MOUSE_DOWN,
    TOOL_MOUSE_UP,
    TOOL_MOUSE_WHEEL,
    TOOL_SET_VIEWPORT,
    TOOL_SET_DEVICE,
    TOOL_SET_GEO,
    TOOL_SET_MEDIA,
    TOOL_TAP,
    TOOL_SWIPE,
    TOOL_DEVICE,
];

/// Run the MCP stdio server until stdin closes or a `shutdown` request is
/// received.
pub fn run_mcp(args: &[String]) -> Result<(), String> {
    let config = parse_mcp_config(args)?;
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let mut exit_after_response = false;
        let response = match line {
            Ok(line) => handle_line(&line, &config, &mut exit_after_response),
            Err(e) => Some(error_response(
                Value::Null,
                -32603,
                format!("Failed to read stdin: {}", e),
            )),
        };

        if let Some(response) = response {
            if write_json_line(&mut stdout, &response).is_err() {
                break;
            }
        }

        if exit_after_response {
            break;
        }
    }

    Ok(())
}

fn parse_mcp_config(args: &[String]) -> Result<McpConfig, String> {
    let mut tools_arg: Option<String> = None;
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if arg == "--tools" {
            let Some(value) = args.get(i + 1) else {
                return Err("Missing value for --tools".to_string());
            };
            tools_arg = Some(value.to_string());
            i += 2;
        } else if let Some(value) = arg.strip_prefix("--tools=") {
            tools_arg = Some(value.to_string());
            i += 1;
        } else {
            return Err(format!(
                "Unknown mcp option: {}\nUsage: agent-browser mcp [--tools <profiles>]",
                arg
            ));
        }
    }

    let Some(tools_arg) = tools_arg else {
        return Ok(McpConfig::default());
    };

    let mut profiles = Vec::new();
    for raw_name in tools_arg.split(',') {
        let name = raw_name.trim();
        if name.is_empty() {
            continue;
        }
        let Some(profile) = ToolProfile::parse(name) else {
            return Err(format!(
                "Unknown MCP tools profile: {}\nValid profiles: {}",
                name,
                tool_profile_names().join(", ")
            ));
        };
        profiles.push(profile);
    }

    if profiles.is_empty() {
        return Err("Missing value for --tools".to_string());
    }

    Ok(McpConfig::from_profiles(profiles))
}

fn handle_line(line: &str, config: &McpConfig, exit_after_response: &mut bool) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(e) => {
            return Some(error_response(
                Value::Null,
                -32700,
                format!("Parse error: {}", e),
            ));
        }
    };

    let id = message.get("id").cloned();
    let method = match message.get("method").and_then(|v| v.as_str()) {
        Some(method) => method,
        None => {
            return id.map(|id| error_response(id, -32600, "Invalid request: missing method"));
        }
    };

    // Notifications do not receive responses.
    let id = id?;

    match handle_request(method, message.get("params"), config, exit_after_response) {
        Ok(result) => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        })),
        Err(err) => Some(error_response(id, err.code, err.message)),
    }
}

fn handle_request(
    method: &str,
    params: Option<&Value>,
    config: &McpConfig,
    exit_after_response: &mut bool,
) -> Result<Value, ProtocolError> {
    match method {
        "initialize" => Ok(initialize_result(params, config)),
        "ping" => Ok(json!({})),
        "tools/list" => list_tools(params, config),
        "tools/call" => call_tool(params, config),
        "shutdown" => {
            *exit_after_response = true;
            Ok(json!({}))
        }
        _ => Err(ProtocolError::method_not_found(method)),
    }
}

fn list_tools(params: Option<&Value>, config: &McpConfig) -> Result<Value, ProtocolError> {
    let tools = tools_for_config(config);
    let start = tool_list_cursor(params, tools.len())?;
    let end = (start + TOOL_LIST_PAGE_SIZE).min(tools.len());
    let mut result = json!({
        "tools": tools[start..end].to_vec(),
    });

    if end < tools.len() {
        result["nextCursor"] = json!(end.to_string());
    }

    Ok(result)
}

fn tool_list_cursor(params: Option<&Value>, total: usize) -> Result<usize, ProtocolError> {
    let Some(cursor) = params.and_then(|p| p.get("cursor")) else {
        return Ok(0);
    };

    let cursor = cursor
        .as_str()
        .ok_or_else(|| ProtocolError::invalid_params("tools/list cursor must be a string"))?;
    let index = cursor
        .parse::<usize>()
        .map_err(|_| ProtocolError::invalid_params("Invalid tools/list cursor"))?;

    if index > total {
        return Err(ProtocolError::invalid_params("Invalid tools/list cursor"));
    }

    Ok(index)
}

fn initialize_result(params: Option<&Value>, config: &McpConfig) -> Value {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(|v| v.as_str())
        .unwrap_or(PROTOCOL_VERSION);
    let protocol_version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&requested) {
        requested
    } else {
        PROTOCOL_VERSION
    };

    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "agent-browser",
            "title": "agent-browser",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": format!(
            "Use the typed agent_browser_* tools to control a browser. Active MCP tools profile(s): {}. Prefer agent_browser_snapshot after navigation to obtain stable element refs before clicking or typing. Use agent_browser_tools_profiles to see available startup profiles.",
            config.profile_names().join(", ")
        )
    })
}

fn tools_for_config(config: &McpConfig) -> Vec<Value> {
    tools()
        .into_iter()
        .filter(|tool| {
            tool.get("name")
                .and_then(|name| name.as_str())
                .is_some_and(|name| config.allows(name))
        })
        .collect()
}

fn tool_profile_names() -> Vec<&'static str> {
    [
        ToolProfile::Core,
        ToolProfile::Network,
        ToolProfile::State,
        ToolProfile::Debug,
        ToolProfile::Tabs,
        ToolProfile::React,
        ToolProfile::Mobile,
        ToolProfile::All,
    ]
    .iter()
    .map(|profile| profile.name())
    .collect()
}

fn tool_profile_summaries() -> Vec<Value> {
    [
        ToolProfile::Core,
        ToolProfile::Network,
        ToolProfile::State,
        ToolProfile::Debug,
        ToolProfile::Tabs,
        ToolProfile::React,
        ToolProfile::Mobile,
        ToolProfile::All,
    ]
    .iter()
    .map(|profile| {
        let tool_count = if *profile == ToolProfile::All {
            tools().len()
        } else {
            profile.tools().len()
        };
        json!({
            "name": profile.name(),
            "description": profile.description(),
            "toolCount": tool_count,
            "usage": format!("agent-browser mcp --tools {}", profile.name()),
        })
    })
    .collect()
}

fn tools() -> Vec<Value> {
    let mut tools = vec![
        tool(
            TOOL_TOOLS_PROFILES,
            "MCP tool profiles",
            "List MCP startup tool profiles and how to enable them.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_OPEN,
            "Open page",
            "Launch the browser and optionally navigate to a URL.",
            json!({
                "url": { "type": "string", "description": "URL to open. Omit to launch about:blank." },
                "headed": { "type": "boolean", "default": false, "description": "Show the browser window." }
            }),
            &[],
        ),
        tool(
            TOOL_SNAPSHOT,
            "Snapshot page",
            "Return an accessibility-tree snapshot with stable element refs.",
            json!({
                "interactive": { "type": "boolean", "default": true, "description": "Only include interactive elements." },
                "compact": { "type": "boolean", "default": false, "description": "Remove empty structural elements." },
                "depth": { "type": "integer", "minimum": 0, "description": "Limit tree depth." },
                "selector": { "type": "string", "description": "Scope the snapshot to a CSS selector." },
                "includeUrls": { "type": "boolean", "default": false, "description": "Include href URLs on links." }
            }),
            &[],
        ),
        tool(
            TOOL_CLICK,
            "Click element",
            "Click an element by @ref or CSS selector.",
            json!({
                "selector": selector_schema(),
                "newTab": { "type": "boolean", "default": false, "description": "Open link targets in a new tab." }
            }),
            &["selector"],
        ),
        tool(
            TOOL_FILL,
            "Fill input",
            "Clear and fill an input by @ref or CSS selector.",
            json!({
                "selector": selector_schema(),
                "text": { "type": "string", "description": "Text to fill." }
            }),
            &["selector", "text"],
        ),
        tool(
            TOOL_TYPE,
            "Type text",
            "Type text into an element by @ref or CSS selector.",
            json!({
                "selector": selector_schema(),
                "text": { "type": "string", "description": "Text to type." },
                "clear": { "type": "boolean", "default": false, "description": "Clear the field before typing." },
                "delayMs": { "type": "integer", "minimum": 0, "description": "Delay between keystrokes." }
            }),
            &["selector", "text"],
        ),
        tool(
            TOOL_PRESS,
            "Press key",
            "Press a key at the current focus.",
            json!({
                "key": { "type": "string", "description": "Key name such as Enter, Tab, or Control+a." }
            }),
            &["key"],
        ),
        tool(TOOL_HOVER, "Hover element", "Hover an element.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(TOOL_FOCUS, "Focus element", "Focus an element.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(TOOL_CHECK, "Check element", "Check a checkbox or switch.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(TOOL_UNCHECK, "Uncheck element", "Uncheck a checkbox or switch.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(
            TOOL_SELECT,
            "Select options",
            "Select one or more options in a select element.",
            json!({
                "selector": selector_schema(),
                "values": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Option values or labels to select."
                }
            }),
            &["selector", "values"],
        ),
        tool(
            TOOL_SCROLL,
            "Scroll page",
            "Scroll the page or an element.",
            json!({
                "direction": { "type": "string", "enum": ["up", "down", "left", "right"], "default": "down" },
                "amount": { "type": "integer", "default": 300, "description": "Pixels to scroll." },
                "selector": { "type": "string", "description": "Optional element selector to scroll." }
            }),
            &[],
        ),
        tool(TOOL_SCROLL_INTO_VIEW, "Scroll into view", "Scroll an element into view.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(TOOL_WAIT_MS, "Wait milliseconds", "Wait for a fixed time.", json!({ "ms": { "type": "integer", "minimum": 0 } }), &["ms"]),
        wait_tool(TOOL_WAIT_FOR_SELECTOR, "Wait for selector", "Wait for an element to appear.", json!({ "selector": selector_schema() }), &["selector"]),
        wait_tool(TOOL_WAIT_FOR_TEXT, "Wait for text", "Wait for text to appear.", json!({ "text": { "type": "string" } }), &["text"]),
        wait_tool(TOOL_WAIT_FOR_URL, "Wait for URL", "Wait for the current URL to match a pattern.", json!({ "url": { "type": "string", "description": "URL glob or pattern." } }), &["url"]),
        wait_tool(TOOL_WAIT_FOR_LOAD, "Wait for load state", "Wait for a page load state.", json!({ "state": { "type": "string", "enum": ["load", "domcontentloaded", "networkidle"] } }), &["state"]),
        wait_tool(TOOL_WAIT_FOR_FUNCTION, "Wait for function", "Wait for a JavaScript expression to become truthy.", json!({ "expression": { "type": "string" } }), &["expression"]),
        tool(
            TOOL_SCREENSHOT,
            "Take screenshot",
            "Capture a screenshot and return the saved path. Small PNG/JPEG screenshots are also returned as image content.",
            json!({
                "path": { "type": "string", "description": "Optional output path." },
                "selector": { "type": "string", "description": "Optional @ref or CSS selector to capture." },
                "fullPage": { "type": "boolean", "default": false },
                "annotate": { "type": "boolean", "default": false, "description": "Number visible elements in the screenshot." },
                "format": { "type": "string", "enum": ["png", "jpeg"], "description": "Screenshot format." },
                "quality": { "type": "integer", "minimum": 0, "maximum": 100, "description": "JPEG quality." },
                "screenshotDir": { "type": "string", "description": "Default output directory when path is omitted." }
            }),
            &[],
        ),
        tool(TOOL_GET_TEXT, "Get text", "Get visible text from an element.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(TOOL_GET_HTML, "Get HTML", "Get innerHTML from an element.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(TOOL_GET_VALUE, "Get value", "Get an input value.", json!({ "selector": selector_schema() }), &["selector"]),
        tool(TOOL_GET_URL, "Get URL", "Get the current page URL.", json!({}), &[]),
        tool(TOOL_GET_TITLE, "Get title", "Get the current page title.", json!({}), &[]),
        tool(TOOL_GET_CDP_URL, "Get CDP URL", "Get the current browser CDP URL.", json!({}), &[]),
        tool(
            TOOL_EVAL,
            "Evaluate JavaScript",
            "Run JavaScript in the page using stdin to avoid shell escaping.",
            json!({
                "script": { "type": "string", "description": "JavaScript expression or script to evaluate." }
            }),
            &["script"],
        ),
        tool(
            TOOL_CLOSE,
            "Close browser",
            "Close the current browser session.",
            json!({
                "all": { "type": "boolean", "default": false, "description": "Close all active sessions." }
            }),
            &[],
        ),
    ];
    tools.extend(parity_tools());
    tools
}

fn parity_tools() -> Vec<Value> {
    vec![
        tool(TOOL_BACK, "Back", "Navigate back.", json!({}), &[]),
        tool(TOOL_FORWARD, "Forward", "Navigate forward.", json!({}), &[]),
        tool(TOOL_RELOAD, "Reload", "Reload the page.", json!({}), &[]),
        tool(
            TOOL_DBLCLICK,
            "Double-click element",
            "Double-click an element.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_DRAG,
            "Drag and drop",
            "Drag one element to another.",
            json!({ "source": selector_schema(), "target": selector_schema() }),
            &["source", "target"],
        ),
        tool(
            TOOL_UPLOAD,
            "Upload files",
            "Upload files through a file input.",
            json!({ "selector": selector_schema(), "files": string_array_schema("File paths to upload.") }),
            &["selector", "files"],
        ),
        tool(
            TOOL_DOWNLOAD,
            "Download file",
            "Click an element and save the download.",
            json!({ "selector": selector_schema(), "path": { "type": "string" } }),
            &["selector", "path"],
        ),
        tool(
            TOOL_KEYDOWN,
            "Key down",
            "Press and hold a key.",
            json!({ "key": key_schema() }),
            &["key"],
        ),
        tool(
            TOOL_KEYUP,
            "Key up",
            "Release a key.",
            json!({ "key": key_schema() }),
            &["key"],
        ),
        tool(
            TOOL_KEYBOARD_TYPE,
            "Keyboard type",
            "Type text at the current focus using real key events.",
            json!({ "text": { "type": "string" } }),
            &["text"],
        ),
        tool(
            TOOL_KEYBOARD_INSERT_TEXT,
            "Keyboard insert text",
            "Insert text at the current focus without key events.",
            json!({ "text": { "type": "string" } }),
            &["text"],
        ),
        tool(
            TOOL_WAIT_FOR_DOWNLOAD,
            "Wait for download",
            "Wait for a browser download.",
            json!({ "path": { "type": "string", "description": "Optional output path." }, "waitTimeoutMs": wait_timeout_schema() }),
            &[],
        ),
        tool(
            TOOL_PDF,
            "Save PDF",
            "Save the current page as PDF.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            TOOL_GET_ATTR,
            "Get attribute",
            "Get an element attribute.",
            json!({ "selector": selector_schema(), "name": { "type": "string" } }),
            &["selector", "name"],
        ),
        tool(
            TOOL_GET_COUNT,
            "Get count",
            "Count matching elements.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_GET_BOX,
            "Get box",
            "Get an element bounding box.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_GET_STYLES,
            "Get styles",
            "Get computed styles for an element.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_IS_VISIBLE,
            "Is visible",
            "Check whether an element is visible.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_IS_ENABLED,
            "Is enabled",
            "Check whether an element is enabled.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_IS_CHECKED,
            "Is checked",
            "Check whether an element is checked.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_FIND,
            "Find element",
            "Find an element with semantic locators and optionally act on it.",
            json!({
                "locator": { "type": "string", "enum": ["role", "text", "label", "placeholder", "alt", "title", "testid", "first", "last", "nth"] },
                "value": { "type": "string", "description": "Role, text, label, selector, or test id." },
                "action": { "type": "string", "description": "Optional action: click, fill, type, hover, focus, check, uncheck, text." },
                "text": { "type": "string", "description": "Optional text/value for fill or type actions." },
                "index": { "type": "integer", "description": "Index for nth locator." },
                "name": { "type": "string", "description": "Accessible name filter for role locator." },
                "exact": { "type": "boolean", "default": false }
            }),
            &["locator", "value"],
        ),
        tool(
            TOOL_MOUSE_MOVE,
            "Mouse move",
            "Move the mouse.",
            json!({ "x": number_schema(), "y": number_schema() }),
            &["x", "y"],
        ),
        tool(
            TOOL_MOUSE_DOWN,
            "Mouse down",
            "Press a mouse button.",
            json!({ "button": mouse_button_schema() }),
            &[],
        ),
        tool(
            TOOL_MOUSE_UP,
            "Mouse up",
            "Release a mouse button.",
            json!({ "button": mouse_button_schema() }),
            &[],
        ),
        tool(
            TOOL_MOUSE_WHEEL,
            "Mouse wheel",
            "Scroll with the mouse wheel.",
            json!({ "dy": number_schema(), "dx": number_schema() }),
            &["dy"],
        ),
        tool(
            TOOL_SET_VIEWPORT,
            "Set viewport",
            "Set viewport size.",
            json!({ "width": int_schema(), "height": int_schema(), "scale": number_schema() }),
            &["width", "height"],
        ),
        tool(
            TOOL_SET_DEVICE,
            "Set device",
            "Emulate a device by name.",
            json!({ "device": { "type": "string" } }),
            &["device"],
        ),
        tool(
            TOOL_SET_GEO,
            "Set geolocation",
            "Set geolocation.",
            json!({ "latitude": number_schema(), "longitude": number_schema() }),
            &["latitude", "longitude"],
        ),
        tool(
            TOOL_SET_OFFLINE,
            "Set offline",
            "Toggle offline mode.",
            json!({ "enabled": { "type": "boolean" } }),
            &["enabled"],
        ),
        tool(
            TOOL_SET_HEADERS,
            "Set headers",
            "Set extra HTTP headers from a JSON object.",
            json!({ "headers": { "type": "object", "additionalProperties": { "type": "string" } } }),
            &["headers"],
        ),
        tool(
            TOOL_SET_CREDENTIALS,
            "Set credentials",
            "Set HTTP credentials.",
            json!({ "username": { "type": "string" }, "password": { "type": "string" } }),
            &["username", "password"],
        ),
        tool(
            TOOL_SET_MEDIA,
            "Set media",
            "Set media emulation.",
            json!({ "colorScheme": { "type": "string", "enum": ["dark", "light", "no-preference"] }, "reducedMotion": { "type": "string", "enum": ["reduce", "no-preference"] } }),
            &[],
        ),
        tool(
            TOOL_NETWORK_ROUTE,
            "Network route",
            "Route matching requests.",
            json!({ "url": { "type": "string" }, "abort": { "type": "boolean" }, "body": { "type": "string" }, "resourceType": { "type": "string", "description": "Comma-separated resource types." } }),
            &["url"],
        ),
        tool(
            TOOL_NETWORK_UNROUTE,
            "Network unroute",
            "Remove network routes.",
            json!({ "url": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_NETWORK_REQUESTS,
            "Network requests",
            "List captured network requests.",
            json!({ "clear": { "type": "boolean" }, "filter": { "type": "string" }, "type": { "type": "string" }, "method": { "type": "string" }, "status": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_NETWORK_REQUEST,
            "Network request detail",
            "Show one request by id.",
            json!({ "requestId": { "type": "string" } }),
            &["requestId"],
        ),
        tool(
            TOOL_NETWORK_HAR_START,
            "HAR start",
            "Start HAR capture.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_NETWORK_HAR_STOP,
            "HAR stop",
            "Stop HAR capture.",
            json!({ "path": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_STORAGE_GET,
            "Storage get",
            "Get localStorage or sessionStorage.",
            json!({ "storageType": storage_type_schema(), "key": { "type": "string" } }),
            &["storageType"],
        ),
        tool(
            TOOL_STORAGE_SET,
            "Storage set",
            "Set localStorage or sessionStorage.",
            json!({ "storageType": storage_type_schema(), "key": { "type": "string" }, "value": { "type": "string" } }),
            &["storageType", "key", "value"],
        ),
        tool(
            TOOL_STORAGE_CLEAR,
            "Storage clear",
            "Clear localStorage or sessionStorage.",
            json!({ "storageType": storage_type_schema() }),
            &["storageType"],
        ),
        tool(
            TOOL_COOKIES_GET,
            "Cookies get",
            "Get cookies.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_COOKIES_SET,
            "Cookies set",
            "Set one cookie.",
            json!({ "name": { "type": "string" }, "value": { "type": "string" }, "url": { "type": "string" }, "domain": { "type": "string" }, "path": { "type": "string" }, "httpOnly": { "type": "boolean" }, "secure": { "type": "boolean" }, "sameSite": { "type": "string", "enum": ["Strict", "Lax", "None"] }, "expires": { "type": "integer" } }),
            &["name", "value"],
        ),
        tool(
            TOOL_COOKIES_SET_CURL,
            "Cookies set from cURL",
            "Set cookies from JSON, cURL, or Cookie header file.",
            json!({ "file": { "type": "string" }, "domain": { "type": "string" }, "url": { "type": "string" } }),
            &["file"],
        ),
        tool(
            TOOL_COOKIES_CLEAR,
            "Cookies clear",
            "Clear cookies.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_TAB_NEW,
            "Tab new",
            "Open a new tab.",
            json!({ "url": { "type": "string" }, "label": { "type": "string" } }),
            &[],
        ),
        tool(TOOL_TAB_LIST, "Tab list", "List tabs.", json!({}), &[]),
        tool(
            TOOL_TAB_SWITCH,
            "Tab switch",
            "Switch to a tab by id or label.",
            json!({ "tab": { "type": "string" } }),
            &["tab"],
        ),
        tool(
            TOOL_TAB_CLOSE,
            "Tab close",
            "Close a tab.",
            json!({ "tab": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_WINDOW_NEW,
            "Window new",
            "Open a new browser window.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_FRAME_SWITCH,
            "Frame switch",
            "Switch frame by selector, ref, or id.",
            json!({ "frame": { "type": "string" } }),
            &["frame"],
        ),
        tool(
            TOOL_FRAME_MAIN,
            "Frame main",
            "Switch to the main frame.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_DIALOG_STATUS,
            "Dialog status",
            "Show pending JavaScript dialog status.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_DIALOG_ACCEPT,
            "Dialog accept",
            "Accept a JavaScript dialog.",
            json!({ "text": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_DIALOG_DISMISS,
            "Dialog dismiss",
            "Dismiss a JavaScript dialog.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_TRACE_START,
            "Trace start",
            "Start Chrome trace capture.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_TRACE_STOP,
            "Trace stop",
            "Stop Chrome trace capture.",
            json!({ "path": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_PROFILER_START,
            "Profiler start",
            "Start Chrome profiler capture.",
            json!({ "categories": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_PROFILER_STOP,
            "Profiler stop",
            "Stop profiler capture.",
            json!({ "path": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_RECORD_START,
            "Record start",
            "Start video recording.",
            json!({ "path": { "type": "string" }, "url": { "type": "string" } }),
            &["path"],
        ),
        tool(
            TOOL_RECORD_STOP,
            "Record stop",
            "Stop video recording.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_RECORD_RESTART,
            "Record restart",
            "Restart video recording.",
            json!({ "path": { "type": "string" }, "url": { "type": "string" } }),
            &["path"],
        ),
        tool(
            TOOL_CONSOLE,
            "Console logs",
            "Read console logs.",
            json!({ "clear": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_ERRORS,
            "Page errors",
            "Read page errors.",
            json!({ "clear": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_HIGHLIGHT,
            "Highlight element",
            "Highlight an element.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_INSPECT,
            "Inspect",
            "Open Chrome DevTools.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_CLIPBOARD_READ,
            "Clipboard read",
            "Read clipboard text.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_CLIPBOARD_WRITE,
            "Clipboard write",
            "Write clipboard text.",
            json!({ "text": { "type": "string" } }),
            &["text"],
        ),
        tool(
            TOOL_CLIPBOARD_COPY,
            "Clipboard copy",
            "Copy current selection.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_CLIPBOARD_PASTE,
            "Clipboard paste",
            "Paste clipboard text.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_AUTH_SAVE,
            "Auth save",
            "Save an auth profile.",
            json!({
                "name": { "type": "string" },
                "url": { "type": "string" },
                "username": { "type": "string" },
                "password": { "type": "string", "description": "Password to send through child stdin." },
                "usernameSelector": { "type": "string" },
                "passwordSelector": { "type": "string" },
                "submitSelector": { "type": "string" }
            }),
            &["name", "url", "username", "password"],
        ),
        tool(
            TOOL_AUTH_LOGIN,
            "Auth login",
            "Log in with a saved auth profile.",
            json!({ "name": { "type": "string" } }),
            &["name"],
        ),
        tool(
            TOOL_AUTH_LIST,
            "Auth list",
            "List auth profiles.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_AUTH_SHOW,
            "Auth show",
            "Show auth profile metadata.",
            json!({ "name": { "type": "string" } }),
            &["name"],
        ),
        tool(
            TOOL_AUTH_DELETE,
            "Auth delete",
            "Delete an auth profile.",
            json!({ "name": { "type": "string" } }),
            &["name"],
        ),
        tool(
            TOOL_STATE_SAVE,
            "State save",
            "Save cookies and storage state.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            TOOL_STATE_LOAD,
            "State load",
            "Load cookies and storage state.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            TOOL_STATE_LIST,
            "State list",
            "List saved states.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_STATE_CLEAR,
            "State clear",
            "Clear saved state.",
            json!({ "name": { "type": "string" }, "all": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_STATE_SHOW,
            "State show",
            "Show a saved state file.",
            json!({ "path": { "type": "string" } }),
            &["path"],
        ),
        tool(
            TOOL_STATE_CLEAN,
            "State clean",
            "Delete old saved states.",
            json!({ "olderThanDays": { "type": "integer", "minimum": 0 } }),
            &["olderThanDays"],
        ),
        tool(
            TOOL_STATE_RENAME,
            "State rename",
            "Rename saved state.",
            json!({ "oldName": { "type": "string" }, "newName": { "type": "string" } }),
            &["oldName", "newName"],
        ),
        tool(
            TOOL_TAP,
            "Tap",
            "Tap an element on iOS/touch backends.",
            json!({ "selector": selector_schema() }),
            &["selector"],
        ),
        tool(
            TOOL_SWIPE,
            "Swipe",
            "Swipe in a direction.",
            json!({ "direction": { "type": "string", "enum": ["up", "down", "left", "right"] }, "amount": { "type": "integer" } }),
            &["direction"],
        ),
        tool(
            TOOL_DEVICE,
            "Device",
            "List available iOS simulators.",
            json!({ "action": { "type": "string", "enum": ["list"], "default": "list" } }),
            &[],
        ),
        tool(
            TOOL_DIFF_SNAPSHOT,
            "Diff snapshot",
            "Diff current snapshot against last or baseline.",
            json!({ "baseline": { "type": "string" }, "selector": { "type": "string" }, "compact": { "type": "boolean" }, "depth": { "type": "integer" } }),
            &[],
        ),
        tool(
            TOOL_DIFF_SCREENSHOT,
            "Diff screenshot",
            "Diff screenshot against a baseline image.",
            json!({ "baseline": { "type": "string" }, "output": { "type": "string" }, "threshold": number_schema(), "selector": { "type": "string" }, "fullPage": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_DIFF_URL,
            "Diff URL",
            "Compare two URLs.",
            json!({ "url1": { "type": "string" }, "url2": { "type": "string" }, "screenshot": { "type": "boolean" }, "fullPage": { "type": "boolean" }, "waitUntil": { "type": "string" }, "selector": { "type": "string" }, "compact": { "type": "boolean" }, "depth": { "type": "integer" } }),
            &["url1", "url2"],
        ),
        tool(
            TOOL_BATCH,
            "Batch",
            "Run multiple commands sequentially.",
            json!({ "commands": { "type": "array", "items": { "type": "array", "items": { "type": "string" }, "minItems": 1 }, "minItems": 1 }, "bail": { "type": "boolean" } }),
            &["commands"],
        ),
        tool(
            TOOL_REACT_TREE,
            "React tree",
            "Inspect React tree.",
            json!({ "json": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_REACT_INSPECT,
            "React inspect",
            "Inspect a React fiber.",
            json!({ "id": { "type": "integer", "minimum": 0 }, "json": { "type": "boolean" } }),
            &["id"],
        ),
        tool(
            TOOL_REACT_RENDERS_START,
            "React renders start",
            "Start render recording.",
            json!({ "json": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_REACT_RENDERS_STOP,
            "React renders stop",
            "Stop render recording.",
            json!({ "json": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_REACT_SUSPENSE,
            "React suspense",
            "Inspect Suspense boundaries.",
            json!({ "onlyDynamic": { "type": "boolean" }, "json": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_VITALS,
            "Vitals",
            "Collect Core Web Vitals and hydration metrics.",
            json!({ "url": { "type": "string" }, "json": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_PUSHSTATE,
            "Push state",
            "Perform SPA client-side navigation.",
            json!({ "url": { "type": "string" } }),
            &["url"],
        ),
        tool(
            TOOL_REMOVE_INIT_SCRIPT,
            "Remove init script",
            "Remove a registered init script.",
            json!({ "id": { "type": "string" } }),
            &["id"],
        ),
        tool(
            TOOL_CONFIRM,
            "Confirm action",
            "Approve a pending action.",
            json!({ "id": { "type": "string" } }),
            &["id"],
        ),
        tool(
            TOOL_DENY,
            "Deny action",
            "Deny a pending action.",
            json!({ "id": { "type": "string" } }),
            &["id"],
        ),
        tool(
            TOOL_CONNECT,
            "Connect CDP",
            "Connect to a browser over CDP.",
            json!({ "target": { "type": "string", "description": "CDP port or URL." } }),
            &["target"],
        ),
        tool(
            TOOL_STREAM_ENABLE,
            "Stream enable",
            "Enable runtime WebSocket streaming.",
            json!({ "port": { "type": "integer" } }),
            &[],
        ),
        tool(
            TOOL_STREAM_DISABLE,
            "Stream disable",
            "Disable streaming.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_STREAM_STATUS,
            "Stream status",
            "Show streaming status.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_SESSION,
            "Session",
            "Show current session.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_SESSION_LIST,
            "Session list",
            "List active sessions.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_PROFILES,
            "Profiles",
            "List Chrome profiles.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_SKILLS_LIST,
            "Skills list",
            "List bundled skills.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_SKILLS_GET,
            "Skills get",
            "Get bundled skill content.",
            json!({ "names": string_array_schema("Skill names."), "all": { "type": "boolean" }, "full": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_SKILLS_PATH,
            "Skills path",
            "Print skill directory path.",
            json!({ "name": { "type": "string" } }),
            &[],
        ),
        tool(
            TOOL_PLUGIN_ADD,
            "Plugin add",
            "Add a plugin from npm or GitHub to agent-browser config.",
            json!({
                "reference": { "type": "string", "description": "npm package, scoped package, or owner/repo GitHub reference." },
                "name": { "type": "string", "description": "Override the configured plugin name." },
                "capabilities": string_array_schema("Capabilities to declare when manifest discovery is skipped or unavailable."),
                "global": { "type": "boolean", "default": false, "description": "Write ~/.agent-browser/config.json instead of ./agent-browser.json." },
                "noManifest": { "type": "boolean", "default": false, "description": "Skip plugin.manifest discovery." }
            }),
            &["reference"],
        ),
        tool(
            TOOL_PLUGIN_LIST,
            "Plugin list",
            "List configured plugins.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_PLUGIN_SHOW,
            "Plugin show",
            "Show one configured plugin.",
            json!({ "name": { "type": "string" } }),
            &["name"],
        ),
        tool(
            TOOL_PLUGIN_RUN,
            "Plugin run",
            "Run a command.run or custom plugin request.",
            json!({
                "name": { "type": "string", "description": "Configured plugin name." },
                "requestType": { "type": "string", "description": "Namespaced request type to send to the plugin." },
                "payload": { "type": "object", "additionalProperties": true, "description": "JSON object payload to send as the plugin request." }
            }),
            &["name", "requestType"],
        ),
        tool(
            TOOL_DOCTOR,
            "Doctor",
            "Diagnose the installation.",
            json!({ "offline": { "type": "boolean" }, "quick": { "type": "boolean" }, "fix": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_DASHBOARD_START,
            "Dashboard start",
            "Start dashboard server.",
            json!({ "port": { "type": "integer" } }),
            &[],
        ),
        tool(
            TOOL_DASHBOARD_STOP,
            "Dashboard stop",
            "Stop dashboard server.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_INSTALL,
            "Install",
            "Install browser binaries.",
            json!({ "withDeps": { "type": "boolean" } }),
            &[],
        ),
        tool(
            TOOL_UPGRADE,
            "Upgrade",
            "Upgrade agent-browser.",
            json!({}),
            &[],
        ),
        tool(
            TOOL_CHAT,
            "Chat",
            "Run a single-shot natural-language browser instruction.",
            json!({ "message": { "type": "string" }, "model": { "type": "string" }, "verbose": { "type": "boolean" }, "quiet": { "type": "boolean" } }),
            &["message"],
        ),
    ]
}

fn selector_schema() -> Value {
    json!({
        "type": "string",
        "description": "Element @ref from snapshot, or a CSS selector."
    })
}

fn string_array_schema(description: &str) -> Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "minItems": 1,
        "description": description,
    })
}

fn key_schema() -> Value {
    json!({
        "type": "string",
        "description": "Key name such as Enter, Tab, or Control+a."
    })
}

fn mouse_button_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["left", "right", "middle"],
        "default": "left",
    })
}

fn storage_type_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["local", "session"],
    })
}

fn number_schema() -> Value {
    json!({ "type": "number" })
}

fn int_schema() -> Value {
    json!({ "type": "integer" })
}

fn wait_timeout_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "description": "Maximum time for the browser wait condition."
    })
}

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut props = match properties {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    props.insert(
        "session".to_string(),
        json!({
            "type": "string",
            "description": "Optional isolated browser session name."
        }),
    );
    props.insert(
        "extraArgs".to_string(),
        json!({
            "type": "array",
            "items": { "type": "string" },
            "description": "Advanced: extra CLI arguments for this command, preserving full CLI parity."
        }),
    );
    props.insert(
        "timeoutMs".to_string(),
        json!({
            "type": "integer",
            "minimum": 1,
            "default": DEFAULT_TIMEOUT_MS,
            "description": "Maximum time to wait for this tool call."
        }),
    );

    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), json!("object"));
    schema.insert("properties".to_string(), Value::Object(props));
    schema.insert("additionalProperties".to_string(), json!(false));
    if !required.is_empty() {
        schema.insert("required".to_string(), json!(required));
    }

    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": Value::Object(schema),
        "annotations": tool_annotations(name),
    })
}

fn tool_annotations(name: &str) -> Value {
    json!({
        "readOnlyHint": is_read_only_tool(name),
        "openWorldHint": is_open_world_tool(name),
    })
}

fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_SNAPSHOT
            | TOOL_WAIT_MS
            | TOOL_WAIT_FOR_SELECTOR
            | TOOL_WAIT_FOR_TEXT
            | TOOL_WAIT_FOR_URL
            | TOOL_WAIT_FOR_LOAD
            | TOOL_WAIT_FOR_FUNCTION
            | TOOL_WAIT_FOR_DOWNLOAD
            | TOOL_GET_TEXT
            | TOOL_GET_HTML
            | TOOL_GET_VALUE
            | TOOL_GET_ATTR
            | TOOL_GET_COUNT
            | TOOL_GET_BOX
            | TOOL_GET_STYLES
            | TOOL_GET_URL
            | TOOL_GET_TITLE
            | TOOL_GET_CDP_URL
            | TOOL_IS_VISIBLE
            | TOOL_IS_ENABLED
            | TOOL_IS_CHECKED
            | TOOL_NETWORK_REQUEST
            | TOOL_STORAGE_GET
            | TOOL_COOKIES_GET
            | TOOL_TAB_LIST
            | TOOL_DIALOG_STATUS
            | TOOL_CLIPBOARD_READ
            | TOOL_AUTH_LIST
            | TOOL_AUTH_SHOW
            | TOOL_STATE_LIST
            | TOOL_STATE_SHOW
            | TOOL_DEVICE
            | TOOL_REACT_TREE
            | TOOL_REACT_INSPECT
            | TOOL_REACT_SUSPENSE
            | TOOL_VITALS
            | TOOL_STREAM_STATUS
            | TOOL_SESSION
            | TOOL_SESSION_LIST
            | TOOL_PROFILES
            | TOOL_SKILLS_LIST
            | TOOL_SKILLS_GET
            | TOOL_SKILLS_PATH
            | TOOL_PLUGIN_LIST
            | TOOL_PLUGIN_SHOW
    )
}

fn is_open_world_tool(name: &str) -> bool {
    !matches!(
        name,
        TOOL_SESSION
            | TOOL_SESSION_LIST
            | TOOL_PROFILES
            | TOOL_SKILLS_LIST
            | TOOL_SKILLS_GET
            | TOOL_SKILLS_PATH
            | TOOL_PLUGIN_LIST
            | TOOL_PLUGIN_SHOW
            | TOOL_DOCTOR
            | TOOL_DASHBOARD_START
            | TOOL_DASHBOARD_STOP
            | TOOL_INSTALL
            | TOOL_UPGRADE
    )
}

fn is_known_tool(name: &str) -> bool {
    tools()
        .iter()
        .any(|tool| tool.get("name").and_then(|v| v.as_str()) == Some(name))
}

fn wait_tool(
    name: &str,
    title: &str,
    description: &str,
    mut properties: Value,
    required: &[&str],
) -> Value {
    if let Value::Object(ref mut props) = properties {
        props.insert(
            "waitTimeoutMs".to_string(),
            json!({
                "type": "integer",
                "minimum": 1,
                "description": "Maximum time for the browser wait condition."
            }),
        );
    }
    tool(name, title, description, properties, required)
}

fn call_tool(params: Option<&Value>, config: &McpConfig) -> Result<Value, ProtocolError> {
    let params =
        params.ok_or_else(|| ProtocolError::invalid_params("tools/call requires params"))?;
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ProtocolError::invalid_params("tools/call params.name must be a string"))?;
    let arguments = params.get("arguments").unwrap_or(&Value::Null);

    if !is_known_tool(name) {
        return Err(ProtocolError::invalid_params(format!(
            "Unknown tool: {}",
            name
        )));
    }

    if !config.allows(name) {
        return Err(ProtocolError::invalid_params(format!(
            "Tool {} is not enabled by the active MCP tools profile(s): {}. Restart with `agent-browser mcp --tools all` or add a profile that includes it.",
            name,
            config.profile_names().join(", ")
        )));
    }

    match name {
        TOOL_TOOLS_PROFILES => call_tools_profiles(config),
        TOOL_OPEN => call_open(arguments),
        TOOL_SNAPSHOT => call_snapshot(arguments),
        TOOL_CLICK => call_click(arguments),
        TOOL_BACK => call_literal(arguments, &["back"]),
        TOOL_FORWARD => call_literal(arguments, &["forward"]),
        TOOL_RELOAD => call_literal(arguments, &["reload"]),
        TOOL_DBLCLICK => call_simple_selector(arguments, "dblclick"),
        TOOL_FILL => call_fill(arguments),
        TOOL_TYPE => call_type(arguments),
        TOOL_PRESS => call_press(arguments),
        TOOL_KEYDOWN => call_key_command(arguments, "keydown"),
        TOOL_KEYUP => call_key_command(arguments, "keyup"),
        TOOL_KEYBOARD_TYPE => call_keyboard(arguments, "type"),
        TOOL_KEYBOARD_INSERT_TEXT => call_keyboard(arguments, "inserttext"),
        TOOL_HOVER => call_simple_selector(arguments, "hover"),
        TOOL_FOCUS => call_simple_selector(arguments, "focus"),
        TOOL_CHECK => call_simple_selector(arguments, "check"),
        TOOL_UNCHECK => call_simple_selector(arguments, "uncheck"),
        TOOL_SELECT => call_select(arguments),
        TOOL_DRAG => call_drag(arguments),
        TOOL_UPLOAD => call_upload(arguments),
        TOOL_DOWNLOAD => call_download(arguments),
        TOOL_SCROLL => call_scroll(arguments),
        TOOL_SCROLL_INTO_VIEW => call_simple_selector(arguments, "scrollintoview"),
        TOOL_WAIT_MS => call_wait_ms(arguments),
        TOOL_WAIT_FOR_SELECTOR => call_wait_flag(arguments, None, "selector"),
        TOOL_WAIT_FOR_TEXT => call_wait_flag(arguments, Some("--text"), "text"),
        TOOL_WAIT_FOR_URL => call_wait_flag(arguments, Some("--url"), "url"),
        TOOL_WAIT_FOR_LOAD => call_wait_flag(arguments, Some("--load"), "state"),
        TOOL_WAIT_FOR_FUNCTION => call_wait_flag(arguments, Some("--fn"), "expression"),
        TOOL_WAIT_FOR_DOWNLOAD => call_wait_download(arguments),
        TOOL_SCREENSHOT => call_screenshot(arguments),
        TOOL_PDF => call_one_string(arguments, "pdf", "path"),
        TOOL_GET_TEXT => call_get_selector(arguments, "text"),
        TOOL_GET_HTML => call_get_selector(arguments, "html"),
        TOOL_GET_VALUE => call_get_selector(arguments, "value"),
        TOOL_GET_ATTR => call_get_attr(arguments),
        TOOL_GET_COUNT => call_get_selector(arguments, "count"),
        TOOL_GET_BOX => call_get_selector(arguments, "box"),
        TOOL_GET_STYLES => call_get_selector(arguments, "styles"),
        TOOL_GET_URL => call_cli_tool(arguments, vec!["get".to_string(), "url".to_string()], None),
        TOOL_GET_TITLE => call_cli_tool(
            arguments,
            vec!["get".to_string(), "title".to_string()],
            None,
        ),
        TOOL_GET_CDP_URL => call_cli_tool(
            arguments,
            vec!["get".to_string(), "cdp-url".to_string()],
            None,
        ),
        TOOL_IS_VISIBLE => call_is(arguments, "visible"),
        TOOL_IS_ENABLED => call_is(arguments, "enabled"),
        TOOL_IS_CHECKED => call_is(arguments, "checked"),
        TOOL_FIND => call_find(arguments),
        TOOL_MOUSE_MOVE => call_mouse_move(arguments),
        TOOL_MOUSE_DOWN => call_mouse_button(arguments, "down"),
        TOOL_MOUSE_UP => call_mouse_button(arguments, "up"),
        TOOL_MOUSE_WHEEL => call_mouse_wheel(arguments),
        TOOL_SET_VIEWPORT => call_set_viewport(arguments),
        TOOL_SET_DEVICE => call_one_string(arguments, "set device", "device"),
        TOOL_SET_GEO => call_set_geo(arguments),
        TOOL_SET_OFFLINE => call_set_bool(arguments, "offline", "enabled"),
        TOOL_SET_HEADERS => call_set_headers(arguments),
        TOOL_SET_CREDENTIALS => call_set_credentials(arguments),
        TOOL_SET_MEDIA => call_set_media(arguments),
        TOOL_NETWORK_ROUTE => call_network_route(arguments),
        TOOL_NETWORK_UNROUTE => call_optional_one(arguments, &["network", "unroute"], "url"),
        TOOL_NETWORK_REQUESTS => call_network_requests(arguments),
        TOOL_NETWORK_REQUEST => call_one_string(arguments, "network request", "requestId"),
        TOOL_NETWORK_HAR_START => call_literal(arguments, &["network", "har", "start"]),
        TOOL_NETWORK_HAR_STOP => call_optional_one(arguments, &["network", "har", "stop"], "path"),
        TOOL_STORAGE_GET => call_storage_get(arguments),
        TOOL_STORAGE_SET => call_storage_set(arguments),
        TOOL_STORAGE_CLEAR => call_storage_clear(arguments),
        TOOL_COOKIES_GET => call_literal(arguments, &["cookies", "get"]),
        TOOL_COOKIES_SET => call_cookies_set(arguments),
        TOOL_COOKIES_SET_CURL => call_cookies_set_curl(arguments),
        TOOL_COOKIES_CLEAR => call_literal(arguments, &["cookies", "clear"]),
        TOOL_TAB_NEW => call_tab_new(arguments),
        TOOL_TAB_LIST => call_literal(arguments, &["tab", "list"]),
        TOOL_TAB_SWITCH => call_one_string(arguments, "tab", "tab"),
        TOOL_TAB_CLOSE => call_optional_one(arguments, &["tab", "close"], "tab"),
        TOOL_WINDOW_NEW => call_literal(arguments, &["window", "new"]),
        TOOL_FRAME_SWITCH => call_one_string(arguments, "frame", "frame"),
        TOOL_FRAME_MAIN => call_literal(arguments, &["frame", "main"]),
        TOOL_DIALOG_STATUS => call_literal(arguments, &["dialog", "status"]),
        TOOL_DIALOG_ACCEPT => call_optional_one(arguments, &["dialog", "accept"], "text"),
        TOOL_DIALOG_DISMISS => call_literal(arguments, &["dialog", "dismiss"]),
        TOOL_TRACE_START => call_literal(arguments, &["trace", "start"]),
        TOOL_TRACE_STOP => call_optional_one(arguments, &["trace", "stop"], "path"),
        TOOL_PROFILER_START => call_profiler_start(arguments),
        TOOL_PROFILER_STOP => call_optional_one(arguments, &["profiler", "stop"], "path"),
        TOOL_RECORD_START => call_record_start(arguments, "start"),
        TOOL_RECORD_STOP => call_literal(arguments, &["record", "stop"]),
        TOOL_RECORD_RESTART => call_record_start(arguments, "restart"),
        TOOL_CONSOLE => call_clearable(arguments, "console"),
        TOOL_ERRORS => call_clearable(arguments, "errors"),
        TOOL_HIGHLIGHT => call_simple_selector(arguments, "highlight"),
        TOOL_INSPECT => call_literal(arguments, &["inspect"]),
        TOOL_CLIPBOARD_READ => call_literal(arguments, &["clipboard", "read"]),
        TOOL_CLIPBOARD_WRITE => call_one_string(arguments, "clipboard write", "text"),
        TOOL_CLIPBOARD_COPY => call_literal(arguments, &["clipboard", "copy"]),
        TOOL_CLIPBOARD_PASTE => call_literal(arguments, &["clipboard", "paste"]),
        TOOL_AUTH_SAVE => call_auth_save(arguments),
        TOOL_AUTH_LOGIN => call_one_string(arguments, "auth login", "name"),
        TOOL_AUTH_LIST => call_literal(arguments, &["auth", "list"]),
        TOOL_AUTH_SHOW => call_one_string(arguments, "auth show", "name"),
        TOOL_AUTH_DELETE => call_one_string(arguments, "auth delete", "name"),
        TOOL_STATE_SAVE => call_one_string(arguments, "state save", "path"),
        TOOL_STATE_LOAD => call_one_string(arguments, "state load", "path"),
        TOOL_STATE_LIST => call_literal(arguments, &["state", "list"]),
        TOOL_STATE_CLEAR => call_state_clear(arguments),
        TOOL_STATE_SHOW => call_one_string(arguments, "state show", "path"),
        TOOL_STATE_CLEAN => call_state_clean(arguments),
        TOOL_STATE_RENAME => call_state_rename(arguments),
        TOOL_TAP => call_simple_selector(arguments, "tap"),
        TOOL_SWIPE => call_swipe(arguments),
        TOOL_DEVICE => call_device(arguments),
        TOOL_DIFF_SNAPSHOT => call_diff_snapshot(arguments),
        TOOL_DIFF_SCREENSHOT => call_diff_screenshot(arguments),
        TOOL_DIFF_URL => call_diff_url(arguments),
        TOOL_BATCH => call_batch(arguments),
        TOOL_REACT_TREE => call_react_tree(arguments),
        TOOL_REACT_INSPECT => call_react_inspect(arguments),
        TOOL_REACT_RENDERS_START => call_react_renders_start(arguments),
        TOOL_REACT_RENDERS_STOP => call_react_renders_stop(arguments),
        TOOL_REACT_SUSPENSE => call_react_suspense(arguments),
        TOOL_VITALS => call_vitals(arguments),
        TOOL_PUSHSTATE => call_one_string(arguments, "pushstate", "url"),
        TOOL_REMOVE_INIT_SCRIPT => call_one_string(arguments, "removeinitscript", "id"),
        TOOL_CONFIRM => call_one_string(arguments, "confirm", "id"),
        TOOL_DENY => call_one_string(arguments, "deny", "id"),
        TOOL_CONNECT => call_one_string(arguments, "connect", "target"),
        TOOL_STREAM_ENABLE => call_stream_enable(arguments),
        TOOL_STREAM_DISABLE => call_literal(arguments, &["stream", "disable"]),
        TOOL_STREAM_STATUS => call_literal(arguments, &["stream", "status"]),
        TOOL_SESSION => call_literal(arguments, &["session"]),
        TOOL_SESSION_LIST => call_literal(arguments, &["session", "list"]),
        TOOL_PROFILES => call_literal(arguments, &["profiles"]),
        TOOL_SKILLS_LIST => call_literal(arguments, &["skills", "list"]),
        TOOL_SKILLS_GET => call_skills_get(arguments),
        TOOL_SKILLS_PATH => call_optional_one(arguments, &["skills", "path"], "name"),
        TOOL_PLUGIN_ADD => call_plugin_add(arguments),
        TOOL_PLUGIN_LIST => call_literal(arguments, &["plugin", "list"]),
        TOOL_PLUGIN_SHOW => call_one_string(arguments, "plugin show", "name"),
        TOOL_PLUGIN_RUN => call_plugin_run(arguments),
        TOOL_DOCTOR => call_doctor(arguments),
        TOOL_DASHBOARD_START => call_dashboard_start(arguments),
        TOOL_DASHBOARD_STOP => call_literal(arguments, &["dashboard", "stop"]),
        TOOL_INSTALL => call_install(arguments),
        TOOL_UPGRADE => call_literal(arguments, &["upgrade"]),
        TOOL_CHAT => call_chat(arguments),
        TOOL_EVAL => call_eval(arguments),
        TOOL_CLOSE => call_close(arguments),
        _ => unreachable!("known MCP tool missing call handler: {}", name),
    }
}

fn call_tools_profiles(config: &McpConfig) -> Result<Value, ProtocolError> {
    let profiles = tool_profile_summaries();
    let text = format!(
        "Active MCP tools profile(s): {}\n\nAvailable profiles:\n{}\n\nRestart the MCP server with `agent-browser mcp --tools <profile>` or combine profiles with commas, for example `agent-browser mcp --tools core,network,react`. Use `agent-browser mcp --tools all` for the full typed CLI parity surface.",
        config.profile_names().join(", "),
        profiles
            .iter()
            .filter_map(|profile| {
                Some(format!(
                    "- {}: {} tools. {}",
                    profile.get("name")?.as_str()?,
                    profile.get("toolCount")?.as_u64()?,
                    profile.get("description")?.as_str()?
                ))
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    Ok(json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
        "structuredContent": {
            "activeProfiles": config.profile_names(),
            "profiles": profiles,
            "usage": {
                "default": "agent-browser mcp",
                "compose": "agent-browser mcp --tools core,network,react",
                "all": "agent-browser mcp --tools all",
            }
        },
        "isError": false,
    }))
}

fn call_cli_tool(
    arguments: &Value,
    command_args: Vec<String>,
    stdin_body: Option<String>,
) -> Result<Value, ProtocolError> {
    validate_arguments_object(arguments)?;
    let session = optional_string(arguments, "session")?;
    let timeout_ms = optional_timeout(arguments)?;
    let extra_args = optional_string_array(arguments, "extraArgs")?.unwrap_or_default();

    let mut cli_args = vec!["--json".to_string()];
    append_session_args(&mut cli_args, session.as_deref());
    cli_args.extend(command_args);
    cli_args.extend(extra_args);

    let run = run_cli(&cli_args, stdin_body, timeout_ms).map_err(|e| {
        ProtocolError::invalid_params(format!("Failed to run agent-browser: {}", e))
    })?;
    Ok(tool_result_from_run(run))
}

fn command_parts(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(ToString::to_string)
        .collect()
}

fn call_literal(arguments: &Value, parts: &[&str]) -> Result<Value, ProtocolError> {
    call_cli_tool(
        arguments,
        parts.iter().map(|s| s.to_string()).collect(),
        None,
    )
}

fn call_one_string(arguments: &Value, command: &str, key: &str) -> Result<Value, ProtocolError> {
    let mut args = command_parts(command);
    args.push(required_string(arguments, key)?);
    call_cli_tool(arguments, args, None)
}

fn call_optional_one(arguments: &Value, parts: &[&str], key: &str) -> Result<Value, ProtocolError> {
    let mut args: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    if let Some(value) = optional_string(arguments, key)? {
        if !value.is_empty() {
            args.push(value);
        }
    }
    call_cli_tool(arguments, args, None)
}

fn call_key_command(arguments: &Value, command: &str) -> Result<Value, ProtocolError> {
    let key = required_string(arguments, "key")?;
    call_cli_tool(arguments, vec![command.to_string(), key], None)
}

fn call_keyboard(arguments: &Value, subcommand: &str) -> Result<Value, ProtocolError> {
    let text = required_string(arguments, "text")?;
    call_cli_tool(
        arguments,
        vec!["keyboard".to_string(), subcommand.to_string(), text],
        None,
    )
}

fn call_open(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = Vec::new();
    if optional_bool(arguments, "headed")?.unwrap_or(false) {
        args.push("--headed".to_string());
    }
    args.push("open".to_string());
    if let Some(url) = optional_string(arguments, "url")? {
        if !url.is_empty() {
            args.push(url);
        }
    }
    call_cli_tool(arguments, args, None)
}

fn call_snapshot(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["snapshot".to_string()];
    if optional_bool(arguments, "interactive")?.unwrap_or(true) {
        args.push("-i".to_string());
    }
    if optional_bool(arguments, "compact")?.unwrap_or(false) {
        args.push("-c".to_string());
    }
    if optional_bool(arguments, "includeUrls")?.unwrap_or(false) {
        args.push("-u".to_string());
    }
    if let Some(depth) = optional_u64(arguments, "depth")? {
        args.push("-d".to_string());
        args.push(depth.to_string());
    }
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push("-s".to_string());
        args.push(selector);
    }

    call_cli_tool(arguments, args, None)
}

fn call_simple_selector(arguments: &Value, command: &str) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    call_cli_tool(arguments, vec![command.to_string(), selector], None)
}

fn click_command_args(arguments: &Value) -> Result<Vec<String>, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let mut args = vec!["click".to_string(), selector];
    if optional_bool(arguments, "newTab")?.unwrap_or(false) {
        args.push("--new-tab".to_string());
    }
    Ok(args)
}

fn call_click(arguments: &Value) -> Result<Value, ProtocolError> {
    call_cli_tool(arguments, click_command_args(arguments)?, None)
}

fn call_fill(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let text = required_string(arguments, "text")?;
    call_cli_tool(arguments, vec!["fill".to_string(), selector, text], None)
}

fn call_type(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let text = required_string(arguments, "text")?;
    let mut args = vec!["type".to_string(), selector, text];
    if optional_bool(arguments, "clear")?.unwrap_or(false) {
        args.push("--clear".to_string());
    }
    if let Some(delay) = optional_u64(arguments, "delayMs")? {
        args.push("--delay".to_string());
        args.push(delay.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_press(arguments: &Value) -> Result<Value, ProtocolError> {
    let key = required_string(arguments, "key")?;
    call_cli_tool(arguments, vec!["press".to_string(), key], None)
}

fn call_drag(arguments: &Value) -> Result<Value, ProtocolError> {
    let source = required_string(arguments, "source")?;
    let target = required_string(arguments, "target")?;
    call_cli_tool(arguments, vec!["drag".to_string(), source, target], None)
}

fn call_upload(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let files = required_string_array(arguments, "files")?;
    let mut args = vec!["upload".to_string(), selector];
    args.extend(files);
    call_cli_tool(arguments, args, None)
}

fn call_download(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let path = required_string(arguments, "path")?;
    call_cli_tool(
        arguments,
        vec!["download".to_string(), selector, path],
        None,
    )
}

fn call_select(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let values = required_string_array(arguments, "values")?;
    let mut args = vec!["select".to_string(), selector];
    args.extend(values);
    call_cli_tool(arguments, args, None)
}

fn call_scroll(arguments: &Value) -> Result<Value, ProtocolError> {
    let direction = optional_string(arguments, "direction")?.unwrap_or_else(|| "down".to_string());
    let amount = optional_i64(arguments, "amount")?.unwrap_or(300);
    let mut args = vec!["scroll".to_string(), direction, amount.to_string()];
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push("--selector".to_string());
        args.push(selector);
    }
    call_cli_tool(arguments, args, None)
}

fn call_wait_ms(arguments: &Value) -> Result<Value, ProtocolError> {
    let ms = required_u64(arguments, "ms")?;
    call_cli_tool(arguments, vec!["wait".to_string(), ms.to_string()], None)
}

fn call_wait_flag(
    arguments: &Value,
    flag: Option<&str>,
    value_key: &str,
) -> Result<Value, ProtocolError> {
    let value = required_string(arguments, value_key)?;
    let mut args = vec!["wait".to_string()];
    if let Some(flag) = flag {
        args.push(flag.to_string());
        args.push(value);
    } else {
        args.push(value);
    }
    if let Some(timeout) = optional_u64(arguments, "waitTimeoutMs")? {
        args.push("--timeout".to_string());
        args.push(timeout.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_wait_download(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["wait".to_string(), "--download".to_string()];
    if let Some(path) = optional_string(arguments, "path")? {
        args.push(path);
    }
    if let Some(timeout) = optional_u64(arguments, "waitTimeoutMs")? {
        args.push("--timeout".to_string());
        args.push(timeout.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_screenshot(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = Vec::new();
    if optional_bool(arguments, "annotate")?.unwrap_or(false) {
        args.push("--annotate".to_string());
    }
    if let Some(format) = optional_string(arguments, "format")? {
        args.push("--screenshot-format".to_string());
        args.push(format);
    }
    if let Some(quality) = optional_u64(arguments, "quality")? {
        args.push("--screenshot-quality".to_string());
        args.push(quality.to_string());
    }
    if let Some(dir) = optional_string(arguments, "screenshotDir")? {
        args.push("--screenshot-dir".to_string());
        args.push(dir);
    }

    args.push("screenshot".to_string());
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push(selector);
    }
    if let Some(path) = optional_string(arguments, "path")? {
        args.push(path);
    }
    if optional_bool(arguments, "fullPage")?.unwrap_or(false) {
        args.push("--full".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_get_selector(arguments: &Value, what: &str) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    call_cli_tool(
        arguments,
        vec!["get".to_string(), what.to_string(), selector],
        None,
    )
}

fn call_get_attr(arguments: &Value) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    let name = required_string(arguments, "name")?;
    call_cli_tool(
        arguments,
        vec!["get".to_string(), "attr".to_string(), selector, name],
        None,
    )
}

fn call_is(arguments: &Value, what: &str) -> Result<Value, ProtocolError> {
    let selector = required_string(arguments, "selector")?;
    call_cli_tool(
        arguments,
        vec!["is".to_string(), what.to_string(), selector],
        None,
    )
}

fn call_find(arguments: &Value) -> Result<Value, ProtocolError> {
    let locator = required_string(arguments, "locator")?;
    let value = required_string(arguments, "value")?;
    let mut args = vec!["find".to_string(), locator.clone()];
    if locator == "nth" {
        let index = optional_i64(arguments, "index")?.unwrap_or(0);
        args.push(index.to_string());
    }
    args.push(value);
    let action = optional_string(arguments, "action")?;
    let text = optional_string(arguments, "text")?;
    let name = optional_string(arguments, "name")?;
    let exact = optional_bool(arguments, "exact")?.unwrap_or(false);
    if let Some(action) = action {
        args.push(action);
    } else if name.is_some() || exact || text.is_some() {
        args.push("click".to_string());
    }
    if let Some(text) = text {
        args.push(text);
    }
    if let Some(name) = name {
        args.push("--name".to_string());
        args.push(name);
    }
    if exact {
        args.push("--exact".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_mouse_move(arguments: &Value) -> Result<Value, ProtocolError> {
    let x = required_number_string(arguments, "x")?;
    let y = required_number_string(arguments, "y")?;
    call_cli_tool(
        arguments,
        vec!["mouse".to_string(), "move".to_string(), x, y],
        None,
    )
}

fn call_mouse_button(arguments: &Value, action: &str) -> Result<Value, ProtocolError> {
    let mut args = vec!["mouse".to_string(), action.to_string()];
    if let Some(button) = optional_string(arguments, "button")? {
        args.push(button);
    }
    call_cli_tool(arguments, args, None)
}

fn call_mouse_wheel(arguments: &Value) -> Result<Value, ProtocolError> {
    let dy = required_number_string(arguments, "dy")?;
    let dx = optional_number_string(arguments, "dx")?;
    let mut args = vec!["mouse".to_string(), "wheel".to_string(), dy];
    if let Some(dx) = dx {
        args.push(dx);
    }
    call_cli_tool(arguments, args, None)
}

fn call_set_viewport(arguments: &Value) -> Result<Value, ProtocolError> {
    let width = required_u64(arguments, "width")?;
    let height = required_u64(arguments, "height")?;
    let mut args = vec![
        "set".to_string(),
        "viewport".to_string(),
        width.to_string(),
        height.to_string(),
    ];
    if let Some(scale) = optional_number_string(arguments, "scale")? {
        args.push(scale);
    }
    call_cli_tool(arguments, args, None)
}

fn call_set_geo(arguments: &Value) -> Result<Value, ProtocolError> {
    let latitude = required_number_string(arguments, "latitude")?;
    let longitude = required_number_string(arguments, "longitude")?;
    call_cli_tool(
        arguments,
        vec!["set".to_string(), "geo".to_string(), latitude, longitude],
        None,
    )
}

fn call_set_bool(arguments: &Value, setting: &str, key: &str) -> Result<Value, ProtocolError> {
    let enabled = optional_bool(arguments, key)?.unwrap_or(true);
    call_cli_tool(
        arguments,
        vec![
            "set".to_string(),
            setting.to_string(),
            if enabled { "on" } else { "off" }.to_string(),
        ],
        None,
    )
}

fn call_set_headers(arguments: &Value) -> Result<Value, ProtocolError> {
    let headers = optional_value(arguments, "headers")?
        .ok_or_else(|| ProtocolError::invalid_params("headers must be an object"))?;
    let headers_json = serde_json::to_string(headers)
        .map_err(|e| ProtocolError::invalid_params(format!("headers encode error: {}", e)))?;
    call_cli_tool(
        arguments,
        vec!["set".to_string(), "headers".to_string(), headers_json],
        None,
    )
}

fn call_set_credentials(arguments: &Value) -> Result<Value, ProtocolError> {
    let username = required_string(arguments, "username")?;
    let password = required_string(arguments, "password")?;
    call_cli_tool(
        arguments,
        vec![
            "set".to_string(),
            "credentials".to_string(),
            username,
            password,
        ],
        None,
    )
}

fn call_set_media(arguments: &Value) -> Result<Value, ProtocolError> {
    call_cli_tool(arguments, set_media_args(arguments)?, None)
}

fn set_media_args(arguments: &Value) -> Result<Vec<String>, ProtocolError> {
    let mut args = vec!["set".to_string(), "media".to_string()];
    if let Some(color_scheme) = optional_string(arguments, "colorScheme")? {
        match color_scheme.as_str() {
            "dark" | "light" | "no-preference" => {}
            _ => {
                return Err(ProtocolError::invalid_params(
                    "colorScheme must be dark, light, or no-preference",
                ));
            }
        }
        args.push(color_scheme);
    }
    if let Some(reduced_motion) = optional_string(arguments, "reducedMotion")? {
        match reduced_motion.as_str() {
            "reduce" => args.push("reduced-motion".to_string()),
            "no-preference" => args.push("no-preference".to_string()),
            _ => {
                return Err(ProtocolError::invalid_params(
                    "reducedMotion must be reduce or no-preference",
                ));
            }
        }
    }
    Ok(args)
}

fn call_network_route(arguments: &Value) -> Result<Value, ProtocolError> {
    let url = required_string(arguments, "url")?;
    let mut args = vec!["network".to_string(), "route".to_string(), url];
    if optional_bool(arguments, "abort")?.unwrap_or(false) {
        args.push("--abort".to_string());
    }
    if let Some(body) = optional_string(arguments, "body")? {
        args.push("--body".to_string());
        args.push(body);
    }
    if let Some(resource_type) = optional_string(arguments, "resourceType")? {
        args.push("--resource-type".to_string());
        args.push(resource_type);
    }
    call_cli_tool(arguments, args, None)
}

fn call_network_requests(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["network".to_string(), "requests".to_string()];
    if optional_bool(arguments, "clear")?.unwrap_or(false) {
        args.push("--clear".to_string());
    }
    for (key, flag) in [
        ("filter", "--filter"),
        ("type", "--type"),
        ("method", "--method"),
        ("status", "--status"),
    ] {
        if let Some(value) = optional_string(arguments, key)? {
            args.push(flag.to_string());
            args.push(value);
        }
    }
    call_cli_tool(arguments, args, None)
}

fn call_storage_get(arguments: &Value) -> Result<Value, ProtocolError> {
    let storage_type = required_string(arguments, "storageType")?;
    let mut args = vec!["storage".to_string(), storage_type, "get".to_string()];
    if let Some(key) = optional_string(arguments, "key")? {
        args.push(key);
    }
    call_cli_tool(arguments, args, None)
}

fn call_storage_set(arguments: &Value) -> Result<Value, ProtocolError> {
    let storage_type = required_string(arguments, "storageType")?;
    let key = required_string(arguments, "key")?;
    let value = required_string(arguments, "value")?;
    call_cli_tool(
        arguments,
        vec![
            "storage".to_string(),
            storage_type,
            "set".to_string(),
            key,
            value,
        ],
        None,
    )
}

fn call_storage_clear(arguments: &Value) -> Result<Value, ProtocolError> {
    let storage_type = required_string(arguments, "storageType")?;
    call_cli_tool(
        arguments,
        vec!["storage".to_string(), storage_type, "clear".to_string()],
        None,
    )
}

fn call_cookies_set(arguments: &Value) -> Result<Value, ProtocolError> {
    let name = required_string(arguments, "name")?;
    let value = required_string(arguments, "value")?;
    let mut args = vec!["cookies".to_string(), "set".to_string(), name, value];
    for (key, flag) in [
        ("url", "--url"),
        ("domain", "--domain"),
        ("path", "--path"),
        ("sameSite", "--sameSite"),
    ] {
        if let Some(value) = optional_string(arguments, key)? {
            args.push(flag.to_string());
            args.push(value);
        }
    }
    if let Some(value) = optional_i64(arguments, "expires")? {
        args.push("--expires".to_string());
        args.push(value.to_string());
    }
    if optional_bool(arguments, "httpOnly")?.unwrap_or(false) {
        args.push("--httpOnly".to_string());
    }
    if optional_bool(arguments, "secure")?.unwrap_or(false) {
        args.push("--secure".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_cookies_set_curl(arguments: &Value) -> Result<Value, ProtocolError> {
    let file = required_string(arguments, "file")?;
    let mut args = vec![
        "cookies".to_string(),
        "set".to_string(),
        "--curl".to_string(),
        file,
    ];
    if let Some(domain) = optional_string(arguments, "domain")? {
        args.push("--domain".to_string());
        args.push(domain);
    }
    if let Some(url) = optional_string(arguments, "url")? {
        args.push("--url".to_string());
        args.push(url);
    }
    call_cli_tool(arguments, args, None)
}

fn call_tab_new(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["tab".to_string(), "new".to_string()];
    if let Some(url) = optional_string(arguments, "url")? {
        args.push(url);
    }
    if let Some(label) = optional_string(arguments, "label")? {
        args.push("--label".to_string());
        args.push(label);
    }
    call_cli_tool(arguments, args, None)
}

fn call_profiler_start(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["profiler".to_string(), "start".to_string()];
    if let Some(categories) = optional_string(arguments, "categories")? {
        args.push("--categories".to_string());
        args.push(categories);
    }
    call_cli_tool(arguments, args, None)
}

fn call_record_start(arguments: &Value, action: &str) -> Result<Value, ProtocolError> {
    let path = required_string(arguments, "path")?;
    let mut args = vec!["record".to_string(), action.to_string(), path];
    if let Some(url) = optional_string(arguments, "url")? {
        args.push(url);
    }
    call_cli_tool(arguments, args, None)
}

fn call_clearable(arguments: &Value, command: &str) -> Result<Value, ProtocolError> {
    let mut args = vec![command.to_string()];
    if optional_bool(arguments, "clear")?.unwrap_or(false) {
        args.push("--clear".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_auth_save(arguments: &Value) -> Result<Value, ProtocolError> {
    let name = required_string(arguments, "name")?;
    let url = required_string(arguments, "url")?;
    let username = required_string(arguments, "username")?;
    let password = required_string(arguments, "password")?;
    let mut args = vec!["auth".to_string(), "save".to_string(), name];
    for (key, flag) in [
        ("url", "--url"),
        ("username", "--username"),
        ("usernameSelector", "--username-selector"),
        ("passwordSelector", "--password-selector"),
        ("submitSelector", "--submit-selector"),
    ] {
        let value = match key {
            "url" => Some(url.clone()),
            "username" => Some(username.clone()),
            _ => optional_string(arguments, key)?,
        };
        if let Some(value) = value {
            args.push(flag.to_string());
            args.push(value);
        }
    }
    args.push("--password-stdin".to_string());
    call_cli_tool(arguments, args, Some(password))
}

fn call_state_clear(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["state".to_string(), "clear".to_string()];
    if optional_bool(arguments, "all")?.unwrap_or(false) {
        args.push("--all".to_string());
    }
    if let Some(name) = optional_string(arguments, "name")? {
        args.push(name);
    }
    call_cli_tool(arguments, args, None)
}

fn call_state_clean(arguments: &Value) -> Result<Value, ProtocolError> {
    let days = required_u64(arguments, "olderThanDays")?;
    call_cli_tool(
        arguments,
        vec![
            "state".to_string(),
            "clean".to_string(),
            "--older-than".to_string(),
            days.to_string(),
        ],
        None,
    )
}

fn call_state_rename(arguments: &Value) -> Result<Value, ProtocolError> {
    let old_name = required_string(arguments, "oldName")?;
    let new_name = required_string(arguments, "newName")?;
    call_cli_tool(
        arguments,
        vec![
            "state".to_string(),
            "rename".to_string(),
            old_name,
            new_name,
        ],
        None,
    )
}

fn call_swipe(arguments: &Value) -> Result<Value, ProtocolError> {
    let direction = required_string(arguments, "direction")?;
    let mut args = vec!["swipe".to_string(), direction];
    if let Some(amount) = optional_u64(arguments, "amount")? {
        args.push(amount.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_device(arguments: &Value) -> Result<Value, ProtocolError> {
    let action = optional_string(arguments, "action")?.unwrap_or_else(|| "list".to_string());
    let args = vec!["device".to_string(), action];
    call_cli_tool(arguments, args, None)
}

fn call_diff_snapshot(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["diff".to_string(), "snapshot".to_string()];
    if let Some(baseline) = optional_string(arguments, "baseline")? {
        args.push("--baseline".to_string());
        args.push(baseline);
    }
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push("--selector".to_string());
        args.push(selector);
    }
    if optional_bool(arguments, "compact")?.unwrap_or(false) {
        args.push("--compact".to_string());
    }
    if let Some(depth) = optional_u64(arguments, "depth")? {
        args.push("--depth".to_string());
        args.push(depth.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_diff_screenshot(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["diff".to_string(), "screenshot".to_string()];
    for (key, flag) in [
        ("baseline", "--baseline"),
        ("output", "--output"),
        ("selector", "--selector"),
    ] {
        if let Some(value) = optional_string(arguments, key)? {
            args.push(flag.to_string());
            args.push(value);
        }
    }
    if let Some(threshold) = optional_number_string(arguments, "threshold")? {
        args.push("--threshold".to_string());
        args.push(threshold);
    }
    if optional_bool(arguments, "fullPage")?.unwrap_or(false) {
        args.push("--full".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_diff_url(arguments: &Value) -> Result<Value, ProtocolError> {
    let url1 = required_string(arguments, "url1")?;
    let url2 = required_string(arguments, "url2")?;
    let mut args = vec!["diff".to_string(), "url".to_string(), url1, url2];
    if optional_bool(arguments, "screenshot")?.unwrap_or(false) {
        args.push("--screenshot".to_string());
    }
    if optional_bool(arguments, "fullPage")?.unwrap_or(false) {
        args.push("--full".to_string());
    }
    if let Some(wait_until) = optional_string(arguments, "waitUntil")? {
        args.push("--wait-until".to_string());
        args.push(wait_until);
    }
    if let Some(selector) = optional_string(arguments, "selector")? {
        args.push("--selector".to_string());
        args.push(selector);
    }
    if optional_bool(arguments, "compact")?.unwrap_or(false) {
        args.push("--compact".to_string());
    }
    if let Some(depth) = optional_u64(arguments, "depth")? {
        args.push("--depth".to_string());
        args.push(depth.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_batch(arguments: &Value) -> Result<Value, ProtocolError> {
    let commands_value = optional_value(arguments, "commands")?
        .ok_or_else(|| ProtocolError::invalid_params("commands must be an array"))?;
    let commands = commands_value
        .as_array()
        .ok_or_else(|| ProtocolError::invalid_params("commands must be an array"))?;
    let mut parsed_commands = Vec::with_capacity(commands.len());
    for (i, command) in commands.iter().enumerate() {
        let items = command.as_array().ok_or_else(|| {
            ProtocolError::invalid_params(format!("commands[{}] must be an array", i))
        })?;
        let mut args = Vec::with_capacity(items.len());
        for (j, item) in items.iter().enumerate() {
            args.push(
                item.as_str()
                    .ok_or_else(|| {
                        ProtocolError::invalid_params(format!(
                            "commands[{}][{}] must be a string",
                            i, j
                        ))
                    })?
                    .to_string(),
            );
        }
        parsed_commands.push(args);
    }
    let mut args = vec!["batch".to_string()];
    if optional_bool(arguments, "bail")?.unwrap_or(false) {
        args.push("--bail".to_string());
    }
    let stdin = serde_json::to_string(&parsed_commands)
        .map_err(|e| ProtocolError::invalid_params(format!("commands encode error: {}", e)))?;
    call_cli_tool(arguments, args, Some(stdin))
}

fn call_react_tree(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["react".to_string(), "tree".to_string()];
    append_react_raw_json_arg(arguments, &mut args)?;
    call_cli_tool(arguments, args, None)
}

fn call_react_inspect(arguments: &Value) -> Result<Value, ProtocolError> {
    let id = required_u64(arguments, "id")?;
    let mut args = vec!["react".to_string(), "inspect".to_string(), id.to_string()];
    append_react_raw_json_arg(arguments, &mut args)?;
    call_cli_tool(arguments, args, None)
}

fn call_react_renders_start(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec![
        "react".to_string(),
        "renders".to_string(),
        "start".to_string(),
    ];
    append_react_raw_json_arg(arguments, &mut args)?;
    call_cli_tool(arguments, args, None)
}

fn call_react_renders_stop(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec![
        "react".to_string(),
        "renders".to_string(),
        "stop".to_string(),
    ];
    append_react_raw_json_arg(arguments, &mut args)?;
    call_cli_tool(arguments, args, None)
}

fn call_react_suspense(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["react".to_string(), "suspense".to_string()];
    if optional_bool(arguments, "onlyDynamic")?.unwrap_or(false) {
        args.push("--only-dynamic".to_string());
    }
    append_react_raw_json_arg(arguments, &mut args)?;
    call_cli_tool(arguments, args, None)
}

fn append_react_raw_json_arg(
    arguments: &Value,
    args: &mut Vec<String>,
) -> Result<(), ProtocolError> {
    if optional_bool(arguments, "json")?.unwrap_or(false) {
        args.push(RAW_JSON_ARG.to_string());
    }
    Ok(())
}

fn call_vitals(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["vitals".to_string()];
    if optional_bool(arguments, "json")?.unwrap_or(false) {
        args.push("--json".to_string());
    }
    if let Some(url) = optional_string(arguments, "url")? {
        args.push(url);
    }
    call_cli_tool(arguments, args, None)
}

fn call_stream_enable(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["stream".to_string(), "enable".to_string()];
    if let Some(port) = optional_u64(arguments, "port")? {
        args.push("--port".to_string());
        args.push(port.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_skills_get(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["skills".to_string(), "get".to_string()];
    if optional_bool(arguments, "all")?.unwrap_or(false) {
        args.push("--all".to_string());
    }
    if let Some(names) = optional_string_array(arguments, "names")? {
        args.extend(names);
    }
    if optional_bool(arguments, "full")?.unwrap_or(false) {
        args.push("--full".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_plugin_add(arguments: &Value) -> Result<Value, ProtocolError> {
    call_cli_tool(arguments, plugin_add_args(arguments)?, None)
}

fn plugin_add_args(arguments: &Value) -> Result<Vec<String>, ProtocolError> {
    let reference = required_string(arguments, "reference")?;
    let mut args = vec!["plugin".to_string(), "add".to_string(), reference];
    if let Some(name) = optional_string(arguments, "name")? {
        args.push("--name".to_string());
        args.push(name);
    }
    if let Some(capabilities) = optional_string_array(arguments, "capabilities")? {
        for capability in capabilities {
            args.push("--capability".to_string());
            args.push(capability);
        }
    }
    if optional_bool(arguments, "global")?.unwrap_or(false) {
        args.push("--global".to_string());
    }
    if optional_bool(arguments, "noManifest")?.unwrap_or(false) {
        args.push("--no-manifest".to_string());
    }
    Ok(args)
}

fn call_plugin_run(arguments: &Value) -> Result<Value, ProtocolError> {
    call_cli_tool(arguments, plugin_run_args(arguments)?, None)
}

fn plugin_run_args(arguments: &Value) -> Result<Vec<String>, ProtocolError> {
    let name = required_string(arguments, "name")?;
    let request_type = required_string(arguments, "requestType")?;
    let mut args = vec!["plugin".to_string(), "run".to_string(), name, request_type];
    if let Some(payload) = optional_value(arguments, "payload")? {
        let payload = serde_json::to_string(payload)
            .map_err(|e| ProtocolError::invalid_params(format!("payload encode error: {}", e)))?;
        args.push("--payload".to_string());
        args.push(payload);
    }
    Ok(args)
}

fn call_doctor(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["doctor".to_string()];
    for (key, flag) in [
        ("offline", "--offline"),
        ("quick", "--quick"),
        ("fix", "--fix"),
    ] {
        if optional_bool(arguments, key)?.unwrap_or(false) {
            args.push(flag.to_string());
        }
    }
    call_cli_tool(arguments, args, None)
}

fn call_dashboard_start(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["dashboard".to_string(), "start".to_string()];
    if let Some(port) = optional_u64(arguments, "port")? {
        args.push("--port".to_string());
        args.push(port.to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_install(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["install".to_string()];
    if optional_bool(arguments, "withDeps")?.unwrap_or(false) {
        args.push("--with-deps".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn call_chat(arguments: &Value) -> Result<Value, ProtocolError> {
    let message = required_string(arguments, "message")?;
    let mut args = Vec::new();
    if let Some(model) = optional_string(arguments, "model")? {
        args.push("--model".to_string());
        args.push(model);
    }
    if optional_bool(arguments, "verbose")?.unwrap_or(false) {
        args.push("--verbose".to_string());
    }
    if optional_bool(arguments, "quiet")?.unwrap_or(false) {
        args.push("--quiet".to_string());
    }
    args.push("chat".to_string());
    args.push(message);
    call_cli_tool(arguments, args, None)
}

fn call_eval(arguments: &Value) -> Result<Value, ProtocolError> {
    let script = required_string(arguments, "script")?;
    call_cli_tool(
        arguments,
        vec!["eval".to_string(), "--stdin".to_string()],
        Some(script),
    )
}

fn call_close(arguments: &Value) -> Result<Value, ProtocolError> {
    let mut args = vec!["close".to_string()];
    if optional_bool(arguments, "all")?.unwrap_or(false) {
        args.push("--all".to_string());
    }
    call_cli_tool(arguments, args, None)
}

fn validate_arguments_object(arguments: &Value) -> Result<(), ProtocolError> {
    if arguments.is_null() || arguments.is_object() {
        Ok(())
    } else {
        Err(ProtocolError::invalid_params(
            "tool arguments must be an object",
        ))
    }
}

fn optional_value<'a>(arguments: &'a Value, key: &str) -> Result<Option<&'a Value>, ProtocolError> {
    validate_arguments_object(arguments)?;
    Ok(arguments.get(key))
}

fn required_string(arguments: &Value, key: &str) -> Result<String, ProtocolError> {
    optional_value(arguments, key)?
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be a string", key)))
}

fn optional_string(arguments: &Value, key: &str) -> Result<Option<String>, ProtocolError> {
    match optional_value(arguments, key)? {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(ProtocolError::invalid_params(format!(
            "{} must be a string",
            key
        ))),
        None => Ok(None),
    }
}

fn optional_bool(arguments: &Value, key: &str) -> Result<Option<bool>, ProtocolError> {
    match optional_value(arguments, key)? {
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(ProtocolError::invalid_params(format!(
            "{} must be a boolean",
            key
        ))),
        None => Ok(None),
    }
}

fn required_u64(arguments: &Value, key: &str) -> Result<u64, ProtocolError> {
    optional_value(arguments, key)?
        .and_then(|v| v.as_u64())
        .ok_or_else(|| {
            ProtocolError::invalid_params(format!("{} must be a non-negative integer", key))
        })
}

fn required_number_string(arguments: &Value, key: &str) -> Result<String, ProtocolError> {
    let value = optional_value(arguments, key)?
        .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be a number", key)))?;
    number_to_string(value, key)
}

fn optional_number_string(arguments: &Value, key: &str) -> Result<Option<String>, ProtocolError> {
    match optional_value(arguments, key)? {
        Some(value) => number_to_string(value, key).map(Some),
        None => Ok(None),
    }
}

fn number_to_string(value: &Value, key: &str) -> Result<String, ProtocolError> {
    if let Some(n) = value.as_i64() {
        return Ok(n.to_string());
    }
    if let Some(n) = value.as_u64() {
        return Ok(n.to_string());
    }
    if let Some(n) = value.as_f64() {
        return Ok(n.to_string());
    }
    Err(ProtocolError::invalid_params(format!(
        "{} must be a number",
        key
    )))
}

fn optional_u64(arguments: &Value, key: &str) -> Result<Option<u64>, ProtocolError> {
    match optional_value(arguments, key)? {
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            ProtocolError::invalid_params(format!("{} must be a non-negative integer", key))
        }),
        None => Ok(None),
    }
}

fn optional_i64(arguments: &Value, key: &str) -> Result<Option<i64>, ProtocolError> {
    match optional_value(arguments, key)? {
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be an integer", key))),
        None => Ok(None),
    }
}

fn optional_string_array(
    arguments: &Value,
    key: &str,
) -> Result<Option<Vec<String>>, ProtocolError> {
    match optional_value(arguments, key)? {
        Some(value) => parse_string_array(value, key).map(Some),
        None => Ok(None),
    }
}

fn required_string_array(arguments: &Value, key: &str) -> Result<Vec<String>, ProtocolError> {
    let value = optional_value(arguments, key)?
        .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be an array", key)))?;
    parse_string_array(value, key)
}

fn parse_string_array(value: &Value, key: &str) -> Result<Vec<String>, ProtocolError> {
    let arr = value
        .as_array()
        .ok_or_else(|| ProtocolError::invalid_params(format!("{} must be an array", key)))?;
    if arr.is_empty() {
        return Err(ProtocolError::invalid_params(format!(
            "{} must not be empty",
            key
        )));
    }

    arr.iter()
        .enumerate()
        .map(|(i, item)| {
            item.as_str().map(ToString::to_string).ok_or_else(|| {
                ProtocolError::invalid_params(format!("{}[{}] must be a string", key, i))
            })
        })
        .collect()
}

fn optional_timeout(arguments: &Value) -> Result<u64, ProtocolError> {
    match arguments.get("timeoutMs") {
        Some(v) => v
            .as_u64()
            .filter(|ms| *ms > 0)
            .ok_or_else(|| ProtocolError::invalid_params("timeoutMs must be a positive integer")),
        None => Ok(DEFAULT_TIMEOUT_MS),
    }
}

fn append_session_args(args: &mut Vec<String>, session: Option<&str>) {
    if let Some(session) = session {
        args.push("--session".to_string());
        args.push(session.to_string());
    }
}

fn run_cli(args: &[String], stdin_body: Option<String>, timeout_ms: u64) -> Result<CliRun, String> {
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let mut command = Command::new(exe);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if stdin_body.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });

    let mut child = command.spawn().map_err(|e| e.to_string())?;

    if let Some(body) = stdin_body {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "failed to open child stdin".to_string())?;
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("failed to write child stdin: {}", e))?;
    }

    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to open child stdout".to_string())?;
    let mut child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| "failed to open child stderr".to_string())?;

    let stdout_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        child_stdout.read_to_end(&mut buf).map(|_| buf)
    });
    let stderr_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        child_stderr.read_to_end(&mut buf).map(|_| buf)
    });

    let started = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    let stdout = join_output(stdout_thread)?;
                    let stderr = join_output(stderr_thread)?;
                    return Ok(CliRun {
                        exit_code: None,
                        stdout,
                        stderr: append_timeout_message(stderr, timeout_ms),
                    });
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.to_string()),
        }
    };

    let stdout = join_output(stdout_thread)?;
    let stderr = join_output(stderr_thread)?;

    Ok(CliRun {
        exit_code: status.code(),
        stdout,
        stderr,
    })
}

fn join_output(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> Result<String, String> {
    let bytes = handle
        .join()
        .map_err(|_| "failed to join output reader".to_string())?
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn append_timeout_message(stderr: String, timeout_ms: u64) -> String {
    let msg = format!("agent-browser command timed out after {}ms", timeout_ms);
    if stderr.trim().is_empty() {
        msg
    } else {
        format!("{}\n{}", stderr.trim_end(), msg)
    }
}

fn tool_result_from_run(run: CliRun) -> Value {
    let parsed = serde_json::from_str::<Value>(run.stdout.trim()).ok();
    let cli_success = parsed
        .as_ref()
        .and_then(|v| v.get("success"))
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| run.exit_code == Some(0));
    let success = run.exit_code == Some(0) && cli_success;
    let mut content = vec![json!({
        "type": "text",
        "text": tool_text(parsed.as_ref(), &run.stdout, &run.stderr),
    })];

    if success {
        if let Some(image) = parsed.as_ref().and_then(image_content_from_response) {
            content.push(image);
        }
    }

    json!({
        "content": content,
        "structuredContent": {
            "exitCode": run.exit_code,
            "stdout": run.stdout,
            "stderr": run.stderr,
            "response": parsed,
        },
        "isError": !success,
    })
}

fn tool_text(parsed: Option<&Value>, stdout: &str, stderr: &str) -> String {
    let mut text = match parsed {
        Some(value) => response_text(value).unwrap_or_else(|| {
            serde_json::to_string_pretty(value).unwrap_or_else(|_| stdout.trim().to_string())
        }),
        None => stdout.trim().to_string(),
    };

    let stderr = stderr.trim();
    if !stderr.is_empty() {
        if !text.is_empty() {
            text.push_str("\n\nstderr:\n");
        }
        text.push_str(stderr);
    }

    if text.is_empty() {
        "(no output)".to_string()
    } else {
        text
    }
}

fn response_text(value: &Value) -> Option<String> {
    if let Some(obj) = value.as_object() {
        if obj.get("success").and_then(|v| v.as_bool()) == Some(false) {
            return obj
                .get("error")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
        }

        if let Some(data) = obj.get("data") {
            for key in [
                "snapshot", "text", "html", "report", "value", "title", "url", "path",
            ] {
                if let Some(s) = data.get(key).and_then(|v| v.as_str()) {
                    return Some(s.to_string());
                }
            }
            if let Some(result) = data.get("result") {
                return Some(
                    serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string()),
                );
            }
        }
    }

    None
}

fn image_content_from_response(value: &Value) -> Option<Value> {
    let path = value.get("data")?.get("path")?.as_str()?;
    let mime_type = image_mime_type(path)?;
    let metadata = fs::metadata(path).ok()?;
    if metadata.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    Some(json!({
        "type": "image",
        "data": STANDARD.encode(bytes),
        "mimeType": mime_type,
    }))
}

fn image_mime_type(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else {
        None
    }
}

fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into(),
        },
    })
}

fn write_json_line(stdout: &mut io::Stdout, value: &Value) -> io::Result<()> {
    let line = serde_json::to_string(value)?;
    stdout.write_all(line.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_contains_typed_tools() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&TOOL_TOOLS_PROFILES));
        assert!(names.contains(&TOOL_OPEN));
        assert!(names.contains(&TOOL_SNAPSHOT));
        assert!(names.contains(&TOOL_CLICK));
        assert!(names.contains(&TOOL_SCREENSHOT));
        assert!(names.contains(&TOOL_GET_CDP_URL));
        assert!(names.contains(&TOOL_NETWORK_HAR_START));
        assert!(names.contains(&TOOL_REACT_SUSPENSE));
        assert!(names.contains(&TOOL_SKILLS_GET));
        assert!(names.contains(&TOOL_PLUGIN_ADD));
        assert!(names.contains(&TOOL_PLUGIN_LIST));
        assert!(names.contains(&TOOL_PLUGIN_SHOW));
        assert!(names.contains(&TOOL_PLUGIN_RUN));
        assert!(!names.contains(&"agent_browser_frame_list"));
    }

    #[test]
    fn tools_list_uses_unique_names() {
        let tools = tools();
        let mut names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tools.len());
    }

    #[test]
    fn tools_list_is_paginated() {
        let config = McpConfig::all();
        let first_page = list_tools(None, &config).unwrap();
        let first_tools = first_page["tools"].as_array().unwrap();
        assert_eq!(first_tools.len(), TOOL_LIST_PAGE_SIZE);
        let next_cursor = first_page["nextCursor"].as_str().unwrap();

        let second_page = list_tools(Some(&json!({ "cursor": next_cursor })), &config).unwrap();
        let second_tools = second_page["tools"].as_array().unwrap();
        assert!(!second_tools.is_empty());
        assert_ne!(first_tools[0]["name"], second_tools[0]["name"]);
    }

    #[test]
    fn tools_list_rejects_invalid_cursor() {
        let err = list_tools(
            Some(&json!({ "cursor": "not-a-cursor" })),
            &McpConfig::all(),
        )
        .unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn tools_list_defaults_to_core_profile() {
        let config = McpConfig::default();
        let result = list_tools(None, &config).unwrap();
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();

        assert!(names.contains(&TOOL_TOOLS_PROFILES));
        assert!(names.contains(&TOOL_OPEN));
        assert!(names.contains(&TOOL_SNAPSHOT));
        assert!(names.contains(&TOOL_CLICK));
        assert!(names.contains(&TOOL_SCREENSHOT));
        assert!(!names.contains(&TOOL_NETWORK_HAR_START));
        assert!(!names.contains(&TOOL_PLUGIN_LIST));
        assert!(!names.contains(&TOOL_REACT_TREE));
        assert!(result.get("nextCursor").is_none());
    }

    #[test]
    fn tools_list_supports_composed_profiles() {
        let config = McpConfig::from_profiles(vec![ToolProfile::Core, ToolProfile::React]);
        let result = list_tools(None, &config).unwrap();
        let names: Vec<&str> = result["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();

        assert!(names.contains(&TOOL_OPEN));
        assert!(names.contains(&TOOL_REACT_TREE));
        assert!(names.contains(&TOOL_VITALS));
        assert!(!names.contains(&TOOL_NETWORK_HAR_START));
    }

    #[test]
    fn parse_mcp_config_accepts_tools_profiles() {
        let config = parse_mcp_config(&["--tools".into(), "core,network".into()]).unwrap();
        assert!(config.allows(TOOL_OPEN));
        assert!(config.allows(TOOL_NETWORK_REQUESTS));
        assert!(!config.allows(TOOL_REACT_TREE));
    }

    #[test]
    fn parse_mcp_config_accepts_all_profile() {
        let config = parse_mcp_config(&["--tools=all".into()]).unwrap();
        assert!(config.allows(TOOL_OPEN));
        assert!(config.allows(TOOL_NETWORK_HAR_START));
        assert!(config.allows(TOOL_REACT_TREE));
    }

    #[test]
    fn parse_mcp_config_rejects_unknown_profile() {
        let err = parse_mcp_config(&["--tools".into(), "bogus".into()]).unwrap_err();
        assert!(err.contains("Unknown MCP tools profile"));
    }

    #[test]
    fn call_tool_rejects_disabled_profile_tool() {
        let err = call_tool(
            Some(&json!({
                "name": TOOL_NETWORK_HAR_START,
                "arguments": {}
            })),
            &McpConfig::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, -32602);
        assert!(err.message.contains("not enabled"));
    }

    #[test]
    fn tools_profiles_tool_lists_startup_profiles() {
        let result = call_tool(
            Some(&json!({
                "name": TOOL_TOOLS_PROFILES,
                "arguments": {}
            })),
            &McpConfig::default(),
        )
        .unwrap();
        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["activeProfiles"][0], "core");
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("agent-browser mcp --tools all"));
    }

    #[test]
    fn click_command_args_include_new_tab() {
        let args = click_command_args(&json!({
            "selector": "@e1",
            "newTab": true,
        }))
        .unwrap();

        assert_eq!(args, vec!["click", "@e1", "--new-tab"]);
    }

    #[test]
    fn react_json_uses_command_local_raw_json_flag() {
        let mut args = vec!["react".to_string(), "tree".to_string()];
        append_react_raw_json_arg(&json!({ "json": true }), &mut args).unwrap();

        assert_eq!(args, vec!["react", "tree", RAW_JSON_ARG]);
    }

    #[test]
    fn set_media_args_translate_reduced_motion_for_cli_parser() {
        let args = set_media_args(&json!({
            "colorScheme": "dark",
            "reducedMotion": "reduce",
        }))
        .unwrap();

        assert_eq!(args, vec!["set", "media", "dark", "reduced-motion"]);
    }

    #[test]
    fn plugin_add_args_include_registry_options() {
        let args = plugin_add_args(&json!({
            "reference": "@company/agent-browser-plugin-vault",
            "name": "vault",
            "capabilities": ["credential.read", "command.run"],
            "global": true,
            "noManifest": true,
        }))
        .unwrap();

        assert_eq!(
            args,
            vec![
                "plugin",
                "add",
                "@company/agent-browser-plugin-vault",
                "--name",
                "vault",
                "--capability",
                "credential.read",
                "--capability",
                "command.run",
                "--global",
                "--no-manifest",
            ]
        );
    }

    #[test]
    fn plugin_run_args_encode_payload() {
        let args = plugin_run_args(&json!({
            "name": "captcha",
            "requestType": "captcha.solve",
            "payload": {
                "siteKey": "abc",
                "url": "https://example.com"
            },
        }))
        .unwrap();

        assert_eq!(args[0..4], ["plugin", "run", "captcha", "captcha.solve"]);
        assert_eq!(args[4], "--payload");
        let payload: Value = serde_json::from_str(&args[5]).unwrap();
        assert_eq!(payload["siteKey"], "abc");
        assert_eq!(payload["url"], "https://example.com");
    }

    #[test]
    fn tool_schema_includes_extra_args_for_cli_parity() {
        let tools = tools();
        let open = tools
            .iter()
            .find(|t| t["name"].as_str() == Some(TOOL_OPEN))
            .unwrap();
        assert_eq!(
            open["inputSchema"]["properties"]["extraArgs"]["type"],
            "array"
        );
    }

    #[test]
    fn tool_schema_includes_context_management_annotations() {
        let tools = tools();
        let open = tools
            .iter()
            .find(|t| t["name"].as_str() == Some(TOOL_OPEN))
            .unwrap();
        let get_url = tools
            .iter()
            .find(|t| t["name"].as_str() == Some(TOOL_GET_URL))
            .unwrap();
        let skills_get = tools
            .iter()
            .find(|t| t["name"].as_str() == Some(TOOL_SKILLS_GET))
            .unwrap();

        assert_eq!(open["annotations"]["readOnlyHint"], false);
        assert_eq!(open["annotations"]["openWorldHint"], true);
        assert_eq!(get_url["annotations"]["readOnlyHint"], true);
        assert_eq!(get_url["annotations"]["openWorldHint"], true);
        assert_eq!(skills_get["annotations"]["openWorldHint"], false);
    }

    #[test]
    fn required_string_reads_present_field() {
        let value = required_string(&json!({ "selector": "@e1" }), "selector").unwrap();
        assert_eq!(value, "@e1");
    }

    #[test]
    fn required_string_rejects_missing_field() {
        let err = required_string(&json!({}), "selector").unwrap_err();
        assert_eq!(err.code, -32602);
    }

    #[test]
    fn required_string_array_reads_values() {
        let values = required_string_array(&json!({ "values": ["a", "b"] }), "values").unwrap();
        assert_eq!(values, vec!["a", "b"]);
    }

    #[test]
    fn initialize_echoes_supported_protocol_version() {
        let result = initialize_result(
            Some(&json!({
                "protocolVersion": "2024-11-05"
            })),
            &McpConfig::default(),
        );
        assert_eq!(result["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn initialize_defaults_to_latest_protocol_version() {
        let result = initialize_result(None, &McpConfig::default());
        assert_eq!(result["protocolVersion"], PROTOCOL_VERSION);
    }
}
