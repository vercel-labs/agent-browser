//! End-to-end tests for the native daemon.
//!
//! These tests launch a real Chrome instance and exercise the full command
//! pipeline. They require Chrome to be installed and are marked `#[ignore]`
//! so they don't run during normal `cargo test`.
//!
//! Run serially to avoid Chrome instance contention:
//!   cargo test e2e -- --ignored --test-threads=1

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::actions::{execute_command, DaemonState};

fn assert_success(resp: &Value) {
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(true),
        "Expected success but got: {}",
        serde_json::to_string_pretty(resp).unwrap_or_default()
    );
}

fn get_data(resp: &Value) -> &Value {
    resp.get("data").expect("Missing 'data' in response")
}

fn native_test_fixture_html(name: &str) -> &'static str {
    match name {
        "drag_probe" => include_str!("test_fixtures/drag_probe.html"),
        "html5_drag_probe" => include_str!("test_fixtures/html5_drag_probe.html"),
        "pointer_capture_probe" => include_str!("test_fixtures/pointer_capture_probe.html"),
        _ => panic!("Unknown native test fixture: {}", name),
    }
}

fn native_test_fixture_url(name: &str) -> String {
    format!(
        "data:text/html;base64,{}",
        STANDARD.encode(native_test_fixture_html(name))
    )
}

