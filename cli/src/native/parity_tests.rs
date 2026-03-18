//! Parity tests for the native daemon's command interface.
//!
//! These unit tests verify:
//! - All documented actions are handled (not returning "Not yet implemented")
//! - Response format consistency (success/error structure)
//! - Credential and state actions work without a browser

use serde_json::{json, Value};

use super::actions::{execute_command, DaemonState};

const ENCRYPTION_KEY_ENV: &str = "AGENT_BROWSER_ENCRYPTION_KEY";

struct TestKeyGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    original: Option<String>,
}

impl TestKeyGuard {
    fn new() -> Self {
        let lock = super::auth::AUTH_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let original = std::env::var(ENCRYPTION_KEY_ENV).ok();
        // SAFETY: AUTH_TEST_MUTEX serializes all test access so no concurrent mutation.
        unsafe { std::env::set_var(ENCRYPTION_KEY_ENV, "a".repeat(64)) };
        Self {
            _lock: lock,
            original,
        }
    }
}

impl Drop for TestKeyGuard {
    fn drop(&mut self) {
        // SAFETY: AUTH_TEST_MUTEX is held via _lock.
        match &self.original {
            Some(val) => unsafe { std::env::set_var(ENCRYPTION_KEY_ENV, val) },
            None => unsafe { std::env::remove_var(ENCRYPTION_KEY_ENV) },
        }
    }
}

/// All documented action names that should be implemented.
const DOCUMENTED_ACTIONS: &[&str] = &[
    "launch",
    "navigate",
    "url",
    "title",
    "content",
    "evaluate",
    "close",
    "snapshot",
    "screenshot",
    "click",
    "dblclick",
    "fill",
    "type",
    "press",
    "hover",
    "scroll",
    "select",
    "check",
    "uncheck",
    "wait",
    "gettext",
    "getattribute",
    "isvisible",
    "isenabled",
    "ischecked",
    "back",
    "forward",
    "reload",
    "cookies_get",
    "cookies_set",
    "cookies_clear",
    "storage_get",
    "storage_set",
    "storage_clear",
    "setcontent",
    "headers",
    "offline",
    "console",
    "errors",
    "state_save",
    "state_load",
    "state_list",
    "state_show",
    "state_clear",
    "state_clean",
    "state_rename",
    "trace_start",
    "trace_stop",
    "profiler_start",
    "profiler_stop",
    "recording_start",
    "recording_stop",
    "recording_restart",
    "pdf",
    "tab_list",
    "tab_new",
    "tab_switch",
    "tab_close",
    "viewport",
    "user_agent",
    "set_media",
    "download",
    "diff_snapshot",
    "diff_url",
    "credentials_set",
    "credentials_get",
    "credentials_delete",
    "credentials_list",
    "mouse",
    "keyboard",
    "focus",
    "clear",
    "selectall",
    "scrollintoview",
    "dispatch",
    "highlight",
    "tap",
    "boundingbox",
    "innertext",
    "innerhtml",
    "inputvalue",
    "setvalue",
    "count",
    "styles",
    "bringtofront",
    "timezone",
    "locale",
    "geolocation",
    "permissions",
    "dialog",
    "upload",
    "addscript",
    "addinitscript",
    "addstyle",
    "clipboard",
    "wheel",
    "device",
    "screencast_start",
    "screencast_stop",
    "waitforurl",
    "waitforloadstate",
    "waitforfunction",
    "frame",
    "mainframe",
    "getbyrole",
    "getbytext",
    "getbylabel",
    "getbyplaceholder",
    "getbyalttext",
    "getbytitle",
    "getbytestid",
    "nth",
    "find",
    "evalhandle",
    "drag",
    "expose",
    "pause",
    "multiselect",
    "responsebody",
    "waitfordownload",
    "window_new",
    "diff_screenshot",
    "video_start",
    "video_stop",
    "har_start",
    "har_stop",
    "route",
    "unroute",
    "requests",
    "credentials",
    "auth_save",
    "auth_login",
    "auth_list",
    "auth_delete",
    "auth_show",
    "confirm",
    "deny",
    "swipe",
    "device_list",
    "input_mouse",
    "input_keyboard",
    "input_touch",
    "keydown",
    "keyup",
    "inserttext",
    "mousemove",
    "mousedown",
    "mouseup",
];