// ---------------------------------------------------------------------------
// Core: launch, navigate, evaluate, url, title, close
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_launch_navigate_evaluate_close() {
    let mut state = DaemonState::new();

    // Launch headless Chrome
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["launched"], true);

    // Navigate to example.com
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], "https://example.com/");
    assert_eq!(get_data(&resp)["title"], "Example Domain");

    // Get URL
    let resp = execute_command(&json!({ "id": "3", "action": "url" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], "https://example.com/");

    // Get title
    let resp = execute_command(&json!({ "id": "4", "action": "title" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["title"], "Example Domain");

    // Evaluate JS
    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "1 + 2" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], 3);

    // Evaluate document.title
    let resp = execute_command(
        &json!({ "id": "6", "action": "evaluate", "script": "document.title" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "Example Domain");

    // Close
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["closed"], true);
}

#[tokio::test]
#[ignore]
async fn e2e_lightpanda_launch_can_open_page() {
    let lightpanda_bin = match std::env::var("LIGHTPANDA_BIN") {
        Ok(path) if !path.is_empty() => path,
        _ => return,
    };

    let mut state = DaemonState::new();

    let resp = tokio::time::timeout(
        tokio::time::Duration::from_secs(20),
        execute_command(
            &json!({
                "id": "1",
                "action": "launch",
                "headless": true,
                "engine": "lightpanda",
                "executablePath": lightpanda_bin,
            }),
            &mut state,
        ),
    )
    .await
    .expect("Lightpanda launch should not hang");

    assert_success(&resp);
    assert_eq!(get_data(&resp)["launched"], true);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], "https://example.com/");
    assert_eq!(get_data(&resp)["title"], "Example Domain");

    let resp = execute_command(&json!({ "id": "3", "action": "close" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["closed"], true);
}

#[tokio::test]
#[ignore]
async fn e2e_lightpanda_auto_launch_can_open_page() {
    let lightpanda_bin = match std::env::var("LIGHTPANDA_BIN") {
        Ok(path) if !path.is_empty() => path,
        _ => return,
    };

    let prev_engine = std::env::var("AGENT_BROWSER_ENGINE").ok();
    let prev_path = std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok();
    std::env::set_var("AGENT_BROWSER_ENGINE", "lightpanda");
    std::env::set_var("AGENT_BROWSER_EXECUTABLE_PATH", &lightpanda_bin);

    let mut state = DaemonState::new();

    let resp = tokio::time::timeout(
        tokio::time::Duration::from_secs(20),
        execute_command(
            &json!({ "id": "1", "action": "navigate", "url": "https://example.com" }),
            &mut state,
        ),
    )
    .await
    .expect("Lightpanda auto-launch should not hang");

    match prev_engine {
        Some(value) => std::env::set_var("AGENT_BROWSER_ENGINE", value),
        None => std::env::remove_var("AGENT_BROWSER_ENGINE"),
    }
    match prev_path {
        Some(value) => std::env::set_var("AGENT_BROWSER_EXECUTABLE_PATH", value),
        None => std::env::remove_var("AGENT_BROWSER_EXECUTABLE_PATH"),
    }

    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], "https://example.com/");
    assert_eq!(get_data(&resp)["title"], "Example Domain");

    let resp = execute_command(&json!({ "id": "2", "action": "close" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["closed"], true);
}

// ---------------------------------------------------------------------------
// Snapshot with refs and ref-based click
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_snapshot_and_click_ref() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Take snapshot
    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();
    assert!(
        snapshot.contains("Example Domain"),
        "Snapshot should contain heading"
    );
    assert!(snapshot.contains("ref=e1"), "Snapshot should have ref e1");
    assert!(snapshot.contains("ref=e2"), "Snapshot should have ref e2");
    assert!(
        snapshot.contains("link"),
        "Snapshot should have a link element"
    );

    // Click the link by ref (e2 is the "More information..." link)
    let resp = execute_command(
        &json!({ "id": "4", "action": "click", "selector": "e2" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Wait for navigation
    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    // Verify URL changed
    let resp = execute_command(&json!({ "id": "5", "action": "url" }), &mut state).await;
    assert_success(&resp);
    let url = get_data(&resp)["url"].as_str().unwrap();
    assert!(
        url.contains("iana.org"),
        "Should have navigated to iana.org, got: {}",
        url
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Screenshot
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_screenshot() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Default screenshot
    let resp = execute_command(&json!({ "id": "3", "action": "screenshot" }), &mut state).await;
    assert_success(&resp);
    let path = get_data(&resp)["path"].as_str().unwrap();
    assert!(path.ends_with(".png"), "Screenshot path should be .png");
    let metadata = std::fs::metadata(path).expect("Screenshot file should exist");
    assert!(
        metadata.len() > 1000,
        "Screenshot should be non-trivial size"
    );

    // Named screenshot
    let tmp_path = std::env::temp_dir()
        .join("agent-browser-e2e-test-screenshot.png")
        .to_string_lossy()
        .to_string();
    let resp = execute_command(
        &json!({ "id": "4", "action": "screenshot", "path": tmp_path }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(std::path::Path::new(&tmp_path).exists());
    let _ = std::fs::remove_file(&tmp_path);

    let resp = execute_command(
        &json!({
            "id": "5",
            "action": "setcontent",
            "html": r##"
                <html><body>
                  <button onclick="document.getElementById('result').textContent = 'clicked'">Submit</button>
                  <a href="#">Home</a>
                  <div id="result"></div>
                </body></html>
            "##,
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "screenshot", "annotate": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let annotations = get_data(&resp)["annotations"]
        .as_array()
        .expect("Annotated screenshot should return annotations");
    assert!(
        !annotations.is_empty(),
        "Annotated screenshot should have at least one annotation"
    );

    let submit_ref = annotations
        .iter()
        .find(|ann| ann.get("name").and_then(|v| v.as_str()) == Some("Submit"))
        .and_then(|ann| ann.get("ref").and_then(|v| v.as_str()))
        .expect("Expected a Submit annotation");

    let resp = execute_command(
        &json!({
            "id": "7",
            "action": "evaluate",
            "script": "document.getElementById('__agent_browser_annotations__') === null"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], true);

    let resp = execute_command(
        &json!({ "id": "8", "action": "click", "selector": format!("@{}", submit_ref) }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "9",
            "action": "evaluate",
            "script": "document.getElementById('result').textContent"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "clicked");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Form interaction: fill, type, select, check
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_form_interaction() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = concat!(
        "data:text/html,<html><body>",
        "<input id='name' type='text' placeholder='Name'>",
        "<input id='email' type='email'>",
        "<select id='color'><option value='red'>Red</option><option value='blue'>Blue</option></select>",
        "<input id='agree' type='checkbox'>",
        "<textarea id='bio'></textarea>",
        "<button id='submit'>Submit</button>",
        "</body></html>"
    );

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Fill name
    let resp = execute_command(
        &json!({ "id": "10", "action": "fill", "selector": "#name", "value": "John Doe" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Verify fill
    let resp = execute_command(
        &json!({ "id": "11", "action": "evaluate", "script": "document.getElementById('name').value" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "John Doe");

    // Type email – the type action now correctly handles punctuation like '.'
    let resp = execute_command(
        &json!({ "id": "12", "action": "type", "selector": "#email", "text": "john@example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "13", "action": "evaluate", "script": "document.getElementById('email').value" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "john@example.com");

    // Select option
    let resp = execute_command(
        &json!({ "id": "14", "action": "select", "selector": "#color", "values": ["blue"] }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "15", "action": "evaluate", "script": "document.getElementById('color').value" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "blue");

    // Check checkbox
    let resp = execute_command(
        &json!({ "id": "16", "action": "check", "selector": "#agree" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "17", "action": "ischecked", "selector": "#agree" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["checked"], true);

    // Uncheck
    let resp = execute_command(
        &json!({ "id": "18", "action": "uncheck", "selector": "#agree" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "19", "action": "ischecked", "selector": "#agree" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["checked"], false);

    // Snapshot should show form state
    let resp = execute_command(&json!({ "id": "20", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let snap = get_data(&resp)["snapshot"].as_str().unwrap();
    assert!(
        snap.contains("John Doe"),
        "Snapshot should show filled value"
    );
    assert!(snap.contains("textbox"), "Snapshot should show textbox");
    assert!(snap.contains("button"), "Snapshot should show button");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Navigation: back, forward, reload
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_navigation_history() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to page 1
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>Page 1</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to page 2
    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": "data:text/html,<h1>Page 2</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Back
    let resp = execute_command(&json!({ "id": "4", "action": "back" }), &mut state).await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "document.querySelector('h1').textContent" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "Page 1");

    // Forward
    let resp = execute_command(&json!({ "id": "6", "action": "forward" }), &mut state).await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "7", "action": "evaluate", "script": "document.querySelector('h1').textContent" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "Page 2");

    // Reload
    let resp = execute_command(&json!({ "id": "8", "action": "reload" }), &mut state).await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "9", "action": "evaluate", "script": "document.querySelector('h1').textContent" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "Page 2");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Cookies
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_cookies() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set cookie
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "cookies_set",
            "name": "test_cookie",
            "value": "hello123"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Get cookies
    let resp = execute_command(&json!({ "id": "4", "action": "cookies_get" }), &mut state).await;
    assert_success(&resp);
    let cookies = get_data(&resp)["cookies"].as_array().unwrap();
    let found = cookies
        .iter()
        .any(|c| c["name"] == "test_cookie" && c["value"] == "hello123");
    assert!(found, "Should find the set cookie");

    // Clear cookies
    let resp = execute_command(&json!({ "id": "5", "action": "cookies_clear" }), &mut state).await;
    assert_success(&resp);

    // Verify cleared
    let resp = execute_command(&json!({ "id": "6", "action": "cookies_get" }), &mut state).await;
    assert_success(&resp);
    let cookies = get_data(&resp)["cookies"].as_array().unwrap();
    let found = cookies.iter().any(|c| c["name"] == "test_cookie");
    assert!(!found, "Cookie should be cleared");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// localStorage / sessionStorage
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_storage() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set local storage
    let resp = execute_command(
        &json!({ "id": "3", "action": "storage_set", "type": "local", "key": "mykey", "value": "myvalue" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Get local storage key
    let resp = execute_command(
        &json!({ "id": "4", "action": "storage_get", "type": "local", "key": "mykey" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["value"], "myvalue");

    // Get all local storage
    let resp = execute_command(
        &json!({ "id": "5", "action": "storage_get", "type": "local" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["data"]["mykey"], "myvalue");

    // Clear
    let resp = execute_command(
        &json!({ "id": "6", "action": "storage_clear", "type": "local" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Verify cleared
    let resp = execute_command(
        &json!({ "id": "7", "action": "storage_get", "type": "local" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = &get_data(&resp)["data"];
    assert!(
        data.as_object().map(|m| m.is_empty()).unwrap_or(true),
        "Storage should be empty after clear"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Tab management
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_tabs() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>Tab 1</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Tab list should show 1 tab
    let resp = execute_command(&json!({ "id": "3", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["active"], true);

    // Open new tab
    let resp = execute_command(
        &json!({ "id": "4", "action": "tab_new", "url": "data:text/html,<h1>Tab 2</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["index"], 1);

    // Tab list should show 2 tabs
    let resp = execute_command(&json!({ "id": "5", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[1]["active"], true);

    // Switch to first tab
    let resp = execute_command(
        &json!({ "id": "6", "action": "tab_switch", "index": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "7", "action": "evaluate", "script": "document.querySelector('h1').textContent" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "Tab 1");

    // Close second tab
    let resp = execute_command(
        &json!({ "id": "8", "action": "tab_close", "index": 1 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Should have 1 tab left
    let resp = execute_command(&json!({ "id": "9", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Element queries: isvisible, isenabled, gettext, getattribute
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_element_queries() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = concat!(
        "data:text/html,<html><body>",
        "<p id='visible'>Hello World</p>",
        "<p id='hidden' style='display:none'>Hidden</p>",
        "<input id='enabled' value='test'>",
        "<input id='disabled' disabled value='nope'>",
        "<a id='link' href='https://example.com' data-testid='my-link'>Click me</a>",
        "</body></html>"
    );

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // isvisible
    let resp = execute_command(
        &json!({ "id": "3", "action": "isvisible", "selector": "#visible" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["visible"], true);

    let resp = execute_command(
        &json!({ "id": "4", "action": "isvisible", "selector": "#hidden" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["visible"], false);

    // isenabled
    let resp = execute_command(
        &json!({ "id": "5", "action": "isenabled", "selector": "#enabled" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["enabled"], true);

    let resp = execute_command(
        &json!({ "id": "6", "action": "isenabled", "selector": "#disabled" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["enabled"], false);

    // gettext
    let resp = execute_command(
        &json!({ "id": "7", "action": "gettext", "selector": "#visible" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "Hello World");

    // getattribute
    let resp = execute_command(
        &json!({ "id": "8", "action": "getattribute", "selector": "#link", "attribute": "href" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["value"], "https://example.com");

    let resp = execute_command(
        &json!({ "id": "9", "action": "getattribute", "selector": "#link", "attribute": "data-testid" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["value"], "my-link");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Wait command
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_wait() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = concat!(
        "data:text/html,<html><body>",
        "<div id='target' style='display:none'>Appeared!</div>",
        "<script>setTimeout(() => document.getElementById('target').style.display='block', 500)</script>",
        "</body></html>"
    );

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Wait for selector to become visible
    let resp = execute_command(
        &json!({ "id": "3", "action": "wait", "selector": "#target", "state": "visible", "timeout": 5000 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Wait for text
    let resp = execute_command(
        &json!({ "id": "4", "action": "wait", "text": "Appeared!", "timeout": 5000 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Timeout wait
    let start = std::time::Instant::now();
    let resp = execute_command(
        &json!({ "id": "5", "action": "wait", "timeout": 200 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(
        start.elapsed().as_millis() >= 150,
        "Timeout wait should sleep at least 150ms"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Viewport with deviceScaleFactor (retina)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_viewport_scale_factor() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "about:blank" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Default devicePixelRatio should be 1
    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": "window.devicePixelRatio" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let default_dpr = get_data(&resp)["result"].as_f64().unwrap();
    assert_eq!(default_dpr, 1.0, "Default devicePixelRatio should be 1");

    // Set viewport with 2x scale factor
    let resp = execute_command(
        &json!({ "id": "4", "action": "viewport", "width": 1920, "height": 1080, "deviceScaleFactor": 2.0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["width"], 1920);
    assert_eq!(get_data(&resp)["height"], 1080);
    assert_eq!(get_data(&resp)["deviceScaleFactor"], 2.0);

    // devicePixelRatio should now be 2
    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "window.devicePixelRatio" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let new_dpr = get_data(&resp)["result"].as_f64().unwrap();
    assert_eq!(
        new_dpr, 2.0,
        "devicePixelRatio should be 2 after setting scale factor"
    );

    // CSS viewport width should still be 1920 (not 3840)
    let resp = execute_command(
        &json!({ "id": "6", "action": "evaluate", "script": "window.innerWidth" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let css_width = get_data(&resp)["result"].as_i64().unwrap();
    assert_eq!(css_width, 1920, "CSS width should remain 1920 at 2x scale");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Viewport and emulation
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_viewport_emulation() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>Viewport</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Get initial width
    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": "window.innerWidth" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let initial_width = get_data(&resp)["result"].as_i64().unwrap();

    // Set viewport to a different size
    let resp = execute_command(
        &json!({ "id": "4", "action": "viewport", "width": 375, "height": 812, "deviceScaleFactor": 3.0, "mobile": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["width"], 375);
    assert_eq!(get_data(&resp)["height"], 812);
    assert_eq!(get_data(&resp)["mobile"], true);

    // Reload to apply viewport change
    let resp = execute_command(&json!({ "id": "5", "action": "reload" }), &mut state).await;
    assert_success(&resp);

    // Width should differ from default (setDeviceMetricsOverride applied)
    let resp = execute_command(
        &json!({ "id": "6", "action": "evaluate", "script": "window.innerWidth" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let new_width = get_data(&resp)["result"].as_i64().unwrap();
    assert!(
        new_width != initial_width || new_width == 375,
        "Viewport should change from {} after setDeviceMetricsOverride (got {})",
        initial_width,
        new_width
    );

    // Set user agent
    let resp = execute_command(
        &json!({ "id": "5", "action": "user_agent", "userAgent": "TestBot/1.0" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "evaluate", "script": "navigator.userAgent" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "TestBot/1.0");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Hover, scroll, press
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_hover_scroll_press() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = concat!(
        "data:text/html,<html><body style='height:3000px'>",
        "<button id='btn' onmouseover=\"this.textContent='hovered'\">Hover me</button>",
        "<input id='input' onkeydown=\"this.dataset.key=event.key\">",
        "</body></html>"
    );

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Hover
    let resp = execute_command(
        &json!({ "id": "3", "action": "hover", "selector": "#btn" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Scroll
    let resp = execute_command(
        &json!({ "id": "4", "action": "scroll", "y": 500 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "window.scrollY" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let scroll_y = get_data(&resp)["result"].as_f64().unwrap();
    assert!(scroll_y > 0.0, "Should have scrolled down");

    // Press key
    let resp = execute_command(
        &json!({ "id": "6", "action": "press", "key": "Enter" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["pressed"], "Enter");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Raw mouse regressions
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_mouse_down_move_up_preserves_drag_state() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "2",
            "action": "navigate",
            "url": native_test_fixture_url("drag_probe")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "evaluate",
            "script": r#"(() => {
                const rect = document.getElementById('target').getBoundingClientRect();
                return {
                    left: Math.round(rect.left),
                    top: Math.round(rect.top),
                    x: Math.round(rect.left + rect.width / 2),
                    y: Math.round(rect.top + rect.height / 2)
                };
            })()"#
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let start = &get_data(&resp)["result"];
    let initial_left = start["left"]
        .as_i64()
        .expect("target left should be numeric");
    let initial_top = start["top"].as_i64().expect("target top should be numeric");
    let start_x = start["x"].as_i64().expect("target x should be numeric");
    let start_y = start["y"].as_i64().expect("target y should be numeric");
    let end_x = start_x + 80;
    let end_y = start_y + 60;

    let resp = execute_command(
        &json!({ "id": "4", "action": "mousemove", "x": start_x, "y": start_y }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "5", "action": "mousedown", "button": "left" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "mousemove", "x": end_x, "y": end_y }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "7", "action": "mouseup", "button": "left" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "8", "action": "evaluate", "script": "window.__dragProbe" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let probe = &get_data(&resp)["result"];
    assert_eq!(probe["finalLeft"].as_i64(), Some(initial_left + 80));
    assert_eq!(probe["finalTop"].as_i64(), Some(initial_top + 60));

    let events = probe["events"]
        .as_array()
        .expect("drag probe should expose events");
    assert!(
        events.iter().any(|event| {
            event["type"] == "mousedown"
                && event["x"].as_f64() == Some(start_x as f64)
                && event["y"].as_f64() == Some(start_y as f64)
                && event["buttons"].as_i64() == Some(1)
        }),
        "Expected a non-zero mousedown event in drag probe"
    );
    assert!(
        events.iter().any(|event| {
            event["type"] == "mousemove"
                && event["x"].as_f64() == Some(end_x as f64)
                && event["y"].as_f64() == Some(end_y as f64)
                && event["buttons"].as_i64() == Some(1)
        }),
        "Expected a drag mousemove with the button still pressed"
    );
    assert!(
        events.iter().any(|event| {
            event["type"] == "mouseup"
                && event["x"].as_f64() == Some(end_x as f64)
                && event["y"].as_f64() == Some(end_y as f64)
                && event["buttons"].as_i64() == Some(0)
        }),
        "Expected mouseup at the last drag position"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_mouse_drag_reaches_pointer_capture_target() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "2",
            "action": "navigate",
            "url": native_test_fixture_url("pointer_capture_probe")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "evaluate",
            "script": r#"(() => {
                const rect = document.getElementById('handle').getBoundingClientRect();
                return {
                    x: Math.round(rect.left + rect.width / 2),
                    y: Math.round(rect.top + rect.height / 2)
                };
            })()"#
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let start = &get_data(&resp)["result"];
    let start_x = start["x"].as_i64().expect("handle x should be numeric");
    let start_y = start["y"].as_i64().expect("handle y should be numeric");
    let end_x = start_x + 80;
    let end_y = start_y + 60;

    let resp = execute_command(
        &json!({ "id": "4", "action": "mousemove", "x": start_x, "y": start_y }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "5", "action": "mousedown", "button": "left" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "mousemove", "x": end_x, "y": end_y }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "7", "action": "mouseup", "button": "left" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "8", "action": "evaluate", "script": "window.__pointerCaptureProbe" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let probe = &get_data(&resp)["result"];
    assert_eq!(probe["moved"].as_bool(), Some(true));

    let events = probe["events"]
        .as_array()
        .expect("pointer capture probe should expose events");
    assert!(
        events.iter().any(|event| {
            event["type"] == "pointermove"
                && event["phase"] == "drag"
                && event["hasCapture"].as_bool() == Some(true)
                && event["x"].as_f64() == Some(end_x as f64)
                && event["y"].as_f64() == Some(end_y as f64)
        }),
        "Expected pointermove with capture during the drag"
    );
    assert!(
        events.iter().any(|event| {
            event["type"] == "pointerup"
                && event["phase"] == "up"
                && event["hadCapture"].as_bool() == Some(true)
        }),
        "Expected pointerup to observe an active pointer capture"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// State save/load, state management
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_state_management() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set some storage
    let resp = execute_command(
        &json!({ "id": "3", "action": "storage_set", "type": "local", "key": "persist_key", "value": "persist_val" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Save state
    let tmp_state = std::env::temp_dir()
        .join("agent-browser-e2e-state.json")
        .to_string_lossy()
        .to_string();
    let resp = execute_command(
        &json!({ "id": "4", "action": "state_save", "path": &tmp_state }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(std::path::Path::new(&tmp_state).exists());

    // State show
    let resp = execute_command(
        &json!({ "id": "5", "action": "state_show", "path": &tmp_state }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let state_data = get_data(&resp);
    assert!(state_data.get("state").is_some());

    // State list
    let resp = execute_command(&json!({ "id": "6", "action": "state_list" }), &mut state).await;
    assert_success(&resp);
    assert!(get_data(&resp)["files"].is_array());

    // Clean up
    let _ = std::fs::remove_file(&tmp_state);

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Domain filter
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_domain_filter() {
    let mut state = DaemonState::new();

    // Set domain filter BEFORE launch so Fetch.enable is called during
    // launch and the background fetch handler intercepts from the start.
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("example.com"));
    }

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Allowed domain
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Blocked domain
    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": "https://blocked.com" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    let error = resp["error"].as_str().unwrap();
    assert!(
        error.contains("blocked") || error.contains("not allowed"),
        "Should reject blocked domain, got: {}",
        error
    );

    // Verify that in-page fetch to a blocked domain is also blocked by
    // the Fetch interception layer (not just the navigate-level check).
    // First navigate to the allowed domain.
    let resp = execute_command(
        &json!({ "id": "4", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Attempt a cross-origin fetch to a blocked domain from the page.
    let resp = execute_command(
        &json!({
            "id": "5", "action": "evaluate",
            "script": "fetch('https://blocked.com/data').then(() => 'ok').catch(e => 'blocked:' + e.message)",
            "await": true,
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let result = get_data(&resp)["result"].as_str().unwrap_or("");
    assert!(
        result.starts_with("blocked:"),
        "Fetch to blocked domain should fail, got: {}",
        result,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Diff engine
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_diff_snapshot() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>Hello</h1><p>World</p>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Take a snapshot and use it as baseline for diff
    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let baseline = get_data(&resp)["snapshot"].as_str().unwrap().to_string();

    // Modify the page
    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "document.querySelector('h1').textContent = 'Changed'" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Diff against baseline
    let resp = execute_command(
        &json!({ "id": "5", "action": "diff_snapshot", "baseline": baseline }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(data["changed"], true, "Diff should detect the h1 change");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Phase 8 commands: focus, clear, count, boundingbox, innertext, setvalue
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_phase8_commands() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = concat!(
        "data:text/html,<html><body>",
        "<input id='a' value='original'>",
        "<input id='b' value='other'>",
        "<p class='item'>One</p>",
        "<p class='item'>Two</p>",
        "<p class='item'>Three</p>",
        "<div id='box' style='width:200px;height:100px;background:red'>Box</div>",
        "</body></html>"
    );

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Focus
    let resp = execute_command(
        &json!({ "id": "10", "action": "focus", "selector": "#a" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Clear
    let resp = execute_command(
        &json!({ "id": "11", "action": "clear", "selector": "#a" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "12", "action": "evaluate", "script": "document.getElementById('a').value" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "");

    // Set value
    let resp = execute_command(
        &json!({ "id": "13", "action": "setvalue", "selector": "#b", "value": "new-value" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "14", "action": "inputvalue", "selector": "#b" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["value"], "new-value");

    // Count
    let resp = execute_command(
        &json!({ "id": "15", "action": "count", "selector": ".item" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["count"], 3);

    // Bounding box
    let resp = execute_command(
        &json!({ "id": "16", "action": "boundingbox", "selector": "#box" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let bbox = get_data(&resp);
    assert_eq!(bbox["width"], 200.0);
    assert_eq!(bbox["height"], 100.0);
    assert!(bbox["x"].as_f64().is_some());
    assert!(bbox["y"].as_f64().is_some());

    // Inner text
    let resp = execute_command(
        &json!({ "id": "17", "action": "innertext", "selector": "#box" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "Box");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Auto-launch (tests that commands auto-launch when no browser exists)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_auto_launch() {
    let mut state = DaemonState::new();

    // Navigate without explicit launch -- should auto-launch
    let resp = execute_command(
        &json!({ "id": "1", "action": "navigate", "url": "data:text/html,<h1>Auto</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(state.browser.is_some(), "Browser should be auto-launched");

    let resp = execute_command(
        &json!({ "id": "2", "action": "evaluate", "script": "document.querySelector('h1').textContent" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "Auto");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Error handling
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_error_handling() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>Errors</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Unknown action
    let resp = execute_command(
        &json!({ "id": "10", "action": "nonexistent_action" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    assert!(resp["error"]
        .as_str()
        .unwrap()
        .contains("Not yet implemented"));

    // Missing required parameter
    let resp = execute_command(
        &json!({ "id": "11", "action": "fill", "selector": "#x" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    assert!(resp["error"].as_str().unwrap().contains("value"));

    // Click on non-existent element
    let resp = execute_command(
        &json!({ "id": "12", "action": "click", "selector": "#does-not-exist" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);

    // Evaluate syntax error
    let resp = execute_command(
        &json!({ "id": "13", "action": "evaluate", "script": "}{invalid" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    assert!(resp["error"].as_str().unwrap().contains("error"));

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Profile cookie persistence across restarts
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_profile_cookie_persistence() {
    let profile_dir = std::env::temp_dir().join(format!(
        "agent-browser-e2e-profile-{}",
        uuid::Uuid::new_v4()
    ));

    // Session 1: launch with profile, set a cookie, close
    {
        let mut state = DaemonState::new();

        let resp = execute_command(
            &json!({
                "id": "1",
                "action": "launch",
                "headless": true,
                "profile": profile_dir.to_str().unwrap()
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "cookies_set",
                "name": "persist_test",
                "value": "should_survive_restart",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        // Verify cookie is set
        let resp =
            execute_command(&json!({ "id": "4", "action": "cookies_get" }), &mut state).await;
        assert_success(&resp);
        let cookies = get_data(&resp)["cookies"].as_array().unwrap();
        let found = cookies
            .iter()
            .any(|c| c["name"] == "persist_test" && c["value"] == "should_survive_restart");
        assert!(found, "Cookie should exist before close");

        let resp = execute_command(&json!({ "id": "5", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Session 2: reopen with the same profile, verify cookie persisted
    {
        let mut state = DaemonState::new();

        let resp = execute_command(
            &json!({
                "id": "10",
                "action": "launch",
                "headless": true,
                "profile": profile_dir.to_str().unwrap()
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "11", "action": "navigate", "url": "https://example.com" }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp =
            execute_command(&json!({ "id": "12", "action": "cookies_get" }), &mut state).await;
        assert_success(&resp);
        let cookies = get_data(&resp)["cookies"].as_array().unwrap();
        let found = cookies
            .iter()
            .any(|c| c["name"] == "persist_test" && c["value"] == "should_survive_restart");
        assert!(
            found,
            "Cookie should persist across restart with --profile. Cookies found: {:?}",
            cookies
                .iter()
                .map(|c| c["name"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    let _ = std::fs::remove_dir_all(&profile_dir);
}

// ---------------------------------------------------------------------------
// Inspect / CDP URL
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_get_cdp_url() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "2", "action": "cdp_url" }), &mut state).await;
    assert_success(&resp);
    let cdp_url = get_data(&resp)["cdpUrl"]
        .as_str()
        .expect("cdpUrl should be a string");
    assert!(
        cdp_url.starts_with("ws://"),
        "CDP URL should start with ws://, got: {}",
        cdp_url
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_inspect() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "inspect" }), &mut state).await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(data["opened"], true);
    let url = data["url"]
        .as_str()
        .expect("inspect url should be a string");
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "Inspect URL should be http://127.0.0.1:<port>, got: {}",
        url
    );

    // Verify the HTTP redirect serves a 302 to the DevTools frontend
    let http_resp = reqwest::get(url).await;
    match http_resp {
        Ok(r) => {
            let final_url = r.url().to_string();
            assert!(
                final_url.contains("devtools/devtools_app.html"),
                "Redirect should point to DevTools frontend, got: {}",
                final_url
            );
        }
        Err(e) => {
            panic!("HTTP GET to inspect URL failed: {}", e);
        }
    }

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Stale ref fallback (#805): clicking a ref after the DOM has been replaced
// should fall back to role/name lookup instead of failing.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_click_stale_ref_falls_back_to_role_name() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to a page with a button that replaces the DOM when clicked.
    let html = r#"data:text/html,<body>
        <div id="c">
            <button onclick="
                var c = document.getElementById('c');
                c.innerHTML = '';
                var b = document.createElement('button');
                b.textContent = 'Target';
                b.onclick = function() { document.title = 'clicked'; };
                c.appendChild(b);
                document.title = 'replaced';
            ">Replace</button>
            <button>Target</button>
        </div>
    </body>"#;

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Snapshot to populate the ref_map with backend_node_ids.
    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();
    assert!(
        snapshot.contains("Replace"),
        "Snapshot should contain Replace button"
    );
    assert!(
        snapshot.contains("Target"),
        "Snapshot should contain Target button"
    );

    // Click "Replace" — this removes all DOM nodes and recreates them,
    // making the backend_node_id for "Target" stale.
    let resp = execute_command(
        &json!({ "id": "4", "action": "click", "selector": "e1" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Verify the DOM was actually replaced.
    let resp = execute_command(&json!({ "id": "5", "action": "title" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["title"], "replaced");

    // Now click the stale "Target" ref. Before the fix this returned:
    //   "CDP error (DOM.getBoxModel): Could not compute box model."
    // After the fix it falls back to role/name lookup and succeeds.
    let resp = execute_command(
        &json!({ "id": "6", "action": "click", "selector": "e2" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Verify the fallback click hit the right (recreated) button.
    let resp = execute_command(&json!({ "id": "7", "action": "title" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["title"],
        "clicked",
        "Stale ref should have been resolved via role/name fallback"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Regression: Material Design checkbox/radio (#832)
//
// Material Design controls hide the native <input> off-screen and place
// overlay elements (ripple, touch-target) on top.  Coordinate-based CDP
// clicks may therefore miss the actual input.  The check/uncheck actions
// must detect this and fall back to a JS .click() — matching the behaviour
// that Playwright provided in v0.19.0.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_material_checkbox_check_uncheck() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Inline HTML that reproduces the Material Design DOM pattern:
    // - Native <input> is visually hidden (position:absolute, opacity:0, off-screen)
    // - A ripple overlay sits on top with pointer-events:all, intercepting coordinate clicks
    // - An ARIA-only checkbox uses role="checkbox" + aria-checked (no native input)
    let html = concat!(
        "data:text/html,<html><body>",
        // -- Native baseline --
        "<input id='native' type='checkbox'>",
        // -- Material-style hidden-input checkbox --
        "<div id='mat' style='position:relative;padding:12px'>",
          "<input id='mat-input' type='checkbox' style='position:absolute;opacity:0;width:1px;height:1px;top:-9999px;left:-9999px;pointer-events:none'>",
          "<div style='position:absolute;top:0;left:0;width:48px;height:48px;pointer-events:all;z-index:10'></div>",
          "<span>Material CB</span>",
        "</div>",
        // -- ARIA-only checkbox (no native input) --
        "<div id='aria' role='checkbox' aria-checked='false' tabindex='0'>ARIA CB</div>",
        "<script>",
          "document.getElementById('aria').addEventListener('click',function(){",
            "var c=this.getAttribute('aria-checked')==='true';",
            "this.setAttribute('aria-checked',String(!c));",
          "});",
        "</script>",
        "</body></html>"
    );

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // ---- Native checkbox (sanity baseline) ----
    let resp = execute_command(
        &json!({ "id": "10", "action": "ischecked", "selector": "#native" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["checked"], false);

    let resp = execute_command(
        &json!({ "id": "11", "action": "check", "selector": "#native" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "12", "action": "ischecked", "selector": "#native" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["checked"], true, "native check failed");

    // ---- Material checkbox (hidden input + overlay) ----
    // ischecked on the wrapper should detect the nested hidden input's state
    let resp = execute_command(
        &json!({ "id": "20", "action": "ischecked", "selector": "#mat" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["checked"], false);

    let resp = execute_command(
        &json!({ "id": "21", "action": "check", "selector": "#mat" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "22", "action": "ischecked", "selector": "#mat" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["checked"],
        true,
        "Material checkbox should be checked after check action (#832)"
    );

    // Idempotency: check again should be a no-op
    let resp = execute_command(
        &json!({ "id": "23", "action": "check", "selector": "#mat" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "24", "action": "ischecked", "selector": "#mat" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["checked"],
        true,
        "Material checkbox should stay checked on redundant check"
    );

    // Uncheck
    let resp = execute_command(
        &json!({ "id": "25", "action": "uncheck", "selector": "#mat" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "26", "action": "ischecked", "selector": "#mat" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["checked"],
        false,
        "Material checkbox should be unchecked after uncheck action"
    );

    // ---- ARIA-only checkbox ----
    let resp = execute_command(
        &json!({ "id": "30", "action": "ischecked", "selector": "#aria" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["checked"], false);

    let resp = execute_command(
        &json!({ "id": "31", "action": "check", "selector": "#aria" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "32", "action": "ischecked", "selector": "#aria" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["checked"],
        true,
        "ARIA checkbox should be checked after check action"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Issue #841 – snapshot -C and screenshot --annotate must not hang over WSS
// ---------------------------------------------------------------------------

/// Verifies that `snapshot -C` (cursor-interactive mode) detects elements with
/// cursor:pointer / onclick / tabindex, produces the correct v0.19.0-compatible
/// output format, deduplicates against the ARIA tree, and completes in bounded
/// time (no sequential CDP round-trip explosion).
#[tokio::test]
#[ignore]
async fn e2e_snapshot_cursor_interactive() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Page with:
    //  - <button> and <a> (standard interactive – ARIA tree, NOT in cursor section)
    //  - <div cursor:pointer onclick> (clickable – cursor section)
    //  - <div tabindex=0> (focusable – cursor section)
    //  - <span cursor:pointer> (clickable – cursor section)
    //  - <span cursor:pointer> child of <div cursor:pointer> (inherited – skip)
    let html = concat!(
        "<html><body>",
        "<a href='#'>Link</a>",
        "<button>Btn</button>",
        "<div style='cursor:pointer' onclick='x()'>ClickDiv</div>",
        "<div tabindex='0'>FocusDiv</div>",
        "<span style='cursor:pointer'>PointerSpan</span>",
        "<div style='cursor:pointer'><span>InheritChild</span></div>",
        "</body></html>",
    );

    let resp = execute_command(
        &json!({ "id": "2", "action": "setcontent", "html": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // snapshot -i -C: interactive tree + cursor section
    let start = std::time::Instant::now();
    let resp = execute_command(
        &json!({ "id": "3", "action": "snapshot", "interactive": true, "cursor": true }),
        &mut state,
    )
    .await;
    let elapsed = start.elapsed();
    assert_success(&resp);

    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();

    // v0.19.0 output format: role + hints
    assert!(
        snapshot.contains("clickable") && snapshot.contains("[cursor:pointer"),
        "Expected v0.19.0-format cursor output with hints:\n{}",
        snapshot,
    );

    // Role differentiation: tabindex-only → focusable
    assert!(
        snapshot.contains("focusable") && snapshot.contains("[tabindex]"),
        "Expected focusable role for tabindex-only element:\n{}",
        snapshot,
    );

    // Text dedup: "Link" and "Btn" are in the ARIA tree, so must NOT suffix
    // with cursor-interactive info. Verify line by line.
    for line in snapshot.lines() {
        assert!(
            !(line.contains("\"Link\"")
                && (line.contains("clickable")
                    || line.contains("focusable")
                    || line.contains("editable"))),
            "Standard <a> element should not have cursor-interactive info:\n{}",
            line
        );
        assert!(
            !(line.contains("\"Btn\"")
                && (line.contains("clickable")
                    || line.contains("focusable")
                    || line.contains("editable"))),
            "Standard <button> element should not have cursor-interactive info:\n{}",
            line
        );
    }

    // Must complete quickly (< 5s), not hit the 30s CDP timeout
    assert!(
        elapsed.as_secs() < 5,
        "snapshot -C took {:?}, expected < 5s (Issue #841 regression)",
        elapsed,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verifies that `screenshot --annotate` completes in bounded time even with
/// many interactive elements. Guards against the sequential CDP round-trip
/// regression that caused hangs over high-latency WSS (Issue #841).
#[tokio::test]
#[ignore]
async fn e2e_screenshot_annotate_many_elements() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // 50 buttons: old sequential code would do 50×2×200ms ≈ 20s over WSS.
    let mut html = String::from("<html><body>");
    for i in 1..=50 {
        html.push_str(&format!("<button>Button {}</button>", i));
    }
    html.push_str("</body></html>");

    let resp = execute_command(
        &json!({ "id": "2", "action": "setcontent", "html": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let start = std::time::Instant::now();
    let resp = execute_command(
        &json!({ "id": "3", "action": "screenshot", "annotate": true }),
        &mut state,
    )
    .await;
    let elapsed = start.elapsed();
    assert_success(&resp);

    let annotations = get_data(&resp)["annotations"]
        .as_array()
        .expect("Annotated screenshot should return annotations");

    assert!(
        annotations.len() >= 50,
        "Expected at least 50 annotations, got {}",
        annotations.len(),
    );

    // Must complete quickly (< 10s), not hit the 30s CDP timeout
    assert!(
        elapsed.as_secs() < 10,
        "screenshot --annotate with 50 elements took {:?}, expected < 10s (Issue #841)",
        elapsed,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verifies `snapshot -C` with many cursor-interactive elements completes in
/// bounded time. Direct regression test for Issue #841's root cause: N×2
/// sequential CDP round-trips per cursor-interactive element.
#[tokio::test]
#[ignore]
async fn e2e_snapshot_cursor_many_elements() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // 100 cursor-interactive divs: old code = 200 sequential CDP calls,
    // at 200ms WSS latency = 40s timeout. New code must finish in seconds.
    let mut html = String::from("<html><body>");
    for i in 1..=100 {
        html.push_str(&format!(
            "<div style='cursor:pointer' onclick='x()'>Item {}</div>",
            i,
        ));
    }
    html.push_str("</body></html>");

    let resp = execute_command(
        &json!({ "id": "2", "action": "setcontent", "html": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let start = std::time::Instant::now();
    let resp = execute_command(
        &json!({ "id": "3", "action": "snapshot", "interactive": true, "cursor": true }),
        &mut state,
    )
    .await;
    let elapsed = start.elapsed();
    assert_success(&resp);

    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();

    // All 100 items should appear
    assert!(
        snapshot.contains("Item 1") && snapshot.contains("Item 100"),
        "Expected all 100 cursor-interactive items in output",
    );

    // All should have v0.19.0-format hints
    assert!(
        snapshot.contains("[cursor:pointer, onclick]"),
        "Expected v0.19.0-format hints",
    );

    // Must complete quickly
    assert!(
        elapsed.as_secs() < 10,
        "snapshot -C with 100 cursor elements took {:?}, expected < 10s (Issue #841)",
        elapsed,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Test that InlineTextBox nodes are filtered from snapshot output while preserving
/// the actual text content from parent elements.
#[tokio::test]
#[ignore]
async fn e2e_snapshot_inline_text_box_filtered() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Simple HTML with text content that would generate InlineTextBox nodes
    let html =
        "data:text/html,<html><body><div><span>Hello</span> <span>World</span></div></body></html>";

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Take snapshot to capture full output and verify InlineTextBox filtering
    let start = std::time::Instant::now();
    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let elapsed = start.elapsed();

    let snapshot_output = get_data(&resp)["snapshot"].as_str().unwrap();

    // Verify that InlineTextBox does not appear in the output
    assert!(
        !snapshot_output.contains("InlineTextBox"),
        "Snapshot output should not contain InlineTextBox: {}",
        snapshot_output
    );

    // Verify that the actual text content is preserved
    assert!(
        snapshot_output.contains("Hello"),
        "Snapshot should contain 'Hello': {}",
        snapshot_output
    );
    assert!(
        snapshot_output.contains("World"),
        "Snapshot should contain 'World': {}",
        snapshot_output
    );

    // Must complete quickly
    assert!(
        elapsed.as_secs() < 5,
        "snapshot with InlineTextBox filtering took {:?}, expected < 5s",
        elapsed,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Helper: tiny HTTP server that echoes request headers as JSON
// ---------------------------------------------------------------------------

/// Starts a TCP listener on localhost:0 and spawns a task that accepts
/// connections, reads the HTTP request, and responds with a JSON body
/// containing all received request headers. Returns the server's base URL.
async fn start_echo_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{}", port);

    let handle = tokio::spawn(async move {
        // Serve up to 20 requests then exit (enough for all tests).
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);

                // Parse headers from the HTTP request.
                let mut headers = serde_json::Map::new();
                for line in request.lines().skip(1) {
                    if line.is_empty() {
                        break;
                    }
                    if let Some((key, value)) = line.split_once(": ") {
                        headers.insert(key.to_string(), Value::String(value.to_string()));
                    }
                }

                let body = serde_json::to_string(&json!({ "headers": headers })).unwrap();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Access-Control-Allow-Origin: *\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    (base_url, handle)
}

// ---------------------------------------------------------------------------
// Origin-scoped --headers tests
// ---------------------------------------------------------------------------

/// Headers passed via --headers on open persist for subsequent same-origin
/// navigations (the core regression from the Rust rewrite).
#[tokio::test]
#[ignore]
async fn e2e_headers_persist_same_origin_navigation() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate with --headers.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/first", base_url),
            "headers": { "X-Test": "scoped" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to the same origin WITHOUT --headers.
    let resp = execute_command(
        &json!({
            "id": "3", "action": "navigate",
            "url": format!("{}/second", base_url),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // The page body is the echo JSON. Read it via evaluate.
    let resp = execute_command(
        &json!({
            "id": "4", "action": "evaluate",
            "script": "JSON.parse(document.body.innerText)",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let result = &get_data(&resp)["result"];
    assert_eq!(
        result["headers"]["X-Test"], "scoped",
        "X-Test header should persist on same-origin navigation without --headers"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Headers passed via --headers on open persist for in-page fetch/XHR to
/// the same origin.
#[tokio::test]
#[ignore]
async fn e2e_headers_persist_same_origin_fetch() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate with --headers.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/page", base_url),
            "headers": { "X-Test": "fetched" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // In-page fetch to the same origin (relative URL).
    let resp = execute_command(
        &json!({
            "id": "3", "action": "evaluate",
            "script": "fetch('/echo').then(r => r.json())",
            "await": true,
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let result = &get_data(&resp)["result"];
    assert_eq!(
        result["headers"]["X-Test"], "fetched",
        "X-Test header should be present on in-page fetch to same origin"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Headers set via --headers do NOT leak to a different origin.
#[tokio::test]
#[ignore]
async fn e2e_headers_do_not_leak_cross_origin() {
    let (server_a, _ha) = start_echo_server().await;
    let (server_b, _hb) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to server A with --headers.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/page", server_a),
            "headers": { "X-Secret": "a-only" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to server B (different origin) without --headers.
    let resp = execute_command(
        &json!({
            "id": "3", "action": "navigate",
            "url": format!("{}/page", server_b),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "4", "action": "evaluate",
            "script": "JSON.parse(document.body.innerText)",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let result = &get_data(&resp)["result"];
    assert!(
        result["headers"].get("X-Secret").is_none(),
        "X-Secret header must NOT leak to a different origin, got: {}",
        result["headers"],
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// In-page fetch to a cross-origin URL must NOT include the origin-scoped
/// headers (sub-resource isolation).
#[tokio::test]
#[ignore]
async fn e2e_headers_do_not_leak_cross_origin_fetch() {
    let (server_a, _ha) = start_echo_server().await;
    let (server_b, _hb) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to server A with --headers.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/page", server_a),
            "headers": { "X-Secret": "a-only" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Fetch from the page to server B (cross-origin sub-resource).
    let resp = execute_command(
        &json!({
            "id": "3", "action": "evaluate",
            "script": format!("fetch('{}/echo').then(r => r.json())", server_b),
            "await": true,
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let result = &get_data(&resp)["result"];
    assert!(
        result["headers"].get("X-Secret").is_none(),
        "X-Secret header must NOT leak to cross-origin fetch, got: {}",
        result["headers"],
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// `set headers` (global headers via the headers action) must not be
/// regressed — they should persist across navigations without being
/// cleared by the origin-scoped header logic.
#[tokio::test]
#[ignore]
async fn e2e_set_headers_not_regressed() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set global headers via the `headers` action (not --headers on navigate).
    let resp = execute_command(
        &json!({
            "id": "2", "action": "headers",
            "headers": { "X-Global": "everywhere" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate — global headers should be present.
    let resp = execute_command(
        &json!({
            "id": "3", "action": "navigate",
            "url": format!("{}/page", base_url),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "4", "action": "evaluate",
            "script": "JSON.parse(document.body.innerText)",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let result = &get_data(&resp)["result"];
    assert_eq!(
        result["headers"]["X-Global"], "everywhere",
        "Global headers set via `set headers` must persist across navigations"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Multiple origins each get their own independent headers.
#[tokio::test]
#[ignore]
async fn e2e_headers_multiple_origins_independent() {
    let (server_a, _ha) = start_echo_server().await;
    let (server_b, _hb) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set headers for origin A.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/page", server_a),
            "headers": { "X-From": "alpha" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set different headers for origin B.
    let resp = execute_command(
        &json!({
            "id": "3", "action": "navigate",
            "url": format!("{}/page", server_b),
            "headers": { "X-From": "beta" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Verify B got its own header.
    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "JSON.parse(document.body.innerText)" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"]["headers"]["X-From"], "beta");

    // Navigate back to A — should get A's header, not B's.
    let resp = execute_command(
        &json!({ "id": "5", "action": "navigate", "url": format!("{}/check", server_a) }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "evaluate", "script": "JSON.parse(document.body.innerText)" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"]["headers"]["X-From"], "alpha");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Headers persist when navigating away to a different origin and back.
#[tokio::test]
#[ignore]
async fn e2e_headers_persist_after_roundtrip() {
    let (server_a, _ha) = start_echo_server().await;
    let (server_b, _hb) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set headers for origin A.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/page", server_a),
            "headers": { "X-Persist": "roundtrip" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate away to B (no headers).
    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": format!("{}/page", server_b) }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate back to A without --headers.
    let resp = execute_command(
        &json!({ "id": "4", "action": "navigate", "url": format!("{}/back", server_a) }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "JSON.parse(document.body.innerText)" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"]["headers"]["X-Persist"],
        "roundtrip",
        "Headers should persist after navigating away and back to the same origin"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Passing --headers a second time to the same origin replaces the previous headers.
#[tokio::test]
#[ignore]
async fn e2e_headers_override_same_origin() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set initial headers.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/first", base_url),
            "headers": { "X-Version": "v1" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Override with new headers.
    let resp = execute_command(
        &json!({
            "id": "3", "action": "navigate",
            "url": format!("{}/second", base_url),
            "headers": { "X-Version": "v2" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "JSON.parse(document.body.innerText)" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"]["headers"]["X-Version"],
        "v2",
        "Second --headers should replace the first for the same origin"
    );

    // Subsequent navigation without --headers should use v2.
    let resp = execute_command(
        &json!({ "id": "5", "action": "navigate", "url": format!("{}/third", base_url) }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "evaluate", "script": "JSON.parse(document.body.innerText)" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"]["headers"]["X-Version"], "v2");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// `set headers` (global) and `--headers` (origin-scoped) stack together.
#[tokio::test]
#[ignore]
async fn e2e_global_and_scoped_headers_stack() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set global headers via `set headers`.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "headers",
            "headers": { "X-Global": "everywhere" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Set origin-scoped headers via --headers.
    let resp = execute_command(
        &json!({
            "id": "3", "action": "navigate",
            "url": format!("{}/page", base_url),
            "headers": { "X-Scoped": "this-origin" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "JSON.parse(document.body.innerText)" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let headers = &get_data(&resp)["result"]["headers"];
    assert_eq!(
        headers["X-Global"], "everywhere",
        "Global header should be present alongside scoped header"
    );
    assert_eq!(
        headers["X-Scoped"], "this-origin",
        "Scoped header should be present alongside global header"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Origin-scoped headers with different casing than the browser's original
/// request headers must not produce duplicates (HTTP headers are
/// case-insensitive per RFC 7230).
#[tokio::test]
#[ignore]
async fn e2e_headers_case_insensitive_no_duplicates() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Chrome sends "Accept: ..." by default on navigations. Pass "accept"
    // (lowercase) via --headers to verify the merge is case-insensitive
    // and doesn't produce a duplicate Accept header.
    let resp = execute_command(
        &json!({
            "id": "2", "action": "navigate",
            "url": format!("{}/page", base_url),
            "headers": { "accept": "application/test" },
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3", "action": "evaluate",
            "script": "JSON.parse(document.body.innerText)",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let result = &get_data(&resp)["result"]["headers"];

    // The echo server stores headers keyed by name as received on the wire.
    // If deduplication works, only our custom "accept" value should appear
    // (Chrome's original "Accept: text/html,..." should be suppressed).
    let accept_val = result
        .get("accept")
        .or_else(|| result.get("Accept"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        accept_val, "application/test",
        "Case-insensitive merge should replace Chrome's Accept header, got headers: {}",
        result,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}