fn minimal_command(action: &str, id: &str) -> Value {
    let mut cmd = json!({ "action": action, "id": id });
    let obj = cmd.as_object_mut().unwrap();

    match action {
        "navigate" | "diff_url" | "waitforurl" => {
            obj.insert("url".to_string(), json!("https://example.com"));
        }
        "evaluate" | "expose" => {
            obj.insert("script".to_string(), json!("1"));
        }
        "click" | "dblclick" | "fill" | "type" | "press" | "hover" | "scroll" | "select"
        | "check" | "uncheck" | "gettext" | "getattribute" | "isvisible" | "isenabled"
        | "ischecked" | "focus" | "clear" | "selectall" | "scrollintoview" | "dispatch"
        | "highlight" | "tap" | "boundingbox" | "innertext" | "innerhtml" | "inputvalue"
        | "setvalue" | "count" | "find" | "nth" | "getbytext" | "getbylabel"
        | "getbyplaceholder" | "getbyalttext" | "getbytitle" | "getbytestid" => {
            obj.insert("selector".to_string(), json!("body"));
        }
        "getbyrole" => {
            obj.insert("role".to_string(), json!("button"));
            obj.insert("selector".to_string(), json!("body"));
        }
        "setcontent" => {
            obj.insert("html".to_string(), json!("<html></html>"));
        }
        "cookies_set" => {
            obj.insert("name".to_string(), json!("test"));
            obj.insert("value".to_string(), json!("val"));
        }
        "storage_get" | "storage_set" | "storage_clear" => {
            obj.insert("origin".to_string(), json!("https://example.com"));
        }
        "state_save" | "state_load" | "state_show" | "state_clear" => {
            obj.insert("path".to_string(), json!("test-parity-state.json"));
        }
        "state_rename" => {
            obj.insert("path".to_string(), json!("test-parity-state.json"));
            obj.insert("name".to_string(), json!("renamed"));
        }
        "state_clean" => {
            obj.insert("days".to_string(), json!(7));
        }
        "credentials_set" => {
            obj.insert("name".to_string(), json!("parity-test-cred"));
            obj.insert("username".to_string(), json!("u"));
            obj.insert("password".to_string(), json!("p"));
        }
        "auth_save" => {
            obj.insert("name".to_string(), json!("parity-test-cred"));
            obj.insert("url".to_string(), json!("https://example.com"));
            obj.insert("username".to_string(), json!("u"));
            obj.insert("password".to_string(), json!("p"));
        }
        "credentials_get" | "credentials_delete" | "auth_show" | "auth_delete" => {
            obj.insert("name".to_string(), json!("parity-test-cred"));
        }
        "tab_switch" | "tab_close" => {
            obj.insert("index".to_string(), json!(0));
        }
        "viewport" | "user_agent" | "set_media" | "timezone" | "locale" | "geolocation"
        | "permissions" | "device" => {
            obj.insert("value".to_string(), json!(null));
        }
        "headers" => {
            obj.insert("headers".to_string(), json!({}));
        }
        "offline" => {
            obj.insert("offline".to_string(), json!(false));
        }
        "wait" => {
            obj.insert("timeout".to_string(), json!(100));
        }
        "waitforloadstate" => {
            obj.insert("state".to_string(), json!("load"));
        }
        "waitforfunction" => {
            obj.insert("script".to_string(), json!("() => true"));
        }
        "frame" => {
            obj.insert("selector".to_string(), json!("iframe"));
        }
        "addscript" => {
            obj.insert("content".to_string(), json!("console.log('test')"));
        }
        "addinitscript" => {
            obj.insert("script".to_string(), json!("console.log('init')"));
        }
        "addstyle" => {
            obj.insert("content".to_string(), json!("body { color: red }"));
        }
        "wheel" => {
            obj.insert("deltaX".to_string(), json!(0));
            obj.insert("deltaY".to_string(), json!(0));
        }
        "upload" => {
            obj.insert("selector".to_string(), json!("input[type=file]"));
            obj.insert("files".to_string(), json!([]));
        }
        "dialog" => {
            obj.insert("accept".to_string(), json!(true));
        }
        "credentials" => {
            obj.insert("username".to_string(), json!("u"));
            obj.insert("password".to_string(), json!("p"));
        }
        "auth_login" => {
            obj.insert("name".to_string(), json!("parity-test-cred"));
        }
        "route" => {
            obj.insert("url".to_string(), json!("*"));
            obj.insert("handler".to_string(), json!("continue"));
        }
        "diff_snapshot" | "diff_screenshot" => {
            obj.insert("selector".to_string(), json!("body"));
        }
        "recording_start" | "recording_restart" => {
            obj.insert("path".to_string(), json!("/tmp/parity-recording.webm"));
        }
        "video_start" => {
            obj.insert("path".to_string(), json!("/tmp/parity-video.webm"));
        }
        "profiler_start" => {
            obj.insert("path".to_string(), json!("/tmp/parity-profile"));
        }
        "trace_stop" | "har_stop" => {
            obj.insert("path".to_string(), json!("/tmp/parity-trace"));
        }
        "download" => {
            obj.insert("path".to_string(), json!("/tmp/parity-download"));
        }
        "multiselect" => {
            obj.insert("selector".to_string(), json!("select"));
            obj.insert("values".to_string(), json!([]));
        }
        "responsebody" => {
            obj.insert("url".to_string(), json!("https://example.com"));
        }
        "waitfordownload" => {
            obj.insert("path".to_string(), json!("/tmp/parity-download"));
        }
        "styles" => {
            obj.insert("selector".to_string(), json!("body"));
            obj.insert("names".to_string(), json!([]));
        }
        "evalhandle" => {
            obj.insert("handle".to_string(), json!(""));
            obj.insert("script".to_string(), json!("h => h"));
        }
        "drag" => {
            obj.insert("selector".to_string(), json!("body"));
            obj.insert("target".to_string(), json!("body"));
        }
        "swipe" => {
            obj.insert("selector".to_string(), json!("body"));
            obj.insert("direction".to_string(), json!("left"));
        }
        "input_mouse" | "mousemove" | "mousedown" | "mouseup" => {
            obj.insert("x".to_string(), json!(100));
            obj.insert("y".to_string(), json!(100));
        }
        "input_keyboard" | "keydown" | "keyup" => {
            obj.insert("key".to_string(), json!("a"));
        }
        "input_touch" => {
            obj.insert("type".to_string(), json!("touchStart"));
            obj.insert("touchPoints".to_string(), json!([]));
        }
        "inserttext" => {
            obj.insert("text".to_string(), json!("test"));
        }
        _ => {}
    }
    cmd
}

// ---------------------------------------------------------------------------
// 1. Action dispatch coverage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_all_documented_actions_are_handled() {
    let mut state = DaemonState::new();

    for (i, action) in DOCUMENTED_ACTIONS.iter().enumerate() {
        let id = format!("parity-{}", i);
        let cmd = minimal_command(action, &id);
        let result = execute_command(&cmd, &mut state).await;

        assert!(
            result.get("id").is_some(),
            "Action '{}': response missing 'id'",
            action
        );

        let error = result.get("error").and_then(|v| v.as_str()).unwrap_or("");

        assert!(
            !error.contains("Not yet implemented"),
            "Action '{}' returned 'Not yet implemented')",
            action
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Response format consistency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_success_response_format() {
    let mut state = DaemonState::new();
    let cmd = json!({ "action": "state_list", "id": "fmt-1" });
    let result = execute_command(&cmd, &mut state).await;

    assert_eq!(result["success"], true);
    assert!(result.get("id").is_some());
    assert!(result.get("data").is_some());
    assert!(result.get("error").is_none());
}

#[tokio::test]
async fn test_error_response_format() {
    let mut state = DaemonState::new();
    let cmd = json!({ "action": "nonexistent_action_xyz", "id": "fmt-2" });
    let result = execute_command(&cmd, &mut state).await;

    assert_eq!(result["success"], false);
    assert!(result.get("id").is_some());
    assert!(result.get("error").is_some());
}

// ---------------------------------------------------------------------------
// 3. Credential/state actions work without a browser
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_state_list_without_browser() {
    let mut state = DaemonState::new();
    let cmd = json!({ "action": "state_list", "id": "nb-1" });
    let result = execute_command(&cmd, &mut state).await;

    assert_eq!(result["success"], true);
    assert!(result["data"]["files"].is_array());
}

#[tokio::test]
async fn test_credentials_list_without_browser() {
    let mut state = DaemonState::new();
    let cmd = json!({ "action": "credentials_list", "id": "nb-2" });
    let result = execute_command(&cmd, &mut state).await;

    assert_eq!(result["success"], true);
    assert!(result["data"]["credentials"].is_array() || result["data"]["profiles"].is_array());
}

// ---------------------------------------------------------------------------
// 4. New feature parity tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_auth_profile_name_validation() {
    use super::auth;
    let _key_guard = TestKeyGuard::new();
    let valid = auth::credentials_set("valid-name_123", "u", "p", None);
    assert!(valid.is_ok());
    let invalid = auth::credentials_set("invalid/name", "u", "p", None);
    assert!(invalid.is_err());
    let invalid2 = auth::credentials_set("", "u", "p", None);
    assert!(invalid2.is_err());
    let invalid3 = auth::credentials_set("has space", "u", "p", None);
    assert!(invalid3.is_err());
    // Cleanup
    let _ = auth::credentials_delete("valid-name_123");
}

#[tokio::test]
async fn test_auth_save_and_show() {
    use super::auth;
    let _key_guard = TestKeyGuard::new();
    let result = auth::auth_save(
        "parity-roundtrip",
        "https://example.com",
        "user",
        "pass",
        Some("input#user"),
        None,
        None,
    );
    assert!(result.is_ok());

    let show = auth::auth_show("parity-roundtrip");
    assert!(show.is_ok());
    let data = show.unwrap();
    assert_eq!(data["profile"]["username"], "user");
    assert_eq!(data["profile"]["usernameSelector"], "input#user");

    let full = auth::credentials_get_full("parity-roundtrip");
    assert!(full.is_ok());
    assert_eq!(full.unwrap().password, "pass");

    // Cleanup
    let _ = auth::credentials_delete("parity-roundtrip");
}

#[tokio::test]
async fn test_har_start_stop_without_browser() {
    let mut state = DaemonState::new();
    // har_start requires a browser. Because execute_command auto-launches when
    // no browser is present, the result depends on Chrome availability: success
    // if Chrome is found (CI), failure if not. Both outcomes are valid.
    let cmd = json!({ "action": "har_start", "id": "har-1" });
    let result = execute_command(&cmd, &mut state).await;
    let success = result["success"].as_bool().unwrap_or(false);
    if success {
        assert!(state.har_recording);
    } else {
        assert!(result["error"].as_str().is_some());
    }
}

#[tokio::test]
async fn test_state_clean_action() {
    let mut state = DaemonState::new();
    let cmd = json!({ "action": "state_clean", "id": "clean-1", "days": 30 });
    let result = execute_command(&cmd, &mut state).await;
    assert_eq!(result["success"], true);
}

#[tokio::test]
async fn test_daemon_state_new_defaults() {
    let state = DaemonState::new();
    assert!(state.browser.is_none());
    assert!(!state.har_recording);
    assert!(state.har_entries.is_empty());
    assert!(state.pending_confirmation.is_none());
    assert!(!state.request_tracking);
    assert!(state.tracked_requests.is_empty());
    assert!(state.active_frame_id.is_none());
    assert!(state.webdriver_backend.is_none());
    assert!(state.stream_client.is_none());
}

#[tokio::test]
async fn test_tracked_request_struct() {
    use super::actions::TrackedRequest;
    let tr = TrackedRequest {
        url: "https://example.com/api".to_string(),
        method: "GET".to_string(),
        headers: json!({"Accept": "text/html"}),
        timestamp: 12345,
        resource_type: "Document".to_string(),
    };
    let serialized = serde_json::to_value(&tr).unwrap();
    assert_eq!(serialized["url"], "https://example.com/api");
    assert_eq!(serialized["method"], "GET");
    assert_eq!(serialized["resourceType"], "Document");
    assert_eq!(serialized["timestamp"], 12345);
}

#[tokio::test]
async fn test_request_tracking_state() {
    let mut state = DaemonState::new();
    assert!(!state.request_tracking);
    assert!(state.tracked_requests.is_empty());

    state.tracked_requests.push(super::actions::TrackedRequest {
        url: "https://example.com".to_string(),
        method: "GET".to_string(),
        headers: json!({}),
        timestamp: 1,
        resource_type: "Document".to_string(),
    });
    state.tracked_requests.push(super::actions::TrackedRequest {
        url: "https://other.com".to_string(),
        method: "POST".to_string(),
        headers: json!({}),
        timestamp: 2,
        resource_type: "XHR".to_string(),
    });
    assert_eq!(state.tracked_requests.len(), 2);

    // Filter
    let filtered: Vec<_> = state
        .tracked_requests
        .iter()
        .filter(|r| r.url.contains("example"))
        .collect();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].url, "https://example.com");

    // Clear
    state.tracked_requests.clear();
    assert!(state.tracked_requests.is_empty());
}

#[tokio::test]
async fn test_addscript_and_addinitscript_separate_dispatch() {
    let mut state = DaemonState::new();

    // Both should be handled (not "Not yet implemented") even without a browser
    let cmd1 = json!({ "action": "addscript", "id": "as-1", "content": "console.log(1)" });
    let result1 = execute_command(&cmd1, &mut state).await;
    let err1 = result1["error"].as_str().unwrap_or("");
    assert!(
        !err1.contains("Not yet implemented"),
        "addscript should be handled"
    );

    let cmd2 = json!({ "action": "addinitscript", "id": "ais-1", "script": "console.log(2)" });
    let result2 = execute_command(&cmd2, &mut state).await;
    let err2 = result2["error"].as_str().unwrap_or("");
    assert!(
        !err2.contains("Not yet implemented"),
        "addinitscript should be handled"
    );
}

#[tokio::test]
async fn test_frame_context_management() {
    let mut state = DaemonState::new();
    assert!(state.active_frame_id.is_none());

    // Set a frame ID and verify it persists
    state.active_frame_id = Some("child-frame-123".to_string());
    assert_eq!(state.active_frame_id.as_deref(), Some("child-frame-123"));

    // Clearing the frame ID (what mainframe does)
    state.active_frame_id = None;
    assert!(state.active_frame_id.is_none());
}

#[tokio::test]
async fn test_addstyle_supports_content_and_url() {
    let mut state = DaemonState::new();

    // Both content-based and url-based addstyle should be recognized
    let cmd1 = json!({ "action": "addstyle", "id": "style-1", "content": "body { color: red }" });
    let result1 = execute_command(&cmd1, &mut state).await;
    let err1 = result1["error"].as_str().unwrap_or("");
    assert!(!err1.contains("Not yet implemented"));

    let cmd2 =
        json!({ "action": "addstyle", "id": "style-2", "url": "https://example.com/style.css" });
    let result2 = execute_command(&cmd2, &mut state).await;
    let err2 = result2["error"].as_str().unwrap_or("");
    assert!(!err2.contains("Not yet implemented"));
}

#[tokio::test]
async fn test_domain_filter_sanitize() {
    use super::network::DomainFilter;
    let filter = DomainFilter::new("example.com");
    assert!(filter.is_allowed("example.com"));
    assert!(!filter.is_allowed("evil.com"));
    filter.check_url("https://example.com/path").unwrap();
    assert!(filter.check_url("https://evil.com").is_err());
}

#[tokio::test]
async fn test_state_find_auto_returns_none_for_nonexistent() {
    use super::state;
    let result = state::find_auto_state_file("nonexistent-session-xyz");
    assert!(result.is_none());
}
