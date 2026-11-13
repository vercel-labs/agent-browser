//! End-to-end tests for the native daemon.
//!
//! These tests launch a real Chrome instance and exercise the full command
//! pipeline. They require Chrome to be installed and are marked `#[ignore]`
//! so they don't run during normal `cargo test`.
//!
//! Run serially to avoid Chrome instance contention:
//!   cargo test e2e -- --ignored --test-threads=1

use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::test_utils::EnvGuard;

use super::actions::{
    close_current_browser, execute_command, maybe_autosave_restore_state, DaemonState,
};
use super::bidi::firefox as bidi_firefox;
use super::bidi::target::{resolve_target, Target};

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

async fn select_values(
    state: &mut DaemonState,
    id: &str,
    selector: &str,
    values: &[&str],
) -> Value {
    execute_command(
        &json!({ "id": id, "action": "select", "selector": selector, "values": values }),
        state,
    )
    .await
}

async fn assert_evaluate(state: &mut DaemonState, id: &str, script: &str, expected: Value) {
    let resp = execute_command(
        &json!({ "id": id, "action": "evaluate", "script": script }),
        state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], expected);
}

fn assert_error_code(resp: &Value, code: &str) {
    assert_eq!(
        resp.get("success").and_then(Value::as_bool),
        Some(false),
        "Expected failure but got: {}",
        serde_json::to_string_pretty(resp).unwrap_or_default()
    );
    assert_eq!(resp.get("code").and_then(Value::as_str), Some(code));
}

#[tokio::test]
#[ignore]
async fn e2e_empty_table_cell_refs_resolve() {
    let html = r#"<!doctype html><table role="table"><tbody><tr>
<td role="cell" id="empty-cell"></td>
<td role="cell" id="named-cell">Named cell</td>
<td role="cell" id="nbsp-cell">&nbsp;</td>
<th role="columnheader" id="empty-columnheader"></th>
<th role="rowheader" id="empty-rowheader"></th>
</tr></tbody></table><div role="grid"><div role="row"><div role="gridcell" id="empty-gridcell"></div></div></div>"#;
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let refs = get_data(&resp)["refs"]
        .as_object()
        .expect("snapshot refs should be an object");
    let mut resolved = Vec::new();
    for (reference, metadata) in refs {
        let role = metadata["role"].as_str().unwrap_or_default();
        if ["cell", "gridcell", "columnheader", "rowheader"].contains(&role) {
            let resp = execute_command(
                &json!({
                    "id": format!("resolve-{reference}"),
                    "action": "getattribute",
                    "selector": format!("@{reference}"),
                    "attribute": "id"
                }),
                &mut state,
            )
            .await;
            assert_success(&resp);
            resolved.push((
                get_data(&resp)["value"].as_str().unwrap().to_string(),
                role.to_string(),
            ));
        }
    }
    resolved.sort();
    assert_eq!(
        resolved,
        [
            ("empty-cell".to_string(), "cell".to_string()),
            ("empty-columnheader".to_string(), "columnheader".to_string()),
            ("empty-gridcell".to_string(), "gridcell".to_string()),
            ("empty-rowheader".to_string(), "rowheader".to_string()),
            ("named-cell".to_string(), "cell".to_string()),
            ("nbsp-cell".to_string(), "cell".to_string()),
        ]
    );

    let resp = execute_command(&json!({ "id": "close", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

fn native_test_fixture_html(name: &str) -> &'static str {
    match name {
        "drag_probe" => include_str!("test_fixtures/drag_probe.html"),
        "wheel_probe" => include_str!("test_fixtures/wheel_probe.html"),
        "html5_drag_probe" => include_str!("test_fixtures/html5_drag_probe.html"),
        "pointer_capture_probe" => include_str!("test_fixtures/pointer_capture_probe.html"),
        "snapshot_diff_probe" => include_str!("test_fixtures/snapshot_diff_probe.html"),
        "upload_probe" => include_str!("test_fixtures/upload_probe.html"),
        "webmcp_delayed_probe" => include_str!("test_fixtures/webmcp_delayed_probe.html"),
        "webmcp_frame_probe" => include_str!("test_fixtures/webmcp_frame_probe.html"),
        "webmcp_probe" => include_str!("test_fixtures/webmcp_probe.html"),
        "webmcp_context_probe" => include_str!("test_fixtures/webmcp_context_probe.html"),
        _ => panic!("Unknown native test fixture: {}", name),
    }
}

#[tokio::test]
#[ignore]
async fn e2e_webmcp_discovery_invocation_and_cancellation() {
    let (fixture_url, fixture_server) = start_webmcp_server().await;
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
            "url": fixture_url
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["webmcp"]["experimental"], true);
    assert_eq!(get_data(&resp)["webmcp"]["available"], true);
    assert!(
        get_data(&resp)["webmcp"]["toolCount"]
            .as_u64()
            .is_some_and(|count| count >= 4),
        "navigation did not advertise the fixture's WebMCP tools: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    let child_ready = tokio::time::timeout(tokio::time::Duration::from_secs(5), async {
        loop {
            let resp = execute_command(
                &json!({
                    "id": "2b",
                    "action": "evaluate",
                    "script": "document.getElementById('tool-frame')?.contentDocument?.body?.dataset?.webmcpReady === 'true'"
                }),
                &mut state,
            )
            .await;
            if get_data(&resp)["result"] == true {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    assert!(
        child_ready.is_ok(),
        "child WebMCP fixture did not become ready"
    );
    state.drain_cdp_events_background().await.unwrap();

    let resp = execute_command(&json!({ "id": "3", "action": "webmcp_list" }), &mut state).await;
    assert_success(&resp);
    let tools = get_data(&resp)["tools"].as_array().unwrap();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "set_message")
        .expect("set_message should be discovered");
    assert!(tool["frameId"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(tool["origin"]
        .as_str()
        .is_some_and(|origin| origin.starts_with("http://127.0.0.1:")));
    assert_eq!(tool["inputSchema"]["type"], "object");
    let duplicate_tools = tools
        .iter()
        .filter(|tool| tool["name"] == "duplicate_tool")
        .collect::<Vec<_>>();
    assert_eq!(
        duplicate_tools.len(),
        2,
        "unexpected tools: {}",
        serde_json::to_string_pretty(tools).unwrap_or_default()
    );
    let main_frame_id = tool["frameId"].as_str().unwrap();
    let child_frame_id = duplicate_tools
        .iter()
        .find_map(|tool| {
            let frame_id = tool["frameId"].as_str()?;
            (frame_id != main_frame_id).then_some(frame_id)
        })
        .unwrap()
        .to_string();

    let resp = execute_command(
        &json!({
            "id": "3b",
            "action": "webmcp_invoke",
            "tool": "duplicate_tool",
            "params": {}
        }),
        &mut state,
    )
    .await;
    assert_error_code(&resp, "webmcp_ambiguous_tool");

    let resp = execute_command(
        &json!({
            "id": "3c",
            "action": "webmcp_invoke",
            "tool": "duplicate_tool",
            "frameId": child_frame_id,
            "params": {},
            "timeout": 5000
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["output"]["scope"], "frame");

    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "webmcp_invoke",
            "tool": "set_message",
            "params": { "message": "WebMCP works" },
            "timeout": 5000
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["status"], "completed");
    assert_eq!(get_data(&resp)["output"]["message"], "WebMCP works");

    let resp = execute_command(
        &json!({
            "id": "4b",
            "action": "webmcp_invoke",
            "tool": "fail_tool",
            "params": {},
            "timeout": 5000
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["status"], "failed");
    assert!(get_data(&resp)["error"].is_string());

    let resp = execute_command(
        &json!({
            "id": "5",
            "action": "evaluate",
            "script": "document.getElementById('result').textContent"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "WebMCP works");

    let resp = execute_command(
        &json!({
            "id": "6",
            "action": "webmcp_invoke",
            "tool": "wait_for_cancel",
            "params": {},
            "detach": true
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let invocation_id = get_data(&resp)["invocationId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(get_data(&resp)["status"], "pending");
    let pending = execute_command(
        &json!({
            "id": "6b",
            "action": "webmcp_result",
            "invocationId": invocation_id,
            "timeout": 10
        }),
        &mut state,
    )
    .await;
    assert_success(&pending);
    assert_eq!(get_data(&pending)["status"], "timed_out");

    let resp = execute_command(
        &json!({
            "id": "6c",
            "action": "webmcp_invoke",
            "tool": "wait_for_cancel",
            "params": {},
            "detach": true
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let invocation_id = get_data(&resp)["invocationId"]
        .as_str()
        .unwrap()
        .to_string();
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    state.drain_cdp_events_background().await.unwrap();
    assert_eq!(
        state.webmcp.invocations[&invocation_id].status,
        "pending",
        "long-running fixture terminated before cancellation: {}",
        state.webmcp.invocations[&invocation_id].to_json()
    );

    let resp = execute_command(
        &json!({
            "id": "7",
            "action": "webmcp_cancel",
            "invocationId": invocation_id,
            "timeout": 5000
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["status"], "canceled");

    let resp = execute_command(
        &json!({
            "id": "8",
            "action": "webmcp_invoke",
            "tool": "wait_for_cancel",
            "params": {},
            "timeout": 25
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["status"], "timed_out");

    let resp = execute_command(
        &json!({
            "id": "9",
            "action": "webmcp_invoke",
            "tool": "missing_tool",
            "params": {}
        }),
        &mut state,
    )
    .await;
    assert_error_code(&resp, "webmcp_tool_not_found");

    let resp = execute_command(
        &json!({
            "id": "10",
            "action": "webmcp_invoke",
            "tool": "set_message",
            "params": ["not", "an", "object"]
        }),
        &mut state,
    )
    .await;
    assert_error_code(&resp, "webmcp_invalid_input");

    let resp = execute_command(
        &json!({
            "id": "11",
            "action": "webmcp_invoke",
            "tool": "wait_for_cancel",
            "params": {},
            "detach": true
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let stale_id = get_data(&resp)["invocationId"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = execute_command(
        &json!({
            "id": "12",
            "action": "navigate",
            "url": "about:blank"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({
            "id": "13",
            "action": "webmcp_result",
            "invocationId": stale_id,
            "timeout": 100
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["status"], "failed");
    assert!(get_data(&resp)["error"]
        .as_str()
        .is_some_and(|error| error.starts_with("webmcp_context_changed:")));

    let resp = execute_command(
        &json!({
            "id": "14",
            "action": "navigate",
            "url": fixture_url
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(&json!({ "id": "15", "action": "webmcp_list" }), &mut state).await;
    assert_success(&resp);
    let frame_id = get_data(&resp)["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "frame_wait")
        .and_then(|tool| tool["frameId"].as_str())
        .unwrap()
        .to_string();
    let resp = execute_command(
        &json!({
            "id": "16",
            "action": "webmcp_invoke",
            "tool": "frame_wait",
            "frameId": frame_id,
            "params": {},
            "detach": true
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let frame_invocation_id = get_data(&resp)["invocationId"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = execute_command(
        &json!({
            "id": "17",
            "action": "evaluate",
            "script": "document.getElementById('tool-frame').remove()"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    let resp = execute_command(
        &json!({
            "id": "18",
            "action": "webmcp_result",
            "invocationId": frame_invocation_id,
            "timeout": 100
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["status"], "failed");
    assert!(get_data(&resp)["error"]
        .as_str()
        .is_some_and(|error| error.starts_with("webmcp_context_changed:")));

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    assert!(state.webmcp.invocations.is_empty());
    fixture_server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_webmcp_delayed_registration_appears_on_next_action() {
    let (fixture_url, fixture_server) = start_webmcp_server().await;
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
            "url": format!("{fixture_url}/delayed.html")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let already_observed = get_data(&resp).get("webmcp").is_some();
    let resp = execute_command(
        &json!({"id": "wait", "action": "wait", "timeout": 150}),
        &mut state,
    )
    .await;
    assert_success(&resp);
    if !already_observed {
        assert_eq!(get_data(&resp)["webmcp"]["experimental"], true);
        assert_eq!(get_data(&resp)["webmcp"]["available"], true);
        assert_eq!(get_data(&resp)["webmcp"]["toolCount"], 1);
    }

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    fixture_server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_webmcp_context_follows_actions_without_explicit_discovery() {
    let (url, server) = start_webmcp_server().await;
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({"id": "launch", "action": "launch", "headless": true}),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let commands = [
        (
            json!({"action": "navigate", "url": format!("{url}/context.html")}),
            -1,
        ),
        (json!({"action": "click", "selector": "#register"}), 1),
        (json!({"action": "snapshot"}), -1),
        (json!({"action": "webmcp_list", "tool": "set_message"}), -1),
        (
            json!({"action": "fill", "selector": "#description", "value": "Updated description"}),
            1,
        ),
        (
            json!({"action": "webmcp_invoke", "tool": "set_message", "params": {"message": "Hello from discovered tool"}}),
            -1,
        ),
        (json!({"action": "gettext", "selector": "#result"}), -1),
        (
            json!({"action": "evaluate", "script": "history.pushState({}, '', '#route')"}),
            -1,
        ),
        (
            json!({"action": "tab_new", "url": format!("{url}/empty.html")}),
            0,
        ),
        (json!({"action": "tab_switch", "tabId": "t1"}), 1),
        (json!({"action": "tab_close", "tabId": "t2"}), -1),
        (json!({"action": "click", "selector": "#remove"}), 0),
        (json!({"action": "wait", "timeout": 50}), -1),
        (json!({"action": "click", "selector": "#register"}), 1),
        (
            json!({"action": "navigate", "url": format!("{url}/empty.html")}),
            0,
        ),
    ];
    for (mut cmd, count) in commands {
        cmd["id"] = json!("context");
        let resp = execute_command(&cmd, &mut state).await;
        assert_success(&resp);
        let mut context = get_data(&resp)["webmcp"].clone();
        // Chrome may deliver tool events after the action's reply. Passive
        // observation reports them on a later ordinary action, without listing
        // tools or retrying the mutation. Re-registration can also briefly
        // report removal before the replacement tool is observed.
        if count >= 0 && context["toolCount"] != count {
            context = tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    let update = execute_command(
                        &json!({"id": "context-update", "action": "wait", "timeout": 50}),
                        &mut state,
                    )
                    .await;
                    assert_success(&update);
                    let context = &get_data(&update)["webmcp"];
                    if context["toolCount"] == count {
                        break context.clone();
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("Missing WebMCP update after {cmd}: {resp}"));
        }
        if count == -1 {
            assert!(
                context.is_null(),
                "Unchanged or empty page must stay quiet: {cmd}: {resp}"
            );
        } else {
            assert_eq!(context["status"], "ready", "{cmd}: {resp}");
            assert_eq!(context["toolCount"], count, "{cmd}: {resp}");
            assert_eq!(context["available"], count > 0);
        }
        if count > 0 {
            let tool = &context["tools"][0];
            assert_eq!(tool["name"], "set_message");
            assert!(tool.get("inputSchema").is_none());
            assert!(tool["frameId"].as_str().is_some_and(|id| !id.is_empty()));
            assert_eq!(tool["origin"], url);
            if cmd["action"] == "fill" {
                assert_eq!(tool["description"], "Updated description");
            }
        }
        if cmd["action"] == "webmcp_list" {
            assert_eq!(get_data(&resp)["tools"].as_array().unwrap().len(), 1);
            assert_eq!(
                get_data(&resp)["tools"][0]["inputSchema"]["required"],
                json!(["message"])
            );
        }
        if cmd["action"] == "gettext" {
            assert_eq!(get_data(&resp)["text"], "Hello from discovered tool");
        }
    }
    let resp = execute_command(&json!({"id": "close", "action": "close"}), &mut state).await;
    assert_success(&resp);
    assert!(get_data(&resp).get("webmcp").is_none());
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_webmcp_same_document_navigation_preserves_catalog_and_invocations() {
    let (url, server) = start_webmcp_server().await;
    let mut state = DaemonState::new();
    for mut cmd in [
        json!({"action": "launch", "headless": true}),
        json!({"action": "navigate", "url": url}),
    ] {
        cmd["id"] = json!("setup");
        let resp = execute_command(&cmd, &mut state).await;
        assert_success(&resp);
    }
    let resp = execute_command(
        &json!({"id": "invoke", "action": "webmcp_invoke", "tool": "wait_for_cancel", "params": {}, "detach": true}),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let invocation_id = get_data(&resp)["invocationId"]
        .as_str()
        .unwrap()
        .to_string();
    let browser = state.browser.as_ref().unwrap();
    let session = browser.active_session_id().unwrap().to_string();
    // Stop tool events so this test cannot pass by silently rediscovering the
    // catalog. Page navigation events remain enabled for document invalidation.
    browser
        .client
        .send_command_no_params("WebMCP.disable", Some(&session))
        .await
        .unwrap();
    let resp = execute_command(&json!({"id": "settle", "action": "title"}), &mut state).await;
    assert_success(&resp);
    let tools = state.webmcp.tools(&session).unwrap();
    assert!(tools.iter().any(|tool| tool.name == "wait_for_cancel"));
    assert_eq!(state.webmcp.invocations[&invocation_id].status, "pending");

    for mut cmd in [
        json!({"action": "navigate", "url": format!("{url}/#route")}),
        json!({"action": "snapshot"}),
        json!({"action": "navigate", "url": format!("{url}/#next")}),
        json!({"action": "evaluate", "script": "history.pushState({}, '', '#history')"}),
        json!({"action": "title"}),
    ] {
        cmd["id"] = json!("same-document");
        let resp = execute_command(&cmd, &mut state).await;
        assert_success(&resp);
        assert!(get_data(&resp).get("webmcp").is_none(), "{cmd}: {resp}");
        assert_eq!(state.webmcp.tools(&session).unwrap(), tools, "{cmd}");
        assert_eq!(state.webmcp.invocations[&invocation_id].status, "pending");
    }

    let resp = execute_command(
        &json!({"id": "new-document", "action": "navigate", "url": format!("{url}/empty.html")}),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["webmcp"]["toolCount"], 0);
    assert!(state.webmcp.tools(&session).unwrap().is_empty());
    assert!(state.webmcp.invocations[&invocation_id].to_json()["error"]
        .as_str()
        .is_some_and(|error| error.starts_with("webmcp_context_changed:")));

    // New documents still populate the catalog through the existing
    // subscription and reannounce tools even when the records are identical.
    state
        .browser
        .as_ref()
        .unwrap()
        .client
        .send_command_no_params("WebMCP.enable", Some(&session))
        .await
        .unwrap();
    for _ in 0..2 {
        let resp = execute_command(
            &json!({"id": "reload", "action": "navigate", "url": url}),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert!(get_data(&resp)["webmcp"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "wait_for_cancel"));
    }
    let resp = execute_command(&json!({"id": "close", "action": "close"}), &mut state).await;
    assert_success(&resp);
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_webmcp_ordinary_page_stays_quiet_without_reprobing() {
    let (url, server) = start_webmcp_server().await;
    let mut state = DaemonState::new();
    for (index, mut cmd) in [
        json!({"action": "launch", "headless": true}),
        json!({"action": "navigate", "url": format!("{url}/empty.html")}),
        json!({"action": "snapshot"}),
        json!({"action": "title"}),
        json!({"action": "evaluate", "script": "document.title"}),
        json!({"action": "wait", "timeout": 1}),
    ]
    .into_iter()
    .enumerate()
    {
        cmd["id"] = json!(index.to_string());
        let resp = execute_command(&cmd, &mut state).await;
        assert_success(&resp);
        assert!(get_data(&resp).get("webmcp").is_none(), "{cmd}: {resp}");
    }
    // Disable browser-side events behind the daemon. If ordinary commands
    // re-enable discovery, the registration below would incorrectly appear.
    let browser = state.browser.as_ref().unwrap();
    let session = browser.active_session_id().unwrap().to_string();
    browser
        .client
        .send_command_no_params("WebMCP.disable", Some(&session))
        .await
        .unwrap();
    let resp = execute_command(&json!({"id": "register", "action": "evaluate", "script": "document.modelContext.registerTool({name: 'probe', description: 'probe', inputSchema: {type: 'object'}, execute: async () => ({ok: true})}); true"}), &mut state).await;
    assert_success(&resp);
    assert!(get_data(&resp).get("webmcp").is_none());
    assert_eq!(state.webmcp.observations.len(), 1);
    let resp = execute_command(&json!({"id": "snapshot", "action": "snapshot"}), &mut state).await;
    assert_success(&resp);
    assert!(get_data(&resp).get("webmcp").is_none());
    execute_command(&json!({"id": "close", "action": "close"}), &mut state).await;
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_webmcp_opt_out_returns_no_tools() {
    let (fixture_url, fixture_server) = start_webmcp_server().await;
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({
            "id": "1",
            "action": "launch",
            "headless": true,
            "webmcp": false
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "2", "action": "webmcp_list" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["tools"], json!([]));

    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": fixture_url }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(get_data(&resp).get("webmcp").is_none());

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    fixture_server.abort();
}

fn native_test_fixture_url(name: &str) -> String {
    format!(
        "data:text/html;base64,{}",
        STANDARD.encode(native_test_fixture_html(name))
    )
}

async fn create_storage_state_with_cookie(path: &str, cookie_name: &str, cookie_value: &str) {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({
            "id": "1",
            "action": "launch",
            "headless": true,
            "args": ["--no-sandbox", "--disable-dev-shm-usage"]
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
            "name": cookie_name,
            "value": cookie_value,
            "domain": ".example.com",
            "path": "/",
            "expires": 2000000000
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "4", "action": "state_save", "path": path }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "5", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

async fn create_restore_state_with_cookie(
    restore_key: &str,
    cookie_name: &str,
    cookie_value: &str,
) {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({
            "id": "1",
            "action": "navigate",
            "url": "https://example.com",
            "restoreKey": restore_key
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "2",
            "action": "cookies_set",
            "name": cookie_name,
            "value": cookie_value,
            "domain": ".example.com",
            "path": "/",
            "expires": 2000000000
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "close" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["saveStatus"], "saved");
}

fn cleanup_restore_state_files(restore_key: &str) {
    let Some(sessions_dir) = std::env::home_dir().map(|home| home.join(".agent-browser/sessions"))
    else {
        return;
    };

    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.starts_with(&format!("{}-", restore_key)) {
                let path = entry.path();
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(format!("{}.previous", path.to_string_lossy()));
            }
        }
    }
}

async fn send_raw_http_request(port: u64, request: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .await
        .expect("HTTP client should connect to stream server");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("HTTP request should be written");
    stream
        .shutdown()
        .await
        .expect("HTTP client write side should shut down");

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("HTTP response should be read");
    String::from_utf8(response).expect("HTTP response should be utf-8")
}

#[cfg(unix)]
async fn spawn_fake_daemon_socket(
    socket_dir: &std::path::Path,
    session_name: &str,
) -> tokio::sync::oneshot::Receiver<String> {
    use tokio::io::AsyncBufReadExt;

    let socket_path = socket_dir.join(format!("{session_name}.sock"));
    let _ = std::fs::remove_file(&socket_path);
    let listener =
        tokio::net::UnixListener::bind(&socket_path).expect("fake daemon socket should bind");
    let (tx, rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let mut reader = tokio::io::BufReader::new(stream);
        let mut command = String::new();
        if reader.read_line(&mut command).await.is_err() {
            return;
        }

        let mut stream = reader.into_inner();
        let _ = stream
            .write_all(br#"{"success":true,"data":{"ok":true}}"#)
            .await;
        let _ = stream.write_all(b"\n").await;
        let _ = tx.send(command);
    });

    rx
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

#[tokio::test]
async fn test_obscura_launch_uses_request_proxy_bypass() {
    let env = EnvGuard::new(&["AGENT_BROWSER_PROXY_BYPASS"]);
    env.set("AGENT_BROWSER_PROXY_BYPASS", "stale-daemon-value");
    for (options, expected) in [
        (json!({"proxyBypass": "localhost"}), "--proxy-bypass"),
        (json!({"proxyBypass": null}), "Failed to launch Obscura"),
        (json!({"proxy": {"bypass": "localhost"}}), "--proxy-bypass"),
        // A launch that names no bypass defers to the running one; the CLI
        // clears it with an explicit null.
        (json!({}), "--proxy-bypass"),
    ] {
        let mut state = DaemonState::new();
        let mut request = json!({
            "id": "1", "action": "launch", "headless": true,
            "engine": "obscura", "executablePath": "/nonexistent/obscura",
        });
        request
            .as_object_mut()
            .unwrap()
            .extend(options.as_object().unwrap().clone());
        let resp = execute_command(&request, &mut state).await;
        assert_eq!(resp["success"], false, "{resp}");
        assert!(
            resp["error"]
                .as_str()
                .is_some_and(|error| error.contains(expected)),
            "{resp}"
        );
    }
}

fn required_obscura_binary() -> String {
    let path = std::env::var("OBSCURA_BIN")
        .expect("OBSCURA_BIN is required for explicitly invoked Obscura E2E verification");
    assert!(!path.trim().is_empty(), "OBSCURA_BIN must not be empty");
    assert!(
        std::path::Path::new(&path).is_file(),
        "OBSCURA_BIN must point to an existing executable: {path}"
    );
    path
}

struct ObscuraFixture {
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for ObscuraFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn obscura_fixture() -> ObscuraFixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            let body = include_str!("test_fixtures/obscura_probe.html");
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    ObscuraFixture { url, server }
}

async fn obscura_command(state: &mut DaemonState, command: Value) -> Value {
    tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        execute_command(&command, state),
    )
    .await
    .expect("Obscura command must not hang")
}

fn obscura_test_environment() -> EnvGuard<'static> {
    let env = EnvGuard::new(&[
        "AGENT_BROWSER_CDP",
        "AGENT_BROWSER_AUTO_CONNECT",
        "AGENT_BROWSER_PROVIDER",
        "AGENT_BROWSER_ENGINE",
        "AGENT_BROWSER_EXECUTABLE_PATH",
        "OBSCURA_ALLOW_PRIVATE_NETWORK",
        "AGENT_BROWSER_OBSCURA_STEALTH",
        "AGENT_BROWSER_PROXY",
        "AGENT_BROWSER_PROXY_BYPASS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ]);
    for key in [
        "AGENT_BROWSER_CDP",
        "AGENT_BROWSER_AUTO_CONNECT",
        "AGENT_BROWSER_PROVIDER",
        "AGENT_BROWSER_ENGINE",
        "AGENT_BROWSER_EXECUTABLE_PATH",
        "AGENT_BROWSER_OBSCURA_STEALTH",
        "AGENT_BROWSER_PROXY",
        "AGENT_BROWSER_PROXY_BYPASS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        env.remove(key);
    }
    env.set("OBSCURA_ALLOW_PRIVATE_NETWORK", "1");
    env
}

async fn verify_obscura_page(auto_launch: bool) {
    let obscura_bin = required_obscura_binary();
    let env = obscura_test_environment();
    let fixture = obscura_fixture().await;
    if auto_launch {
        env.set("AGENT_BROWSER_ENGINE", "obscura");
        env.set("AGENT_BROWSER_EXECUTABLE_PATH", &obscura_bin);
    }
    // The daemon reads its settings once, when the state is created.
    let mut state = DaemonState::new();
    if !auto_launch {
        let resp = obscura_command(
            &mut state,
            json!({
                "id": "1", "action": "launch", "headless": true,
                "engine": "obscura", "executablePath": obscura_bin,
            }),
        )
        .await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["launched"], true);
    }
    let resp = obscura_command(
        &mut state,
        json!({
            "id": "2", "action": "navigate", "url": fixture.url,
        }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], fixture.url);
    assert_eq!(get_data(&resp)["title"], "Obscura adapter probe");
    assert_eq!(state.engine, "obscura");
    assert!(
        state
            .browser
            .as_ref()
            .is_some_and(|manager| manager.owns_obscura_process()),
        "Obscura verification must own an Obscura process, not an attached browser"
    );

    let resp = obscura_command(&mut state, json!({
        "id": "3", "action": "evaluate", "script": "document.querySelector('#message').textContent",
    })).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "JavaScript ready");

    let resp = obscura_command(
        &mut state,
        json!({
            "id": "4", "action": "snapshot",
        }),
    )
    .await;
    assert_success(&resp);
    assert!(
        get_data(&resp)["snapshot"]
            .as_str()
            .is_some_and(|text| text.contains("Adapter probe")),
        "{resp}"
    );

    let resp = obscura_command(&mut state, json!({ "id": "5", "action": "close" })).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["closed"], true);
}

#[test]
fn test_obscura_e2e_clears_inherited_connection_modes() {
    const PROBE: &str = "OBSCURA_TEST_ENV_PROBE";
    let keys = [
        "AGENT_BROWSER_CDP",
        "AGENT_BROWSER_AUTO_CONNECT",
        "AGENT_BROWSER_PROVIDER",
    ];
    if std::env::var(PROBE).as_deref() == Ok("1") {
        let _env = obscura_test_environment();
        for key in keys {
            assert!(
                std::env::var_os(key).is_none(),
                "Obscura E2E must clear {key}"
            );
        }
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .env("HOME", dir.path())
        .env(PROBE, "1")
        .env(keys[0], format!("ws://{}", listener.local_addr().unwrap()))
        .env(keys[1], "1")
        .env(keys[2], "obscura-test-not-a-provider")
        .args([
            "--exact",
            "native::e2e_tests::test_obscura_e2e_clears_inherited_connection_modes",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_obscura_e2e_unlaunchable_binary_does_not_attach_to_inherited_cdp() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let server_accepted = accepted.clone();
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            server_accepted.fetch_add(1, Ordering::SeqCst);
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                match message {
                    Message::Text(text) => {
                        let command: Value = serde_json::from_str(&text).unwrap();
                        let response = json!({"id": command["id"], "error": {
                            "code": -32000, "message": "controlled CDP endpoint must not be used by Obscura verification"
                        }});
                        let _ = ws.send(Message::Text(response.to_string())).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    });
    let fixture = ObscuraFixture {
        url: endpoint.clone(),
        server,
    };
    let dir = tempfile::tempdir().unwrap();
    let false_bin = if std::path::Path::new("/usr/bin/false").is_file() {
        "/usr/bin/false"
    } else {
        "/bin/false"
    };
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .env_clear()
        .env("HOME", dir.path())
        .env("AGENT_BROWSER_SOCKET_DIR", dir.path())
        .env("OBSCURA_BIN", false_bin)
        .env("AGENT_BROWSER_CDP", &fixture.url)
        .env("AGENT_BROWSER_PROVIDER", "obscura-test-not-a-provider")
        .args([
            "--exact",
            "native::e2e_tests::e2e_obscura_auto_launch_can_open_page",
            "--ignored",
            "--nocapture",
        ])
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
        .await
        .expect("Obscura false-binary probe must terminate")
        .unwrap();
    let diagnostics = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.status.code(), Some(101), "{diagnostics}");
    assert!(
        diagnostics.contains("Obscura exited before CDP became ready"),
        "accepted {} inherited CDP connections\n{diagnostics}",
        accepted.load(Ordering::SeqCst)
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        0,
        "Obscura E2E attached to the inherited endpoint"
    );
}

/// Requires a real OBSCURA_BIN; missing binaries fail rather than silently skip verification.
#[tokio::test]
#[ignore = "requires OBSCURA_BIN; run serially"]
async fn e2e_obscura_launch_can_open_page() {
    verify_obscura_page(false).await;
}

#[tokio::test]
#[ignore = "requires OBSCURA_BIN; run serially"]
async fn e2e_obscura_auto_launch_can_open_page() {
    verify_obscura_page(true).await;
}

// ---------------------------------------------------------------------------
// Runtime stream lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_runtime_stream_enable_before_launch_attaches_and_disables() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
    let socket_dir = std::env::temp_dir().join(format!(
        "agent-browser-e2e-stream-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&socket_dir).expect("socket dir should be created");
    guard.set(
        "AGENT_BROWSER_SOCKET_DIR",
        socket_dir.to_str().expect("socket dir should be utf-8"),
    );
    guard.set("AGENT_BROWSER_SESSION", "e2e-runtime-stream");

    let mut state = DaemonState::new();

    let resp = execute_command(&json!({ "id": "1", "action": "stream_status" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["enabled"], false);

    let resp = execute_command(
        &json!({ "id": "2", "action": "stream_enable", "port": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let port = get_data(&resp)["port"]
        .as_u64()
        .expect("stream enable should report the bound port");
    assert_eq!(get_data(&resp)["connected"], false);

    let stream_path = socket_dir.join("e2e-runtime-stream.stream");
    assert!(
        stream_path.exists(),
        "runtime enable should create .stream metadata"
    );

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("websocket client should connect to runtime stream");

    let initial = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next())
        .await
        .expect("websocket should emit initial status")
        .expect("websocket should stay open")
        .expect("websocket message should be valid");
    let initial_text = initial.into_text().expect("initial message should be text");
    let initial_status: Value =
        serde_json::from_str(&initial_text).expect("status JSON should parse");
    assert_eq!(initial_status["type"], "status");
    assert_eq!(initial_status["connected"], false);

    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": "data:text/html,<h1>Runtime Stream</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let mut observed_connected = false;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let Some(message) = tokio::time::timeout(tokio::time::Duration::from_secs(2), ws.next())
            .await
            .expect("websocket should emit status after browser launch")
        else {
            continue;
        };
        let message = message.expect("websocket message should be valid");
        if !message.is_text() {
            continue;
        }
        let parsed: Value =
            serde_json::from_str(message.to_text().expect("text message should be readable"))
                .expect("runtime stream payload should be valid JSON");
        if parsed.get("type") == Some(&json!("status"))
            && parsed.get("connected") == Some(&json!(true))
        {
            observed_connected = true;
            break;
        }
    }
    assert!(
        observed_connected,
        "runtime stream should report connected=true after browser launch"
    );

    let resp = execute_command(
        &json!({ "id": "4", "action": "stream_disable" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["disabled"], true);
    assert!(
        !stream_path.exists(),
        "stream disable should remove .stream metadata"
    );

    let close_message = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next())
        .await
        .expect("websocket should close after disable");
    assert!(
        close_message.is_none() || close_message.expect("ws result should exist").is_ok(),
        "websocket should shut down cleanly when the runtime stream is disabled"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[cfg(unix)]
#[tokio::test]
#[ignore]
async fn e2e_stream_command_requires_same_origin_before_daemon_relay() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
    let temp_parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("t");
    std::fs::create_dir_all(&temp_parent).expect("socket temp parent should be created");
    let socket_dir = tempfile::Builder::new()
        .prefix("ab-e2e-")
        .tempdir_in(temp_parent)
        .expect("socket dir should be created");
    guard.set(
        "AGENT_BROWSER_SOCKET_DIR",
        socket_dir
            .path()
            .to_str()
            .expect("socket dir should be utf-8"),
    );
    guard.set("AGENT_BROWSER_SESSION", "x");

    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "stream_enable", "port": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let port = get_data(&resp)["port"]
        .as_u64()
        .expect("stream enable should report the bound port");

    let mut daemon_command = spawn_fake_daemon_socket(socket_dir.path(), "x").await;
    let body = r#"{"action":"tabs"}"#;
    let cross_origin_request = format!(
        "POST /api/command HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: https://evil.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );

    let response = send_raw_http_request(port, &cross_origin_request).await;
    assert!(
        response.starts_with("HTTP/1.1 403 Forbidden"),
        "unexpected cross-origin response: {response}"
    );
    assert!(
        !response.contains("Access-Control-Allow-Origin: *"),
        "forbidden command response exposed wildcard CORS: {response}"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut daemon_command)
            .await
            .is_err(),
        "cross-origin command request reached daemon relay"
    );

    let same_origin_request = format!(
        "POST /api/command HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let response = send_raw_http_request(port, &same_origin_request).await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "unexpected same-origin response: {response}"
    );
    assert!(
        response.contains(&format!(
            "Access-Control-Allow-Origin: http://localhost:{port}"
        )),
        "same-origin command response did not reflect origin: {response}"
    );
    assert!(
        !response.contains("Access-Control-Allow-Origin: *"),
        "same-origin command response exposed wildcard CORS: {response}"
    );

    let relayed = tokio::time::timeout(std::time::Duration::from_secs(1), daemon_command)
        .await
        .expect("same-origin request should reach fake daemon")
        .expect("fake daemon should return relayed command");
    assert!(relayed.contains(r#""action":"tabs""#), "{relayed}");

    let resp = execute_command(
        &json!({ "id": "2", "action": "stream_disable" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Snapshot with refs and ref-based click
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_snapshot_and_click_ref() {
    let (base_url, _server) = start_link_fixture_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": base_url }),
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

    // Click the link by ref (e2 is the "Next page" link)
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
        url.ends_with("/next"),
        "Should have navigated to /next, got: {}",
        url
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_snapshot_refs_survive_dom_updates_and_never_recycle() {
    let mut state = DaemonState::new();
    assert_success(
        &execute_command(
            &json!({ "id": "1", "action": "launch", "headless": true }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "2", "action": "navigate", "url": "about:blank" }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "3", "action": "setcontent", "html": "<button id='a'>Alpha</button><button id='b'>Beta</button>" }),
            &mut state,
        )
        .await,
    );

    let first = execute_command(&json!({ "id": "4", "action": "snapshot" }), &mut state).await;
    assert_success(&first);
    let alpha_ref = get_data(&first)["refs"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, node)| node["name"] == "Alpha")
        .map(|(ref_id, _)| ref_id.clone())
        .unwrap();

    assert_success(
        &execute_command(
            &json!({ "id": "5", "action": "evaluate", "script": "document.body.prepend(document.getElementById('a')); document.getElementById('b').remove()" }),
            &mut state,
        )
        .await,
    );
    let second = execute_command(&json!({ "id": "6", "action": "snapshot" }), &mut state).await;
    assert_success(&second);
    assert_eq!(get_data(&second)["refs"][&alpha_ref]["name"], "Alpha");
    assert_eq!(
        get_data(&second)["removedRefs"].as_array().unwrap().len(),
        1
    );

    assert_success(
        &execute_command(
            &json!({ "id": "7", "action": "navigate", "url": "about:blank?new-document" }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "8", "action": "setcontent", "html": "<button>Alpha</button>" }),
            &mut state,
        )
        .await,
    );
    let third = execute_command(&json!({ "id": "9", "action": "snapshot" }), &mut state).await;
    assert_success(&third);
    assert!(get_data(&third)["refs"].get(&alpha_ref).is_none());
    assert_success(&execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await);
}

#[tokio::test]
#[ignore]
async fn e2e_snapshot_refs_invalidate_iframe_navigation() {
    let mut state = DaemonState::new();
    for command in [
        json!({"action": "launch", "headless": true}),
        json!({"action": "navigate", "url": "about:blank"}),
        json!({"action": "setcontent", "html": "<button>Parent</button><iframe id='child'></iframe>"}),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    let replace = json!({"action": "evaluate", "script": "new Promise(resolve => { const f = document.getElementById('child'); f.onload = () => resolve(true); f.srcdoc = '<button>Child</button>'; })"});
    assert_success(&execute_command(&replace, &mut state).await);
    let first = execute_command(&json!({"action": "snapshot"}), &mut state).await;
    assert_success(&first);
    let find_ref = |snapshot: &Value, name: &str| {
        get_data(snapshot)["refs"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, node)| node["name"] == name)
            .unwrap()
            .0
            .clone()
    };
    let parent = find_ref(&first, "Parent");
    let child = find_ref(&first, "Child");
    let unchanged = execute_command(&json!({"action": "snapshot"}), &mut state).await;
    assert_eq!(find_ref(&unchanged, "Child"), child);
    assert_success(&execute_command(&replace, &mut state).await);
    let replaced = execute_command(&json!({"action": "snapshot"}), &mut state).await;
    assert_success(&replaced);
    assert_eq!(find_ref(&replaced, "Parent"), parent);
    assert_ne!(find_ref(&replaced, "Child"), child);
    assert!(get_data(&replaced)["removedRefs"]
        .as_array()
        .unwrap()
        .contains(&json!(format!("@{child}"))));
    assert_success(&execute_command(&json!({"action": "close"}), &mut state).await);
}

#[tokio::test]
#[ignore]
async fn e2e_stale_presentational_iframe_ref_recovers_for_read_and_hover() {
    let mut state = DaemonState::new();
    for command in [
        json!({"action": "launch", "headless": true}),
        json!({"action": "navigate", "url": "about:blank"}),
        json!({"action": "setcontent", "html": "<iframe role='none' data-version='original'></iframe>"}),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    let snapshot = execute_command(&json!({"action": "snapshot"}), &mut state).await;
    assert_success(&snapshot);
    let refs = get_data(&snapshot)["refs"].as_object().unwrap();
    let reference = refs
        .iter()
        .find(|(_, node)| node["role"] == "Iframe")
        .unwrap()
        .0
        .clone();
    let selector = format!("@{reference}");
    for role in ["presentation", "none"] {
        assert_success(
            &execute_command(
                &json!({"action": "evaluate", "script": format!("document.querySelector('iframe').outerHTML = \"<iframe role='{role}' data-version='{role}'></iframe>\"; true")}),
                &mut state,
            )
            .await,
        );
        let attribute = execute_command(
            &json!({"action": "getattribute", "selector": selector, "attribute": "data-version"}),
            &mut state,
        )
        .await;
        assert_success(&attribute);
        assert_eq!(get_data(&attribute)["value"], role);
        assert_success(
            &execute_command(
                &json!({"action": "hover", "selector": selector}),
                &mut state,
            )
            .await,
        );
    }
    assert_success(&execute_command(&json!({"action": "close"}), &mut state).await);
}

#[tokio::test]
#[ignore]
async fn e2e_presentational_cross_origin_iframe_refs_support_fill_and_inputvalue() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/");
    let server = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = vec![0; 4096];
                let read = stream.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let body = if request.starts_with("GET /child ") {
                    "<!doctype html><input aria-label='Child field'>".to_string()
                } else {
                    format!(
                        "<!doctype html><iframe role='none' src='http://localhost:{port}/child'></iframe><iframe role='presentation' src='http://localhost:{port}/child'></iframe>"
                    )
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    let mut state = DaemonState::new();
    for (id, command) in [
        ("1", json!({"action": "launch", "headless": true})),
        ("2", json!({"action": "navigate", "url": url})),
    ] {
        let mut command = command;
        command["id"] = json!(id);
        assert_success(&execute_command(&command, &mut state).await);
    }
    let snapshot = execute_command(&json!({"id": "3", "action": "snapshot"}), &mut state).await;
    assert_success(&snapshot);
    let refs = get_data(&snapshot)["refs"].as_object().unwrap();
    assert_eq!(
        refs.values()
            .filter(|node| node["role"] == "Iframe")
            .count(),
        2
    );
    let child_refs = refs
        .iter()
        .filter(|(_, node)| node["role"] == "textbox" && node["name"] == "Child field")
        .map(|(reference, _)| reference.clone())
        .collect::<Vec<_>>();
    assert_eq!(child_refs.len(), 2);
    for (index, reference) in child_refs.iter().enumerate() {
        let selector = format!("@{reference}");
        let fill = execute_command(
            &json!({"id": format!("fill-{index}"), "action": "fill", "selector": selector, "value": format!("filled-{index}")}),
            &mut state,
        )
        .await;
        assert_success(&fill);
        let value = execute_command(
            &json!({"id": format!("value-{index}"), "action": "inputvalue", "selector": selector}),
            &mut state,
        )
        .await;
        assert_success(&value);
        assert_eq!(get_data(&value)["value"], format!("filled-{index}"));
    }
    assert_success(&execute_command(&json!({"id": "close", "action": "close"}), &mut state).await);
    server.abort();
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
        &json!({ "id": "4a", "action": "screenshot", "ifChanged": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["changed"], true);
    assert_eq!(get_data(&resp)["revision"], 1);
    let conditional_path = get_data(&resp)["path"].as_str().unwrap().to_string();

    let resp = execute_command(
        &json!({ "id": "4b", "action": "screenshot", "ifChanged": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["changed"], false);
    assert_eq!(get_data(&resp)["revision"], 2);
    assert_eq!(get_data(&resp)["pixelChangeRatio"], 0.0);
    assert!(get_data(&resp).get("path").is_none());
    let _ = std::fs::remove_file(conditional_path);

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
        &json!({ "id": "5a", "action": "screenshot", "ifChanged": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["changed"], true);
    assert_eq!(get_data(&resp)["revision"], 3);
    assert!(get_data(&resp)["pixelChangeRatio"].as_f64().unwrap() > 0.0);
    let _ = std::fs::remove_file(get_data(&resp)["path"].as_str().unwrap());

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

#[tokio::test]
#[ignore]
async fn e2e_select_option_label_override_names() {
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<html><body></body></html>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let script = r#"(() => {
        const select = document.createElement('select');
        select.id = 'label-overrides';
        for (const [value, label, text] of [
            ['initial', 'Initial', 'Initial'],
            ['target', 'Capital\u00A0Federal', 'Target source'],
            ['decoy', 'Other Province', 'Capital\u00A0\u00A0Federal'],
            ['hidden', 'Visible Province', 'Hidden\u00A0Only'],
            ['plain', null, 'Plain\u00A0Text']
        ]) {
            const option = document.createElement('option');
            option.value = value;
            if (label !== null) option.label = label;
            option.textContent = text;
            select.appendChild(option);
        }
        select.value = 'initial';
        select.dataset.changes = '0';
        select.addEventListener('change', () => select.dataset.changes++);
        document.body.appendChild(select);
        const multi = select.cloneNode(true);
        multi.id = 'label-overrides-multi';
        multi.multiple = true;
        multi.value = 'initial';
        multi.addEventListener('change', () => multi.dataset.changes++);
        document.body.appendChild(multi);
    })()"#;
    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": script }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let snapshot = execute_command(&json!({ "id": "4", "action": "snapshot" }), &mut state).await;
    let mut results = Vec::new();
    for (selector, values, expected_values, changes, success) in [
        (
            "#label-overrides",
            vec!["Capital Federal"],
            vec!["target"],
            "1",
            true,
        ),
        (
            "#label-overrides",
            vec!["Hidden Only"],
            vec!["target"],
            "1",
            false,
        ),
        ("#label-overrides", vec!["decoy"], vec!["decoy"], "2", true),
        (
            "#label-overrides",
            vec!["Capital\u{00A0}Federal"],
            vec!["target"],
            "3",
            true,
        ),
        (
            "#label-overrides",
            vec!["Hidden\u{00A0}Only"],
            vec!["hidden"],
            "4",
            true,
        ),
        (
            "#label-overrides",
            vec!["Plain Text"],
            vec!["plain"],
            "5",
            true,
        ),
        (
            "#label-overrides-multi",
            vec!["target", "Hidden Only"],
            vec!["initial"],
            "0",
            false,
        ),
    ] {
        let selection = select_values(&mut state, "select", selector, &values).await;
        let script = format!(
            "(() => {{ const select = document.querySelector('{}'); return {{ values: [...select.selectedOptions].map(option => option.value), changes: select.dataset.changes }}; }})()",
            selector
        );
        let actual = execute_command(
            &json!({ "id": "state", "action": "evaluate", "script": script }),
            &mut state,
        )
        .await;
        results.push((selection, actual, expected_values, changes, success));
    }
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    assert_success(&snapshot);
    let snapshot = get_data(&snapshot)["snapshot"].as_str().unwrap();
    assert!(snapshot.contains("option \"Capital Federal\""));
    assert!(snapshot.contains("option \"Other Province\""));
    assert!(!snapshot.contains("option \"Hidden Only\""));
    for (selection, actual, expected_values, changes, success) in results {
        assert_eq!(selection["success"], success, "{selection}");
        if !success {
            assert!(selection["error"]
                .as_str()
                .unwrap()
                .contains("No option matched"));
        }
        assert_success(&actual);
        assert_eq!(
            get_data(&actual)["result"],
            json!({ "values": expected_values, "changes": changes })
        );
    }
}

#[tokio::test]
#[ignore]
async fn e2e_select_option_normalized_names() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<html><body></body></html>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let script = r#"(() => {
        const add = (select, value, label, selected = false) => {
            const option = document.createElement('option');
            option.value = value;
            option.textContent = label;
            option.selected = selected;
            select.appendChild(option);
        };
        const province = document.createElement('select');
        province.id = 'province';
        add(province, '', 'Provincia');
        add(province, 'B', 'Buenos\u00A0Aires', true);
        add(province, 'C', 'Capital\u00A0Federal');
        add(province, 'Z', 'Zero\u200BWidth');
        province.dataset.changes = '0';
        province.addEventListener('change', () => province.dataset.changes++);
        document.body.appendChild(province);

        const collision = document.createElement('select');
        collision.id = 'collision';
        add(collision, 'ascii', 'Alpha Beta', true);
        add(collision, 'nbsp', 'Alpha\u00A0Beta');
        document.body.appendChild(collision);

        const cases = document.createElement('select');
        cases.id = 'cases';
        add(cases, 'US', 'Upper');
        add(cases, 'us', 'Lower');
        document.body.appendChild(cases);

        const multi = document.createElement('select');
        multi.id = 'multi';
        multi.multiple = true;
        add(multi, 'a', 'One\u00A0A');
        add(multi, 'b', 'Two B');
        document.body.appendChild(multi);
    })()"#;
    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": script }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "4", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();
    assert!(snapshot.contains("option \"Capital Federal\""));
    assert!(!snapshot.contains("CapitalFederal"));
    assert!(snapshot.contains("option \"ZeroWidth\""));

    let resp = select_values(&mut state, "5", "#province", &["Capital Federal"]).await;
    assert_success(&resp);
    assert_evaluate(
        &mut state,
        "6",
        "({ value: province.value, changes: province.dataset.changes })",
        json!({ "value": "C", "changes": "1" }),
    )
    .await;

    let resp = select_values(&mut state, "7", "#province", &["missing"]).await;
    assert_eq!(resp["success"], false);
    assert_evaluate(
        &mut state,
        "8",
        "({ value: province.value, changes: province.dataset.changes })",
        json!({ "value": "C", "changes": "1" }),
    )
    .await;

    let resp = select_values(&mut state, "9", "#collision", &["Alpha Beta"]).await;
    assert_success(&resp);
    assert_evaluate(&mut state, "10", "collision.value", json!("ascii")).await;

    let resp = select_values(&mut state, "11", "#collision", &["Alpha  Beta"]).await;
    assert_eq!(resp["success"], false);
    assert!(resp["error"]
        .as_str()
        .unwrap()
        .contains("Multiple options matched"));
    assert_evaluate(&mut state, "12", "collision.value", json!("ascii")).await;

    let resp = select_values(&mut state, "13", "#cases", &["us"]).await;
    assert_success(&resp);
    assert_evaluate(&mut state, "14", "cases.value", json!("us")).await;

    let resp = select_values(&mut state, "15", "#multi", &["One A", "b"]).await;
    assert_success(&resp);
    assert_evaluate(
        &mut state,
        "16",
        "[...multi.selectedOptions].map(option => option.value)",
        json!(["a", "b"]),
    )
    .await;

    let resp = select_values(&mut state, "17", "#multi", &["a", "missing"]).await;
    assert_eq!(resp["success"], false);
    assert_evaluate(
        &mut state,
        "18",
        "[...multi.selectedOptions].map(option => option.value)",
        json!(["a", "b"]),
    )
    .await;

    let resp = select_values(&mut state, "19", "#province", &["ZeroWidth"]).await;
    assert_success(&resp);
    assert_evaluate(&mut state, "20", "province.value", json!("Z")).await;

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

    // Tab list should show 1 tab with tabId 1
    let resp = execute_command(&json!({ "id": "3", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0]["active"], true);
    assert_eq!(tabs[0]["tabId"], "t1", "First tab should have tabId t1");

    // Open new tab
    let resp = execute_command(
        &json!({ "id": "4", "action": "tab_new", "url": "data:text/html,<h1>Tab 2</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["tabId"],
        "t2",
        "New tab should have tabId t2"
    );
    assert_eq!(get_data(&resp)["total"], 2);

    // Tab list should show 2 tabs with distinct, incrementing tabIds
    let resp = execute_command(&json!({ "id": "5", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs[1]["active"], true);
    assert_eq!(tabs[0]["tabId"], "t1", "First tab should keep tabId t1");
    assert_eq!(tabs[1]["tabId"], "t2", "Second tab should have tabId t2");

    // Switch to first tab
    let resp = execute_command(
        &json!({ "id": "6", "action": "tab_switch", "tabId": "t1" }),
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
        &json!({ "id": "8", "action": "tab_close", "tabId": "t2" }),
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

#[tokio::test]
#[ignore]
async fn e2e_tab_ids_not_reused() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // First tab gets tabId 1
    let resp = execute_command(&json!({ "id": "2", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(tabs[0]["tabId"], "t1");

    // Open tab 2 and tab 3
    let resp = execute_command(
        &json!({ "id": "3", "action": "tab_new", "url": "data:text/html,<h1>Tab 2</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["tabId"], "t2");

    let resp = execute_command(
        &json!({ "id": "4", "action": "tab_new", "url": "data:text/html,<h1>Tab 3</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["tabId"], "t3");

    // Close tab 2
    let resp = execute_command(
        &json!({ "id": "5", "action": "tab_close", "tabId": "t2" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Open a new tab — should get tabId 4, NOT 2
    let resp = execute_command(
        &json!({ "id": "6", "action": "tab_new", "url": "data:text/html,<h1>Tab 4</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["tabId"],
        "t4",
        "Tab IDs must not be reused after closing"
    );

    // Verify final state: tabs t1, t3, t4
    let resp = execute_command(&json!({ "id": "7", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 3);
    let ids: Vec<String> = tabs
        .iter()
        .map(|t| t["tabId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec!["t1", "t3", "t4"]);

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// `tab_close` with an explicit `tabId` must close that tab regardless of
/// whether it's active, and leave the remaining tab active without leaking
/// per-tab state (refs, iframe sessions, frame id) from the closed tab.
#[tokio::test]
#[ignore]
async fn e2e_tab_close_with_tab_id_closes_active_tab() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<title>A</title>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "tab_new", "url": "data:text/html,<title>B</title>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "4", "action": "tab_close", "tabId": "t2" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "5", "action": "title" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["title"], "A");
    assert!(state.ref_map.get("e1").is_none());
    assert!(state.iframe_sessions.is_empty());
    assert!(state.active_frame_id.is_none());

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Tabs can be opened with a user-assigned label and then addressed by that
/// label anywhere a `t<N>` id is accepted (switch, close, and JSON `tabId`
/// on `tab_switch` / `tab_close`). Labels are the agent-friendly way to
/// write multi-tab workflows without memorizing ids.
#[tokio::test]
#[ignore]
async fn e2e_tab_new_with_label_can_be_switched_and_closed() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<title>Home</title>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    for (index, label) in ["t1", "T1", "t0", "T0", "t01", "T01", "T4294967296"]
        .iter()
        .enumerate()
    {
        let resp = execute_command(
            &json!({
                "id": format!("reserved-{index}"),
                "action": "tab_new",
                "url": "about:blank",
                "label": label,
            }),
            &mut state,
        )
        .await;
        assert_eq!(
            resp.get("success").and_then(Value::as_bool),
            Some(false),
            "reserved label `{label}` should be rejected: {}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        );
        assert!(
            resp.get("error")
                .and_then(Value::as_str)
                .is_some_and(|error| error.contains("reserved for tab IDs")),
            "reserved label `{label}` should report the reserved-ID error: {}",
            serde_json::to_string_pretty(&resp).unwrap_or_default()
        );

        let resp = execute_command(
            &json!({ "id": format!("reserved-list-{index}"), "action": "tab_list" }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["tabs"].as_array().unwrap().len(), 1);
    }

    // Open a labeled tab and verify the response echoes the label and a
    // `t<N>` style tabId.
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "tab_new",
            "url": "data:text/html,<title>Docs</title>",
            "label": "docs",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["tabId"], "t2");
    assert_eq!(get_data(&resp)["label"], "docs");

    // tab_list exposes the label alongside the id.
    let resp = execute_command(&json!({ "id": "4", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    let docs = tabs
        .iter()
        .find(|t| t["tabId"] == "t2")
        .expect("docs tab should be present");
    assert_eq!(docs["label"], "docs");

    // tab_switch accepts the label.
    let resp = execute_command(
        &json!({ "id": "5", "action": "tab_switch", "tabId": "t1" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.browser.as_ref().unwrap().active_tab_id(), Some(1));

    let resp = execute_command(
        &json!({ "id": "6", "action": "tab_switch", "tabId": "docs" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.browser.as_ref().unwrap().active_tab_id(), Some(2));

    // Once switched, the active tab is the labeled one and normal commands
    // work against it.
    let resp = execute_command(&json!({ "id": "7", "action": "title" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["title"], "Docs");

    // tab_close accepts the label.
    let resp = execute_command(
        &json!({ "id": "8", "action": "tab_close", "tabId": "docs" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["label"], "docs");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_rejected_tab_label_preserves_refs_and_frame() {
    let mut state = DaemonState::new();
    for command in [
        json!({ "action": "launch", "headless": true }),
        json!({ "action": "navigate", "url": "data:text/html,<iframe id='child' srcdoc='<button>Preserved</button>'></iframe>" }),
        json!({ "action": "frame", "selector": "#child" }),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    let snapshot = execute_command(&json!({ "action": "snapshot" }), &mut state).await;
    assert_success(&snapshot);
    let button_ref = get_data(&snapshot)["refs"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, node)| node["name"] == "Preserved")
        .unwrap()
        .0
        .clone();
    let frame_id = state.active_frame_id.clone().unwrap();

    let response =
        execute_command(&json!({ "action": "tab_new", "label": "t1" }), &mut state).await;
    assert_eq!(response["success"], false);
    assert!(state.ref_map.get(&button_ref).is_some());
    assert_eq!(state.active_frame_id.as_deref(), Some(frame_id.as_str()));

    for command in [
        json!({ "action": "click", "selector": format!("@{button_ref}") }),
        json!({ "action": "close" }),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
}

/// Duplicate labels must be rejected so agents can treat a label as a unique
/// handle. The first tab keeps the label; the second tab's creation errors.
#[tokio::test]
#[ignore]
async fn e2e_tab_new_with_duplicate_label_errors() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "tab_new", "url": "about:blank", "label": "docs" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "tab_new", "url": "about:blank", "label": "docs" }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "duplicate label should error: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("already used"),
        "error should explain the collision: {}",
        err
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Positional integers passed as `tabId` on tab-switch / tab-close must be
/// rejected by the daemon-layer parser, not silently coerced. The error
/// should teach the user the correct form (`t<N>`).
#[tokio::test]
#[ignore]
async fn e2e_tab_switch_rejects_bare_integer() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "tab_switch", "tabId": "2" }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "bare integer tabId on tab_switch should error: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("t2") && err.contains("positional integers"),
        "error should teach `t<N>` convention: {}",
        err
    );

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

#[tokio::test]
#[ignore]
async fn e2e_getbyrole_uses_accessibility_tree_for_implicit_roles() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = concat!(
        "<html><head>",
        "<link rel='stylesheet' href='data:text/css,body{}'>",
        "</head><body>",
        "<h1 style='text-transform:uppercase'>Welcome</h1>",
        "<a id='services' href='#services' onclick='window.__clicked = \"services\"'>Services</a>",
        "<button id='submit'>Submit</button>",
        "</body></html>"
    );
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Implicit role from a plain HTML tag (<h1> -> heading), matched
    // case-insensitively against the accessible name despite the CSS
    // text-transform rendering it as "WELCOME".
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "getbyrole",
            "role": "heading",
            "name": "welcome",
            "subaction": "text"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "WELCOME");

    // A real <a href> must win over the unrelated <link rel="stylesheet">
    // element, which is not exposed as an AX "link" node at all.
    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "getbyrole",
            "role": "link",
            "name": "Services",
            "exact": true,
            "subaction": "click"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "window.__clicked" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "services");

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

// wait --load on a page that already finished loading must resolve
// immediately instead of waiting for a Page.loadEventFired that will never
// come (the common case after a click that triggers an SPA navigation).
#[tokio::test]
#[ignore]
async fn e2e_wait_load_state_resolves_immediately_when_already_loaded() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>Loaded</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    for (id, load_state) in [("3", "load"), ("4", "domcontentloaded")] {
        let start = std::time::Instant::now();
        let resp = execute_command(
            &json!({ "id": id, "action": "waitforloadstate", "state": load_state, "timeout": 10000 }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert!(
            start.elapsed().as_millis() < 3000,
            "wait --load {} on an already-loaded page should resolve immediately, took {}ms",
            load_state,
            start.elapsed().as_millis()
        );
    }

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_wait_network_idle_reports_timeout_on_busy_page() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut held = Vec::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).await.unwrap_or(0);
            if String::from_utf8_lossy(&request[..n]).starts_with("GET /hang") {
                held.push(stream);
                continue;
            }
            let body = "<h1>Busy</h1><script>setInterval(() => fetch('/hang'), 200)</script>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });

    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let started = std::time::Instant::now();
    let wait_resp = execute_command(
        &json!({ "id": "3", "action": "waitforloadstate", "state": "networkidle", "timeout": 30000 }),
        &mut state,
    )
    .await;
    let waited = started.elapsed();
    let close_resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    server.abort();
    assert_success(&close_resp);
    assert_eq!(wait_resp["success"], false, "{wait_resp}");
    // The requested budget is the one that counts. The inner wait used to stop
    // at the browser's hardcoded 25s default, so a 30s wait failed early.
    assert!(
        waited >= std::time::Duration::from_millis(28_000),
        "network-idle wait returned after {waited:?}, expected to use the 30000ms budget"
    );
    assert!(
        wait_resp["error"].as_str().is_some_and(|error| {
            error.contains("Timed out after 30000ms") && error.contains("load state 'networkidle'")
        }),
        "expected the network-idle timeout error: {}",
        serde_json::to_string_pretty(&wait_resp).unwrap_or_default()
    );
}

// ---------------------------------------------------------------------------
// Same-document navigation regression test
// ---------------------------------------------------------------------------
//
// Chrome may perform a same-document navigation when it determines the target
// URL is the same document as the current page (ignoring fragment). This
// causes Page.loadEventFired to not fire, making wait_for_lifecycle
// hang forever waiting for an event that never comes.
//
// The fix checks loader_id in the Page.navigate response - if None,
// it's a same-document navigation and we skip waiting for lifecycle events.

#[tokio::test]
#[ignore]
async fn e2e_navigate_same_url_twice_should_not_hang() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to about:blank first to start from a known state
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "about:blank" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Create a simple HTML page that changes its own URL via history.pushState
    // This simulates SPA routing behavior which triggers same-document navigation
    let base_page = "data:text/html,<html><body><script>
        // On first load, change URL via pushState without navigation
        history.pushState({}, '', '/#/home');
    </script><h1>Test</h1></body></html>";

    // Navigate to the page (first time)
    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": base_page }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Verify URL changed due to pushState
    let resp = execute_command(&json!({ "id": "4", "action": "url" }), &mut state).await;
    assert_success(&resp);
    let url_after_push = get_data(&resp)["url"].as_str().unwrap();
    // URL should have changed to include /#/home due to pushState
    assert!(
        url_after_push.contains("/%23/home") || url_after_push.contains("/#/home"),
        "URL should have changed via pushState, got: {}",
        url_after_push
    );

    // Navigate to the SAME base URL again
    // Without fix: Chrome may do same-document nav, wait_for_lifecycle hangs
    // With fix: We detect loader_id is None and skip waiting
    let start = std::time::Instant::now();
    let resp = execute_command(
        &json!({ "id": "5", "action": "navigate", "url": base_page }),
        &mut state,
    )
    .await;
    let elapsed = start.elapsed().as_secs();

    // Should complete quickly (< 5 seconds) without hanging
    // Without fix, this times out after 25 seconds (default_timeout_ms)
    assert!(
        elapsed < 5,
        "Second navigation should not hang, but took {}s",
        elapsed
    );
    assert_success(&resp);

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

/// One `press` must produce exactly one keydown/keyup pair whose `code`
/// matches the key. Sending the Windows VK code as `nativeVirtualKeyCode`
/// made macOS Chrome resolve a different physical key and repeat keydown
/// thousands of times per second until navigation.
#[tokio::test]
#[ignore]
async fn e2e_press_dispatches_single_keystroke() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = concat!(
        "data:text/html,<html><body><script>",
        "window.__events=[];",
        "addEventListener('keydown',e=>__events.push('down:'+e.key+'/'+e.code));",
        "addEventListener('keyup',e=>__events.push('up:'+e.key+'/'+e.code));",
        "</script></body></html>"
    );
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    for (i, (key, expected)) in [
        ("a", "a/KeyA"),
        ("Escape", "Escape/Escape"),
        ("End", "End/End"),
        ("Enter", "Enter/Enter"),
    ]
    .into_iter()
    .enumerate()
    {
        let resp = execute_command(
            &json!({ "id": format!("reset-{i}"), "action": "evaluate", "script": "window.__events=[]" }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": format!("press-{i}"), "action": "press", "key": key }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        // Give a stuck key time to auto-repeat before counting.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        let resp = execute_command(
            &json!({ "id": format!("read-{i}"), "action": "evaluate", "script": "window.__events" }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(
            get_data(&resp)["result"],
            json!([format!("down:{expected}"), format!("up:{expected}")]),
            "press {key}"
        );
    }

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Raw mouse regressions
// ---------------------------------------------------------------------------

async fn wait_for_wheel_events(state: &mut DaemonState, expected: usize) -> Value {
    for _ in 0..100 {
        let resp = execute_command(
            &json!({
                "action": "evaluate",
                "script": "({ last: window.__wheelProbe, events: window.__wheelEvents, buttons: window.__wheelButtons, scrollTop: document.getElementById('target').scrollTop })"
            }),
            state,
        )
        .await;
        assert_success(&resp);
        let result = get_data(&resp)["result"].clone();
        if result["events"].as_array().map(Vec::len) == Some(expected)
            && result["scrollTop"].as_f64().unwrap_or_default() > 0.0
        {
            return result;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("Expected {expected} wheel events and nested scrolling");
}

async fn launch_wheel_probe() -> DaemonState {
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
            "url": native_test_fixture_url("wheel_probe")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "evaluate",
            "script": "document.addEventListener('wheel', event => { window.__wheelButtons = event.buttons; }); 'armed'"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    state
}

async fn wheel_target_coordinates(state: &mut DaemonState) -> (i64, i64) {
    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "evaluate",
            "script": r#"(() => {
                const rect = document.getElementById('target').getBoundingClientRect();
                return {
                    x: Math.round(rect.left + rect.width / 2),
                    y: Math.round(rect.top + rect.height / 2)
                };
            })()"#
        }),
        state,
    )
    .await;
    assert_success(&resp);
    let target = &get_data(&resp)["result"];
    let x = target["x"].as_i64().expect("target x should be numeric");
    let y = target["y"].as_i64().expect("target y should be numeric");
    (x, y)
}

#[tokio::test]
#[ignore]
async fn e2e_mouse_wheel_uses_last_pointer_coordinates() {
    let mut state = launch_wheel_probe().await;
    let (x, y) = wheel_target_coordinates(&mut state).await;

    let resp = execute_command(
        &json!({ "id": "5", "action": "mousemove", "x": x, "y": y }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "wheel", "deltaX": -12.5, "deltaY": 37.25 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["deltaX"], -12.5);
    assert_eq!(get_data(&resp)["deltaY"], 37.25);

    let probe = wait_for_wheel_events(&mut state, 1).await;
    assert_eq!(probe["events"].as_array().map(Vec::len), Some(1), "{probe}");
    let wheel = &probe["last"];
    assert_eq!(wheel["target"], "target", "wheel probe: {wheel}");
    assert_eq!(wheel["x"].as_i64(), Some(x));
    assert_eq!(wheel["y"].as_i64(), Some(y));
    assert_eq!(wheel["deltaX"].as_f64(), Some(-12.5));
    assert_eq!(wheel["deltaY"].as_f64(), Some(37.25));
    assert_eq!(probe["buttons"], 0);
    assert!(probe["scrollTop"].as_f64().unwrap() > 0.0, "{probe}");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_mouse_wheel_honors_explicit_coordinates() {
    let mut state = launch_wheel_probe().await;
    let (x, y) = wheel_target_coordinates(&mut state).await;

    let resp = execute_command(
        &json!({ "id": "5", "action": "mousemove", "x": x + 20, "y": y + 10 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let explicit_x = x - 10;
    let explicit_y = y - 10;
    let resp = execute_command(
        &json!({
            "id": "6",
            "action": "wheel",
            "x": explicit_x,
            "y": explicit_y,
            "deltaX": 2.5,
            "deltaY": 31.5
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["deltaX"], 2.5);
    assert_eq!(get_data(&resp)["deltaY"], 31.5);

    let probe = wait_for_wheel_events(&mut state, 1).await;
    let wheel = &probe["last"];
    assert_eq!(wheel["target"], "target", "wheel probe: {wheel}");
    assert_eq!(wheel["x"].as_i64(), Some(explicit_x));
    assert_eq!(wheel["y"].as_i64(), Some(explicit_y));
    assert_eq!(wheel["deltaX"].as_f64(), Some(2.5));
    assert_eq!(wheel["deltaY"].as_f64(), Some(31.5));
    assert!(probe["scrollTop"].as_f64().unwrap() > 0.0, "{probe}");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_raw_mouse_wheel_preserves_held_button_after_rejected_dispatch() {
    let mut state = launch_wheel_probe().await;
    let (x, y) = wheel_target_coordinates(&mut state).await;

    let raw_x = x + 20;
    let raw_y = y + 10;
    let resp = Box::pin(execute_command(
        &json!({
            "id": "7",
            "action": "mouse",
            "eventType": "mouseMoved",
            "x": raw_x,
            "y": raw_y
        }),
        &mut state,
    ))
    .await;
    assert_success(&resp);

    let resp = Box::pin(execute_command(
        &json!({
            "id": "7a",
            "action": "mouse",
            "eventType": "mousePressed",
            "x": raw_x,
            "y": raw_y,
            "button": "left"
        }),
        &mut state,
    ))
    .await;
    assert_success(&resp);

    let resp = Box::pin(execute_command(
        &json!({
            "id": "7b",
            "action": "mouse",
            "eventType": "mousePressed",
            "x": raw_x + 30,
            "y": raw_y + 20,
            "button": "invalid"
        }),
        &mut state,
    ))
    .await;
    assert_eq!(
        resp.get("success").and_then(Value::as_bool),
        Some(false),
        "invalid button should fail: {resp}"
    );

    let resp = Box::pin(execute_command(
        &json!({ "id": "8", "action": "wheel", "deltaX": 2, "deltaY": 29 }),
        &mut state,
    ))
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["deltaX"], 2.0);
    assert_eq!(get_data(&resp)["deltaY"], 29.0);

    let probe = wait_for_wheel_events(&mut state, 1).await;
    let wheel = &probe["last"];
    assert_eq!(wheel["target"], "target", "wheel probe: {wheel}");
    assert_eq!(wheel["x"].as_i64(), Some(raw_x));
    assert_eq!(wheel["y"].as_i64(), Some(raw_y));
    assert_eq!(wheel["deltaX"].as_f64(), Some(2.0));
    assert_eq!(wheel["deltaY"].as_f64(), Some(29.0));
    assert_eq!(probe["buttons"], 1);
    assert!(probe["scrollTop"].as_f64().unwrap() > 0.0, "{probe}");

    let resp = Box::pin(execute_command(
        &json!({
            "id": "9",
            "action": "mouse",
            "eventType": "mouseReleased",
            "x": raw_x,
            "y": raw_y,
            "button": "left"
        }),
        &mut state,
    ))
    .await;
    assert_success(&resp);

    let resp = Box::pin(execute_command(
        &json!({ "id": "99", "action": "close" }),
        &mut state,
    ))
    .await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_input_mouse_wheel_uses_input_mouse_coordinates() {
    let mut state = launch_wheel_probe().await;
    let (x, y) = wheel_target_coordinates(&mut state).await;
    let input_x = x + 35;
    let input_y = y + 15;

    let resp = execute_command(
        &json!({
            "id": "10",
            "action": "input_mouse",
            "type": "mouseMoved",
            "x": input_x,
            "y": input_y,
            "button": "none"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "11", "action": "wheel", "deltaX": -3.5, "deltaY": 8.25 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["deltaX"], -3.5);
    assert_eq!(get_data(&resp)["deltaY"], 8.25);

    let probe = wait_for_wheel_events(&mut state, 1).await;
    let wheel = &probe["last"];
    assert_eq!(wheel["target"], "target", "wheel probe: {wheel}");
    assert_eq!(wheel["x"].as_i64(), Some(input_x));
    assert_eq!(wheel["y"].as_i64(), Some(input_y));
    assert_eq!(wheel["deltaX"].as_f64(), Some(-3.5));
    assert_eq!(wheel["deltaY"].as_f64(), Some(8.25));
    assert_eq!(probe["buttons"], 0);
    assert!(probe["scrollTop"].as_f64().unwrap() > 0.0, "{probe}");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

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

#[tokio::test]
#[ignore]
async fn e2e_drag_action_sends_buttons_during_move() {
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
            "url": native_test_fixture_url("html5_drag_probe")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "drag",
            "source": "#source",
            "target": "#dest"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["dragged"].as_bool(), Some(true));

    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "window.__html5DragProbe" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let probe = &get_data(&resp)["result"];
    let events = probe["events"]
        .as_array()
        .expect("html5 drag probe should expose events");

    // The mousemove events emitted while the button is held should carry
    // buttons == 1 so the browser recognises the gesture as a drag.
    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "mousemove" && event["buttons"].as_i64() == Some(1) }),
        "Expected at least one mousemove with buttons == 1 during drag"
    );

    // dragstart must fire on the source element.
    assert!(
        events.iter().any(|event| event["type"] == "dragstart"),
        "Expected dragstart to fire on the source element"
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
// Cross-domain state save (issue #1060)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_save_state_cross_domain() {
    let mut state = DaemonState::new();

    // Launch
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to domain A and set cookie + localStorage
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://httpbin.org/html" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3", "action": "cookies_set",
            "name": "domainA_cookie", "value": "from_httpbin"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "4", "action": "storage_set",
            "type": "local", "key": "domainA_key", "value": "domainA_val"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Navigate to domain B and set cookie + localStorage
    let resp = execute_command(
        &json!({ "id": "5", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "6", "action": "cookies_set",
            "name": "domainB_cookie", "value": "from_example"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "7", "action": "storage_set",
            "type": "local", "key": "domainB_key", "value": "domainB_val"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Save state (currently on example.com)
    let tmp_state = std::env::temp_dir()
        .join("agent-browser-e2e-cross-domain-state.json")
        .to_string_lossy()
        .to_string();
    let resp = execute_command(
        &json!({ "id": "8", "action": "state_save", "path": &tmp_state }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Read and verify saved state
    let saved = std::fs::read_to_string(&tmp_state).expect("State file should exist");
    let state_data: serde_json::Value = serde_json::from_str(&saved).unwrap();

    // Verify BOTH domain cookies are present
    let cookies = state_data["cookies"].as_array().unwrap();
    let has_domain_a = cookies.iter().any(|c| c["name"] == "domainA_cookie");
    let has_domain_b = cookies.iter().any(|c| c["name"] == "domainB_cookie");
    assert!(
        has_domain_a,
        "Should include cross-domain cookie from httpbin.org: {:?}",
        cookies
    );
    assert!(
        has_domain_b,
        "Should include cookie from example.com: {:?}",
        cookies
    );

    // Verify BOTH origins' localStorage are present
    let origins = state_data["origins"].as_array().unwrap();
    let has_origin_a = origins.iter().any(|o| {
        o["origin"].as_str().is_some_and(|s| s.contains("httpbin"))
            && o["localStorage"]
                .as_array()
                .is_some_and(|ls| ls.iter().any(|e| e["name"] == "domainA_key"))
    });
    let has_origin_b = origins.iter().any(|o| {
        o["origin"].as_str().is_some_and(|s| s.contains("example"))
            && o["localStorage"]
                .as_array()
                .is_some_and(|ls| ls.iter().any(|e| e["name"] == "domainB_key"))
    });
    assert!(
        has_origin_a,
        "Should include localStorage from httpbin.org origin: {:?}",
        origins
    );
    assert!(
        has_origin_b,
        "Should include localStorage from example.com origin: {:?}",
        origins
    );

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

    // The active about:blank document must be patched immediately, not only
    // after a later navigation.
    let resp = execute_command(
        &json!({
            "id": "1-rtc", "action": "evaluate",
            "script": "(() => { try { new RTCPeerConnection({iceServers:[{urls:'stun:secret.blocked.com:3478'}]}); return 'NOT_BLOCKED'; } catch (error) { return error.name; } })()",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "SecurityError");

    // New tabs created after launch must receive the same controls before a
    // requested URL starts loading.
    let resp = execute_command(
        &json!({ "id": "1-tab", "action": "tab_new", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({
            "id": "1-tab-rtc", "action": "evaluate",
            "script": "(() => { try { new RTCPeerConnection({iceServers:[{urls:'stun:secret.blocked.com:3478'}]}); return 'NOT_BLOCKED'; } catch (error) { return error.name; } })()",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "SecurityError");

    let resp = execute_command(
        &json!({ "id": "1-tab-blocked", "action": "tab_new", "url": "https://blocked.com" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    let error = resp["error"].as_str().unwrap_or("");
    assert!(
        error.contains("blocked.com") || error.contains("not allowed"),
        "Blocked tab URL should fail before loading, got: {}",
        error
    );

    let resp = execute_command(
        &json!({ "id": "1-window", "action": "window_new" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({
            "id": "1-window-rtc", "action": "evaluate",
            "script": "(() => { try { new RTCPeerConnection({iceServers:[{urls:'stun:secret.blocked.com:3478'}]}); return 'NOT_BLOCKED'; } catch (error) { return error.name; } })()",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "SecurityError");

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

    // WebRTC uses DNS and UDP outside CDP Fetch interception, so the domain
    // filter must disable both Chromium constructor names before page scripts
    // can create a peer connection.
    let resp = execute_command(
        &json!({
            "id": "6", "action": "evaluate",
            "script": "['RTCPeerConnection','webkitRTCPeerConnection'].filter(name => typeof window[name] === 'function').map(name => { try { new window[name]({iceServers:[{urls:'stun:secret.blocked.com:3478'}]}); return name + ':NOT_BLOCKED'; } catch (error) { return name + ':' + error.name; } })",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let results = get_data(&resp)["result"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "RTCPeerConnection should be available in Chrome"
    );
    assert!(
        results.iter().all(|result| result
            .as_str()
            .is_some_and(|value| value.ends_with(":SecurityError"))),
        "Every peer connection constructor should be blocked, got: {:?}",
        results,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_domain_filter_reports_blocked_redirect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = requests_for_server.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or("").to_string();
                let response = if request_line.contains(" /redirect ") {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{}/denied\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        port,
                    )
                } else {
                    let body = "<!doctype html><title>Blocked</title>allowed";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body,
                    )
                };
                if let Ok(mut logged) = requests.lock() {
                    logged.push(request_line);
                }
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut state = DaemonState::new();
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("localhost"));
    }

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
            "url": format!("http://localhost:{}/redirect", port),
        }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp["success"], false,
        "Blocked redirect reported success: {}",
        resp
    );
    let error = resp["error"].as_str().unwrap_or("");
    assert!(
        error.contains(&format!("http://127.0.0.1:{}/denied", port))
            && error.contains("domain filter"),
        "Blocked redirect should name the blocked URL, got: {}",
        error
    );
    let logged = requests.lock().unwrap().clone();
    assert!(
        !logged.iter().any(|line| line.contains(" /denied ")),
        "Blocked redirect target was requested: {:?}",
        logged
    );

    // An allowed page whose own title is "Blocked" is still a success.
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "navigate",
            "url": format!("http://localhost:{}/", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_domain_filter_navigates_page_with_cross_origin_iframes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = requests_for_server.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or("").to_string();
                let body = if request_line.contains(" /probe ") {
                    "<p id=r>pending</p><script>fetch('https://example.com/').then(() => r.textContent = 'fetch allowed', (e) => r.textContent = 'fetch ' + e.name)</script>".to_string()
                } else {
                    format!(
                        "<title>frames</title><iframe src=\"http://127.0.0.1:{port}/probe\"></iframe><iframe src=\"http://blocked.localtest.me:{port}/denied\"></iframe>"
                    )
                };
                if let Ok(mut logged) = requests.lock() {
                    logged.push(request_line);
                }
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut state = DaemonState::new();
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("localhost,127.0.0.1"));
    }

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Both iframes are cross-origin, so each attaches paused before it has an
    // execution context.
    let resp = execute_command(
        &json!({
            "id": "2",
            "action": "navigate",
            "url": format!("http://localhost:{}/", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // The probe frame reports its fetch result asynchronously.
    let mut snapshot = String::new();
    for _ in 0..50 {
        let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
        assert_success(&resp);
        snapshot = get_data(&resp)["snapshot"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if snapshot.contains("fetch SecurityError")
            && snapshot.contains("not allowed by domain filter")
        {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    assert!(
        snapshot.contains("fetch SecurityError"),
        "Allowed iframe should run the domain filter script: {}",
        snapshot
    );
    assert!(
        snapshot.contains("not allowed by domain filter"),
        "Blocked iframe should receive the domain filter block page: {}",
        snapshot
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_domain_filter_blocks_page_created_popup_before_first_request() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = requests_for_server.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or("").to_string();
                if let Ok(mut logged) = requests.lock() {
                    logged.push(request_line);
                }

                let body = "<!doctype html><title>allowed</title><button id=\"go\">go</button>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut state = DaemonState::new();
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("localhost"));
    }

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
            "url": format!("http://localhost:{}/", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "evaluate",
            "script": format!("window.open('http://127.0.0.1:{}/leak'); 'done'", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let logged = requests.lock().unwrap().clone();
    assert!(
        !logged.iter().any(|line| line.contains(" /leak ")),
        "Blocked popup made a server request: {:?}",
        logged
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_domain_filter_blocks_service_worker_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = requests_for_server.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or("").to_string();
                if let Ok(mut logged) = requests.lock() {
                    logged.push(request_line.clone());
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");

                let (content_type, body) = if path == "/sw.js" {
                    (
                        "application/javascript",
                        format!(
                            r#"self.addEventListener('message', event => {{
    event.source && event.source.postMessage('started');
    const controller = new AbortController();
    setTimeout(() => controller.abort(), 500);
    fetch('http://127.0.0.1:{}/leak', {{ mode: 'no-cors', signal: controller.signal }})
        .then(() => event.source && event.source.postMessage('leaked'))
        .catch(error => event.source && event.source.postMessage('blocked:' + error.name));
}});"#,
                            port
                        ),
                    )
                } else {
                    (
                        "text/html",
                        r#"<!doctype html><title>allowed</title><script>
window.swResult = 'pending';
navigator.serviceWorker.register('/sw.js').then(async reg => {
    await navigator.serviceWorker.ready;
    const sw = reg.active || reg.waiting || reg.installing;
    navigator.serviceWorker.addEventListener('message', event => {
        window.swResult = event.data;
    });
    sw.postMessage('go');
}).catch(error => {
    window.swResult = 'register:' + error.name;
});
</script>"#
                            .to_string(),
                    )
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    content_type,
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut state = DaemonState::new();
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("localhost"));
    }

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
            "url": format!("http://localhost:{}/", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let mut sw_result = "pending".to_string();
    for _ in 0..50 {
        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "evaluate",
                "script": "window.swResult || 'pending'",
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        sw_result = get_data(&resp)["result"]
            .as_str()
            .unwrap_or("pending")
            .to_string();
        if !matches!(sw_result.as_str(), "pending" | "started") {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let logged = requests.lock().unwrap().clone();
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();

    assert!(
        sw_result.starts_with("blocked:"),
        "Service worker request should be blocked, got: {}",
        sw_result
    );
    assert!(
        !logged.iter().any(|line| line.contains(" /leak ")),
        "Blocked service worker request reached the server: {:?}",
        logged
    );
}

#[tokio::test]
#[ignore]
async fn e2e_domain_filter_blocks_worker_websocket_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = requests_for_server.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or("").to_string();
                if let Ok(mut logged) = requests.lock() {
                    logged.push(request_line.clone());
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");

                let (content_type, body) = if path == "/worker.js" {
                    (
                        "application/javascript",
                        format!(
                            r#"self.addEventListener('message', async () => {{
    try {{
        const response = await fetch('/worker-ping');
        const text = await response.text();
        if (text !== 'pong') {{
            self.postMessage('fetch:' + text);
            return;
        }}
    }} catch (error) {{
        self.postMessage('fetch-error:' + error.name);
        return;
    }}

    try {{
        const ws = new WebSocket('ws://127.0.0.1:{}/leak');
        ws.onopen = () => self.postMessage('leaked');
        ws.onerror = () => self.postMessage('blocked:error');
    }} catch (error) {{
        self.postMessage('blocked:' + error.name);
    }}
}});"#,
                            port
                        ),
                    )
                } else if path == "/worker-ping" {
                    ("text/plain", "pong".to_string())
                } else {
                    (
                        "text/html",
                        r#"<!doctype html><title>allowed</title><script>
window.workerWsResult = 'pending';
const worker = new Worker('/worker.js');
worker.onmessage = event => {
    window.workerWsResult = event.data;
};
worker.onerror = () => {
    window.workerWsResult = 'worker:error';
};
worker.postMessage('go');
</script>"#
                            .to_string(),
                    )
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    content_type,
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut state = DaemonState::new();
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("localhost"));
    }

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
            "url": format!("http://localhost:{}/", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let mut worker_result = "pending".to_string();
    for _ in 0..50 {
        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "evaluate",
                "script": "window.workerWsResult || 'pending'",
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        worker_result = get_data(&resp)["result"]
            .as_str()
            .unwrap_or("pending")
            .to_string();
        if worker_result != "pending" {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let logged = requests.lock().unwrap().clone();
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();

    assert!(
        worker_result.starts_with("blocked:"),
        "Worker WebSocket should be blocked, got: {}",
        worker_result
    );
    assert!(
        !logged.iter().any(|line| line.contains(" /leak ")),
        "Blocked worker WebSocket reached the server: {:?}",
        logged
    );
}

#[tokio::test]
#[ignore]
async fn e2e_domain_filter_blocks_csp_self_worker_fallback() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = requests_for_server.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or("").to_string();
                if let Ok(mut logged) = requests.lock() {
                    logged.push(request_line.clone());
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");

                let (content_type, body, csp) = if path == "/worker.js" {
                    (
                        "application/javascript",
                        format!(
                            r#"self.addEventListener('message', async () => {{
    try {{
        const response = await fetch('/worker-ping');
        const text = await response.text();
        try {{
            await fetch('http://127.0.0.1:{}/leak', {{ mode: 'no-cors' }});
            self.postMessage('leaked');
        }} catch (error) {{
            self.postMessage('started:' + text + ';blocked:' + error.name);
        }}
    }} catch (error) {{
        self.postMessage('blocked:' + error.name);
    }}
}});"#,
                            port
                        ),
                        None,
                    )
                } else if path == "/worker-ping" {
                    ("text/plain", "pong".to_string(), None)
                } else {
                    (
                        "text/html",
                        r#"<!doctype html><title>allowed</title><script>
window.workerCspResult = 'pending';
const worker = new Worker('/worker.js');
worker.onmessage = event => {
    window.workerCspResult = event.data;
};
worker.onerror = () => {
    window.workerCspResult = 'worker:error';
};
worker.postMessage('go');
</script>"#
                            .to_string(),
                        Some("Content-Security-Policy: default-src 'self'; script-src 'self' 'unsafe-inline'; worker-src 'self'\r\n"),
                    )
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    content_type,
                    csp.unwrap_or(""),
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut state = DaemonState::new();
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("localhost"));
    }

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
            "url": format!("http://localhost:{}/", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let mut worker_result = "pending".to_string();
    for _ in 0..50 {
        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "evaluate",
                "script": "window.workerCspResult || 'pending'",
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        worker_result = get_data(&resp)["result"]
            .as_str()
            .unwrap_or("pending")
            .to_string();
        if worker_result != "pending" {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let logged = requests.lock().unwrap().clone();
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();

    assert_eq!(
        worker_result, "worker:error",
        "CSP-blocked worker bootstrap must fail closed instead of running an unguarded worker"
    );
    assert!(
        !logged.iter().any(|line| line.contains(" /leak ")),
        "Blocked CSP fallback worker request reached the server: {:?}",
        logged
    );
}

#[tokio::test]
#[ignore]
async fn e2e_domain_filter_blocks_module_worker_top_level_requests() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let requests_for_server = requests.clone();
    let server = tokio::spawn(async move {
        for _ in 0..20 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let requests = requests_for_server.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or("").to_string();
                if let Ok(mut logged) = requests.lock() {
                    logged.push(request_line.clone());
                }
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");

                let (content_type, body) = if path == "/module-worker.js" {
                    (
                        "application/javascript",
                        format!(
                            r#"try {{
    await fetch('http://127.0.0.1:{}/leak', {{ mode: 'no-cors' }});
    self.postMessage('leaked');
}} catch (error) {{
    self.postMessage('blocked:' + error.name);
}}"#,
                            port
                        ),
                    )
                } else {
                    (
                        "text/html",
                        r#"<!doctype html><title>allowed</title><script>
window.moduleWorkerResult = 'pending';
const worker = new Worker('/module-worker.js', { type: 'module' });
worker.onmessage = event => {
    window.moduleWorkerResult = event.data;
};
worker.onerror = () => {
    window.moduleWorkerResult = 'worker:error';
};
</script>"#
                            .to_string(),
                    )
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    content_type,
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    let mut state = DaemonState::new();
    {
        let mut df = state.domain_filter.write().await;
        *df = Some(super::network::DomainFilter::new("localhost"));
    }

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
            "url": format!("http://localhost:{}/", port),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let mut worker_result = "pending".to_string();
    for _ in 0..50 {
        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "evaluate",
                "script": "window.moduleWorkerResult || 'pending'",
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        worker_result = get_data(&resp)["result"]
            .as_str()
            .unwrap_or("pending")
            .to_string();
        if worker_result != "pending" {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let logged = requests.lock().unwrap().clone();
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();

    assert!(
        worker_result.starts_with("blocked:"),
        "Module worker top-level request should be blocked, got: {}",
        worker_result
    );
    assert!(
        !logged.iter().any(|line| line.contains(" /leak ")),
        "Blocked module worker request reached the server: {:?}",
        logged
    );
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
        &json!({
            "id": "2",
            "action": "navigate",
            "url": native_test_fixture_url("snapshot_diff_probe")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Take a snapshot and use it as baseline for diff
    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let baseline = get_data(&resp)["snapshot"].as_str().unwrap().to_string();
    assert!(baseline.starts_with("- button \"Primary action\" [ref=e1]"));
    let baseline_dir = tempfile::tempdir().unwrap();
    let baseline_path = baseline_dir.path().join("baseline.txt");
    std::fs::write(&baseline_path, format!("{}\n", baseline)).unwrap();

    // A failed diff must preserve the refs from the last successful snapshot.
    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "diff_snapshot",
            "baseline": baseline_path,
            "selector": "#missing"
        }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    assert!(resp["error"]
        .as_str()
        .unwrap()
        .contains("did not match any element"));

    let resp = execute_command(
        &json!({ "id": "5", "action": "click", "selector": "e1" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Repeated diffs preserve document refs and report no content changes.
    for id in ["6", "7"] {
        let resp = execute_command(
            &json!({ "id": id, "action": "diff_snapshot", "baseline": baseline_path }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        let data = get_data(&resp);
        assert_eq!(data["changed"], false);
        assert_eq!(data["additions"], 0);
        assert_eq!(data["removals"], 0);
        assert_eq!(data["diff"], "");
    }

    // Modify the page
    let resp = execute_command(
        &json!({
            "id": "8",
            "action": "evaluate",
            "script": "document.querySelector('#primary-action').textContent = 'Updated action'"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Diff against baseline
    let resp = execute_command(
        &json!({ "id": "9", "action": "diff_snapshot", "baseline": baseline_path }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(
        data["changed"], true,
        "Diff should detect the button change"
    );
    assert_eq!(data["additions"], 1);
    assert_eq!(data["removals"], 1);
    assert!(data["diff"].as_str().unwrap().contains("Updated action"));

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_diff_url_aligns_refs_after_snapshot() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let stale_url = "data:text/html,<button id='stale-action'>Stale action</button>";
    let url = native_test_fixture_url("snapshot_diff_probe");
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": stale_url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Populate refs that must be invalidated once the URL diff starts navigating.
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "snapshot",
            "selector": "#stale-action"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(state.ref_map.get("e1").is_some());

    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "diff_url",
            "url1": url,
            "url2": "http://[invalid"
        }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    assert!(state.ref_map.entries_sorted().is_empty());

    let resp = execute_command(
        &json!({
            "id": "5",
            "action": "evaluate",
            "script": "document.querySelector('#primary-action')?.textContent"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "Primary action");

    let resp = execute_command(
        &json!({ "id": "6", "action": "click", "selector": "e1" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false);
    assert!(resp["error"].as_str().unwrap().contains("Unknown ref: e1"));

    // Populate the session ref map before comparing the same URL to itself.
    let resp = execute_command(&json!({ "id": "7", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "8",
            "action": "diff_url",
            "url1": url,
            "url2": url
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(data["diff"]["identical"], true);
    assert_eq!(data["diff"]["changed"], false);
    assert_eq!(data["diff"]["additions"], 0);
    assert_eq!(data["diff"]["removals"], 0);
    assert_ne!(
        data["snapshot1"], data["snapshot2"],
        "Replaced documents must not recycle actionable IDs"
    );
    assert_eq!(
        super::diff::snapshot_comparison_text(data["snapshot1"].as_str().unwrap()),
        super::diff::snapshot_comparison_text(data["snapshot2"].as_str().unwrap())
    );
    let primary_ref = state
        .ref_map
        .entries_sorted()
        .into_iter()
        .find(|(_, entry)| entry.name == "Primary action")
        .unwrap()
        .0;
    let next = execute_command(&json!({"id": "9", "action": "snapshot"}), &mut state).await;
    assert_success(&next);
    assert_eq!(
        get_data(&next)["refs"][&primary_ref]["name"],
        "Primary action"
    );
    assert_success(
        &execute_command(
            &json!({"id": "10", "action": "click", "selector": primary_ref}),
            &mut state,
        )
        .await,
    );

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

#[tokio::test]
#[ignore]
async fn e2e_click_reports_covering_overlay() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = r#"
        <html>
        <body>
            <button id="target" onclick="document.getElementById('result').textContent = 'clicked'">
                Target
            </button>
            <div id="consent-banner" style="position:fixed;inset:0;z-index:10;background:rgba(0,0,0,0.1)">
                <button id="dismiss" style="position:absolute;right:20px;bottom:20px"
                    onclick="document.getElementById('consent-banner').remove()">
                    Dismiss
                </button>
            </div>
            <div id="result">idle</div>
        </body>
        </html>
    "#;
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "click", "selector": "#target" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false, "covered target should fail: {resp}");
    let error = resp["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("covered by <div#consent-banner>"),
        "unexpected covered-click error: {}",
        error
    );

    let resp = execute_command(
        &json!({ "id": "4", "action": "gettext", "selector": "#result" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "idle");

    let resp = execute_command(
        &json!({ "id": "5", "action": "click", "selector": "#dismiss" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "click", "selector": "#target" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "7", "action": "gettext", "selector": "#result" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "clicked");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_click_rejects_unreachable_elements() {
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = r#"
        <html>
        <body>
            <button id="gone" style="display:none" onclick="document.getElementById('result').textContent = 'gone'">Gone</button>
            <button id="hidden-attr" hidden onclick="document.getElementById('result').textContent = 'hidden'">Hidden</button>
            <button id="invisible" style="visibility:hidden" onclick="document.getElementById('result').textContent = 'invisible'">Invisible</button>
            <button id="collapsed" style="visibility:collapse" onclick="document.getElementById('result').textContent = 'collapsed'">Collapsed</button>
            <button id="off-page" style="position:absolute;left:-9999px" onclick="document.getElementById('result').textContent = 'off'">Off page</button>
            <button id="disabled-btn" disabled onclick="document.getElementById('result').textContent = 'disabled'">Disabled</button>
            <fieldset disabled>
                <button id="fieldset-disabled" onclick="document.getElementById('result').textContent = 'fieldset'">Fieldset</button>
            </fieldset>
            <button id="ok" onclick="document.getElementById('result').textContent = 'clicked'">Ok</button>
            <label><input id="hidden-check" type="checkbox" style="display:none"> Accept</label>
            <svg width="300" height="100"><line id="flat-line" x1="20" y1="50" x2="280" y2="50" stroke="black" stroke-width="8" onclick="document.getElementById('result').textContent = 'line'"/></svg>
            <div id="result">idle</div>
        </body>
        </html>
    "#;
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let cases = [
        ("#gone", "is not visible"),
        ("#hidden-attr", "is not visible"),
        ("#invisible", "is not visible"),
        ("#collapsed", "is not visible"),
        ("#off-page", "is outside the viewport"),
        ("#disabled-btn", "is disabled"),
        ("#fieldset-disabled", "is disabled"),
    ];
    for (selector, expected) in cases {
        let resp = execute_command(
            &json!({ "id": "3", "action": "click", "selector": selector }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], false, "{selector} should fail: {resp}");
        let error = resp["error"].as_str().unwrap_or_default();
        assert!(error.contains(expected), "{selector}: {error}");
    }

    async fn result_text(state: &mut DaemonState) -> Value {
        let cmd = json!({ "id": "4", "action": "gettext", "selector": "#result" });
        let resp = execute_command(&cmd, state).await;
        assert_success(&resp);
        get_data(&resp)["text"].clone()
    }
    assert_eq!(result_text(&mut state).await, "idle");

    let snap = execute_command(&json!({ "id": "5", "action": "snapshot" }), &mut state).await;
    assert_success(&snap);
    let find_ref = |name: &str| {
        get_data(&snap)["refs"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, node)| node["name"] == name)
            .unwrap()
            .0
            .clone()
    };
    for (name, expected) in [
        ("Disabled", "is disabled"),
        ("Off page", "is outside the viewport"),
    ] {
        let selector = format!("@{}", find_ref(name));
        let resp = execute_command(
            &json!({ "id": "6", "action": "click", "selector": selector }),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], false, "{name} ref should fail: {resp}");
        let error = resp["error"].as_str().unwrap_or_default();
        assert!(error.contains(expected), "{name}: {error}");
    }
    assert_eq!(result_text(&mut state).await, "idle");

    let resp = execute_command(
        &json!({ "id": "7", "action": "click", "selector": "#ok" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(result_text(&mut state).await, "clicked");

    let resp = execute_command(
        &json!({ "id": "7b", "action": "click", "selector": "#flat-line" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(result_text(&mut state).await, "line");

    let resp = execute_command(
        &json!({ "id": "8", "action": "isenabled", "selector": "#fieldset-disabled" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["enabled"], false);

    let resp = execute_command(
        &json!({ "id": "9", "action": "hover", "selector": "#disabled-btn" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "10", "action": "check", "selector": "#hidden-check" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "11", "action": "ischecked", "selector": "#hidden-check" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["checked"], true);

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

async fn ref_recovery_setup(state: &mut DaemonState, body: &str) -> String {
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        state,
    )
    .await;
    assert_success(&resp);
    let html = format!(
        "data:text/html,<body><script>window.clicks = {{a: 0, b: 0}};</script>{}</body>",
        body
    );
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), state).await;
    assert_success(&resp);
    get_data(&resp)["snapshot"].as_str().unwrap().to_string()
}

fn ref_recovery_refs(snapshot: &str, name: &str) -> Vec<String> {
    let needle = format!("\"{}\"", name);
    snapshot
        .lines()
        .filter(|l| l.contains(&needle))
        .filter_map(|l| {
            let start = l.find("[ref=")? + 5;
            let end = l[start..].find(']')? + start;
            Some(l[start..end].to_string())
        })
        .collect()
}

async fn ref_recovery_eval(state: &mut DaemonState, script: &str) -> Value {
    let resp = execute_command(
        &json!({ "id": "ev", "action": "evaluate", "script": script }),
        state,
    )
    .await;
    assert_success(&resp);
    get_data(&resp)["result"].clone()
}

async fn ref_recovery_click_expect_error(state: &mut DaemonState, selector: &str) {
    let resp = execute_command(
        &json!({ "id": "cl", "action": "click", "selector": selector }),
        state,
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    let error = resp["error"].as_str().unwrap_or("");
    assert!(error.contains("Run snapshot again"), "{error}");
}

const REF_RECOVERY_TWO_BUTTONS: &str = r#"<div id="c"><button id="a" onclick="window.clicks.a++">Approve</button><button id="b" onclick="window.clicks.b++">Approve</button></div>"#;

const REF_RECOVERY_REPLACE_REORDERED: &str = r#"(function(){
    var c = document.getElementById('c');
    c.innerHTML = '';
    var nb = document.createElement('button');
    nb.textContent = 'Approve';
    nb.onclick = function() { window.clicks.b++; };
    var na = document.createElement('button');
    na.textContent = 'Approve';
    na.onclick = function() { window.clicks.a++; };
    c.appendChild(nb);
    c.appendChild(na);
    return true;
})()"#;

#[tokio::test]
#[ignore]
async fn e2e_ref_recovery_duplicates_replaced_and_reordered_is_rejected() {
    let mut state = DaemonState::new();
    let snapshot = ref_recovery_setup(&mut state, REF_RECOVERY_TWO_BUTTONS).await;
    let refs = ref_recovery_refs(&snapshot, "Approve");
    assert_eq!(refs.len(), 2, "{snapshot}");

    ref_recovery_eval(&mut state, REF_RECOVERY_REPLACE_REORDERED).await;
    ref_recovery_click_expect_error(&mut state, &refs[0]).await;
    ref_recovery_click_expect_error(&mut state, &refs[1]).await;

    let clicks = ref_recovery_eval(&mut state, "window.clicks.a + ',' + window.clicks.b").await;
    assert_eq!(clicks, "0,0");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_ref_recovery_duplicate_with_single_survivor_is_rejected() {
    let mut state = DaemonState::new();
    let snapshot = ref_recovery_setup(&mut state, REF_RECOVERY_TWO_BUTTONS).await;
    let refs = ref_recovery_refs(&snapshot, "Approve");
    assert_eq!(refs.len(), 2, "{snapshot}");

    ref_recovery_eval(&mut state, "document.getElementById('a').remove(); true").await;
    ref_recovery_click_expect_error(&mut state, &refs[0]).await;

    let clicks = ref_recovery_eval(&mut state, "window.clicks.a + ',' + window.clicks.b").await;
    assert_eq!(clicks, "0,0");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_ref_recovery_unique_ref_with_two_replacements_is_rejected() {
    let mut state = DaemonState::new();
    let snapshot = ref_recovery_setup(
        &mut state,
        r#"<div id="c"><button id="a" onclick="window.clicks.a++">Approve</button></div>"#,
    )
    .await;
    let refs = ref_recovery_refs(&snapshot, "Approve");
    assert_eq!(refs.len(), 1, "{snapshot}");

    ref_recovery_eval(&mut state, REF_RECOVERY_REPLACE_REORDERED).await;
    ref_recovery_click_expect_error(&mut state, &refs[0]).await;

    let clicks = ref_recovery_eval(&mut state, "window.clicks.a + ',' + window.clicks.b").await;
    assert_eq!(clicks, "0,0");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_ref_recovery_moved_duplicates_keep_exact_node() {
    let mut state = DaemonState::new();
    let snapshot = ref_recovery_setup(&mut state, REF_RECOVERY_TWO_BUTTONS).await;
    let refs = ref_recovery_refs(&snapshot, "Approve");
    assert_eq!(refs.len(), 2, "{snapshot}");

    ref_recovery_eval(
        &mut state,
        "var c = document.getElementById('c'); c.appendChild(document.getElementById('a')); c.firstChild.id",
    )
    .await;
    let resp = execute_command(
        &json!({ "id": "cl", "action": "click", "selector": refs[0] }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let clicks = ref_recovery_eval(&mut state, "window.clicks.a + ',' + window.clicks.b").await;
    assert_eq!(clicks, "1,0");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_ref_recovery_object_path_treats_detached_node_as_stale() {
    let mut state = DaemonState::new();
    let snapshot = ref_recovery_setup(
        &mut state,
        r#"<div id="c"><button>Approve</button><button>Approve</button></div><div id="u"><input aria-label="Email"></div>"#,
    )
    .await;
    let approve = ref_recovery_refs(&snapshot, "Approve");
    let email = ref_recovery_refs(&snapshot, "Email");
    assert_eq!((approve.len(), email.len()), (2, 1), "{snapshot}");

    ref_recovery_eval(
        &mut state,
        "c.innerHTML = '<button>Approve</button><button>Approve</button>'; u.innerHTML = '<input aria-label=\"Email\">'; true",
    )
    .await;

    let resp = execute_command(
        &json!({ "id": "g1", "action": "gettext", "selector": approve[0] }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    assert!(
        resp["error"]
            .as_str()
            .unwrap_or("")
            .contains("Run snapshot again"),
        "{resp}"
    );

    let resp = execute_command(
        &json!({ "id": "f1", "action": "fill", "selector": email[0], "value": "a@b.co" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let value = ref_recovery_eval(&mut state, "u.querySelector('input').value").await;
    assert_eq!(value, "a@b.co");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_ref_recovery_object_fallback_requires_unique_match() {
    let mut state = DaemonState::new();
    ref_recovery_setup(
        &mut state,
        r#"<input id="a" aria-label="Name"><input id="b" aria-label="Name">"#,
    )
    .await;
    state
        .ref_map
        .add("e900".to_string(), None, "textbox", "Name", None);
    state
        .ref_map
        .add("e901".to_string(), None, "textbox", "Name", Some(0));

    let resp = execute_command(
        &json!({ "id": "f1", "action": "fill", "selector": "@e900", "value": "hello" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    let error = resp["error"].as_str().unwrap_or("");
    assert!(error.contains("several elements now match"), "{error}");
    let values = ref_recovery_eval(
        &mut state,
        "document.getElementById('a').value + '|' + document.getElementById('b').value",
    )
    .await;
    assert_eq!(values, "|");

    ref_recovery_eval(&mut state, "document.getElementById('b').remove(); true").await;
    let resp = execute_command(
        &json!({ "id": "f3", "action": "fill", "selector": "@e901", "value": "marked" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    let error = resp["error"].as_str().unwrap_or("");
    assert!(
        error.contains("the snapshot had several matching elements"),
        "{error}"
    );
    let value = ref_recovery_eval(&mut state, "document.getElementById('a').value").await;
    assert_eq!(value, "");

    let resp = execute_command(
        &json!({ "id": "f2", "action": "fill", "selector": "@e900", "value": "hello" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let value = ref_recovery_eval(&mut state, "document.getElementById('a').value").await;
    assert_eq!(value, "hello");

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
// (PS: -C is deprecated, cursor-interactive elements are referred by default now)
// ---------------------------------------------------------------------------

/// Verifies that `snapshot` detects elements with cursor:pointer / onclick / tabindex,
/// produces the correct v0.19.0-compatible output format, deduplicates against the ARIA
/// tree, and completes in bounded time (no sequential CDP round-trip explosion).
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
    //  - <button> and <a> (standard interactive – ARIA tree)
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

    // snapshot -i: interactive tree
    let start = std::time::Instant::now();
    let resp = execute_command(
        &json!({ "id": "3", "action": "snapshot", "interactive": true }),
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
        "snapshot took {:?}, expected < 5s (Issue #841 regression)",
        elapsed,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Interactive snapshots hint ambiguously named or empty controls after the ref brackets.
#[tokio::test]
#[ignore]
async fn e2e_snapshot_hints_ambiguous_controls() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = r##"<!doctype html><title>Hints</title>
<a href="/cart"><img src="/img/cart.png"></a>
<ul><li>Blue mug
<button><svg width="16" height="16"><use href="#icon-trash"/></svg></button>
<button><i class="fa fa-heart"></i></button>
<button id="edit-btn"><img src="/img/pencil.png"></button>
</li></ul>
<input type="email" name="login-email" placeholder="Email">
<label>Email <input type="email" name="newsletter-email"></label>
<button>Save</button>
<ul><li>Invoice 1 <div onclick="void 0">Edit</div></li><li>Invoice 2 <div onclick="void 0">Edit</div></li></ul>
<div onclick="void 0">Archive</div>"##;
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "snapshot", "interactive": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();

    for hint in [
        r#"hint="img:cart""#,
        r#"hint="icon:icon-trash; in:Blue mug""#,
        r#"hint="icon:fa-heart; in:Blue mug""#,
        r#"hint="img:pencil; id:edit-btn; in:Blue mug""#,
        r#"hint="name:login-email""#,
        r#"hint="name:newsletter-email""#,
        r#"hint="in:Invoice 1""#,
        r#"hint="in:Invoice 2""#,
    ] {
        assert!(
            snapshot.contains(hint),
            "Expected snapshot to contain {}:\n{}",
            hint,
            snapshot,
        );
    }

    for unique in ["button \"Save\"", "generic \"Archive\""] {
        let line = snapshot
            .lines()
            .find(|line| line.contains(unique))
            .expect("uniquely named control should be in the snapshot");
        assert!(
            !line.contains("hint="),
            "Uniquely named control should not get a hint:\n{}",
            snapshot,
        );
    }

    for line in snapshot.lines().filter(|line| line.contains("hint=")) {
        assert!(
            line.contains("] hint="),
            "Hint should follow the closed ref brackets:\n{}",
            snapshot,
        );
    }

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// A full-page screenshot with a real device scale factor of 2 (not the
/// emulated one, which already reports CSS pixels) must be the page size
/// times two, with the page filling it, not twice that with the page in the
/// top-left quarter.
#[tokio::test]
#[ignore]
async fn e2e_screenshot_full_page_high_dpi() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true, "args": ["--force-device-scale-factor=2"] }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": "data:text/html,<body style='margin:0'><div style='height:2500px;background:%23c00'></div>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let out = std::env::temp_dir()
        .join("agent-browser-e2e-full-page-high-dpi.png")
        .to_string_lossy()
        .to_string();
    let resp = execute_command(
        &json!({ "id": "4", "action": "screenshot", "fullPage": true, "path": out }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let img = image::open(&out).expect("decode screenshot").to_rgb8();
    let _ = std::fs::remove_file(&out);
    assert_eq!(img.dimensions(), (2560, 5000));
    let corner = img.get_pixel(2550, 4990);
    assert!(
        corner[0] > 150 && corner[1] < 60 && corner[2] < 60,
        "bottom-right corner should be page content, got {corner:?}"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Share of pixels in a screenshot that are pure red (#c00), for checking
/// that an element clip actually contains the element.
fn red_fraction(path: &str) -> (u32, u32, f64) {
    let img = image::open(path).expect("decode screenshot").to_rgb8();
    let (w, h) = img.dimensions();
    let red = img
        .pixels()
        .filter(|p| p[0] > 150 && p[1] < 60 && p[2] < 60)
        .count();
    (w, h, red as f64 / (w * h) as f64)
}

/// Element screenshots must contain the element wherever it sits: below the
/// initial viewport, after scrolling, and inside a same-origin iframe, at a
/// device scale factor of 2.
#[tokio::test]
#[ignore]
async fn e2e_screenshot_selector_beyond_viewport() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "viewport", "width": 1024, "height": 640, "deviceScaleFactor": 2.0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let boxed = |id: &str| {
        format!("<button id='{id}' style='width:200px;height:200px;border:0;background:%23c00'>{id}</button>")
    };
    let html = format!(
        "data:text/html,<body style='margin:0'><div style='height:700px'></div>{}\
         <div style='height:300px'></div><iframe style='border:0;width:600px;height:1500px' \
         srcdoc=\"<body style='margin:0'><div style='height:800px'></div>{}\
         <div style='height:400px'></div>\"></iframe><div style='height:2000px'></div>",
        boxed("box"),
        boxed("ibox"),
    );
    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Elements inside the iframe are only addressable through snapshot refs.
    let snapshot = execute_command(&json!({ "id": "4", "action": "snapshot" }), &mut state).await;
    assert_success(&snapshot);
    let iframe_ref = get_data(&snapshot)["refs"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, node)| node["name"] == "ibox")
        .map(|(ref_id, _)| format!("@{ref_id}"))
        .expect("ref for the button inside the iframe");

    let path = |name: &str| {
        std::env::temp_dir()
            .join(format!("agent-browser-e2e-selector-{name}.png"))
            .to_string_lossy()
            .to_string()
    };

    for (step, (selector, name, scroll)) in [
        ("#box", "below-fold", false),
        ("#box", "scrolled", true),
        (iframe_ref.as_str(), "iframe", false),
    ]
    .into_iter()
    .enumerate()
    {
        if scroll {
            let resp = execute_command(
                &json!({ "id": format!("scroll-{step}"), "action": "scrollintoview", "selector": selector }),
                &mut state,
            )
            .await;
            assert_success(&resp);
        }
        let out = path(name);
        let resp = execute_command(
            &json!({ "id": format!("shot-{step}"), "action": "screenshot", "selector": selector, "path": out }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        let (w, h, red) = red_fraction(&out);
        let _ = std::fs::remove_file(&out);
        assert_eq!((w, h), (400, 400), "{name}: size");
        assert!(
            red > 0.99,
            "{name}: only {:.0}% of the clip is the element",
            red * 100.0
        );
    }

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

/// Verifies `snapshot` with many cursor-interactive elements completes in
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
        &json!({ "id": "3", "action": "snapshot", "interactive": true }),
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
        "snapshot with 100 cursor elements took {:?}, expected < 10s (Issue #841)",
        elapsed,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Test that a selector-scoped snapshot of a web component includes the
/// content of its shadow root.
#[tokio::test]
#[ignore]
async fn e2e_snapshot_selector_includes_shadow_root() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html =
        "data:text/html,<my-card><span>Slotted</span></my-card><button>Outside</button><script>\
        customElements.define('my-card', class extends HTMLElement { constructor() { \
        super(); this.attachShadow({ mode: 'open' }).innerHTML = \
        '<button>Inside shadow</button><slot></slot>'; } });</script>";

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "snapshot", "selector": "my-card" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();

    assert!(
        snapshot.contains("button \"Inside shadow\""),
        "Snapshot should contain the shadow root button: {}",
        snapshot
    );
    assert!(
        snapshot.contains("Slotted"),
        "Snapshot should contain the slotted text: {}",
        snapshot
    );
    assert!(
        !snapshot.contains("Outside"),
        "Snapshot should not contain content outside the selector: {}",
        snapshot
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Test that InlineTextBox nodes are filtered from snapshot output while preserving
/// the actual text content from parent elements.
#[tokio::test]
#[ignore]
async fn e2e_snapshot_continuous_static_text() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Simple HTML with text content that would generate InlineTextBox nodes and sperate to multiple StaticText nodes
    let html =
        "data:text/html,<html><body><div><span>Hello</span> <span>World</span></div></body></html>";

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Take snapshot to capture full output and verify InlineTextBox filtering and StaticText aggregation
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
        snapshot_output.contains("Hello World"),
        "Snapshot should contain 'Hello World': {}",
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

/// Test that a selector-scoped snapshot renders each element once when plain
/// wrapper elements (ignored in the AX tree) sit between the matched element
/// and its descendants.
#[tokio::test]
#[ignore]
async fn e2e_snapshot_selector_no_duplicates() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = "data:text/html,<main><div><div><button>Save</button></div></div>\
        <select><option>Small</option><option>Large</option></select></main>\
        <footer><button>Outside</button></footer>";

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "snapshot", "selector": "main" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();

    for name in ["button \"Save\"", "option \"Small\"", "option \"Large\""] {
        assert_eq!(
            snapshot.matches(name).count(),
            1,
            "{} should appear once: {}",
            name,
            snapshot
        );
    }
    assert!(
        !snapshot.contains("Outside"),
        "Snapshot should not contain content outside the selector: {}",
        snapshot
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
async fn start_link_fixture_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/", listener.local_addr().unwrap());

    let handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = if request.starts_with("GET /next ") {
                "<!doctype html><title>Next</title><h1>Next page</h1>"
            } else {
                "<!doctype html><title>Example Domain</title><h1>Example Domain</h1>\
                 <a href=\"/next\">Next page</a>"
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });

    (base_url, handle)
}

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

async fn start_webmcp_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let request = String::from_utf8_lossy(&buffer);
                let body = if request.starts_with("GET /frame.html ") {
                    native_test_fixture_html("webmcp_frame_probe")
                        .replace("__PORT__", &port.to_string())
                } else if request.starts_with("GET /context.html ") {
                    native_test_fixture_html("webmcp_context_probe").to_string()
                } else if request.starts_with("GET /empty.html ") {
                    "<!doctype html><title>No page tools</title>".to_string()
                } else if request.starts_with("GET /delayed.html ") {
                    native_test_fixture_html("webmcp_delayed_probe").to_string()
                } else {
                    native_test_fixture_html("webmcp_probe").replace("__PORT__", &port.to_string())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

/// Starts a tiny cookie-gated app that behaves like a Next dev target with
/// cookie-backed login state.
async fn start_cookie_login_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{}", port);

    let handle = tokio::spawn(async move {
        for _ in 0..100 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };

            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or_default();
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                let has_auth_cookie = request.lines().any(|line| {
                    line.to_ascii_lowercase().starts_with("cookie:")
                        && line.contains("next_dev_loop_auth=1")
                });

                let mut headers = vec!["Content-Type: text/html".to_string()];
                let body = if path.starts_with("/login") {
                    headers.push(
                        "Set-Cookie: next_dev_loop_auth=1; Path=/; Max-Age=3600; SameSite=Lax"
                            .to_string(),
                    );
                    "<!doctype html><title>Logged in</title><main>Login complete</main>".to_string()
                } else if has_auth_cookie {
                    "<!doctype html><title>Home</title><main>Welcome back</main>".to_string()
                } else {
                    "<!doctype html><title>Home</title><main>Please sign in</main>".to_string()
                };

                let response = format!(
                    "HTTP/1.1 200 OK\r\n{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    headers.join("\r\n"),
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

/// Starts a tiny HTTP server that serves a delayed-render login form.
///
/// The page continuously fetches `/ping` so `networkidle` is hard to reach,
/// while the login form itself appears after `render_delay_ms`.
async fn start_delayed_login_server(
    render_delay_ms: u64,
    ping_interval_ms: u64,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{}", port);

    let handle = tokio::spawn(async move {
        // Serve enough requests for navigation + many background /ping calls.
        for _ in 0..1000 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };

            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let request_line = request.lines().next().unwrap_or_default();
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");

                let (status, content_type, body) = if path.starts_with("/ping") {
                    ("204 No Content", "text/plain", String::new())
                } else {
                    let html = format!(
                        r#"<!doctype html>
<html>
  <head><meta charset="utf-8"><title>Delayed Login</title></head>
  <body>
    <input id="search" type="text" name="search" />
    <div id="root">loading...</div>
    <script>
      setInterval(() => {{
        fetch('/ping?ts=' + Date.now()).catch(() => {{}});
      }}, {ping_interval_ms});

      setTimeout(() => {{
        const root = document.getElementById('root');
        root.innerHTML = `
          <form id="login-form">
            <input type="email" name="email" />
            <input type="password" name="password" />
            <button type="submit">Sign in</button>
          </form>
        `;
        document.getElementById('login-form').addEventListener('submit', function(e) {{
          e.preventDefault();
          e.stopPropagation();
          window.__submitted = true;
        }});
      }}, {render_delay_ms});
    </script>
  </body>
</html>"#,
                    );
                    ("200 OK", "text/html", html)
                };

                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    content_type,
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

/// Starts a stateful login page whose form appears only after an in-page
/// action. The counter covers top-level documents so tests can prove that
/// no-navigation login did not reload or replace the page.
async fn start_stateful_auth_login_server(
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base_url = format!("http://127.0.0.1:{}", port);
    let document_requests = Arc::new(AtomicUsize::new(0));
    let counter = document_requests.clone();

    let handle = tokio::spawn(async move {
        for _ in 0..100 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let counter = counter.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                if !path.starts_with("/favicon") {
                    counter.fetch_add(1, Ordering::SeqCst);
                }

                let body = r#"<!doctype html>
<html>
  <head><meta charset="utf-8"><title>Stateful Login</title><link rel="icon" href="data:," /></head>
  <body>
    <button id="reveal" type="button">Continue to login</button>
    <form id="login-form" style="display:none">
      <input type="email" name="email" />
      <input type="password" name="password" />
      <button type="submit">Sign in</button>
    </form>
    <script>
      document.getElementById('reveal').addEventListener('click', () => {
        document.getElementById('login-form').style.display = 'block';
        window.__revealed = true;
      });
      document.getElementById('login-form').addEventListener('submit', (event) => {
        event.preventDefault();
        window.__submitted = true;
      });
    </script>
  </body>
</html>"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body,
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    (base_url, document_requests, handle)
}

fn unique_auth_profile_name(suffix: &str) -> String {
    format!(
        "e2e-auth-login-{}-{}",
        suffix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_else(|_| std::time::Duration::from_secs(0))
            .as_nanos()
    )
}

#[tokio::test]
#[ignore]
async fn e2e_auth_login_selects_usable_controls_and_preserves_credential_targets() {
    let (base_url, _, server) = start_stateful_auth_login_server().await;
    let mut state = DaemonState::new();
    let profile = unique_auth_profile_name("changed-target");
    for command in [
        json!({ "id": "launch", "action": "launch", "headless": true }),
        json!({ "id": "open", "action": "navigate", "url": base_url }),
        json!({ "id": "save", "action": "auth_save", "name": profile,
            "url": base_url, "username": "target@example.test", "password": "target-password" }),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    let mut results = Vec::new();
    for field in ["user", "pass"] {
        for phase in [
            "normal",
            "selection",
            "focus-detach",
            "focus-redirect",
            "select-redirect",
            "focus-readonly",
            "input-detach",
            "exception",
            "textarea",
            "contenteditable",
            "no-native-edit",
            "no-native-edit-textarea",
            "no-native-edit-contenteditable",
        ] {
            let script = format!(
                r#"(() => {{
                    document.body.innerHTML = '<form><input id="decoy" value="decoy:"><input id="hidden-user" type="email" autocomplete="username" hidden><input id="user" type="email" autocomplete="username webauthn"><input id="hidden-pass" type="password" hidden><input id="pass" type="password"><button hidden type="submit" onclick="window.hiddenClicked = true">Hidden</button><button type="submit" onclick="window.visibleClicked = true">Sign in</button></form>';
                    if ('{phase}'.endsWith('textarea')) document.getElementById('{field}').outerHTML = '<textarea id="{field}">old value</textarea>';
                    if ('{phase}'.endsWith('contenteditable')) document.getElementById('{field}').outerHTML = '<div id="{field}" contenteditable="true">old value</div>';
                    window.events = [];
                    window.submitted = false;
                    window.hiddenClicked = false;
                    window.visibleClicked = false;
                    window.nativeEdit ??= document.execCommand;
                    document.execCommand = '{phase}'.startsWith('no-native-edit') ? undefined : window.nativeEdit;
                    const decoy = document.getElementById('decoy');
                    const target = document.getElementById('{field}');
                    const replace = () => {{
                        window.detached = target;
                        target.replaceWith(target.cloneNode(true));
                        decoy.focus();
                    }};
                    window.detached = null;
                    document.querySelector('form').onsubmit = event => {{
                        event.preventDefault();
                        window.submitted = true;
                    }};
                    document.oninput = event => window.events.push({{
                        id: event.target.id, value: event.target.value ?? event.target.textContent,
                        trusted: event.isTrusted
                    }});
                    decoy.focus();
                    decoy.setSelectionRange(6, 6);
                    if ('{phase}' === 'selection') {{
                        const box = target.getBoundingClientRect.bind(target);
                        target.getBoundingClientRect = () => {{
                            const rect = box();
                            queueMicrotask(replace);
                            return rect;
                        }};
                    }}
                    if ('{phase}' === 'focus-detach') target.onfocus = replace;
                    if ('{phase}' === 'focus-redirect') target.onfocus = () => decoy.focus();
                    if ('{phase}' === 'select-redirect') target.select = () => decoy.focus();
                    if ('{phase}' === 'focus-readonly') target.onfocus = () => target.readOnly = true;
                    if ('{phase}' === 'input-detach') target.oninput = replace;
                    if ('{phase}' === 'exception') target.focus = () => {{ throw new Error('page failure'); }};
                }})()"#
            );
            assert_success(
                &execute_command(
                    &json!({ "id": "fixture", "action": "evaluate", "script": script }),
                    &mut state,
                )
                .await,
            );
            let mut command = json!({
                "id": "login", "action": "auth_login", "name": profile,
                "noNavigate": true, "timeout": 200
            });
            if phase.ends_with("textarea") || phase.ends_with("contenteditable") {
                command["usernameSelector"] = json!("#user");
                command["passwordSelector"] = json!("#pass");
            }
            let login = execute_command(&command, &mut state).await;
            let observation = execute_command(
                &json!({
                    "id": "observe", "action": "evaluate",
                    "script": "({ user: document.getElementById('user').value ?? document.getElementById('user').textContent, pass: document.getElementById('pass').value ?? document.getElementById('pass').textContent, decoy: document.getElementById('decoy').value, events: window.events, hiddenUser: document.getElementById('hidden-user').value, hiddenPass: document.getElementById('hidden-pass').value, hiddenClicked: window.hiddenClicked, visibleClicked: window.visibleClicked, submitted: window.submitted, detached: !!window.detached && !window.detached.isConnected })"
                }),
                &mut state,
            )
            .await;
            results.push((field, phase, login, observation));
        }
    }
    for command in [
        json!({ "id": "delete", "action": "auth_delete", "name": profile }),
        json!({ "id": "close", "action": "close" }),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    server.abort();
    for (field, phase, login, observation) in results {
        assert_success(&observation);
        let observed = &get_data(&observation)["result"];
        let normal = ["normal", "textarea", "contenteditable"].contains(&phase)
            || phase.starts_with("no-native-edit");
        assert_eq!(observed["decoy"], "decoy:", "{field}/{phase}: {observed}");
        assert_eq!(observed["hiddenUser"], "", "{field}/{phase}: {observed}");
        assert_eq!(observed["hiddenPass"], "", "{field}/{phase}: {observed}");
        assert_eq!(
            observed["hiddenClicked"], false,
            "{field}/{phase}: {observed}"
        );
        assert_eq!(
            observed["visibleClicked"], normal,
            "{field}/{phase}: {observed}"
        );
        assert_eq!(login["success"], normal, "{field}/{phase}: {login}");
        assert_eq!(observed["submitted"], normal, "{field}/{phase}: {observed}");
        if phase == "selection" || phase == "focus-detach" || phase == "input-detach" {
            assert_eq!(observed["detached"], true, "{field}/{phase}: {observed}");
        }
        for event in observed["events"].as_array().unwrap() {
            let expected = match event["id"].as_str().unwrap() {
                "user" => "target@example.test",
                "pass" => "target-password",
                other => panic!("{field}/{phase}: input delivered to {other}: {event}"),
            };
            assert_eq!(event["value"], expected, "{field}/{phase}: {event}");
            assert_eq!(
                event["trusted"],
                !phase.starts_with("no-native-edit"),
                "{field}/{phase}: {event}"
            );
        }
        let expected_user = if normal || field == "pass" || phase == "input-detach" {
            "target@example.test"
        } else {
            ""
        };
        let expected_pass = if normal || (field == "pass" && phase == "input-detach") {
            "target-password"
        } else {
            ""
        };
        assert_eq!(
            observed["user"], expected_user,
            "{field}/{phase}: {observed}"
        );
        assert_eq!(
            observed["pass"], expected_pass,
            "{field}/{phase}: {observed}"
        );
    }
}

#[tokio::test]
#[ignore]
async fn e2e_auth_login_no_navigate_preserves_active_page_state() {
    let (base_url, document_requests, _server) = start_stateful_auth_login_server().await;
    let mut state = DaemonState::new();
    let profile_name = unique_auth_profile_name("no-navigate");

    assert_success(
        &execute_command(
            &json!({ "id": "1", "action": "launch", "headless": true }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "2", "action": "navigate", "url": format!("{}/flow/start?state=ready#login", base_url) }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "3", "action": "click", "selector": "#reveal" }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "4", "action": "evaluate", "script": "window.__marker = 'preserved'" }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({
                "id": "5",
                "action": "auth_save",
                "name": profile_name.clone(),
                "url": format!("{}/credentials/login?credential-query-sentinel#credential-fragment-sentinel", base_url),
                "username": "stateful-user@example.com",
                "password": "stateful-password-secret",
            }),
            &mut state,
        )
        .await,
    );
    let request_count_before = document_requests.load(Ordering::SeqCst);

    let login = execute_command(
        &json!({ "id": "6", "action": "auth_login", "name": profile_name.clone(), "noNavigate": true }),
        &mut state,
    )
    .await;
    assert_success(&login);
    assert_eq!(get_data(&login)["loggedIn"], true);
    assert_eq!(
        document_requests.load(Ordering::SeqCst),
        request_count_before
    );

    let verify = execute_command(
        &json!({
            "id": "7",
            "action": "evaluate",
            "script": "({ marker: window.__marker, revealed: !!window.__revealed, submitted: !!window.__submitted, user: document.querySelector('input[type=email]').value, pass: document.querySelector('input[type=password]').value })",
        }),
        &mut state,
    )
    .await;
    assert_success(&verify);
    let result = &get_data(&verify)["result"];
    assert_eq!(result["marker"], "preserved");
    assert_eq!(result["revealed"], true);
    assert_eq!(result["submitted"], true);
    assert_eq!(result["user"], "stateful-user@example.com");
    assert_eq!(result["pass"], "stateful-password-secret");

    let _ = execute_command(
        &json!({ "id": "8", "action": "auth_delete", "name": profile_name }),
        &mut state,
    )
    .await;
    assert_success(&execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await);
}

#[cfg(unix)]
#[tokio::test]
#[ignore]
async fn e2e_auth_login_no_navigate_preserves_state_with_credential_provider() {
    use std::os::unix::fs::PermissionsExt;

    let (base_url, document_requests, _server) = start_stateful_auth_login_server().await;
    let plugin_dir = tempfile::tempdir().unwrap();
    let plugin_path = plugin_dir.path().join("stateful-credential-provider");
    std::fs::write(
        &plugin_path,
        format!(
            r#"#!/bin/sh
cat >/dev/null
printf '%s' '{{"protocol":"agent-browser.plugin.v1","success":true,"credential":{{"username":"provider-user@example.com","password":"provider-password-secret","url":"{}/provider/login"}}}}'
"#,
            base_url
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&plugin_path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&plugin_path, permissions).unwrap();

    let mut state = DaemonState::new();
    assert_success(
        &execute_command(
            &json!({ "id": "1", "action": "launch", "headless": true }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "2", "action": "navigate", "url": format!("{}/flow/provider", base_url) }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "3", "action": "click", "selector": "#reveal" }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "4", "action": "evaluate", "script": "window.__marker = 'provider-preserved'" }),
            &mut state,
        )
        .await,
    );
    let request_count_before = document_requests.load(Ordering::SeqCst);

    let login = execute_command(
        &json!({
            "id": "5",
            "action": "auth_login",
            "name": "provider-stateful-login",
            "noNavigate": true,
            "credentialProvider": "mock",
            "plugins": [{
                "name": "mock",
                "command": plugin_path.to_string_lossy(),
                "capabilities": ["credential.read"]
            }]
        }),
        &mut state,
    )
    .await;
    assert_success(&login);
    let serialized_login = serde_json::to_string(&login).unwrap();
    assert!(!serialized_login.contains("provider-user@example.com"));
    assert!(!serialized_login.contains("provider-password-secret"));
    assert_eq!(
        document_requests.load(Ordering::SeqCst),
        request_count_before
    );

    let verify = execute_command(
        &json!({
            "id": "6",
            "action": "evaluate",
            "script": "({ marker: window.__marker, submitted: !!window.__submitted, user: document.querySelector('input[type=email]').value, pass: document.querySelector('input[type=password]').value })",
        }),
        &mut state,
    )
    .await;
    assert_success(&verify);
    let result = &get_data(&verify)["result"];
    assert_eq!(result["marker"], "provider-preserved");
    assert_eq!(result["submitted"], true);
    assert_eq!(result["user"], "provider-user@example.com");
    assert_eq!(result["pass"], "provider-password-secret");

    assert_success(&execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await);
}

#[tokio::test]
#[ignore]
async fn e2e_auth_login_rejects_cross_origin_no_navigate() {
    let (active_origin, _, _active_server) = start_stateful_auth_login_server().await;
    let (credential_origin, credential_requests, _credential_server) =
        start_stateful_auth_login_server().await;
    let mut state = DaemonState::new();
    let profile_name = unique_auth_profile_name("origin-mismatch");

    assert_success(
        &execute_command(
            &json!({ "id": "1", "action": "launch", "headless": true }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "2", "action": "navigate", "url": format!("{}/flow/current-path-sentinel", active_origin) }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({ "id": "3", "action": "click", "selector": "#reveal" }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({
                "id": "4",
                "action": "auth_save",
                "name": profile_name.clone(),
                "url": format!("{}/credential-path-sentinel?credential-query-sentinel#credential-fragment-sentinel", credential_origin),
                "username": "username-secret-sentinel",
                "password": "password-secret-sentinel",
            }),
            &mut state,
        )
        .await,
    );

    let login = execute_command(
        &json!({ "id": "5", "action": "auth_login", "name": profile_name.clone(), "noNavigate": true }),
        &mut state,
    )
    .await;
    assert_eq!(login["success"], false);
    let error = login["error"].as_str().unwrap_or_default();
    assert!(error.contains("does not match credential origin"));
    for sentinel in [
        "current-path-sentinel",
        "credential-path-sentinel",
        "credential-query-sentinel",
        "credential-fragment-sentinel",
        "username-secret-sentinel",
        "password-secret-sentinel",
    ] {
        assert!(!error.contains(sentinel));
    }
    assert_eq!(credential_requests.load(Ordering::SeqCst), 0);

    let verify = execute_command(
        &json!({
            "id": "6",
            "action": "evaluate",
            "script": "({ user: document.querySelector('input[type=email]').value, pass: document.querySelector('input[type=password]').value, submitted: !!window.__submitted })",
        }),
        &mut state,
    )
    .await;
    assert_success(&verify);
    let result = &get_data(&verify)["result"];
    assert_eq!(result["user"], "");
    assert_eq!(result["pass"], "");
    assert_eq!(result["submitted"], false);

    let _ = execute_command(
        &json!({ "id": "7", "action": "auth_delete", "name": profile_name }),
        &mut state,
    )
    .await;
    assert_success(&execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await);
}

#[tokio::test]
#[ignore]
async fn e2e_auth_login_no_navigate_requires_usable_active_page() {
    let mut state = DaemonState::new();
    let profile_name = unique_auth_profile_name("missing-page");

    assert_success(
        &execute_command(
            &json!({
                "id": "1",
                "action": "auth_save",
                "name": profile_name.clone(),
                "url": "https://example.com/login",
                "username": "unused-user",
                "password": "unused-password",
            }),
            &mut state,
        )
        .await,
    );
    let login = execute_command(
        &json!({ "id": "2", "action": "auth_login", "name": profile_name.clone(), "noNavigate": true }),
        &mut state,
    )
    .await;
    assert_eq!(login["success"], false);
    assert!(login["error"]
        .as_str()
        .unwrap_or_default()
        .contains("requires an existing active HTTP(S) browser page"));
    assert!(
        state.browser.is_none(),
        "no-navigation login must not launch a browser"
    );

    let _ = execute_command(
        &json!({ "id": "3", "action": "auth_delete", "name": profile_name }),
        &mut state,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn e2e_auth_login_waits_for_delayed_spa_form_render() {
    let (base_url, _server) = start_delayed_login_server(800, 100).await;
    let mut state = DaemonState::new();

    let profile_name = format!(
        "e2e-auth-login-spa-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_else(|_| std::time::Duration::from_secs(0))
            .as_millis()
    );

    let launch = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&launch);

    let save = execute_command(
        &json!({
            "id": "2",
            "action": "auth_save",
            "name": profile_name.clone(),
            "url": format!("{}/login", base_url),
            "username": "user@example.com",
            "password": "super-secret",
        }),
        &mut state,
    )
    .await;
    assert_success(&save);

    let login = execute_command(
        &json!({ "id": "3", "action": "auth_login", "name": profile_name.clone() }),
        &mut state,
    )
    .await;
    assert_success(&login);
    assert_eq!(get_data(&login)["loggedIn"], true);

    let current_url = execute_command(&json!({ "id": "3b", "action": "url" }), &mut state).await;
    assert_success(&current_url);
    assert!(get_data(&current_url)["url"]
        .as_str()
        .unwrap_or_default()
        .starts_with(&format!("{}/login", base_url)));

    let verify = execute_command(
        &json!({
            "id": "4",
            "action": "evaluate",
            "script": "({ user: document.querySelector('input[type=email]')?.value ?? '', pass: document.querySelector('input[type=password]')?.value ?? '', search: document.querySelector('#search')?.value ?? '', submitted: !!window.__submitted })",
        }),
        &mut state,
    )
    .await;
    assert_success(&verify);
    let result = &get_data(&verify)["result"];
    assert_eq!(result["user"], "user@example.com");
    assert_eq!(result["pass"], "super-secret");
    assert_eq!(result["search"], "");
    assert_eq!(result["submitted"], true);

    let _ = execute_command(
        &json!({ "id": "5", "action": "auth_delete", "name": profile_name }),
        &mut state,
    )
    .await;

    let close = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&close);
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

// ---------------------------------------------------------------------------
// Regression: externally opened tabs must appear in tab_list (#1037)
//
// When connected to Chrome (launched or via --cdp), a tab opened outside of
// agent-browser (e.g. by the user or another CDP client) should be detected
// and listed. Previously, chrome://newtab/ was filtered by
// is_internal_chrome_target, and Target.targetInfoChanged for untracked
// targets was silently ignored.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_externally_opened_tab_detected() {
    let mut state = DaemonState::new();

    // Launch headless Chrome
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Verify initial tab count
    let resp = execute_command(&json!({ "id": "2", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let initial_count = get_data(&resp)["tabs"].as_array().unwrap().len();

    // Simulate an external client opening a new tab via the browser-level CDP
    // session (no sessionId). This mirrors what happens when a user manually
    // opens a tab while agent-browser is connected via --cdp.
    let browser = state.browser.as_ref().expect("browser should be launched");
    let _: Value = browser
        .client
        .send_command(
            "Target.createTarget",
            Some(json!({ "url": "data:text/html,<h1>External Tab</h1>" })),
            None, // browser-level session
        )
        .await
        .expect("Target.createTarget should succeed");

    // Give Chrome a moment to fire targetCreated / targetInfoChanged events
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Drain events by issuing tab_list — this triggers execute_command's
    // drain_cdp_events path which processes new and changed targets.
    let resp = execute_command(&json!({ "id": "3", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();

    assert_eq!(
        tabs.len(),
        initial_count + 1,
        "Externally opened tab should appear in tab_list, got: {:?}",
        tabs,
    );

    // Verify the new tab's URL is the data URL we navigated to
    let new_tab = tabs.iter().find(|t| {
        t["url"]
            .as_str()
            .is_some_and(|u| u.starts_with("data:text/html"))
    });
    assert!(
        new_tab.is_some(),
        "Should find the externally opened tab by URL, tabs: {:?}",
        tabs,
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Regression: issue #993 — launch options change must trigger relaunch
// ---------------------------------------------------------------------------

/// When the browser is already running and a second launch command arrives with
/// different options (e.g., extensions added), the daemon must relaunch the
/// browser instead of silently reusing the old one.
///
/// Before the fix, `handle_launch` only checked connection type and liveness,
/// so changed options like extensions were ignored and the old browser was reused.
#[tokio::test]
#[ignore]
async fn e2e_relaunch_on_options_change() {
    let mut state = DaemonState::new();

    // First launch — headless, no extensions.
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["launched"], true);
    assert!(
        get_data(&resp).get("reused").is_none(),
        "first launch must not be a reuse"
    );

    // Second launch — same options → should reuse.
    let resp = execute_command(
        &json!({ "id": "2", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["reused"],
        true,
        "identical options must reuse the browser"
    );

    // Third launch — different options (userAgent changed) → must relaunch, not reuse.
    // We use userAgent instead of extensions because extensions force headed mode,
    // which requires a display server and fails in headless CI environments.
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "launch",
            "headless": true,
            "userAgent": "agent-browser-test/1.0"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(
        get_data(&resp).get("reused").is_none(),
        "changed options must trigger a relaunch, not reuse (issue #993)"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Stream: URL events follow active main-frame navigation
// ---------------------------------------------------------------------------

async fn start_stream_navigation_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stream navigation server should bind");
    let port = listener
        .local_addr()
        .expect("stream navigation server should have an address")
        .port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let Ok(size) = stream.read(&mut buffer).await else {
                    return;
                };
                let request = String::from_utf8_lossy(&buffer[..size]);
                let request_target = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");
                let path = request_target.split('?').next().unwrap_or("/");
                if let Some(destination) = path.strip_prefix("/redirect/") {
                    let location = format!("/landed/{destination}");
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    return;
                }
                let body = match path {
                    "/child" => {
                        "<!doctype html><title>child</title><p id=\"child\">child</p>"
                    }
                    _ => {
                        "<!doctype html><title>main</title><a id=\"anchor\" href=\"#section\">anchor</a><div id=\"section\">section</div><iframe id=\"child\" src=\"/child\"></iframe>"
                    }
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

async fn next_stream_url(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> String {
    loop {
        let message = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next())
            .await
            .expect("stream should emit a URL message")
            .expect("stream should stay open")
            .expect("stream message should be valid");
        if !message.is_text() {
            continue;
        }
        let payload: Value =
            serde_json::from_str(message.to_text().expect("message should be text"))
                .expect("stream payload should be JSON");
        if payload["type"] == "url" {
            return payload["url"]
                .as_str()
                .expect("URL message should carry a URL")
                .to_string();
        }
    }
}

async fn wait_for_stream_navigation_ready(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    loop {
        let message = tokio::time::timeout(tokio::time::Duration::from_secs(10), ws.next())
            .await
            .expect("stream navigation observer should become ready")
            .expect("stream should stay open")
            .expect("stream message should be valid");
        if !message.is_text() {
            continue;
        }
        let payload: Value =
            serde_json::from_str(message.to_text().expect("message should be text"))
                .expect("stream payload should be JSON");
        if payload["type"] == "status"
            && payload["connected"] == true
            && payload["screencasting"] == true
        {
            return;
        }
    }
}

async fn expect_no_stream_url(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    duration: tokio::time::Duration,
) {
    let deadline = tokio::time::Instant::now() + duration;
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let result = tokio::time::timeout(remaining, ws.next()).await;
        let Ok(Some(Ok(message))) = result else {
            return;
        };
        if !message.is_text() {
            continue;
        }
        let payload: Value =
            serde_json::from_str(message.to_text().expect("message should be text"))
                .expect("stream payload should be JSON");
        assert_ne!(
            payload["type"], "url",
            "unexpected background or child URL: {payload}"
        );
    }
}

async fn next_collected_stream_message(
    messages: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    iteration: usize,
    phase: &str,
) -> Value {
    tokio::time::timeout(tokio::time::Duration::from_secs(5), messages.recv())
        .await
        .unwrap_or_else(|_| panic!("stress iteration {iteration} timed out during {phase}"))
        .unwrap_or_else(|| {
            panic!("stress iteration {iteration} stream collector stopped during {phase}")
        })
}

async fn wait_for_active_stream_tab(
    messages: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    tab_id: &str,
    iteration: usize,
) {
    loop {
        let payload =
            next_collected_stream_message(messages, iteration, "active-tab stream rebind").await;
        if payload["type"] != "tabs" {
            continue;
        }
        let is_active = payload["tabs"].as_array().is_some_and(|tabs| {
            tabs.iter()
                .any(|tab| tab["tabId"] == tab_id && tab["active"].as_bool() == Some(true))
        });
        if is_active {
            return;
        }
    }
}

async fn wait_for_stress_stream_url(
    messages: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    expected_url: &str,
    forbidden_marker: &str,
    iteration: usize,
) {
    loop {
        let payload =
            next_collected_stream_message(messages, iteration, "active URL convergence").await;
        if payload["type"] != "url" {
            continue;
        }
        let url = payload["url"]
            .as_str()
            .expect("stress URL payload should contain a URL");
        assert!(
            !url.contains(forbidden_marker),
            "stress iteration {iteration} attributed the previous tab URL to the new active tab: {payload}"
        );
        if url == expected_url {
            return;
        }
    }
}

async fn expect_no_forbidden_stream_url(
    messages: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    forbidden_marker: &str,
    iteration: usize,
) {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(100);
    while tokio::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let Ok(Some(payload)) = tokio::time::timeout(remaining, messages.recv()).await else {
            return;
        };
        if payload["type"] != "url" {
            continue;
        }
        let url = payload["url"]
            .as_str()
            .expect("stress URL payload should contain a URL");
        assert!(
            !url.contains(forbidden_marker),
            "stress iteration {iteration} emitted a forbidden URL after convergence: {payload}"
        );
    }
}

async fn seeded_stream_tabs(port: u64, iteration: usize) -> Vec<Value> {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("stress probe should connect to runtime stream");
    loop {
        let message = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next())
            .await
            .unwrap_or_else(|_| {
                panic!("stress iteration {iteration} probe timed out waiting for tabs")
            })
            .expect("stress probe stream should stay open")
            .expect("stress probe message should be valid");
        if !message.is_text() {
            continue;
        }
        let payload: Value =
            serde_json::from_str(message.to_text().expect("probe message should be text"))
                .expect("probe stream payload should be JSON");
        if payload["type"] == "tabs" {
            return payload["tabs"]
                .as_array()
                .expect("probe tabs payload should be an array")
                .clone();
        }
    }
}

#[tokio::test]
#[ignore]
async fn e2e_stream_url_tracks_active_main_frame_navigation_categories() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
    let socket_dir = std::env::temp_dir().join(format!(
        "agent-browser-e2e-stream-url-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&socket_dir).expect("socket dir should be created");
    guard.set(
        "AGENT_BROWSER_SOCKET_DIR",
        socket_dir.to_str().expect("socket dir should be utf-8"),
    );
    guard.set("AGENT_BROWSER_SESSION", "e2e-stream-url");

    let (base_url, server) = start_stream_navigation_server().await;
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "stream_enable", "port": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let port = get_data(&resp)["port"]
        .as_u64()
        .expect("stream enable should report the bound port");

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": format!("{base_url}/") }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("websocket client should connect to runtime stream");
    wait_for_stream_navigation_ready(&mut ws).await;

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "evaluate",
            "script": "history.pushState({}, '', '/spa'); location.href"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(next_stream_url(&mut ws).await, format!("{base_url}/spa"));

    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "evaluate",
            "script": "location.hash = 'section'; location.href"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        next_stream_url(&mut ws).await,
        format!("{base_url}/spa#section")
    );

    let resp = execute_command(
        &json!({
            "id": "5",
            "action": "evaluate",
            "script": "document.querySelector('#child').contentWindow.history.pushState({}, '', '/child-spa'); 'done'"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    expect_no_stream_url(&mut ws, tokio::time::Duration::from_millis(500)).await;

    let resp = execute_command(
        &json!({ "id": "6", "action": "navigate", "url": format!("{base_url}/full") }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(next_stream_url(&mut ws).await, format!("{base_url}/full"));

    let resp = execute_command(
        &json!({
            "id": "7",
            "action": "tab_new",
            "url": format!("{base_url}/")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let background_session = state
        .browser
        .as_ref()
        .expect("browser should exist")
        .pages_list()
        .into_iter()
        .find(|page| page.tab_id == 2)
        .expect("background tab should exist")
        .session_id;
    let resp = execute_command(
        &json!({ "id": "8", "action": "tab_switch", "tabId": "t1" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    while tokio::time::timeout(tokio::time::Duration::from_millis(100), ws.next())
        .await
        .is_ok()
    {}

    let resp = execute_command(
        &json!({
            "id": "9",
            "action": "evaluate",
            "script": "history.pushState({}, '', '/after-switch'); location.href"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        next_stream_url(&mut ws).await,
        format!("{base_url}/after-switch")
    );

    state
        .browser
        .as_ref()
        .expect("browser should exist")
        .client
        .send_command(
            "Page.navigate",
            Some(json!({ "url": format!("{base_url}/background-full") })),
            Some(&background_session),
        )
        .await
        .expect("background tab should navigate");
    expect_no_stream_url(&mut ws, tokio::time::Duration::from_millis(500)).await;

    state
        .browser
        .as_ref()
        .expect("browser should exist")
        .client
        .send_command(
            "Target.createTarget",
            Some(json!({ "url": format!("{base_url}/external") })),
            None,
        )
        .await
        .expect("external tab should open");
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    let resp = execute_command(&json!({ "id": "10", "action": "url" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], format!("{base_url}/external"));

    let external_session = state
        .browser
        .as_ref()
        .expect("browser should exist")
        .active_session_id()
        .expect("external tab should become active")
        .to_string();
    state
        .browser
        .as_ref()
        .expect("browser should exist")
        .client
        .send_command(
            "Runtime.evaluate",
            Some(json!({
                "expression": "history.pushState({}, '', '/external-spa'); location.href"
            })),
            Some(&external_session),
        )
        .await
        .expect("external tab should navigate within its document");
    assert_eq!(
        next_stream_url(&mut ws).await,
        format!("{base_url}/external-spa")
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
#[ignore]
async fn e2e_lightpanda_stream_url_tracks_active_full_navigation() {
    let lightpanda_bin = match std::env::var("LIGHTPANDA_BIN") {
        Ok(path) if !path.is_empty() => path,
        _ => return,
    };
    let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
    let socket_dir = std::env::temp_dir().join(format!(
        "agent-browser-e2e-lightpanda-stream-url-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&socket_dir).expect("socket dir should be created");
    guard.set(
        "AGENT_BROWSER_SOCKET_DIR",
        socket_dir.to_str().expect("socket dir should be utf-8"),
    );
    guard.set("AGENT_BROWSER_SESSION", "e2e-lightpanda-stream-url");

    let (base_url, server) = start_stream_navigation_server().await;
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "lp-stream", "action": "stream_enable", "port": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let port = get_data(&resp)["port"]
        .as_u64()
        .expect("stream enable should report the bound port");

    let resp = tokio::time::timeout(
        tokio::time::Duration::from_secs(20),
        execute_command(
            &json!({
                "id": "lp-launch",
                "action": "launch",
                "headless": true,
                "engine": "lightpanda",
                "executablePath": lightpanda_bin
            }),
            &mut state,
        ),
    )
    .await
    .expect("Lightpanda stream launch should not hang");
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "lp-initial",
            "action": "navigate",
            "url": format!("{base_url}/lp-initial")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("Lightpanda stream client should connect");
    while tokio::time::timeout(tokio::time::Duration::from_millis(100), ws.next())
        .await
        .is_ok()
    {}

    let active_before_background = format!("{base_url}/lp-active-before-background");
    let resp = execute_command(
        &json!({
            "id": "lp-active-before",
            "action": "navigate",
            "url": active_before_background
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(next_stream_url(&mut ws).await, active_before_background);

    let resp = execute_command(
        &json!({
            "id": "lp-child-navigation",
            "action": "evaluate",
            "script": "document.querySelector('#child').src = '/lp-forbidden-child'; 'done'"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    expect_no_stream_url(&mut ws, tokio::time::Duration::from_millis(500)).await;

    let tabs = seeded_stream_tabs(port, 0).await;
    let active = tabs
        .iter()
        .find(|tab| tab["active"].as_bool() == Some(true))
        .expect("Lightpanda reconnect should seed an active tab");
    assert_eq!(active["tabId"], "t1");
    assert_eq!(active["url"], active_before_background);

    let final_url = format!("{base_url}/lp-active-final");
    let resp = execute_command(
        &json!({
            "id": "lp-active-final",
            "action": "navigate",
            "url": final_url
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(next_stream_url(&mut ws).await, final_url);

    let resp = execute_command(&json!({ "id": "lp-close", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();
    let _ = std::fs::remove_dir_all(&socket_dir);
}

#[tokio::test]
#[ignore]
async fn e2e_stream_url_survives_real_world_navigation_stress() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
    let socket_dir = std::env::temp_dir().join(format!(
        "agent-browser-e2e-stream-url-stress-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&socket_dir).expect("socket dir should be created");
    guard.set(
        "AGENT_BROWSER_SOCKET_DIR",
        socket_dir.to_str().expect("socket dir should be utf-8"),
    );
    guard.set("AGENT_BROWSER_SESSION", "e2e-stream-url-stress");

    let iterations = std::env::var("AGENT_BROWSER_STRESS_ITERATIONS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(40);
    let mut seed = std::env::var("AGENT_BROWSER_STRESS_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1677);

    let (base_url, server) = start_stream_navigation_server().await;
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "stress-stream", "action": "stream_enable", "port": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let port = get_data(&resp)["port"]
        .as_u64()
        .expect("stream enable should report the bound port");

    let resp = execute_command(
        &json!({
            "id": "stress-tab-a",
            "action": "navigate",
            "url": format!("{base_url}/app-a")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({
            "id": "stress-tab-b",
            "action": "tab_new",
            "url": format!("{base_url}/app-b")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let pages = state
        .browser
        .as_ref()
        .expect("stress browser should exist")
        .pages_list();
    let session_a = pages
        .iter()
        .find(|page| page.tab_id == 1)
        .expect("stress tab A should exist")
        .session_id
        .clone();
    let session_b = pages
        .iter()
        .find(|page| page.tab_id == 2)
        .expect("stress tab B should exist")
        .session_id
        .clone();
    let client = Arc::clone(
        &state
            .browser
            .as_ref()
            .expect("stress browser should exist")
            .client,
    );

    let (fast_ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("stress collector should connect to runtime stream");
    let (message_tx, mut messages) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let collector = tokio::spawn(async move {
        let mut ws = fast_ws;
        while let Some(Ok(message)) = ws.next().await {
            if !message.is_text() {
                continue;
            }
            let Ok(payload) = serde_json::from_str::<Value>(
                message.to_text().expect("collector message should be text"),
            ) else {
                continue;
            };
            if message_tx.send(payload).is_err() {
                break;
            }
        }
    });
    wait_for_active_stream_tab(&mut messages, "t2", 0).await;

    let (mut slow_ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("slow stress client should connect to runtime stream");

    let mut active_tab = "t2";
    for iteration in 0..iterations {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let delay_ms = 1 + (seed % 12);
        let target_tab = if active_tab == "t1" { "t2" } else { "t1" };
        let old_session = if active_tab == "t1" {
            session_a.clone()
        } else {
            session_b.clone()
        };
        let forbidden_marker = format!("forbidden-{iteration}");
        let same_document_path = format!("/{forbidden_marker}-old-spa");
        let redirect_url = format!("{base_url}/redirect/{forbidden_marker}-old-full");

        let late_client = Arc::clone(&client);
        let late_session = old_session.clone();
        let late_same_document_path = same_document_path.clone();
        let late_child_marker = forbidden_marker.clone();
        let late_same_document = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            let expression = format!(
                "history.pushState({{}}, '', {}); const child = document.querySelector('#child'); if (child?.contentWindow) child.contentWindow.history.pushState({{}}, '', '/{late_child_marker}-old-child'); location.href",
                serde_json::to_string(&late_same_document_path)
                    .expect("stress path should serialize")
            );
            late_client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({ "expression": expression })),
                    Some(&late_session),
                )
                .await
        });

        let redirect_client = Arc::clone(&client);
        let redirect_session = old_session.clone();
        let late_redirect = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms + 1)).await;
            redirect_client
                .send_command(
                    "Page.navigate",
                    Some(json!({ "url": redirect_url })),
                    Some(&redirect_session),
                )
                .await
        });

        let resp = execute_command(
            &json!({
                "id": format!("stress-switch-{iteration}"),
                "action": "tab_switch",
                "tabId": target_tab
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        wait_for_active_stream_tab(&mut messages, target_tab, iteration).await;
        tokio::time::timeout(tokio::time::Duration::from_secs(5), late_same_document)
            .await
            .unwrap_or_else(|_| panic!("stress iteration {iteration} late SPA task timed out"))
            .expect("late SPA task should join")
            .expect("late SPA CDP command should succeed");
        tokio::time::timeout(tokio::time::Duration::from_secs(5), late_redirect)
            .await
            .unwrap_or_else(|_| panic!("stress iteration {iteration} late redirect task timed out"))
            .expect("late redirect task should join")
            .expect("late redirect CDP command should succeed");

        let resp = execute_command(
            &json!({
                "id": format!("stress-child-{iteration}"),
                "action": "evaluate",
                "script": format!(
                    "const child = document.querySelector('#child'); if (!child?.contentWindow) throw new Error('missing stress iframe'); child.contentWindow.history.pushState({{}}, '', '/{forbidden_marker}-active-child'); 'done'"
                )
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let expected_url = format!("{base_url}/{target_tab}-active-{iteration}");
        let resp = execute_command(
            &json!({
                "id": format!("stress-active-{iteration}"),
                "action": "evaluate",
                "script": format!(
                    "history.replaceState({{}}, '', {}); location.href",
                    serde_json::to_string(&expected_url).expect("stress URL should serialize")
                )
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        wait_for_stress_stream_url(&mut messages, &expected_url, &forbidden_marker, iteration)
            .await;

        let resp = execute_command(
            &json!({ "id": format!("stress-url-{iteration}"), "action": "url" }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(
            get_data(&resp)["url"],
            expected_url,
            "stress iteration {iteration} active browser URL diverged"
        );
        expect_no_forbidden_stream_url(&mut messages, &forbidden_marker, iteration).await;

        if iteration % 5 == 0 {
            let tabs = seeded_stream_tabs(port, iteration).await;
            let active = tabs
                .iter()
                .find(|tab| tab["active"].as_bool() == Some(true))
                .unwrap_or_else(|| {
                    panic!("stress iteration {iteration} reconnect had no active tab")
                });
            assert_eq!(
                active["tabId"], target_tab,
                "stress iteration {iteration} reconnect seeded the wrong active tab"
            );
            assert_eq!(
                active["url"], expected_url,
                "stress iteration {iteration} reconnect seeded a stale URL"
            );
        }

        if iteration % 7 == 6 {
            drop(slow_ws);
            let (replacement, _) =
                tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
                    .await
                    .expect("replacement slow stress client should connect");
            slow_ws = replacement;
        }

        active_tab = target_tab;
    }

    drop(slow_ws);
    collector.abort();
    let resp = execute_command(
        &json!({ "id": "stress-close", "action": "close" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    server.abort();
    let _ = std::fs::remove_dir_all(&socket_dir);
}

// ---------------------------------------------------------------------------
// Stream: custom viewport is reflected in screencast frame metadata
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_stream_frame_metadata_respects_custom_viewport() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
    let socket_dir = std::env::temp_dir().join(format!(
        "agent-browser-e2e-stream-viewport-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&socket_dir).expect("socket dir should be created");
    guard.set(
        "AGENT_BROWSER_SOCKET_DIR",
        socket_dir.to_str().expect("socket dir should be utf-8"),
    );
    guard.set("AGENT_BROWSER_SESSION", "e2e-stream-viewport");

    let mut state = DaemonState::new();

    // Enable stream on an ephemeral port
    let resp = execute_command(
        &json!({ "id": "1", "action": "stream_enable", "port": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let port = get_data(&resp)["port"]
        .as_u64()
        .expect("stream enable should report the bound port");

    // Set a custom viewport before launching the browser
    let resp = execute_command(
        &json!({ "id": "2", "action": "viewport", "width": 800, "height": 600 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Connect a WebSocket client
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("websocket client should connect to runtime stream");

    // Navigate to trigger browser launch and screencast
    let resp = execute_command(
        &json!({ "id": "3", "action": "navigate", "url": "data:text/html,<h1>Viewport Test</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Wait for a frame whose JPEG dimensions match the custom viewport.
    // Early frames may arrive before Chrome fully applies the viewport resize,
    // so skip frames with stale dimensions rather than failing immediately.
    let mut found_frame = false;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        let msg = tokio::time::timeout(tokio::time::Duration::from_secs(3), ws.next()).await;
        let Some(Ok(message)) = msg.ok().flatten() else {
            continue;
        };
        if !message.is_text() {
            continue;
        }
        let parsed: Value =
            serde_json::from_str(message.to_text().expect("text message should be readable"))
                .expect("stream payload should be valid JSON");
        if parsed.get("type") == Some(&json!("frame")) {
            let meta = &parsed["metadata"];
            assert_eq!(
                meta["deviceWidth"], 800,
                "frame metadata deviceWidth should match custom viewport, got: {}",
                meta
            );
            assert_eq!(
                meta["deviceHeight"], 600,
                "frame metadata deviceHeight should match custom viewport, got: {}",
                meta
            );

            let data_str = parsed
                .get("data")
                .and_then(|v| v.as_str())
                .expect("frame message should include base64-encoded 'data' field");
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data_str)
                .expect("frame data should be valid base64");
            let (img_w, img_h) =
                jpeg_dimensions(&bytes).expect("frame data should be a valid JPEG with SOF marker");
            if img_w != 800 || img_h != 600 {
                continue;
            }

            found_frame = true;
            break;
        }
    }
    assert!(
        found_frame,
        "should have received a frame with JPEG dimensions 800x600 within the deadline"
    );

    // Cleanup
    let resp = execute_command(
        &json!({ "id": "4", "action": "stream_disable" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

// ---------------------------------------------------------------------------
// Stream: a click is not queued behind a mouse sweep
// ---------------------------------------------------------------------------

/// Guards the no-await dispatch. Awaiting Chrome's reply per event serialized
/// the reader, so a click arrived one CDP round trip behind every mousemove
/// ahead of it: 300 moves delayed it by ~2.5s. Measured at ~6ms with the fix,
/// so the threshold has three orders of magnitude of headroom and fails only on
/// a real regression.
#[tokio::test]
#[ignore]
async fn e2e_stream_click_is_not_queued_behind_a_mouse_sweep() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "AGENT_BROWSER_SESSION"]);
    let socket_dir = std::env::temp_dir().join(format!(
        "agent-browser-e2e-stream-latency-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos()
    ));
    std::fs::create_dir_all(&socket_dir).expect("socket dir should be created");
    guard.set(
        "AGENT_BROWSER_SOCKET_DIR",
        socket_dir.to_str().expect("socket dir should be utf-8"),
    );
    guard.set("AGENT_BROWSER_SESSION", "e2e-stream-latency");

    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "stream_enable", "port": 0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let port = get_data(&resp)["port"]
        .as_u64()
        .expect("stream enable should report the bound port");

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>latency</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Record the arrival time of the first mousedown inside the page.
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "evaluate",
            "script": "window.__down = null; document.addEventListener('mousedown', () => { if (window.__down === null) window.__down = Date.now(); }); 'armed'"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .expect("websocket client should connect to runtime stream");
    let _ = tokio::time::timeout(tokio::time::Duration::from_secs(5), ws.next()).await;

    // A sweep of moves, then the click that must not wait for them.
    for i in 0..300 {
        let mv = json!({
            "type": "input_mouse", "eventType": "mouseMoved",
            "x": 100 + (i % 400), "y": 100 + (i % 300),
            "button": "none", "clickCount": 0
        });
        ws.send(Message::Text(mv.to_string()))
            .await
            .expect("mouse move should be accepted by the stream socket");
    }
    let sent_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_millis() as u64;
    for event in ["mousePressed", "mouseReleased"] {
        let click = json!({
            "type": "input_mouse", "eventType": event,
            "x": 200, "y": 200, "button": "left", "clickCount": 1
        });
        ws.send(Message::Text(click.to_string()))
            .await
            .expect("click should be accepted by the stream socket");
    }

    // Poll the page for the recorded arrival time.
    let mut latency_ms: Option<u64> = None;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(20);
    while tokio::time::Instant::now() < deadline && latency_ms.is_none() {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        let resp = execute_command(
            &json!({ "id": "4", "action": "evaluate", "script": "window.__down" }),
            &mut state,
        )
        .await;
        if let Some(down) = get_data(&resp)["result"].as_u64() {
            latency_ms = Some(down.saturating_sub(sent_at_ms));
        }
    }

    let latency = latency_ms.expect("the click should reach the page within the deadline");
    assert!(
        latency < 500,
        "click landed {latency}ms after a 300-event mouse sweep; input dispatch is waiting on CDP replies again"
    );

    let resp = execute_command(
        &json!({ "id": "98", "action": "stream_disable" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    let _ = std::fs::remove_dir_all(&socket_dir);
}

/// Extract width and height from a JPEG's SOF0 (0xFFC0) or SOF2 (0xFFC2) marker.
fn jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    for i in 0..data.len().saturating_sub(8) {
        if data[i] == 0xFF && (data[i + 1] == 0xC0 || data[i + 1] == 0xC2) {
            let height = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let width = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Some((width, height));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Upload: ref-based selector support (issue #1107)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_upload_with_ref_selector() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": native_test_fixture_url("upload_probe") }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "snapshot" }), &mut state).await;
    assert_success(&resp);
    let snapshot = get_data(&resp)["snapshot"].as_str().unwrap();

    // Match by label text, not by role which may vary across Chrome versions
    let file_input_ref = snapshot
        .lines()
        .filter_map(|line| {
            if line.contains("Choose file") && line.contains("ref=") {
                let start = line.find("ref=")? + 4;
                let end = line[start..].find(']')? + start;
                Some(line[start..end].to_string())
            } else {
                None
            }
        })
        .next()
        .expect("Snapshot should contain the file input with a ref");

    let tmp = std::env::temp_dir().join(format!("ab-upload-ref-{}.txt", std::process::id()));
    std::fs::write(&tmp, "test").unwrap();

    let resp = execute_command(
        &json!({ "id": "4", "action": "upload", "selector": file_input_ref, "files": [tmp.to_string_lossy()] }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["uploaded"], 1);

    let _ = std::fs::remove_file(&tmp);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_upload_with_css_selector() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": native_test_fixture_url("upload_probe") }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let tmp = std::env::temp_dir().join(format!("ab-upload-css-{}.txt", std::process::id()));
    std::fs::write(&tmp, "test").unwrap();

    let resp = execute_command(
        &json!({ "id": "3", "action": "upload", "selector": "#fileInput", "files": [tmp.to_string_lossy()] }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["uploaded"], 1);

    let _ = std::fs::remove_file(&tmp);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Recording: default records the current active page
// ---------------------------------------------------------------------------

/// `recording_start` attaches the recorder to the active page
/// as-is: no new browser context, no new tab, no navigation. Page state set
/// before `record start` must survive, and the viewport must be untouched.
#[tokio::test]
#[ignore]
async fn e2e_recording_default_records_active_page() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let page_url = "data:text/html,<h1>Current</h1>";
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": page_url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "viewport", "width": 800, "height": 600 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // In-memory page state: a cold navigation or a new page would lose this.
    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "window.__abMarker = 42; window.__abMarker" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], 42);

    let tmp_dir = std::env::temp_dir();
    let rec_path = tmp_dir.join(format!("ab-e2e-rec-current-{}.webm", std::process::id()));
    let resp = execute_command(
        &json!({ "id": "5", "action": "recording_start", "path": rec_path.to_string_lossy() }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    tokio::time::sleep(tokio::time::Duration::from_millis(700)).await;

    // Still exactly one tab: no recording tab was added.
    let resp = execute_command(&json!({ "id": "6", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(
        tabs.len(),
        1,
        "default record start must not open a new tab, got {tabs:?}"
    );

    // Same page, same JS heap: the marker survived and the URL is unchanged.
    let resp = execute_command(
        &json!({ "id": "7", "action": "evaluate", "script": "window.__abMarker" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"],
        42,
        "default record start must not navigate or replace the page"
    );

    let resp = execute_command(
        &json!({ "id": "8", "action": "evaluate", "script": "location.href" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], page_url);

    let resp = execute_command(
        &json!({ "id": "9", "action": "evaluate", "script": "[window.innerWidth, window.innerHeight]" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], json!([800, 600]));

    let resp = execute_command(
        &json!({ "id": "10", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(
        get_data(&resp)["frames"].as_u64().unwrap_or(0) > 0,
        "recorder attached to the active page should have captured frames"
    );

    let _ = std::fs::remove_file(&rec_path);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// `recording_start` with a URL navigates the active
/// tab to that URL before recording. No new tab is created.
#[tokio::test]
#[ignore]
async fn e2e_recording_default_with_url_navigates_active_tab() {
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
            "url": "data:text/html,<button id='before'>Before</button>"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Refs from the page being replaced must not survive the navigation.
    let resp = execute_command(
        &json!({ "id": "2b", "action": "snapshot", "selector": "#before" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(state.ref_map.get("e1").is_some());

    let target_url = "data:text/html,<h1>Recorded</h1>";
    let tmp_dir = std::env::temp_dir();
    let rec_path = tmp_dir.join(format!("ab-e2e-rec-url-{}.webm", std::process::id()));
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "recording_start",
            "path": rec_path.to_string_lossy(),
            "url": target_url
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(
        state.ref_map.entries_sorted().is_empty(),
        "record start <url> must clear refs like navigate does"
    );

    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let resp = execute_command(&json!({ "id": "4", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap();
    assert_eq!(
        tabs.len(),
        1,
        "record start <url> must reuse the active tab"
    );

    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "location.href" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], target_url);

    let resp = execute_command(
        &json!({ "id": "6", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let _ = std::fs::remove_file(&rec_path);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// Recording: requested frame rate
// ---------------------------------------------------------------------------

/// Verify that a requested frame rate sets the timestamp resolution without
/// filling a static recording with duplicate encoded frames.
#[tokio::test]
#[ignore]
async fn e2e_recording_honors_requested_fps() {
    const FPS: u64 = 60;
    const RECORD_MS: u64 = 1000;

    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<h1>Frame rate</h1>" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let tmp_dir = std::env::temp_dir();
    let rec_path = tmp_dir.join(format!("ab-e2e-rec-fps-{}.webm", std::process::id()));
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "recording_start",
            "path": rec_path.to_string_lossy(),
            "fps": FPS,
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["fps"].as_u64(), Some(FPS));

    // The recorder screencasts on its own CDP session attached to the
    // active page. That attachment must not surface as a tab.
    let resp = execute_command(&json!({ "id": "3b", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap().len();
    assert_eq!(tabs, 1, "the recorded page only, nothing else");

    tokio::time::sleep(tokio::time::Duration::from_millis(RECORD_MS)).await;

    let resp = execute_command(
        &json!({ "id": "4", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(data["fps"].as_u64(), Some(FPS));

    let frames = data["frames"].as_u64().unwrap();
    assert!(
        (FPS * 8 / 10..FPS * 15 / 10).contains(&frames),
        "static page should repeat frames at {FPS} fps, got {frames}"
    );
    let captured = data["capturedFrames"].as_u64().unwrap();
    assert!(captured >= 1, "static page should produce an initial frame");

    let size = std::fs::metadata(&rec_path).map(|m| m.len()).unwrap_or(0);
    assert!(size > 0, "recording file should not be empty");
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(&rec_path)
        .output()
        .expect("ffprobe should inspect the recording");
    assert!(probe.status.success());
    let duration: f64 = String::from_utf8_lossy(&probe.stdout)
        .trim()
        .parse()
        .expect("ffprobe duration should be numeric");
    assert!(
        (0.8..1.5).contains(&duration),
        "recording should retain wall-clock duration, got {duration}"
    );
    assert!(
        (duration - frames as f64 / FPS as f64).abs() < 0.03,
        "{frames} frames at {FPS} fps should match duration {duration}"
    );

    let _ = std::fs::remove_file(&rec_path);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verify that a page whose device scale is emulated records at that scale:
/// Chrome's screencast captures at the window's real scale, so the recorder
/// switches to full-density screenshots, and every frame keeps the detail.
#[tokio::test]
#[ignore]
async fn e2e_recording_emulated_scale_captures_full_density() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "viewport", "width": 640, "height": 400, "deviceScaleFactor": 2.0 }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Something always moving, so the screencast sends frames to compare.
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "navigate",
            "url": "data:text/html,<style>@keyframes s{to{transform:rotate(360deg)}}div{width:80px;height:80px;background:%230a72ef;animation:s 1s linear infinite}</style><h1>Density</h1><div></div>",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let tmp_dir = std::env::temp_dir();
    let rec_path = tmp_dir.join(format!("ab-e2e-rec-density-{}.webm", std::process::id()));
    let resp = execute_command(
        &json!({ "id": "4", "action": "recording_start", "path": rec_path.to_string_lossy() }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    let resp = execute_command(
        &json!({ "id": "5", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(data["capture"], "screenshots", "{data}");
    assert_eq!(data["deviceScaleFactor"], 2.0);
    assert!(
        data.get("warning").is_none(),
        "frames should be full density: {data}"
    );
    let captured = data["capturedFrames"].as_u64().unwrap();
    assert!(
        captured >= 10,
        "an animated page should keep capturing frames, got {captured}"
    );

    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(&rec_path)
        .output()
        .expect("ffprobe should inspect the recording");
    assert!(probe.status.success());
    assert_eq!(String::from_utf8_lossy(&probe.stdout).trim(), "1280,800");

    let _ = std::fs::remove_file(&rec_path);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verify that a mobile-emulated page without a viewport meta tag reports its
/// real device scale: its layout viewport is wider than its screen, so the
/// scale comes from `devicePixelRatio`, not the first frame's width.
#[tokio::test]
#[ignore]
async fn e2e_recording_mobile_emulation_uses_device_pixel_ratio() {
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
            "url": "data:text/html,<style>@keyframes s{to{transform:rotate(360deg)}}div{width:80px;height:80px;background:%230a72ef;animation:s 1s linear infinite}</style><h1>No viewport meta</h1><div></div>",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "device", "name": "iPhone 14" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let tmp_dir = std::env::temp_dir();
    let rec_path = tmp_dir.join(format!("ab-e2e-rec-mobile-{}.webm", std::process::id()));
    let resp = execute_command(
        &json!({ "id": "4", "action": "recording_start", "path": rec_path.to_string_lossy() }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    let resp = execute_command(
        &json!({ "id": "5", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(data["capture"], "screenshots", "{data}");
    assert_eq!(data["deviceScaleFactor"], 3.0, "{data}");

    let _ = std::fs::remove_file(&rec_path);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verify that an output path without an extension is rejected before the
/// recording context exists: no new tab, no file, nothing to stop.
#[tokio::test]
#[ignore]
async fn e2e_recording_rejects_extensionless_path_before_context() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let rec_path = std::env::temp_dir().join(format!("ab-e2e-rec-noext-{}", std::process::id()));
    let resp = execute_command(
        &json!({ "id": "2", "action": "recording_start", "path": rec_path.to_string_lossy() }),
        &mut state,
    )
    .await;
    assert_eq!(resp.get("success").and_then(|v| v.as_bool()), Some(false));
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("no extension"),
        "error should explain the path: {}",
        err
    );
    assert!(!rec_path.exists(), "no file should be created");
    assert!(!state.recording_state.active);

    let resp = execute_command(&json!({ "id": "3", "action": "tab_list" }), &mut state).await;
    assert_success(&resp);
    let tabs = get_data(&resp)["tabs"].as_array().unwrap().len();
    assert_eq!(
        tabs, 1,
        "a rejected path must not leave a recording tab behind"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verify that a missing ffmpeg fails `recording_start` itself before an
/// optional navigation, and leaves the state ready for the next start.
#[tokio::test]
#[ignore]
async fn e2e_recording_fails_fast_without_ffmpeg() {
    let guard = EnvGuard::new(&["PATH"]);
    let original_path = std::env::var("PATH").unwrap_or_default();
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let before_url = "data:text/html,<h1>Before</h1>";
    let after_url = "data:text/html,<h1>After</h1>";
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": before_url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let tmp_dir = std::env::temp_dir();
    let rec_path = tmp_dir.join(format!("ab-e2e-rec-noffmpeg-{}.webm", std::process::id()));

    // A PATH with no ffmpeg on it. Chrome is already running, so nothing else
    // needs resolving.
    let empty_dir = tmp_dir.join(format!("ab-e2e-empty-path-{}", std::process::id()));
    std::fs::create_dir_all(&empty_dir).unwrap();
    guard.set("PATH", &empty_dir.to_string_lossy());

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "recording_start",
            "path": rec_path.to_string_lossy(),
            "url": after_url
        }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "record start must fail without ffmpeg: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(err.contains("ffmpeg"), "error should name ffmpeg: {}", err);
    assert!(
        !state.recording_state.active,
        "failed start must not leave the recording active"
    );
    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "location.href" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"],
        before_url,
        "ffmpeg preflight must fail before the requested navigation"
    );

    // With ffmpeg back, the same start succeeds: nothing stale was left behind.
    guard.set("PATH", &original_path);
    let resp = execute_command(
        &json!({
            "id": "5",
            "action": "recording_start",
            "path": rec_path.to_string_lossy(),
            "url": after_url
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    let resp = execute_command(
        &json!({ "id": "6", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let _ = std::fs::remove_file(&rec_path);
    let _ = std::fs::remove_dir(&empty_dir);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verify that an out-of-range frame rate is rejected before the recorder
/// builds its context or attaches to the page, leaving no file and no active
/// recording behind.
#[tokio::test]
#[ignore]
async fn e2e_recording_rejects_invalid_fps() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let tmp_dir = std::env::temp_dir();
    let rec_path = tmp_dir.join(format!("ab-e2e-rec-badfps-{}.webm", std::process::id()));
    let resp = execute_command(
        &json!({
            "id": "2",
            "action": "recording_start",
            "path": rec_path.to_string_lossy(),
            "fps": 240,
        }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "240 fps should be rejected: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(err.contains("fps"), "error should name the field: {}", err);
    assert!(!rec_path.exists(), "no file should be created");

    // Nothing was started, so there is nothing to stop.
    let resp = execute_command(
        &json!({ "id": "3", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_eq!(resp.get("success").and_then(|v| v.as_bool()), Some(false));

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Verify the composited pointer and changed-frame contact sheet through the
/// full daemon pipeline.
#[tokio::test]
#[ignore]
async fn e2e_recording_cursor_and_contact_sheet() {
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = r#"data:text/html,<style>body{margin:0;background:%23f3f4f6;font-family:sans-serif}.card{margin:80px;padding:48px;background:white;border-radius:24px}button{padding:18px 28px;background:%232563eb;color:white;border:0;border-radius:12px}</style><div class=card><h1>Contact sheet demo</h1><p>Review important visual changes at a glance.</p><button>Continue</button></div>"#;
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": html }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let rec_path = std::env::temp_dir().join(format!(
        "ab-e2e-rec-contact-sheet-{}.webm",
        std::process::id()
    ));
    let sheet_path = rec_path.with_file_name(format!(
        "{}.contact-sheet.png",
        rec_path.file_stem().unwrap().to_string_lossy()
    ));
    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "recording_start",
            "path": rec_path.to_string_lossy(),
            "cursor": true,
            "contactSheet": true,
            "contactSheetThreshold": 0.01
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["cursor"], true);
    assert_eq!(get_data(&resp)["contactSheet"], true);

    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "Boolean(document.getElementById('__agent_browser_recording_cursor__'))" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], false);

    let resp = execute_command(
        &json!({ "id": "5", "action": "mousemove", "x": 360, "y": 260 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let cursor = state
        .recording_state
        .shared_cursor
        .lock()
        .unwrap()
        .at(super::recording::cursor_timestamp());
    assert!(cursor.visible);
    assert_eq!((cursor.x, cursor.y), (360.0, 260.0));
    let resp = execute_command(
        &json!({ "id": "6", "action": "evaluate", "script": "document.querySelector('.card').style.background='#dbeafe'; document.querySelector('h1').textContent='Ready to continue'; true" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(tokio::time::Duration::from_millis(350)).await;
    let resp = execute_command(
        &json!({ "id": "7", "action": "evaluate", "script": "document.querySelector('.card').style.background='#dcfce7'; document.querySelector('p').textContent='The important region is highlighted.'; true" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(tokio::time::Duration::from_millis(350)).await;

    let resp = execute_command(
        &json!({ "id": "8", "action": "recording_stop" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(
        data["contactSheetPath"],
        sheet_path.to_string_lossy().as_ref()
    );
    assert!(data["contactSheetFrames"].as_u64().unwrap_or(0) >= 2);
    assert!(std::fs::metadata(&rec_path).unwrap().len() > 0);
    let sheet = image::open(&sheet_path).expect("contact sheet should be a valid PNG");
    assert!(sheet.width() >= 320);
    assert!(sheet.height() >= 150);
    if let Some(example_path) = std::env::var_os("AGENT_BROWSER_CONTACT_SHEET_EXAMPLE_PATH") {
        let example_path = std::path::PathBuf::from(example_path);
        if let Some(parent) = example_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::copy(&sheet_path, example_path).unwrap();
    }

    let resp = execute_command(
        &json!({ "id": "9", "action": "evaluate", "script": "Boolean(document.getElementById('__agent_browser_recording_cursor__'))" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], false);

    let _ = std::fs::remove_file(&rec_path);
    let _ = std::fs::remove_file(&sheet_path);
    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// The cursor must not intercept clicks, leak into snapshots, or survive stop.
#[tokio::test]
#[ignore]
async fn e2e_recording_cursor_overlay_lifecycle() {
    let mut state = DaemonState::new();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cursor-lifecycle.mp4");
    let url =
        "data:text/html,<button id=keep onclick='window.clicks=(window.clicks||0)+1'>Keep</button>";
    for command in [
        json!({"action":"launch","headless":true}),
        json!({"action":"navigate","url":url}),
        json!({"action":"recording_start","path":path,"cursor":true}),
        json!({"action":"click","selector":"#keep"}),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    let inspected = execute_command(&json!({"action":"evaluate","script":"(() => { const host = document.querySelector('[data-agent-browser-recording-cursor]'); return !!host && host.inert && host.getAttribute('aria-hidden') === 'true' && host.shadowRoot === null && typeof globalThis.__agentBrowserRecordingCursorCleanup === 'undefined' && window.clicks === 1; })()"}), &mut state).await;
    assert_success(&inspected);
    assert_eq!(get_data(&inspected)["result"], true);
    let snapshot = execute_command(&json!({"action":"snapshot"}), &mut state).await;
    assert_success(&snapshot);
    assert!(!get_data(&snapshot)
        .to_string()
        .contains("agent-browser-recording-cursor"));
    assert_success(
        &execute_command(
            &json!({"action":"navigate","url":"data:text/html,<button>After navigation</button>"}),
            &mut state,
        )
        .await,
    );
    let after_navigation = execute_command(&json!({"action":"evaluate","script":"document.querySelectorAll('[data-agent-browser-recording-cursor]').length"}), &mut state).await;
    let stopped = execute_command(&json!({"action":"recording_stop"}), &mut state).await;
    let after_stop = execute_command(&json!({"action":"evaluate","script":"document.querySelectorAll('[data-agent-browser-recording-cursor]').length"}), &mut state).await;
    assert_success(&execute_command(&json!({"action":"navigate","url":url}), &mut state).await);
    let after_next_navigation = execute_command(&json!({"action":"evaluate","script":"document.querySelectorAll('[data-agent-browser-recording-cursor]').length"}), &mut state).await;
    assert_success(&execute_command(&json!({"action":"close"}), &mut state).await);
    assert_success(&stopped);
    assert_eq!(get_data(&after_navigation)["result"], 1);
    assert_eq!(get_data(&after_stop)["result"], 0);
    assert_eq!(get_data(&after_next_navigation)["result"], 0);
}

/// Check every encoded frame while a real page control follows drag input.
#[tokio::test]
#[ignore]
async fn e2e_recording_cursor_stays_aligned_during_drag() {
    let mut state = DaemonState::new();
    for command in [
        json!({ "action": "launch", "headless": true }),
        json!({ "action": "viewport", "width": 640, "height": 480 }),
        json!({
            "action": "navigate",
            "url": "data:text/html,<style>body{margin:0;background:%23303030}div{position:fixed;left:80px;top:0;width:2px;height:100vh;background:%2300ff00}</style><div></div><script>document.addEventListener('pointermove',e=>{if(e.buttons)document.querySelector('div').style.left=e.clientX+'px'})</script>"
        }),
        json!({ "action": "mousemove", "x": 80, "y": 240 }),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("drag-cursor.mp4");
    assert_success(
        &execute_command(
            &json!({ "action": "recording_start", "path": path, "cursor": true, "fps": 60 }),
            &mut state,
        )
        .await,
    );
    assert_success(&execute_command(&json!({ "action": "mousedown" }), &mut state).await);
    for x in [560, 80, 560] {
        assert_success(
            &execute_command(
                &json!({ "action": "mousemove", "x": x, "y": 240, "duration": 1000, "inputMode": "human", "seed": 42 }),
                &mut state,
            )
            .await,
        );
    }
    assert_success(&execute_command(&json!({ "action": "mouseup" }), &mut state).await);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_success(&execute_command(&json!({ "action": "recording_stop" }), &mut state).await);
    assert_success(&execute_command(&json!({ "action": "close" }), &mut state).await);

    let output = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args([
            "-vf",
            "crop=640:80:0:200",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let mut measured = 0;
    let mut worst = 0;
    let mut positions = std::collections::HashSet::new();
    for bytes in output.stdout.chunks_exact(640 * 80 * 3) {
        let frame = image::RgbImage::from_raw(640, 80, bytes.to_vec()).unwrap();
        let line = (0..640).find(|&x| {
            let [r, g, b] = frame.get_pixel(x, 0).0;
            g > 150 && g > r.saturating_add(60) && g > b.saturating_add(60)
        });
        let cursor = frame
            .enumerate_pixels()
            .filter_map(|(x, _, pixel)| {
                let [r, g, b] = pixel.0;
                (r > 180 && g > 180 && b > 180).then_some(x)
            })
            .min();
        if let (Some(line), Some(cursor)) = (line, cursor) {
            measured += 1;
            positions.insert(line);
            worst = worst.max(line.abs_diff(cursor));
        }
    }
    assert!(measured >= 100, "only {measured} drag frames measured");
    assert!(
        positions.len() >= 60,
        "drag must exercise moving page frames"
    );
    // The cursor's white fill starts 1-2 pixels inside its black outline.
    assert!(
        worst <= 3,
        "recorded cursor separated from the dragged control by {worst} pixels"
    );
}

/// A completed drag must not pin the recorded cursor to a stale page frame.
#[tokio::test]
#[ignore]
async fn e2e_recording_cursor_moves_after_release_without_repaint() {
    let mut state = DaemonState::new();
    assert_success(
        &execute_command(&json!({ "action": "launch", "headless": true }), &mut state).await,
    );
    assert_success(
        &execute_command(
            &json!({ "action": "viewport", "width": 640, "height": 480 }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &execute_command(
            &json!({
                "action": "navigate",
                "url": "data:text/html,<style>body{margin:0;background:%23202020}</style>"
            }),
            &mut state,
        )
        .await,
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("released-cursor.mp4");
    assert_success(
        &execute_command(
            &json!({ "action": "recording_start", "path": path, "cursor": true, "fps": 60 }),
            &mut state,
        )
        .await,
    );
    for command in [
        json!({ "action": "mousemove", "x": 80, "y": 80 }),
        json!({ "action": "mousedown" }),
        json!({ "action": "mousemove", "x": 120, "y": 100, "duration": 150, "inputMode": "human" }),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    let captured = state
        .recording_state
        .shared_captured_count
        .as_ref()
        .unwrap()
        .clone();
    let before = captured.load(Ordering::Relaxed);
    // Paint while pressed, then leave the page completely static after release.
    assert_success(
        &execute_command(
            &json!({ "action": "evaluate", "script": "document.body.style.background = '#303030'" }),
            &mut state,
        )
        .await,
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while captured.load(Ordering::Relaxed) == before {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the pressed page frame should reach the recorder");
    for command in [
        json!({ "action": "mouseup" }),
        json!({ "action": "mousemove", "x": 240, "y": 180, "duration": 250, "inputMode": "human" }),
    ] {
        assert_success(&execute_command(&command, &mut state).await);
    }
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_success(&execute_command(&json!({ "action": "recording_stop" }), &mut state).await);
    assert_success(&execute_command(&json!({ "action": "close" }), &mut state).await);

    let output = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-sseof", "-0.1", "-i"])
        .arg(&path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "pipe:1",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let frame = image::load_from_memory(&output.stdout).unwrap().to_rgb8();
    assert_eq!(frame.dimensions(), (640, 480));
    assert!(
        frame.get_pixel(244, 184).0.iter().all(|value| *value > 180),
        "the released cursor should reach the new position in the encoded video; got {:?}",
        frame.get_pixel(244, 184).0
    );
    assert!(
        frame.get_pixel(124, 104).0.iter().all(|value| *value < 80),
        "the old drag position should contain only the page background"
    );
}

#[tokio::test]
#[ignore]
async fn e2e_initial_recording_frame_uses_css_viewport_dimensions() {
    let mut state = DaemonState::new();
    assert_success(
        &execute_command(&json!({ "action": "launch", "headless": true }), &mut state).await,
    );
    assert_success(
        &execute_command(
            &json!({
                "action": "viewport",
                "width": 400,
                "height": 300,
                "deviceScaleFactor": 2.0
            }),
            &mut state,
        )
        .await,
    );

    let browser = state.browser.as_ref().unwrap();
    let initial = super::recording::capture_initial_image(
        &browser.client,
        browser.active_session_id().unwrap(),
    )
    .await
    .unwrap();
    let (pixel_width, pixel_height) = image::ImageReader::with_format(
        std::io::Cursor::new(&initial.image_data),
        image::ImageFormat::Png,
    )
    .into_dimensions()
    .unwrap();

    assert_eq!((pixel_width, pixel_height), (800, 600));
    assert_eq!(
        (initial.device_width, initial.device_height),
        (400.0, 300.0)
    );

    assert_success(&execute_command(&json!({ "action": "close" }), &mut state).await);
}

// ---------------------------------------------------------------------------
// tab new: session setup inheritance
// ---------------------------------------------------------------------------

/// `tab new <url>` must replay the session's setup onto the new tab before
/// its first document: an init script registered on the primary page has to
/// run on the initial load, not only after a later navigation.
#[tokio::test]
#[ignore]
async fn e2e_tab_new_inherits_init_script_on_first_load() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "addinitscript", "script": "window.__abTab = 'seeded';" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3", "action": "tab_new",
            "url": "data:text/html,<script>document.title = String(window.__abTab)</script>",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "document.title" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"],
        "seeded",
        "init script should run on the new tab's first document"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Replayed init scripts receive target-specific CDP identifiers. Removing a
/// script from the new tab must translate the original user-facing identifier
/// for every tab where Chrome registered it.
#[tokio::test]
#[ignore]
async fn e2e_tab_new_removes_replayed_init_script_by_original_identifier() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let first = execute_command(
        &json!({
            "id": "2", "action": "addinitscript",
            "script": "window.__abFirstInit = true;",
        }),
        &mut state,
    )
    .await;
    assert_success(&first);
    let first_id = get_data(&first)["identifier"]
        .as_str()
        .expect("first init script should return an identifier")
        .to_string();

    let second = execute_command(
        &json!({
            "id": "3", "action": "addinitscript",
            "script": "window.__abSecondInit = true;",
        }),
        &mut state,
    )
    .await;
    assert_success(&second);
    let second_id = get_data(&second)["identifier"]
        .as_str()
        .expect("second init script should return an identifier")
        .to_string();

    let resp = execute_command(
        &json!({ "id": "4", "action": "removeinitscript", "identifier": first_id }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "5", "action": "tab_new",
            "url": "data:text/html,<title>new tab</title>",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "6", "action": "removeinitscript", "identifier": second_id }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "7", "action": "navigate",
            "url": "data:text/html,<title>after removal</title>",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "8", "action": "evaluate",
            "script": "window.__abSecondInit === true",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], false);

    let resp = execute_command(
        &json!({ "id": "9", "action": "tab_switch", "tabId": "t1" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "10", "action": "navigate",
            "url": "data:text/html,<title>original after removal</title>",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "11", "action": "evaluate",
            "script": "window.__abSecondInit === true",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"],
        false,
        "removing a replayed script should also remove its original registration"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// CDP allocates init-script identifiers independently in each target. Two
/// pre-existing tabs can therefore both return `1` for different scripts, but
/// the daemon must expose distinct handles and remove only the requested one.
#[tokio::test]
#[ignore]
async fn e2e_tab_init_script_handles_are_unique_across_preexisting_tabs() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "2", "action": "tab_new" }), &mut state).await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "tab_switch", "tabId": "t1" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let first = execute_command(
        &json!({
            "id": "4", "action": "addinitscript",
            "script": "window.__abFirstExistingTab = true;",
        }),
        &mut state,
    )
    .await;
    assert_success(&first);
    let first_id = get_data(&first)["identifier"]
        .as_str()
        .expect("first init script should return an identifier")
        .to_string();

    let resp = execute_command(
        &json!({ "id": "5", "action": "tab_switch", "tabId": "t2" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let second = execute_command(
        &json!({
            "id": "6", "action": "addinitscript",
            "script": "window.__abSecondExistingTab = true;",
        }),
        &mut state,
    )
    .await;
    assert_success(&second);
    let second_id = get_data(&second)["identifier"]
        .as_str()
        .expect("second init script should return an identifier")
        .to_string();

    assert_ne!(first_id, second_id, "user-facing handles must be unique");

    let resp = execute_command(
        &json!({ "id": "7", "action": "removeinitscript", "identifier": second_id }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "8", "action": "tab_new",
            "url": "data:text/html,<title>future tab</title>",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "9", "action": "evaluate",
            "script": "[window.__abFirstExistingTab === true, window.__abSecondExistingTab === true]",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], json!([true, false]));

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// `click --new-tab` creates a tab through a separate handler from `tab new`,
/// but it must apply the same session setup before the first request.
#[tokio::test]
#[ignore]
async fn e2e_click_new_tab_inherits_user_agent_and_headers() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({
            "id": "1", "action": "launch", "headless": true,
            "userAgent": "ab-click-new-tab-test/1.0",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "headers", "headers": { "X-Global": "global" } }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "3", "action": "navigate",
            "url": format!("data:text/html,<a id='next' href='{}/click'>next</a>", base_url),
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "4", "action": "click", "selector": "#next", "newTab": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "5", "action": "evaluate",
            "script": "JSON.parse(document.body.innerText)",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let headers = &get_data(&resp)["result"]["headers"];
    assert_eq!(headers["X-Global"], "global");
    assert_eq!(headers["User-Agent"], "ab-click-new-tab-test/1.0");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// Clearing headers and offline mode restores the default setup, so future
/// tabs can use the non-blocking target creation path.
#[tokio::test]
#[ignore]
async fn e2e_cleared_session_setup_is_not_pending() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "offline", "offline": false }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "headers", "headers": {} }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    assert!(state.session_setup.offline.is_none());
    assert!(state.session_setup.extra_headers.is_none());

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// The launch `--user-agent` (Emulation.setUserAgentOverride) and global
/// `set headers` (Network.setExtraHTTPHeaders) are per CDP session. A new tab
/// opened with a URL must send both on its very first document request.
#[tokio::test]
#[ignore]
async fn e2e_tab_new_inherits_user_agent_and_headers() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({
            "id": "1", "action": "launch", "headless": true,
            "userAgent": "ab-tab-new-test/1.0",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "headers", "headers": { "X-Global": "global" } }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "tab_new", "url": format!("{}/tab", base_url) }),
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
    let headers = &get_data(&resp)["result"]["headers"];
    assert_eq!(
        headers["X-Global"], "global",
        "global `headers` should apply to the new tab's first document request, got {headers}"
    );
    assert_eq!(
        headers["User-Agent"], "ab-tab-new-test/1.0",
        "new tab's first document request should carry the launch user agent, got {headers}"
    );

    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "navigator.userAgent" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["result"], "ab-tab-new-test/1.0");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

/// `set credentials` uses target-scoped extra headers, so a new tab must send
/// the resulting Authorization header on its first document request.
#[tokio::test]
#[ignore]
async fn e2e_tab_new_inherits_http_credentials_on_first_load() {
    let (base_url, _server) = start_echo_server().await;
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({
            "id": "2", "action": "credentials",
            "username": "tab-user", "password": "tab-password",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "tab_new", "url": format!("{}/credentials", base_url) }),
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
    let expected = format!("Basic {}", STANDARD.encode("tab-user:tab-password"));
    assert_eq!(
        get_data(&resp)["result"]["headers"]["Authorization"],
        expected,
        "HTTP credentials should apply to the new tab's first document request"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// --state / storageState flag: cookies should be loaded at launch time
// ---------------------------------------------------------------------------

/// Verify that launching with `storageState` in the launch command restores
/// cookies that were previously saved with `state_save`.
///
/// This is the e2e equivalent of `agent-browser --state ./auth.json open <url>`.
/// The launch command accepts a `storageState` field that should load the
/// state file (cookies + localStorage) before the first navigation.
#[tokio::test]
#[ignore]
async fn e2e_state_flag_restores_cookies() {
    let state_path = std::env::temp_dir()
        .join(format!(
            "agent-browser-e2e-state-flag-{}.json",
            uuid::Uuid::new_v4()
        ))
        .to_string_lossy()
        .to_string();

    // Session 1: launch, set a cookie, save state, close
    {
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

        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "cookies_set",
                "name": "state_flag_test",
                "value": "from_state_file",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "4", "action": "state_save", "path": &state_path }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(&json!({ "id": "5", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    // Session 2: launch with storageState pointing to saved file, verify
    // cookies are present before any explicit state_load call.
    {
        let mut state = DaemonState::new();

        let resp = execute_command(
            &json!({
                "id": "10",
                "action": "launch",
                "headless": true,
                "storageState": &state_path
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
            .any(|c| c["name"] == "state_flag_test" && c["value"] == "from_state_file");
        assert!(
            found,
            "Cookie from state file should be present after launch with storageState. \
             Cookies found: {:?}",
            cookies
                .iter()
                .map(|c| c["name"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    let _ = std::fs::remove_file(&state_path);
}

/// Verify that explicit `launch` surfaces storageState load failures instead
/// of reporting success with an empty browser state.
#[tokio::test]
#[ignore]
async fn e2e_state_flag_missing_file_fails_launch() {
    let guard = EnvGuard::new(&["CI"]);
    guard.set("CI", "1");

    let missing_path = std::env::temp_dir()
        .join(format!(
            "agent-browser-e2e-missing-state-{}.json",
            uuid::Uuid::new_v4()
        ))
        .to_string_lossy()
        .to_string();

    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({
            "id": "10",
            "action": "launch",
            "headless": true,
            "args": ["--no-sandbox", "--disable-dev-shm-usage"],
            "storageState": &missing_path
        }),
        &mut state,
    )
    .await;

    assert_eq!(resp["success"], false);
    let error = resp["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("Failed to read state from") || error.contains("storage state"),
        "Unexpected error for missing storageState file: {}",
        error
    );
    assert!(
        state.browser.is_none(),
        "failed storageState launch should roll back the browser"
    );
}

/// Repeated launch calls with `storageState` should relaunch a clean browser so
/// stale cookies do not survive from the previous state file.
#[tokio::test]
#[ignore]
async fn e2e_storage_state_launch_restarts_clean_browser() {
    let state_one = std::env::temp_dir()
        .join(format!(
            "agent-browser-e2e-storage-reuse-1-{}.json",
            uuid::Uuid::new_v4()
        ))
        .to_string_lossy()
        .to_string();
    let state_two = std::env::temp_dir()
        .join(format!(
            "agent-browser-e2e-storage-reuse-2-{}.json",
            uuid::Uuid::new_v4()
        ))
        .to_string_lossy()
        .to_string();

    create_storage_state_with_cookie(&state_one, "storage_reload_first", "first").await;
    create_storage_state_with_cookie(&state_two, "storage_reload_second", "second").await;

    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({
            "id": "10",
            "action": "launch",
            "headless": true,
            "args": ["--no-sandbox", "--disable-dev-shm-usage"],
            "storageState": &state_one
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(
        get_data(&resp).get("reused").is_none(),
        "first launch must create the browser"
    );

    let resp = execute_command(
        &json!({ "id": "11", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "12", "action": "cookies_get" }), &mut state).await;
    assert_success(&resp);
    let cookies = get_data(&resp)["cookies"].as_array().unwrap();
    assert!(
        cookies
            .iter()
            .any(|c| c["name"] == "storage_reload_first" && c["value"] == "first"),
        "first storageState should be applied on the initial launch"
    );

    let resp = execute_command(
        &json!({
            "id": "13",
            "action": "launch",
            "headless": true,
            "args": ["--no-sandbox", "--disable-dev-shm-usage"],
            "storageState": &state_two
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp).get("reused"),
        None,
        "storageState launch should start from a clean browser"
    );

    let resp = execute_command(
        &json!({ "id": "14", "action": "navigate", "url": "https://example.com" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "15", "action": "cookies_get" }), &mut state).await;
    assert_success(&resp);
    let cookies = get_data(&resp)["cookies"].as_array().unwrap();
    assert!(
        cookies
            .iter()
            .any(|c| c["name"] == "storage_reload_second" && c["value"] == "second"),
        "second storageState should be applied after relaunch"
    );
    assert!(
        !cookies
            .iter()
            .any(|c| c["name"] == "storage_reload_first" && c["value"] == "first"),
        "stale cookies from the first storageState should not survive relaunch"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);

    let _ = std::fs::remove_file(&state_one);
    let _ = std::fs::remove_file(&state_two);
}

/// Verify that AGENT_BROWSER_STATE env var restores cookies at auto-launch
/// time (when the browser is lazily launched by a command like `navigate`
/// rather than an explicit `launch` command).
#[tokio::test]
#[ignore]
async fn e2e_state_env_restores_cookies_on_auto_launch() {
    let state_path = std::env::temp_dir()
        .join(format!(
            "agent-browser-e2e-state-env-{}.json",
            uuid::Uuid::new_v4()
        ))
        .to_string_lossy()
        .to_string();

    // Session 1: launch, set a cookie, save state, close
    {
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

        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "cookies_set",
                "name": "env_state_test",
                "value": "from_env_state",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "4", "action": "state_save", "path": &state_path }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(&json!({ "id": "5", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    // Session 2: set AGENT_BROWSER_STATE env var and let auto_launch pick it
    // up. No explicit `launch` command — just navigate, which triggers
    // auto_launch internally.
    {
        let env = EnvGuard::new(&["AGENT_BROWSER_STATE"]);
        env.set("AGENT_BROWSER_STATE", &state_path);

        let mut state = DaemonState::new();

        // Navigate without explicit launch — triggers auto_launch
        let resp = execute_command(
            &json!({ "id": "10", "action": "navigate", "url": "https://example.com" }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp =
            execute_command(&json!({ "id": "11", "action": "cookies_get" }), &mut state).await;
        assert_success(&resp);
        let cookies = get_data(&resp)["cookies"].as_array().unwrap();
        let found = cookies
            .iter()
            .any(|c| c["name"] == "env_state_test" && c["value"] == "from_env_state");
        assert!(
            found,
            "Cookie should be restored via AGENT_BROWSER_STATE env on auto-launch. \
             Cookies found: {:?}",
            cookies
                .iter()
                .map(|c| c["name"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    let _ = std::fs::remove_file(&state_path);
}

/// Verify that --session-name auto-restores cookies saved from a prior
/// session with the same name.
#[tokio::test]
#[ignore]
async fn e2e_session_name_auto_restores_cookies() {
    let session_name = format!(
        "e2e-session-name-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );

    let env = EnvGuard::new(&["AGENT_BROWSER_SESSION_NAME"]);
    env.set("AGENT_BROWSER_SESSION_NAME", &session_name);

    // Session 1: launch, set a cookie, close (which auto-saves state)
    {
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

        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "cookies_set",
                "name": "session_name_test",
                "value": "auto_restored",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        // close triggers auto-save when session_name is set
        let resp = execute_command(&json!({ "id": "5", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    // Session 2: fresh DaemonState with same session_name. Navigate without
    // explicit launch, which triggers auto_launch and restore.
    {
        let mut state = DaemonState::new();

        // Navigate without explicit launch — triggers auto_launch → try_auto_restore_state
        let resp = execute_command(
            &json!({ "id": "10", "action": "navigate", "url": "https://example.com" }),
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
            .any(|c| c["name"] == "session_name_test" && c["value"] == "auto_restored");
        assert!(
            found,
            "Cookie should be auto-restored via --session-name. Cookies found: {:?}",
            cookies
                .iter()
                .map(|c| c["name"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    // Clean up auto-saved state files
    let sessions_dir = std::env::home_dir()
        .unwrap()
        .join(".agent-browser")
        .join("sessions");
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            if fname.starts_with(&format!("{}-", session_name)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[tokio::test]
#[ignore]
async fn e2e_restore_loads_during_explicit_launch_before_navigation() {
    let restore_key = format!(
        "e2e-explicit-restore-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let env = EnvGuard::new(&[
        "AGENT_BROWSER_SESSION_NAME",
        "AGENT_BROWSER_RESTORE_SAVE",
        "AGENT_BROWSER_STATE",
        "AGENT_BROWSER_ENCRYPTION_KEY",
    ]);
    env.remove("AGENT_BROWSER_SESSION_NAME");
    env.remove("AGENT_BROWSER_RESTORE_SAVE");
    env.remove("AGENT_BROWSER_STATE");
    env.remove("AGENT_BROWSER_ENCRYPTION_KEY");

    {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({
                "id": "1",
                "action": "launch",
                "headless": true,
                "restoreKey": restore_key
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
                "name": "explicit_restore_test",
                "value": "loaded_before_navigation",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(&json!({ "id": "4", "action": "close" }), &mut state).await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["saveStatus"], "saved");
    }

    let path = super::state::find_auto_state_file(&restore_key)
        .expect("first close should create restore state");

    {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({
                "id": "10",
                "action": "launch",
                "headless": true,
                "restoreKey": restore_key
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["lifecycle"]["restoreStatus"], "loaded");

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
        let found = cookies.iter().any(|c| {
            c["name"] == "explicit_restore_test" && c["value"] == "loaded_before_navigation"
        });
        assert!(
            found,
            "Cookie should be restored by explicit launch before first navigation. Cookies found: {:?}",
            cookies
                .iter()
                .map(|c| c["name"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.previous", path));
}

/// Verify that periodic autosave persists session state while the browser is
/// open, so state survives the browser dying WITHOUT the daemon's
/// save-on-close path running (e.g. the user closes the window by hand).
#[tokio::test]
#[ignore]
async fn e2e_periodic_autosave_survives_abrupt_browser_exit() {
    let restore_key = format!(
        "e2e-autosave-abrupt-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let env = EnvGuard::new(&[
        "AGENT_BROWSER_SESSION_NAME",
        "AGENT_BROWSER_RESTORE_SAVE",
        "AGENT_BROWSER_STATE",
        "AGENT_BROWSER_ENCRYPTION_KEY",
    ]);
    env.remove("AGENT_BROWSER_SESSION_NAME");
    env.remove("AGENT_BROWSER_RESTORE_SAVE");
    env.remove("AGENT_BROWSER_STATE");
    env.remove("AGENT_BROWSER_ENCRYPTION_KEY");

    {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({
                "id": "1",
                "action": "launch",
                "headless": true,
                "restoreKey": restore_key
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
                "name": "autosave_test",
                "value": "saved_by_tick",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        // The command just finished, so the quiet period must block the tick.
        maybe_autosave_restore_state(&mut state, 30_000).await;
        assert_eq!(state.restore_save_status, "not_attempted");
        assert!(
            super::state::find_auto_state_file(&restore_key).is_none(),
            "autosave must not fire inside the post-command quiet period"
        );

        // Simulate the quiet period having elapsed, then run the tick again.
        state.last_command_finished =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(10));
        maybe_autosave_restore_state(&mut state, 30_000).await;
        assert_eq!(state.restore_save_status, "saved");
        assert!(
            state.last_autosave_attempt.is_some(),
            "successful save should reset the periodic interval"
        );
        assert!(
            super::state::find_auto_state_file(&restore_key).is_some(),
            "periodic autosave should write the session state file"
        );

        // An idle session stays eligible: once the interval elapses again the
        // tick re-saves, capturing page-driven mutations like token refreshes.
        state.last_autosave_attempt =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(31));
        state.restore_save_status = "not_attempted".to_string();
        maybe_autosave_restore_state(&mut state, 30_000).await;
        assert_eq!(
            state.restore_save_status, "saved",
            "idle session should be re-saved on the next interval without new commands"
        );

        // Kill the browser out from under the daemon, the way a user closing
        // the window does: the process exits and CDP dies, so no further save
        // is possible. Then mimic the daemon drain tick, which only closes.
        let mgr = state.browser.as_mut().expect("browser should be running");
        let _ = mgr
            .client
            .send_command_no_params("Browser.close", None)
            .await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !mgr.has_process_exited() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            state
                .browser
                .as_mut()
                .expect("browser manager should still be present")
                .has_process_exited(),
            "browser process should have exited after Browser.close"
        );
        let _ = close_current_browser(&mut state).await;
    }

    let path = super::state::find_auto_state_file(&restore_key)
        .expect("autosaved state should survive the abrupt browser exit");

    {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({
                "id": "10",
                "action": "launch",
                "headless": true,
                "restoreKey": restore_key
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["lifecycle"]["restoreStatus"], "loaded");

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
            .any(|c| c["name"] == "autosave_test" && c["value"] == "saved_by_tick");
        assert!(
            found,
            "Cookie saved only by periodic autosave should be restored after the browser was killed without a graceful close. Cookies found: {:?}",
            cookies
                .iter()
                .map(|c| c["name"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.previous", path));
}

#[tokio::test]
#[ignore]
async fn e2e_restore_preserves_cookie_login_after_close_and_reopen() {
    let restore_key = format!(
        "e2e-next-cookie-login-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let env = EnvGuard::new(&[
        "AGENT_BROWSER_SESSION_NAME",
        "AGENT_BROWSER_RESTORE_SAVE",
        "AGENT_BROWSER_STATE",
        "AGENT_BROWSER_ENCRYPTION_KEY",
    ]);
    env.remove("AGENT_BROWSER_SESSION_NAME");
    env.remove("AGENT_BROWSER_RESTORE_SAVE");
    env.remove("AGENT_BROWSER_STATE");
    env.remove("AGENT_BROWSER_ENCRYPTION_KEY");

    let (base_url, _server) = start_cookie_login_server().await;

    {
        let mut state = DaemonState::new();

        let resp = execute_command(
            &json!({
                "id": "1",
                "action": "navigate",
                "url": base_url.clone(),
                "restoreKey": restore_key
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["lifecycle"]["restoreStatus"], "missing");

        let resp = execute_command(
            &json!({ "id": "2", "action": "evaluate", "script": "document.body.innerText" }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert!(
            get_data(&resp)["result"]
                .as_str()
                .unwrap_or_default()
                .contains("Please sign in"),
            "first homepage load should be logged out: {}",
            resp
        );

        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "navigate",
                "url": format!("{}/login", base_url),
                "restoreKey": restore_key
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({
                "id": "4",
                "action": "navigate",
                "url": base_url.clone(),
                "restoreKey": restore_key
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "5", "action": "evaluate", "script": "document.body.innerText" }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert!(
            get_data(&resp)["result"]
                .as_str()
                .unwrap_or_default()
                .contains("Welcome back"),
            "login route should set cookie for current browser: {}",
            resp
        );

        let resp = execute_command(&json!({ "id": "6", "action": "close" }), &mut state).await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["saveStatus"], "saved");
    }

    let path = super::state::find_auto_state_file(&restore_key)
        .expect("close should save cookie-backed restore state");

    {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({
                "id": "10",
                "action": "navigate",
                "url": base_url.clone(),
                "restoreKey": restore_key
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["lifecycle"]["restoreStatus"], "loaded");

        let resp = execute_command(
            &json!({ "id": "11", "action": "evaluate", "script": "document.body.innerText" }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert!(
            get_data(&resp)["result"]
                .as_str()
                .unwrap_or_default()
                .contains("Welcome back"),
            "reopened session should keep cookie login on homepage: {}",
            resp
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.previous", path));
    cleanup_restore_state_files(&restore_key);
}

#[tokio::test]
#[ignore]
async fn e2e_restore_validation_failure_does_not_overwrite_state() {
    let restore_key = format!(
        "e2e-restore-validation-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );

    {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({
                "id": "1",
                "action": "navigate",
                "url": "https://example.com",
                "restoreKey": restore_key
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({
                "id": "2",
                "action": "cookies_set",
                "name": "restore_validation_test",
                "value": "known_good",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(&json!({ "id": "3", "action": "close" }), &mut state).await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["saveStatus"], "saved");
    }

    let path = super::state::find_auto_state_file(&restore_key)
        .expect("first close should create restore state");
    let before = std::fs::read_to_string(&path).expect("state file should be readable");

    {
        let mut state = DaemonState::new();
        let resp = execute_command(
            &json!({
                "id": "10",
                "action": "navigate",
                "url": "https://example.com",
                "restoreKey": restore_key,
                "restoreCheckText": "text-that-does-not-exist-in-the-page"
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);
        assert_eq!(
            get_data(&resp)["lifecycle"]["restoreStatus"],
            "loaded_but_invalid"
        );

        let resp = execute_command(&json!({ "id": "11", "action": "close" }), &mut state).await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["saveStatus"], "skipped_restore_failed");
    }

    let after = std::fs::read_to_string(&path).expect("state file should still be readable");
    assert_eq!(
        before, after,
        "failed restore validation must not overwrite previous state"
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{}.previous", path));
}

#[tokio::test]
#[ignore]
async fn e2e_restore_key_switch_reloads_instead_of_reusing_live_browser() {
    let restore_key_a = format!(
        "e2e-restore-switch-a-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let restore_key_b = format!(
        "e2e-restore-switch-b-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    let cookie_name = "restore_switch_test";
    let env = EnvGuard::new(&[
        "AGENT_BROWSER_SESSION_NAME",
        "AGENT_BROWSER_RESTORE_SAVE",
        "AGENT_BROWSER_STATE",
        "AGENT_BROWSER_ENCRYPTION_KEY",
    ]);
    env.remove("AGENT_BROWSER_SESSION_NAME");
    env.remove("AGENT_BROWSER_RESTORE_SAVE");
    env.remove("AGENT_BROWSER_STATE");
    env.remove("AGENT_BROWSER_ENCRYPTION_KEY");

    create_restore_state_with_cookie(&restore_key_a, cookie_name, "value-a").await;
    create_restore_state_with_cookie(&restore_key_b, cookie_name, "value-b").await;

    let path_a = super::state::find_auto_state_file(&restore_key_a)
        .expect("first restore key should have saved state");
    let path_b = super::state::find_auto_state_file(&restore_key_b)
        .expect("second restore key should have saved state");

    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({
            "id": "10",
            "action": "navigate",
            "url": "https://example.com",
            "restoreKey": restore_key_a
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "11", "action": "cookies_get" }), &mut state).await;
    assert_success(&resp);
    let cookies = get_data(&resp)["cookies"].as_array().unwrap();
    assert!(
        cookies
            .iter()
            .any(|c| c["name"] == cookie_name && c["value"] == "value-a"),
        "first restore key should load value-a: {:?}",
        cookies
    );

    let resp = execute_command(
        &json!({
            "id": "20",
            "action": "navigate",
            "url": "https://example.com",
            "restoreKey": restore_key_b
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["lifecycle"]["relaunchedBrowser"], true);

    let resp = execute_command(&json!({ "id": "21", "action": "cookies_get" }), &mut state).await;
    assert_success(&resp);
    let cookies = get_data(&resp)["cookies"].as_array().unwrap();
    assert!(
        cookies
            .iter()
            .any(|c| c["name"] == cookie_name && c["value"] == "value-b"),
        "switching restore keys should load value-b, not reuse value-a: {:?}",
        cookies
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);

    let saved_b = std::fs::read_to_string(&path_b).expect("state file should remain readable");
    assert!(saved_b.contains("value-b"));
    assert!(!saved_b.contains("value-a"));

    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(format!("{}.previous", path_a));
    let _ = std::fs::remove_file(&path_b);
    let _ = std::fs::remove_file(format!("{}.previous", path_b));
}

/// Verify that explicit `state_load` restores cookies into an existing
/// session (baseline sanity check — this path is known to work).
#[tokio::test]
#[ignore]
async fn e2e_explicit_state_load_restores_cookies() {
    let state_path = std::env::temp_dir()
        .join(format!(
            "agent-browser-e2e-explicit-load-{}.json",
            uuid::Uuid::new_v4()
        ))
        .to_string_lossy()
        .to_string();

    // Session 1: set cookie, save state
    {
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

        let resp = execute_command(
            &json!({
                "id": "3",
                "action": "cookies_set",
                "name": "explicit_load_test",
                "value": "manually_loaded",
                "domain": ".example.com",
                "path": "/",
                "expires": 2000000000
            }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "4", "action": "state_save", "path": &state_path }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(&json!({ "id": "5", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    // Session 2: launch clean, then explicitly load state
    {
        let mut state = DaemonState::new();

        let resp = execute_command(
            &json!({ "id": "10", "action": "launch", "headless": true }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "11", "action": "state_load", "path": &state_path }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp = execute_command(
            &json!({ "id": "12", "action": "navigate", "url": "https://example.com" }),
            &mut state,
        )
        .await;
        assert_success(&resp);

        let resp =
            execute_command(&json!({ "id": "13", "action": "cookies_get" }), &mut state).await;
        assert_success(&resp);
        let cookies = get_data(&resp)["cookies"].as_array().unwrap();
        let found = cookies
            .iter()
            .any(|c| c["name"] == "explicit_load_test" && c["value"] == "manually_loaded");
        assert!(
            found,
            "Cookie should be present after explicit state_load. Cookies found: {:?}",
            cookies
                .iter()
                .map(|c| c["name"].as_str().unwrap_or("?"))
                .collect::<Vec<_>>()
        );

        let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
        assert_success(&resp);
    }

    let _ = std::fs::remove_file(&state_path);
}

// === React / Web Vitals primitives ===

const REACT_FIXTURE_HTML: &str = r#"<!doctype html>
<html>
  <head><title>React fixture</title></head>
  <body>
    <div id="root"></div>
    <script crossorigin src="https://unpkg.com/react@18/umd/react.production.min.js"></script>
    <script crossorigin src="https://unpkg.com/react-dom@18/umd/react-dom.production.min.js"></script>
    <script>
      const { useState, createElement: h } = React;
      function Counter({ label }) {
        const [n, setN] = useState(0);
        return h("button", { onClick: () => setN(n + 1) }, label + ": " + n);
      }
      function App() {
        return h("div", {}, [
          h("h1", { key: "t" }, "Hello"),
          h(Counter, { key: "c1", label: "A" }),
          h(Counter, { key: "c2", label: "B" }),
        ]);
      }
      ReactDOM.createRoot(document.getElementById("root")).render(h(App));
    </script>
  </body>
</html>
"#;

fn react_fixture_url() -> String {
    format!(
        "data:text/html;base64,{}",
        STANDARD.encode(REACT_FIXTURE_HTML)
    )
}

#[tokio::test]
#[ignore]
async fn e2e_react_tree_errors_without_hook() {
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

    // Without --enable react-devtools, the hook isn't installed and the
    // command should error.
    let resp = execute_command(&json!({ "id": "3", "action": "react_tree" }), &mut state).await;
    let err = resp
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        err.contains("React DevTools") || err.contains("renderer"),
        "Expected hook-missing error, got: {:?}",
        resp
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_react_tree_with_enable_hook() {
    let guard = EnvGuard::new(&["AGENT_BROWSER_ENABLE"]);
    guard.set("AGENT_BROWSER_ENABLE", "react-devtools");
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": &react_fixture_url() }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Give React a moment to boot and register with the hook.
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let resp = execute_command(&json!({ "id": "3", "action": "react_tree" }), &mut state).await;
    assert_success(&resp);
    let tree = get_data(&resp)
        .get("tree")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        tree.contains("App"),
        "Expected tree to contain 'App': {}",
        tree
    );
    assert!(
        tree.contains("Counter"),
        "Expected tree to contain 'Counter': {}",
        tree
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_relaunch_when_enable_changes_installs_react_hook() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": &react_fixture_url() }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "react_tree" }), &mut state).await;
    assert!(
        resp.get("success").and_then(|v| v.as_bool()) == Some(false),
        "React tree should fail before react-devtools is enabled: {}",
        resp
    );

    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "launch",
            "headless": true,
            "enable": ["react-devtools"]
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(
        get_data(&resp).get("reused").is_none(),
        "changed enable list must relaunch, not reuse: {}",
        resp
    );
    assert_eq!(
        get_data(&resp)["lifecycle"]["relaunchedBrowser"],
        true,
        "lifecycle should report browser relaunch"
    );

    let resp = execute_command(
        &json!({ "id": "5", "action": "navigate", "url": &react_fixture_url() }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let resp = execute_command(&json!({ "id": "6", "action": "react_tree" }), &mut state).await;
    assert_success(&resp);
    let tree = get_data(&resp)
        .get("tree")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        tree.contains("App"),
        "Expected tree to contain App: {}",
        tree
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_vitals_reports_metrics() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": &react_fixture_url() }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "vitals" }), &mut state).await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert!(data.get("url").and_then(|v| v.as_str()).is_some());
    assert!(data.get("ttfb").is_some());
    assert!(data.get("cls").and_then(|v| v.get("score")).is_some());
    assert!(data.get("phases").and_then(|v| v.as_array()).is_some());
    assert!(data
        .get("hydratedComponents")
        .and_then(|v| v.as_array())
        .is_some());

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

async fn start_a11y_frame_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        for _ in 0..100 {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("/");

                let (status, content_type, body) = match path {
                    "/top" => (
                        "200 OK",
                        "text/html",
                        format!(
                            r#"<!doctype html><html lang="en"><head><title>Top</title></head>
<body><main aria-label="Top"><h1>Top</h1>
<iframe id="outer" title="Outer" src="http://127.0.0.1:{port}/outer"></iframe>
</main></body></html>"#
                        ),
                    ),
                    "/outer" => (
                        "200 OK",
                        "text/html",
                        r#"<!doctype html><html lang="en"><head><title>Outer</title></head>
<body><main aria-label="Outer"><h1>Outer</h1><img id="outer-image" src="/missing-outer.png">
<iframe id="inner" title="Inner" src="/inner"></iframe></main></body></html>"#
                            .to_string(),
                    ),
                    "/inner" => (
                        "200 OK",
                        "text/html",
                        r#"<!doctype html><html lang="en"><head><title>Inner</title></head>
<body><main aria-label="Inner"><h1>Inner</h1><img id="inner-image" src="/missing-inner.png"></main></body></html>"#
                            .to_string(),
                    ),
                    "/siblings" => (
                        "200 OK",
                        "text/html",
                        format!(
                            r#"<!doctype html><html lang="en"><head><title>Siblings</title></head>
<body><main><h1>Siblings</h1>
<iframe id="first-frame" title="First" src="http://127.0.0.1:{port}/first-frame"></iframe>
<iframe id="second-frame" title="Second" src="http://127.0.0.1:{port}/second-frame"></iframe>
</main></body></html>"#
                        ),
                    ),
                    "/first-frame" => (
                        "200 OK",
                        "text/html",
                        r#"<!doctype html><html lang="en"><head><title>First</title></head>
<body><main><h1>First</h1><img id="first-image" src="/missing-first.png"></main></body></html>"#
                            .to_string(),
                    ),
                    "/second-frame" => (
                        "200 OK",
                        "text/html",
                        r#"<!doctype html><html lang="en"><head><title>Second</title></head>
<body><main><h1>Second</h1><img id="second-image" src="/missing-second.png"></main></body></html>"#
                            .to_string(),
                    ),
                    "/background" => (
                        "200 OK",
                        "text/html",
                        format!(
                            r#"<!doctype html><html lang="en"><head><title>Background</title></head>
<body><main><h1>Background</h1>
<iframe id="background-frame" title="Background frame" src="http://127.0.0.1:{port}/background-frame"></iframe>
</main></body></html>"#
                        ),
                    ),
                    "/background-frame" => (
                        "200 OK",
                        "text/html",
                        r#"<!doctype html><html lang="en"><head><title>Background frame</title></head>
<body><main><h1>Background frame</h1><img id="background-image" src="/missing-background.png"></main></body></html>"#
                            .to_string(),
                    ),
                    _ => ("404 Not Found", "text/plain", "not found".to_string()),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.flush().await;
            });
        }
    });

    (port, handle)
}

#[tokio::test]
#[ignore]
async fn e2e_recording_cursor_uses_page_coordinates_for_oopif() {
    let (port, server) = start_a11y_frame_server().await;
    let mut state = DaemonState::new();
    assert_success(
        &execute_command(&json!({"action": "launch", "headless": true}), &mut state).await,
    );
    assert_success(
        &execute_command(
            &json!({"action": "navigate", "url": format!("http://localhost:{port}/top")}),
            &mut state,
        )
        .await,
    );
    assert_success(&execute_command(&json!({"action": "evaluate", "script": "document.getElementById('outer').style.cssText = 'position:absolute;left:240px;top:160px;width:400px;height:300px;border:10px solid black'"}), &mut state).await);
    let child_session = state
        .iframe_sessions
        .values()
        .next()
        .expect("fixture must use an OOPIF")
        .clone();
    state.browser.as_ref().unwrap().client.send_command("Runtime.evaluate", Some(json!({
        "expression": "document.body.innerHTML = '<button style=\"position:absolute;left:20px;top:30px;width:80px;height:40px\">Cursor target</button>'"
    })), Some(&child_session)).await.unwrap();
    let snapshot = execute_command(&json!({"action": "snapshot"}), &mut state).await;
    assert_success(&snapshot);
    let reference = get_data(&snapshot)["refs"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, entry)| entry["name"] == "Cursor target")
        .unwrap()
        .0
        .clone();
    // Verify the input samples retain top-level coordinates for frame actions.
    super::recording::recording_start(
        &mut state.recording_state,
        "unused.webm",
        super::recording::RecordingOptions {
            cursor: true,
            ..Default::default()
        },
    )
    .unwrap();
    for action in ["hover", "click", "dblclick"] {
        assert_success(&execute_command(&json!({"action": action, "selector": format!("@{reference}"), "inputMode": "human"}), &mut state).await);
        let cursor = state
            .recording_state
            .shared_cursor
            .lock()
            .unwrap()
            .at(f64::INFINITY);
        assert!(cursor.visible);
        assert!(
            (cursor.x - 310.0).abs() < 1.0 && (cursor.y - 220.0).abs() < 1.0,
            "{action}: cursor should be at page (310, 220), got {cursor:?}"
        );
        assert!((state.mouse_state.x - cursor.x).abs() < 1.0);
        assert!((state.mouse_state.y - cursor.y).abs() < 1.0);
    }
    state.recording_state.active = false;
    state.browser.as_ref().unwrap().client.send_command("Runtime.evaluate", Some(json!({
        "expression": "document.body.style.background='#303030'; const button = document.querySelector('button'); button.style.background='#303030'; button.style.border='0'; button.textContent=''"
    })), Some(&child_session)).await.unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("iframe-cursor.mp4");
    assert_success(
        &execute_command(
            &json!({"action":"recording_start", "path":path,"cursor":true,"fps":60}),
            &mut state,
        )
        .await,
    );
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_success(
        &execute_command(
            &json!({"action":"hover", "selector":format!("@{reference}"), "inputMode":"human"}),
            &mut state,
        )
        .await,
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let stopped = execute_command(&json!({"action":"recording_stop"}), &mut state).await;
    assert_success(&execute_command(&json!({"action": "close"}), &mut state).await);
    server.abort();
    assert_success(&stopped);
    let output = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-sseof", "-0.1", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "pipe:1",
        ])
        .output()
        .await
        .unwrap();
    assert!(output.status.success());
    let image = image::load_from_memory(&output.stdout).unwrap().to_rgb8();
    assert!(
        image
            .get_pixel(314, 224)
            .0
            .iter()
            .all(|channel| *channel > 180),
        "the cursor should be visible inside the out-of-process iframe"
    );
}

#[tokio::test]
#[ignore]
async fn e2e_a11y_uses_vendored_engine_and_preserves_shadow_targets() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = r#"<!doctype html>
<html lang="en">
<head><title>Accessibility audit fixture</title></head>
<body>
  <main>
    <h1>Accessibility audit fixture</h1>
    <img id="light-image" src="missing.png">
    <div id="shadow-host"></div>
    <iframe id="audit-frame" title="Audit frame" tabindex="-1" srcdoc="
      <!doctype html><html lang='en'><head><title>Frame</title></head>
      <body><main><h1>Frame</h1><a href='https://example.com'>Frame link</a><img id='frame-image' src='missing.png'></main>
      <script>
        window.frameAxe = { version: 'frame-spoofed' };
        window.frameAxeSetterCalls = 0;
        Object.defineProperty(window, 'axe', {
          configurable: false,
          get() { return window.frameAxe; },
          set() {
            window.frameAxeSetterCalls += 1;
            throw new Error('frame axe setter must not run');
          }
        });
      </script></body></html>
    "></iframe>
  </main>
  <script>
    window.pageAxe = {
      version: 'spoofed',
      run: () => Promise.resolve({
        url: 'spoofed',
        testEngine: { version: 'spoofed' },
        violations: [],
        incomplete: [],
        passes: [],
        inapplicable: []
      })
    };
    window.axeSetterCalls = 0;
    Object.defineProperty(window, 'axe', {
      configurable: false,
      get() { return window.pageAxe; },
      set() {
        window.axeSetterCalls += 1;
        throw new Error('page axe setter must not run');
      }
    });
    window.amdCalls = 0;
    window.define = () => { window.amdCalls += 1; };
    window.define.amd = {};
    document.getElementById('shadow-host').attachShadow({ mode: 'open' }).innerHTML =
      '<img id="shadow-image" src="missing.png">';
  </script>
</body>
</html>"#;
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "a11y" }), &mut state).await;
    assert_success(&resp);
    let data = get_data(&resp);
    assert_eq!(data["axeVersion"], "4.12.1");
    let image_alt = data["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["id"] == "image-alt")
        .expect("vendored axe should report missing image alternatives");
    assert_eq!(image_alt["nodeCount"], 3);
    let nodes = image_alt["nodes"].as_array().unwrap();
    assert!(nodes
        .iter()
        .any(|node| node["target"] == json!(["#light-image"])));
    assert!(nodes
        .iter()
        .any(|node| node["target"] == json!([["#shadow-host", "#shadow-image"]])));
    assert!(nodes
        .iter()
        .any(|node| node["target"] == json!(["#audit-frame", "#frame-image"])));
    let frame_focusable_content = data["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["id"] == "frame-focusable-content")
        .expect("audit should preserve the child context for non-focusable frames");
    assert!(frame_focusable_content["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|node| node["target"] == json!(["#audit-frame", "html"])));

    let resp = execute_command(
        &json!({ "id": "3-selector", "action": "a11y", "selector": "#shadow-host" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let scoped_image_alt = get_data(&resp)["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["id"] == "image-alt")
        .expect("scoped audit should report the shadow image");
    assert_eq!(scoped_image_alt["nodeCount"], 1);

    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "evaluate",
            "script": "[window.axe.version, document.querySelector('#audit-frame').contentWindow.axe.version, window.axeSetterCalls, document.querySelector('#audit-frame').contentWindow.frameAxeSetterCalls, window.amdCalls]"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"],
        json!(["spoofed", "frame-spoofed", 0, 0, 0])
    );

    state
        .ref_map
        .add("e999".to_string(), Some(999), "button", "stale", None);
    state.active_frame_id = Some("stale-frame".to_string());
    state
        .iframe_sessions
        .insert("stale-frame".to_string(), "stale-session".to_string());
    let fresh_url = format!(
        "data:text/html;base64,{}",
        STANDARD.encode(
            "<!doctype html><html lang='en'><head><title>Fresh audit</title></head><body><main><h1>Fresh audit</h1></main></body></html>"
        )
    );
    let resp = execute_command(
        &json!({ "id": "5", "action": "a11y", "url": fresh_url }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(state.ref_map.get("e999").is_none());
    assert!(state.active_frame_id.is_none());
    assert!(!state.iframe_sessions.contains_key("stale-frame"));

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_a11y_preserves_nested_frame_sessions_across_tab_switches() {
    let (port, server) = start_a11y_frame_server().await;
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
            "url": format!("http://localhost:{port}/top")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert!(
        !state.iframe_sessions.is_empty(),
        "cross-origin frame should have an attached target session"
    );
    let top_iframe_sessions = state.active_iframe_sessions.clone();
    assert!(
        !top_iframe_sessions.is_empty(),
        "active frame sessions should include the top tab's cross-origin frame"
    );

    let assert_frame_violations = |resp: &Value| {
        assert_success(resp);
        let image_alt = get_data(resp)["violations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|violation| violation["id"] == "image-alt")
            .unwrap_or_else(|| {
                panic!(
                    "audit should include images in nested cross-origin frames: {}",
                    serde_json::to_string_pretty(resp).unwrap_or_default()
                )
            });
        assert_eq!(image_alt["nodeCount"], 2);
        let nodes = image_alt["nodes"].as_array().unwrap();
        assert!(nodes
            .iter()
            .any(|node| node["target"] == json!(["#outer", "#outer-image"])));
        assert!(nodes
            .iter()
            .any(|node| node["target"] == json!(["#outer", "#inner", "#inner-image"])));
    };

    let resp = execute_command(&json!({ "id": "3", "action": "a11y" }), &mut state).await;
    assert_frame_violations(&resp);

    let resp = execute_command(
        &json!({ "id": "3-selector", "action": "a11y", "selector": "main" }),
        &mut state,
    )
    .await;
    assert_frame_violations(&resp);

    // Simulate a popup created outside the tab commands. Target lifecycle
    // events must move active iframe scoping to the popup and back when it is
    // externally closed.
    let browser_client = state.browser.as_ref().unwrap().client.clone();
    let created = browser_client
        .send_command(
            "Target.createTarget",
            Some(json!({
                "url": format!("http://localhost:{port}/background")
            })),
            None,
        )
        .await
        .unwrap();
    let external_target_id = created["targetId"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let resp = execute_command(
        &json!({ "id": "external-open", "action": "tab_list" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let resp = execute_command(
        &json!({ "id": "external-loaded", "action": "tab_list" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let external_iframe_sessions = state.active_iframe_sessions.clone();
    assert!(!external_iframe_sessions.is_empty());
    assert!(top_iframe_sessions.is_disjoint(&external_iframe_sessions));

    browser_client
        .send_command(
            "Target.closeTarget",
            Some(json!({ "targetId": external_target_id })),
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let resp = execute_command(
        &json!({ "id": "external-close", "action": "tab_list" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.active_iframe_sessions, top_iframe_sessions);

    let resp = execute_command(
        &json!({
            "id": "4",
            "action": "tab_new",
            "url": format!("http://localhost:{port}/background")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let background_iframe_sessions = state.active_iframe_sessions.clone();
    assert!(
        !background_iframe_sessions.is_empty(),
        "active frame sessions should follow the newly opened tab"
    );
    assert!(top_iframe_sessions.is_disjoint(&background_iframe_sessions));
    let resp = execute_command(
        &json!({ "id": "5", "action": "tab_switch", "tabId": "t1" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.active_iframe_sessions, top_iframe_sessions);

    let resp = execute_command(&json!({ "id": "6", "action": "a11y" }), &mut state).await;
    assert_frame_violations(&resp);

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_a11y_preserves_sibling_frame_dom_order() {
    let (port, server) = start_a11y_frame_server().await;
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
            "url": format!("http://localhost:{port}/siblings")
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "3", "action": "a11y" }), &mut state).await;
    assert_success(&resp);
    let image_alt = get_data(&resp)["violations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|violation| violation["id"] == "image-alt")
        .expect("audit should report both sibling frame images");
    assert_eq!(image_alt["nodeCount"], 2);
    let targets: Vec<_> = image_alt["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|node| node["target"].clone())
        .collect();
    assert_eq!(
        targets,
        vec![
            json!(["#first-frame", "#first-image"]),
            json!(["#second-frame", "#second-image"]),
        ]
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_pushstate_changes_url() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "https://example.com/" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "pushstate", "url": "/newpath" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let url = get_data(&resp)
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(
        url.ends_with("/newpath"),
        "Expected pushstate URL to end with /newpath, got: {}",
        url
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_removeinitscript_roundtrip() {
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

    let resp = execute_command(
        &json!({
            "id": "3",
            "action": "addinitscript",
            "script": "window.__AB_ROUNDTRIP__ = 1;"
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let identifier = get_data(&resp)["identifier"]
        .as_str()
        .expect("addinitscript should return an identifier")
        .to_string();
    assert!(!identifier.is_empty());

    let resp = execute_command(
        &json!({ "id": "4", "action": "removeinitscript", "identifier": identifier }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["removed"], true);

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

// ---------------------------------------------------------------------------
// Session-to-tab binding (--pin-tab): two sessions sharing one Chrome over
// CDP must not hijack each other's tabs. A restarted daemon re-binds to its
// persisted target instead of adopting the most recently active tab, and a
// pinned session whose tab is closed gets a tab_gone error instead of
// silently acting on another session's tab.
// ---------------------------------------------------------------------------

/// Environment guard for binding tests: isolated socket dir (so binding
/// files cannot leak between test runs) plus per-session env vars.
fn binding_test_env() -> (EnvGuard<'static>, tempfile::TempDir) {
    let guard = EnvGuard::new(&[
        "AGENT_BROWSER_SOCKET_DIR",
        "XDG_RUNTIME_DIR",
        "AGENT_BROWSER_NAMESPACE",
        "AGENT_BROWSER_SESSION",
        "AGENT_BROWSER_PIN_TAB",
    ]);
    let dir = tempfile::tempdir().unwrap();
    guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
    guard.remove("XDG_RUNTIME_DIR");
    guard.remove("AGENT_BROWSER_NAMESPACE");
    guard.remove("AGENT_BROWSER_PIN_TAB");
    (guard, dir)
}

/// Launch a host Chrome and return (host_state, ws_url) for other sessions
/// to attach to over CDP.
async fn launch_binding_host(guard: &EnvGuard<'static>) -> (DaemonState, String) {
    guard.set("AGENT_BROWSER_SESSION", "e2e-bind-host");
    let mut host = DaemonState::new();
    let resp = execute_command(
        &json!({
            "id": "host-1",
            "action": "launch",
            "headless": true,
            "args": ["--no-sandbox", "--disable-dev-shm-usage"]
        }),
        &mut host,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(&json!({ "id": "host-2", "action": "cdp_url" }), &mut host).await;
    assert_success(&resp);
    let ws_url = get_data(&resp)["cdpUrl"]
        .as_str()
        .expect("cdpUrl should be a string")
        .to_string();
    (host, ws_url)
}

/// Create a pinned session attached to the shared Chrome and navigate its
/// bound tab to `url`. Returns the session's state.
async fn attach_pinned_session(
    guard: &EnvGuard<'static>,
    session: &str,
    ws_url: &str,
    url: &str,
) -> DaemonState {
    guard.set("AGENT_BROWSER_SESSION", session);
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({
            "id": format!("{}-launch", session),
            "action": "launch",
            "cdpUrl": ws_url,
            "pinTab": true
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": format!("{}-nav", session), "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    state
}

async fn current_url(state: &mut DaemonState, id: &str) -> Value {
    execute_command(&json!({ "id": id, "action": "url" }), state).await
}

fn load_binding(session: &str, expect: &str) -> super::tab_binding::TabBinding {
    super::tab_binding::load(session)
        .expect("binding file should be readable")
        .expect(expect)
}

#[tokio::test]
#[ignore]
async fn e2e_cdp_attach_survives_follow_up_launch_without_a_target() {
    let (guard, _dir) = binding_test_env();
    let (mut host, ws_url) = launch_binding_host(&guard).await;
    guard.set("AGENT_BROWSER_SESSION", "e2e-attach-sticky");
    let mut state = DaemonState::new();
    let attach = json!({ "id": "1", "action": "launch", "cdpUrl": ws_url });
    assert_success(&execute_command(&attach, &mut state).await);
    let page = "data:text/html,attached-page";
    let navigate = json!({ "id": "2", "action": "navigate", "url": page });
    assert_success(&execute_command(&navigate, &mut state).await);

    let local =
        json!({ "id": "3", "action": "launch", "headless": true, "args": ["--no-sandbox"] });
    let resp = execute_command(&local, &mut state).await;
    assert_eq!(get_data(&resp)["reused"], true, "got: {}", resp);
    assert_eq!(get_data(&current_url(&mut state, "4").await)["url"], page);

    let allowlist = json!({ "id": "5", "action": "launch", "allowedDomains": ["example.com"] });
    let resp = execute_command(&allowlist, &mut state).await;
    assert_eq!(resp["success"], false, "got: {}", resp);
    assert!(state.domain_filter.read().await.is_none());

    let _ = execute_command(&json!({ "id": "98", "action": "close" }), &mut state).await;
    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut host).await;
}

#[tokio::test]
#[ignore]
async fn e2e_cdp_attach_dead_browser_errors_instead_of_launching_locally() {
    let (guard, _dir) = binding_test_env();
    let (mut host, ws_url) = launch_binding_host(&guard).await;
    guard.set("AGENT_BROWSER_SESSION", "e2e-attach-dead");
    let mut state = DaemonState::new();
    let attach = json!({ "id": "1", "action": "launch", "cdpUrl": ws_url });
    assert_success(&execute_command(&attach, &mut state).await);
    assert_success(&execute_command(&json!({ "id": "h", "action": "close" }), &mut host).await);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let local =
        json!({ "id": "2", "action": "launch", "headless": true, "args": ["--no-sandbox"] });
    assert_eq!(execute_command(&local, &mut state).await["success"], false);
    assert_eq!(current_url(&mut state, "3").await["success"], false);
    assert!(
        state.browser.is_none(),
        "no local browser may replace the attach"
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_pin_tab_rebinds_after_daemon_restart() {
    let (guard, _dir) = binding_test_env();
    let (mut host, ws_url) = launch_binding_host(&guard).await;

    let url_a = "data:text/html,session-a-page";
    let url_b = "data:text/html,session-b-page";

    // Session A binds its own tab (pin mode creates a fresh tab on attach).
    let state_a = attach_pinned_session(&guard, "e2e-bind-a", &ws_url, url_a).await;
    let binding_a = load_binding("e2e-bind-a", "session A binding should persist");
    assert!(binding_a.pinned, "binding should record pinned=true");
    assert_eq!(
        binding_a.url, "",
        "opaque URL payloads must not be persisted in diagnostic state"
    );

    // Session B binds its own tab and navigates last, so B's tab is the most
    // recently active one (the tab a naive re-attach would adopt).
    let mut state_b = attach_pinned_session(&guard, "e2e-bind-b", &ws_url, url_b).await;
    let binding_b = load_binding("e2e-bind-b", "session B binding should persist");
    assert_ne!(
        binding_a.target_id, binding_b.target_id,
        "sessions must bind distinct tabs"
    );

    // Simulate session A's daemon dying (idle timeout, crash, kill): drop the
    // state without a clean close. The binding file survives.
    drop(state_a);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // A restarted daemon for session A must re-bind to A's original tab by
    // targetId, not adopt B's (most recently active) tab.
    guard.set("AGENT_BROWSER_SESSION", "e2e-bind-a");
    let mut state_a2 = DaemonState::new();
    assert!(
        state_a2.pin_tab,
        "pin-tab should be sticky via the persisted binding"
    );
    let resp = execute_command(
        &json!({ "id": "a2-launch", "action": "launch", "cdpUrl": ws_url }),
        &mut state_a2,
    )
    .await;
    assert_success(&resp);

    let resp = current_url(&mut state_a2, "a2-url").await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["url"],
        url_a,
        "restarted session A must operate on A's original tab"
    );
    let binding_a2 = load_binding("e2e-bind-a", "binding should still exist");
    assert_eq!(
        binding_a2.target_id, binding_a.target_id,
        "re-attach must keep the original bound target"
    );
    assert_eq!(
        binding_a2.url, "",
        "re-attach must not reintroduce the opaque URL payload"
    );

    // Lifecycle-dependent commands must work on the restored tab: attach
    // only enables the CDP domains on the first target, so the restore path
    // must enable them on the bound tab too or navigate hangs waiting for
    // Page lifecycle events (timeout guards against the hang regression).
    let url_a2 = "data:text/html,session-a-page-2";
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        execute_command(
            &json!({ "id": "a2-nav", "action": "navigate", "url": url_a2 }),
            &mut state_a2,
        ),
    )
    .await
    .expect("navigate on the restored tab must not hang");
    assert_success(&resp);
    assert_eq!(get_data(&resp)["targetId"], binding_a.target_id);

    // Session B is unaffected.
    let resp = current_url(&mut state_b, "b-url").await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], url_b);

    let resp = execute_command(&json!({ "id": "host-99", "action": "close" }), &mut host).await;
    assert_success(&resp);
}

// Real-Chrome smoke test: a foreign tab opened via `window.open` inside the
// pinned session's own page must never steal the active tab or overwrite the
// pin binding. NOTE: this does NOT specifically guard finding #3 (the
// `Target.attachedToTarget`-before-`Target.targetCreated` race) — empirically
// in this environment Chrome always delivers `Target.targetCreated` first for
// a `window.open()` popup, so this only exercises the `new_targets` drain
// path in actions.rs (~line 1082), which was never the buggy branch. The
// deterministic, race-independent regression coverage for finding #3 itself
// lives at the unit level: `BrowserManager::register_discovered_page` (the
// single decision point both drain handlers call) is exercised directly by
// `test_register_discovered_page_untracked_target_does_not_steal_pinned_tab`
// in browser.rs, which fails if that function's internal `add_page` vs.
// `add_page_without_activation` choice regresses. This e2e test is kept as a
// general non-regression smoke check against real Chrome, not as #3 proof.
#[tokio::test]
#[ignore]
async fn e2e_auto_attached_foreign_tab_does_not_steal_pinned_tab() {
    let (guard, _dir) = binding_test_env();
    let (mut host, ws_url) = launch_binding_host(&guard).await;

    let url_a = "data:text/html,pinned-session-a";
    let mut state_a = attach_pinned_session(&guard, "e2e-foreign-a", &ws_url, url_a).await;
    let binding_before = load_binding("e2e-foreign-a", "session A binding should persist");

    // Open a foreign tab in the shared browser. This is not an agent command
    // (`tab new`), it is a plain popup — the same shape as a human opening a
    // tab or a page calling `window.open`.
    let resp = execute_command(
        &json!({
            "id": "foreign-open",
            "action": "evaluate",
            "script": "window.open('data:text/html,foreign-popup'); 'opened'"
        }),
        &mut state_a,
    )
    .await;
    assert_success(&resp);

    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let resp = current_url(&mut state_a, "a-url-after-foreign").await;
    assert_success(&resp);

    let mgr = state_a.browser.as_ref().expect("browser should be running");
    let page_count_before = 1; // the pinned session's own bound tab
    assert!(
        mgr.page_count() > page_count_before,
        "the foreign popup must still register in tab list (got {} pages)",
        mgr.page_count()
    );
    assert_eq!(
        get_data(&resp)["url"],
        url_a,
        "the pinned session's active tab must not be stolen by the foreign popup"
    );
    assert_eq!(
        mgr.bound_target_id(),
        Some(binding_before.target_id.as_str()),
        "the pin binding must not be overwritten by the foreign popup"
    );

    let resp = execute_command(&json!({ "id": "host-99", "action": "close" }), &mut host).await;
    assert_success(&resp);
}

// SCRATCH (finding #3): explicit agent commands must still end up active
// even under the same auto-attach shared browser, proving the fix does not
// regress `tab new`.
#[tokio::test]
#[ignore]
async fn e2e_tab_new_still_activates_under_auto_attach() {
    let (guard, _dir) = binding_test_env();
    let (mut host, ws_url) = launch_binding_host(&guard).await;

    let url_a = "data:text/html,pinned-session-a";
    let mut state_a = attach_pinned_session(&guard, "e2e-tabnew-a", &ws_url, url_a).await;

    let resp = execute_command(
        &json!({
            "id": "tab-new",
            "action": "tab_new",
            "url": "data:text/html,agent-opened-tab"
        }),
        &mut state_a,
    )
    .await;
    assert_success(&resp);

    let resp = current_url(&mut state_a, "a-url-after-tab-new").await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["url"],
        "data:text/html,agent-opened-tab",
        "an explicit `tab new` must activate the new tab it created"
    );

    let resp = execute_command(&json!({ "id": "host-99", "action": "close" }), &mut host).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_pin_tab_gone_error_and_recovery() {
    let (guard, _dir) = binding_test_env();
    let (mut host, ws_url) = launch_binding_host(&guard).await;

    let url_a = "data:text/html,gone-session-a";
    let url_b = "data:text/html,gone-session-b";

    let mut state_a = attach_pinned_session(&guard, "e2e-gone-a", &ws_url, url_a).await;
    let binding_a = load_binding("e2e-gone-a", "session A binding should persist");
    let mut state_b = attach_pinned_session(&guard, "e2e-gone-b", &ws_url, url_b).await;

    // Session B closes A's tab by targetId (targetIds are accepted anywhere a
    // tab ref is accepted and are stable across daemons).
    let resp = execute_command(
        &json!({ "id": "b-close-a", "action": "tab_close", "tabId": binding_a.target_id }),
        &mut state_b,
    )
    .await;
    assert_success(&resp);
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Session A must now fail loudly with a machine-readable tab_gone error
    // instead of silently adopting a neighboring tab.
    let resp = current_url(&mut state_a, "a-url-gone").await;
    assert_eq!(resp["success"], false);
    assert_eq!(
        resp["code"],
        "tab_gone",
        "response should carry code=tab_gone, got: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    let err = resp["error"].as_str().unwrap_or("");
    assert!(
        err.starts_with(super::browser::TAB_GONE_PREFIX),
        "error should start with the tab_gone prefix, got: {}",
        err
    );
    assert_eq!(resp["data"]["targetId"], binding_a.target_id);
    assert!(
        resp["data"].get("lastUrl").is_none(),
        "opaque URLs must not be exposed in structured diagnostics: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );

    // Recovery commands still work in the gone state: tab_list is allowed,
    // and tab_new binds a fresh tab.
    let resp = execute_command(
        &json!({ "id": "a-list", "action": "tab_list" }),
        &mut state_a,
    )
    .await;
    assert_success(&resp);

    let url_a2 = "data:text/html,recovered-session-a";
    let resp = execute_command(
        &json!({ "id": "a-new", "action": "tab_new", "url": url_a2 }),
        &mut state_a,
    )
    .await;
    assert_success(&resp);

    let resp = current_url(&mut state_a, "a-url-recovered").await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], url_a2);

    let binding_a2 = load_binding("e2e-gone-a", "binding should be rewritten");
    assert_ne!(
        binding_a2.target_id, binding_a.target_id,
        "recovery must bind a new target"
    );
    assert!(binding_a2.pinned, "recovered binding stays pinned");

    // Session B keeps working on its own tab throughout.
    let resp = current_url(&mut state_b, "b-url-after").await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], url_b);

    let resp = execute_command(&json!({ "id": "host-99", "action": "close" }), &mut host).await;
    assert_success(&resp);
}

/// A multiselect locator miss must surface the anchored "No element found"
/// guidance, not a raw "Evaluation error: ...". Guards the handler wiring end to
/// end: a unit test of the mapping alone stays green if the handler stops routing
/// misses through it. Force-red: drop the sentinel miss handling in
/// handle_multiselect and this assertion fails on the raw evaluate error.
#[tokio::test]
#[ignore]
async fn e2e_multiselect_miss_surfaces_anchored_error() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // about:blank has no #picker, so the selector misses.
    let resp = execute_command(
        &json!({ "id": "2", "action": "multiselect", "selector": "#picker", "values": ["a"] }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "multiselect on a missing selector should error: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );
    let err = resp.get("error").and_then(|v| v.as_str()).unwrap_or("");
    // Assert the full anchored shape, not just the guidance suffix: the miss must
    // carry "No element found", retain the "#picker" selector detail, and end with
    // the locator-miss guidance. A generic suffix-only check would pass even if the
    // detail were dropped or a different classifier produced the guidance.
    assert!(
        err.starts_with("No element found") && err.contains("#picker"),
        "miss should keep the anchored shape and selector detail, got: {err}"
    );
    assert!(
        err.contains("Verify the selector, role, or name is correct"),
        "miss should surface the anchored locator-miss guidance, got: {err}"
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

/// An earlier valid ARIA token in a multi-token role attribute is the operative
/// role, so `role="mark none"` is a mark and must not answer `find role none`.
/// Force-red: drop `mark` from the presentational VALID_ROLES set and the query
/// matches this element, so the miss assertion fails.
#[tokio::test]
#[ignore]
async fn e2e_presentational_role_respects_earlier_operative_token() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let html = "<html><body><span role='mark none'>marked</span></body></html>";
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "getbyrole", "role": "none", "subaction": "text" }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "operative role is `mark`, so `find role none` must not match: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

/// `find role directory` must match an explicit `role="directory"` element but
/// not an ordinary list. Chrome collapses `directory` into the `list` AX role,
/// so this goes through the DOM-attribute path, not the AX tree. Force-red:
/// route `directory` back through the AX tree and the explicit element is missed
/// (its AX role is `list`), so the found-text assertion fails.
#[tokio::test]
#[ignore]
async fn e2e_find_role_directory_matches_only_explicit_attribute() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // A plain list must not be matched by `find role directory`.
    let plain = "data:text/html;base64,".to_string()
        + &STANDARD.encode("<html><body><ul><li>item</li></ul></body></html>");
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": plain }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "3", "action": "getbyrole", "role": "directory", "subaction": "text" }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "a plain <ul> must not match `find role directory`: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );

    // An explicit role="directory" element is found.
    let explicit = "data:text/html;base64,".to_string()
        + &STANDARD.encode("<html><body><div role='directory'>DIR</div></body></html>");
    let resp = execute_command(
        &json!({ "id": "4", "action": "navigate", "url": explicit }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "5", "action": "getbyrole", "role": "directory", "subaction": "text" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "DIR");

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

/// The presentational DOM lookup must honor the selected frame, not always the
/// top document. Force-red: revert the frame dispatch in the presentational path
/// (search the top document only) and the in-frame element is missed.
#[tokio::test]
#[ignore]
async fn e2e_presentational_role_honors_selected_frame() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Top document has no role="none"; the same-origin iframe does. `srcdoc`
    // inherits the parent origin, so the child is same-origin (the path this fix
    // targets) without needing a server; a `data:` child would be cross-origin.
    let outer = "<body><iframe id='f' srcdoc=\"<div role='none'>INSIDE</div>\"></iframe></body>";
    let url = format!("data:text/html;base64,{}", STANDARD.encode(outer));
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Before selecting the frame, the top document has no match.
    let resp = execute_command(
        &json!({ "id": "3", "action": "getbyrole", "role": "none", "subaction": "text" }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "top document has no role=none: {}",
        serde_json::to_string_pretty(&resp).unwrap_or_default()
    );

    // Select the frame; the presentational lookup must now find the frame element.
    let resp = execute_command(
        &json!({ "id": "4", "action": "frame", "selector": "#f" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "5", "action": "getbyrole", "role": "none", "subaction": "text" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "INSIDE");

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

/// ARIA presentational-roles conflict resolution: role="none" on a focusable
/// element (or one with global ARIA props) is ignored, so `find role none` must
/// skip it but still match a truly presentational element. Force-red: drop the
/// conflict check and the `<button role="none">` is matched.
#[tokio::test]
#[ignore]
async fn e2e_presentational_role_respects_conflict_resolution() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // Each non-matching element triggers conflict resolution a different way:
    // native focusable, explicit tabindex=-1 (programmatically focusable), and a
    // global ARIA property. Only the last, truly presentational div must match.
    let html = concat!(
        "<body>",
        "<button role='none'>NativeFocusable</button>",
        "<div role='none' tabindex='-1'>TabindexFocusable</div>",
        "<div role='none' aria-label='named'>GlobalAria</div>",
        "<div role='none'>Plain</div>",
        "</body>"
    );
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    // The focusable button keeps its implicit role, so the match must be the div.
    let resp = execute_command(
        &json!({ "id": "3", "action": "getbyrole", "role": "none", "subaction": "text" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["text"],
        "Plain",
        "role=none on a focusable button must be ignored (conflict resolution)"
    );

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

/// `find role document` must match the page root. Chrome exposes it as
/// `RootWebArea`; without the normalization to `document` the query misses.
/// End-to-end guard for that mapping (the unit test only checks the string).
#[tokio::test]
#[ignore]
async fn e2e_find_role_document_matches_root() {
    let mut state = DaemonState::new();

    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let url = format!(
        "data:text/html;base64,{}",
        STANDARD.encode("<title>T</title><body>hi</body>")
    );
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": url }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(
        &json!({ "id": "3", "action": "getbyrole", "role": "document", "subaction": "text" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_mouse_interpolation_starts_at_last_element_interaction() {
    let mut state = DaemonState::new();
    assert_success(
        &execute_command(
            &json!({"id": "1", "action": "launch", "headless": true}),
            &mut state,
        )
        .await,
    );
    assert_success(&execute_command(&json!({"id": "2", "action": "setcontent", "html": "<button id='b' style='position:absolute;left:400px;top:300px;width:100px;height:40px'>Target</button><input id='c' type='checkbox' style='position:absolute;left:200px;top:200px'>"}), &mut state).await);
    for action in ["click", "hover", "dblclick", "check", "uncheck"] {
        let selector = if matches!(action, "check" | "uncheck") {
            "#c"
        } else {
            "#b"
        };
        assert_success(
            &execute_command(
                &json!({"id": "3", "action": action, "selector": selector}),
                &mut state,
            )
            .await,
        );
        let start = (state.mouse_state.x, state.mouse_state.y);
        assert!(start.0 >= 200.0 && start.1 >= 200.0, "{action}: {start:?}");
        assert_success(&execute_command(&json!({"id": "4", "action": "evaluate", "script": "window.moves=[];document.onmousemove=e=>moves.push([e.clientX,e.clientY]);"}), &mut state).await);
        assert_success(&execute_command(&json!({"id": "5", "action": "mousemove", "x": start.0 + 100.0, "y": start.1, "steps": 2}), &mut state).await);
        let result = execute_command(
            &json!({"id": "6", "action": "evaluate", "script": "moves"}),
            &mut state,
        )
        .await;
        assert_success(&result);
        let moves = get_data(&result)["result"].as_array().unwrap();
        assert_eq!(moves.len(), 2, "{action}: {moves:?}");
        assert!((moves[0][0].as_f64().unwrap() - (start.0 + 50.0)).abs() <= 1.0);
        assert!((moves[1][0].as_f64().unwrap() - (start.0 + 100.0)).abs() <= 1.0);
    }
    assert_success(&execute_command(&json!({"id": "99", "action": "close"}), &mut state).await);
}

// ---------------------------------------------------------------------------
// Wedged page recovery (#1713)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_wedged_page_is_replaced_and_session_recovers() {
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<title>spin</title>spin" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let tabs_before = state.browser.as_ref().unwrap().tab_list().len();

    let started = std::time::Instant::now();
    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": "while(true){}" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false, "wedging eval must fail: {}", resp);
    assert_eq!(resp["code"], "page_wedged", "got: {}", resp);
    assert_eq!(resp["data"]["replaced"], false, "first strike only reports");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(40),
        "wedge must surface within one CDP timeout plus probe, took {:?}",
        started.elapsed()
    );

    // Second strike on the same tab replaces it.
    let resp = execute_command(&json!({ "id": "3b", "action": "url" }), &mut state).await;
    assert_eq!(
        resp["data"]["replaced"], true,
        "second strike replaces: {}",
        resp
    );
    assert_eq!(
        state.browser.as_ref().unwrap().tab_list().len(),
        tabs_before
    );

    // The client's read timeout can expire before the replacement finishes.
    // A retry of the same request gets the report, not the blank tab's URL.
    let resp = execute_command(&json!({ "id": "3b", "action": "url" }), &mut state).await;
    assert_eq!(
        resp["code"], "page_wedged",
        "retry replays the report: {}",
        resp
    );

    // A different command runs on the replacement and carries the report once.
    let started = std::time::Instant::now();
    let resp = execute_command(&json!({ "id": "4", "action": "url" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["url"], "about:blank");
    assert!(resp["warning"]
        .as_str()
        .unwrap_or("")
        .starts_with("page_wedged:"));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let resp = execute_command(&json!({ "id": "5", "action": "url" }), &mut state).await;
    assert!(
        resp.get("warning").is_none(),
        "report is shown once: {}",
        resp
    );

    let resp = execute_command(
        &json!({ "id": "6", "action": "navigate", "url": "data:text/html,<title>ok</title>ok" }),
        &mut state,
    )
    .await;
    assert_success(&resp);

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

async fn launch_wedge_session(state: &mut DaemonState) {
    for cmd in [
        json!({ "id": "1", "action": "launch", "headless": true }),
        json!({ "id": "2", "action": "navigate", "url": "data:text/html,<title>spin</title>spin" }),
    ] {
        assert_success(&execute_command(&cmd, state).await);
    }
}

/// Diagnostics do not touch the renderer, so they must not clear a strike.
#[tokio::test]
#[ignore]
async fn e2e_diagnostics_between_wedge_strikes_keep_the_strike() {
    let mut state = DaemonState::new();
    launch_wedge_session(&mut state).await;
    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": "while(true){}" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["data"]["replaced"], false, "first strike: {}", resp);
    for (id, action) in [("4", "tab_list"), ("5", "session_info")] {
        assert_success(&execute_command(&json!({ "id": id, "action": action }), &mut state).await);
    }
    let resp = execute_command(&json!({ "id": "6", "action": "url" }), &mut state).await;
    assert_eq!(
        resp["data"]["replaced"], true,
        "second strike replaces: {}",
        resp
    );
    execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

/// A timeout whose probe answers is a slow command, not a wedge, so it
/// clears the earlier strike.
#[tokio::test]
#[ignore]
async fn e2e_responsive_probe_clears_an_earlier_strike() {
    let mut state = DaemonState::new();
    launch_wedge_session(&mut state).await;
    // Blocks past the CDP timeout and the probe, then yields.
    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate",
                 "script": "const t = Date.now() + 36000; while (Date.now() < t) {}" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["data"]["replaced"], false, "first strike: {}", resp);
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    // Times out while the renderer answers.
    let resp = execute_command(
        &json!({ "id": "4", "action": "evaluate", "script": "new Promise(() => {})" }),
        &mut state,
    )
    .await;
    assert_ne!(
        resp["code"], "page_wedged",
        "a responsive page is not wedged: {}",
        resp
    );
    let resp = execute_command(
        &json!({ "id": "5", "action": "evaluate", "script": "while(true){}" }),
        &mut state,
    )
    .await;
    assert_eq!(
        resp["data"]["replaced"], false,
        "a new hang starts over: {}",
        resp
    );
    execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_wedged_page_replacement_keeps_session_network_controls() {
    let (base_url, server) = start_echo_server().await;
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
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

    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": "while(true){}" }),
        &mut state,
    )
    .await;
    assert_eq!(resp["code"], "page_wedged", "got: {}", resp);
    let resp = execute_command(&json!({ "id": "3b", "action": "url" }), &mut state).await;
    assert_eq!(resp["data"]["replaced"], true, "got: {}", resp);

    let resp = execute_command(
        &json!({ "id": "4", "action": "navigate", "url": format!("{}/second", base_url) }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({
            "id": "5", "action": "evaluate",
            "script": "JSON.parse(document.body.innerText)",
        }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["result"]["headers"]["X-Test"],
        "scoped",
        "the replacement tab must carry the session's origin headers"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
    server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_dialog_blocked_tab_is_not_treated_as_wedged() {
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<title>live</title>live" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let target_before = state
        .browser
        .as_ref()
        .unwrap()
        .active_target_id()
        .unwrap()
        .to_string();

    let resp = execute_command(
        &json!({ "id": "3", "action": "evaluate", "script": "confirm('keep me')" }),
        &mut state,
    )
    .await;
    assert_ne!(
        resp["code"], "page_wedged",
        "a dialog is not a wedge: {}",
        resp
    );
    assert_eq!(
        state.browser.as_ref().unwrap().active_target_id().unwrap(),
        target_before,
        "the dialog-blocked tab must survive"
    );

    let resp = execute_command(
        &json!({ "id": "4", "action": "dialog", "response": "dismiss" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(&json!({ "id": "5", "action": "title" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["title"], "live");

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore]
async fn e2e_slow_finite_script_keeps_its_tab() {
    let mut state = DaemonState::new();
    let resp = execute_command(
        &json!({ "id": "1", "action": "launch", "headless": true }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let resp = execute_command(
        &json!({ "id": "2", "action": "navigate", "url": "data:text/html,<title>slow</title>slow" }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    let target_before = state
        .browser
        .as_ref()
        .unwrap()
        .active_target_id()
        .unwrap()
        .to_string();

    // Busy for longer than the CDP timeout plus the probe, then yields.
    let resp = execute_command(
        &json!({
            "id": "3", "action": "evaluate",
            "script": "const t = Date.now(); while (Date.now() - t < 36000) {}",
        }),
        &mut state,
    )
    .await;
    assert_eq!(resp["code"], "page_wedged", "got: {}", resp);
    assert_eq!(resp["data"]["replaced"], false);

    let resp = execute_command(&json!({ "id": "4", "action": "title" }), &mut state).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["title"], "slow");
    assert_eq!(
        state.browser.as_ref().unwrap().active_target_id().unwrap(),
        target_before,
        "a page that recovers on its own keeps its tab"
    );

    let resp = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    assert_success(&resp);
}

// ---------------------------------------------------------------------------
// wait --download (#561, #1300)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn e2e_wait_download_claims_a_finished_download_and_moves_it() {
    let dir = tempfile::tempdir().unwrap();
    let download_dir = dir.path().join("downloads");
    let dest = dir.path().join("out").join("report.bin");
    let mut state = DaemonState::new();

    for cmd in [
        json!({ "id": "1", "action": "launch", "headless": true,
                "downloadPath": download_dir.to_string_lossy() }),
        json!({ "id": "2", "action": "navigate", "url":
                "data:text/html,<a id=dl download=report.bin href='data:application/octet-stream,report'>dl</a>" }),
        json!({ "id": "3", "action": "evaluate", "script": "document.getElementById('dl').click()" }),
    ] {
        assert_success(&execute_command(&cmd, &mut state).await);
    }

    // Let the download finish before the wait arrives, which used to time out.
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

    let resp = execute_command(
        &json!({ "id": "4", "action": "waitfordownload", "path": dest.to_string_lossy(), "timeout": 5000 }),
        &mut state,
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["path"], json!(dest.to_string_lossy()));
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "report");
    assert!(!saved.exists(), "the file is moved, not copied");

    let resp = execute_command(
        &json!({ "id": "5", "action": "waitfordownload", "timeout": 1000 }),
        &mut state,
    )
    .await;
    assert_eq!(resp["success"], false, "a download is claimed only once");

    execute_command(&json!({ "id": "6", "action": "close" }), &mut state).await;
}

async fn start_raw_http_server(response: String) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (origin, handle)
}

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

/// Boxed so the command future lives on the heap; a long test would
/// otherwise overflow the stack.
fn exec<'a>(
    cmd: &'a Value,
    state: &'a mut DaemonState,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + 'a>> {
    Box::pin(execute_command(cmd, state))
}

#[tokio::test]
#[ignore]
async fn e2e_state_load_keeps_storage_away_from_page_scripts_and_redirects() {
    let (other_origin, other_server) =
        start_raw_http_server(html_response("<title>other</title>")).await;
    let (redirect_origin, redirect_server) = start_raw_http_server(format!(
        "HTTP/1.1 302 Found\r\nLocation: {}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        other_origin
    ))
    .await;
    let (hook_origin, hook_server) = start_raw_http_server(html_response(
        "<script>
            window.__captured = [];
            const parse = JSON.parse;
            JSON.parse = function (...args) { window.__captured.push(args); return parse.apply(this, args); };
            const setItem = Storage.prototype.setItem;
            Storage.prototype.setItem = function (...args) { window.__captured.push(args); return setItem.apply(this, args); };
        </script>",
    ))
    .await;

    let temp_dir = tempfile::tempdir().unwrap();
    let state_path = temp_dir.path().join("state.json");
    std::fs::write(
        &state_path,
        serde_json::to_vec(&json!({
            "cookies": [],
            "origins": [
                {
                    "origin": redirect_origin,
                    "localStorage": [{ "name": "redirect-token", "value": "redirect-secret" }],
                    "sessionStorage": [{ "name": "redirect-session", "value": "redirect-secret" }]
                },
                {
                    "origin": hook_origin,
                    "localStorage": [{ "name": "hook-token", "value": "hook-secret" }],
                    "sessionStorage": [{ "name": "hook-session", "value": "hook-secret" }]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut state = DaemonState::new();
    assert_success(
        &execute_command(
            &json!({ "id": "1", "action": "launch", "headless": true }),
            &mut state,
        )
        .await,
    );
    let load = execute_command(
        &json!({ "id": "2", "action": "state_load", "path": state_path }),
        &mut state,
    )
    .await;
    assert_success(&load);
    assert_eq!(get_data(&load)["skippedOrigins"], json!([redirect_origin]));

    assert_evaluate(&mut state, "3", "window.__captured.length", json!(0)).await;
    assert_evaluate(
        &mut state,
        "4",
        "[localStorage.getItem('hook-token'), sessionStorage.getItem('hook-session')]",
        json!(["hook-secret", "hook-secret"]),
    )
    .await;

    assert_success(
        &execute_command(
            &json!({ "id": "5", "action": "navigate", "url": format!("{}/", other_origin) }),
            &mut state,
        )
        .await,
    );
    assert_evaluate(
        &mut state,
        "6",
        "localStorage.length + sessionStorage.length",
        json!(0),
    )
    .await;

    let _ = execute_command(&json!({ "id": "99", "action": "close" }), &mut state).await;
    other_server.abort();
    redirect_server.abort();
    hook_server.abort();
}

fn required_firefox_binary() -> String {
    let path = std::env::var("FIREFOX_BIN")
        .expect("FIREFOX_BIN is required for explicitly invoked Firefox E2E verification");
    assert!(!path.trim().is_empty(), "FIREFOX_BIN must not be empty");
    assert!(
        std::path::Path::new(&path).is_file(),
        "FIREFOX_BIN must point to an existing executable: {path}"
    );
    path
}

type FirefoxHits = Arc<Mutex<Vec<String>>>;

struct FirefoxFixture {
    url: String,
    other: String,
    hits: FirefoxHits,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

impl FirefoxFixture {
    fn hit_count(&self, needle: &str) -> usize {
        self.hits
            .lock()
            .unwrap()
            .iter()
            .filter(|hit| hit.contains(needle))
            .count()
    }
}

impl Drop for FirefoxFixture {
    fn drop(&mut self) {
        for server in &self.servers {
            server.abort();
        }
    }
}

fn firefox_serve(
    listener: tokio::net::TcpListener,
    other: String,
    hits: FirefoxHits,
) -> tokio::task::JoinHandle<()> {
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).await.unwrap_or(0);
            let head = String::from_utf8_lossy(&request[..n]);
            let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
            hits.lock().unwrap().push(format!("{port}{path}"));
            if head.to_ascii_lowercase().contains("\r\nauthorization:") {
                hits.lock()
                    .unwrap()
                    .push(format!("{port}{path}#authorization"));
            }
            if path == "/har-post" {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            }
            if path == "/stall" {
                let head = "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"conftest.py\"\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(head.as_bytes()).await;
                let _ = stream.write_all(b"partial").await;
                tokio::time::sleep(std::time::Duration::from_secs(90)).await;
                continue;
            }
            let status = if path == "/auth" {
                "401 Unauthorized"
            } else {
                "200 OK"
            };
            let (content_type, extra, body) = match path.as_str() {
                "/" => (
                    "text/html; charset=utf-8",
                    "",
                    include_str!("test_fixtures/firefox_probe.html").replace("__OTHER__", &other),
                ),
                "/frame.html" => (
                    "text/html; charset=utf-8",
                    "",
                    include_str!("test_fixtures/firefox_frame.html").to_string(),
                ),
                "/forms.html" => (
                    "text/html; charset=utf-8",
                    "",
                    include_str!("test_fixtures/firefox_forms.html").replace("__OTHER__", &other),
                ),
                "/forms-frame.html" => (
                    "text/html; charset=utf-8",
                    "",
                    include_str!("test_fixtures/firefox_forms_frame.html").to_string(),
                ),
                "/html5_drag_probe.html" => (
                    "text/html; charset=utf-8",
                    "",
                    include_str!("test_fixtures/html5_drag_probe.html").to_string(),
                ),
                "/echo" => {
                    let headers: serde_json::Map<String, Value> = head
                        .lines()
                        .skip(1)
                        .filter_map(|line| line.split_once(':'))
                        .map(|(name, value)| (name.to_string(), json!(value.trim())))
                        .collect();
                    (
                        "application/json",
                        "Access-Control-Allow-Origin: *\r\n",
                        json!({ "headers": headers }).to_string(),
                    )
                }
                "/api" => ("text/plain", "", "real".to_string()),
                "/auth" => (
                    "text/plain",
                    "WWW-Authenticate: Basic realm=\"origin\"\r\n",
                    "denied".to_string(),
                ),
                "/download.txt" => (
                    "text/plain",
                    "Content-Disposition: attachment; filename=\"download.txt\"\r\n",
                    "downloaded".to_string(),
                ),
                "/payload" => (
                    "text/plain",
                    "Content-Disposition: attachment; filename=\"conftest.py\"\r\n",
                    "page chose this name".to_string(),
                ),
                _ => ("text/plain", "", "ok".to_string()),
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    })
}

/// Serves the probe page on `localhost` and a second origin on `127.0.0.1`.
async fn firefox_fixture() -> FirefoxFixture {
    let main = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let side = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://localhost:{}/", main.local_addr().unwrap().port());
    let other = format!("http://127.0.0.1:{}", side.local_addr().unwrap().port());
    let hits: FirefoxHits = Arc::default();
    let servers = vec![
        firefox_serve(main, other.clone(), hits.clone()),
        firefox_serve(side, other.clone(), hits.clone()),
    ];
    FirefoxFixture {
        url,
        other,
        hits,
        servers,
    }
}

async fn firefox_command(state: &mut DaemonState, command: Value) -> Value {
    tokio::time::timeout(
        tokio::time::Duration::from_secs(60),
        execute_command(&command, state),
    )
    .await
    .expect("Firefox command must not hang")
}

async fn firefox_cookies_get(state: &mut DaemonState, urls: Option<Value>) -> Value {
    let mut command = json!({ "id": "cookies", "action": "cookies_get" });
    if let Some(urls) = urls {
        command["urls"] = urls;
    }
    let resp = firefox_command(state, command).await;
    assert_success(&resp);
    get_data(&resp)["cookies"].clone()
}

fn has_cookie(cookies: &Value, name: &str) -> bool {
    cookies
        .as_array()
        .is_some_and(|cookies| cookies.iter().any(|cookie| cookie["name"] == name))
}

fn firefox_test_environment(tmp: &std::path::Path) -> EnvGuard<'static> {
    const KEYS: [&str; 17] = [
        "AGENT_BROWSER_CDP",
        "AGENT_BROWSER_AUTO_CONNECT",
        "AGENT_BROWSER_PROVIDER",
        "AGENT_BROWSER_ENGINE",
        "AGENT_BROWSER_EXECUTABLE_PATH",
        "AGENT_BROWSER_PROXY",
        "AGENT_BROWSER_PROXY_BYPASS",
        "AGENT_BROWSER_PROXY_USERNAME",
        "AGENT_BROWSER_PROXY_PASSWORD",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ];
    let env = EnvGuard::new(
        &[
            &KEYS[..],
            &["TMPDIR", "HOME", "AGENT_BROWSER_ENCRYPTION_KEY"],
        ]
        .concat(),
    );
    for key in KEYS {
        env.remove(key);
    }
    env.set("TMPDIR", tmp.to_str().unwrap());
    env
}

/// True when any process command line mentions `dir`, which is how an owned Firefox shows up.
fn firefox_processes_using(dir: &std::path::Path) -> bool {
    std::process::Command::new("pgrep")
        .args(["-f", dir.to_str().unwrap()])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

async fn launch_firefox(state: &mut DaemonState, extra: Value) {
    let mut request = json!({
        "id": "launch", "action": "launch", "headless": true,
        "engine": "firefox", "executablePath": required_firefox_binary(),
    });
    request
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let resp = firefox_command(state, request).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["launched"], true);
    assert_eq!(state.engine, "firefox");
}

async fn firefox_open(state: &mut DaemonState, url: &str) {
    let resp = firefox_command(
        state,
        json!({ "id": "nav", "action": "navigate", "url": url }),
    )
    .await;
    assert_success(&resp);
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_snapshot_refs_cursor_interactive() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        r#"document.body.innerHTML = '<div onclick="window.__clicked = true">Clickable div</div>'
          + '<span style="cursor:pointer">Pointer span</span>'
          + '<div tabindex="0">Focusable div</div>'
          + '<label style="cursor:pointer">Sample card<input type="radio" style="display:none"></label>'; 0"#,
    )
    .await;

    let resp = firefox_command(
        &mut state,
        json!({ "id": "snap", "action": "snapshot", "interactive": true }),
    )
    .await;
    assert_success(&resp);
    let tree = get_data(&resp)["snapshot"].as_str().unwrap().to_string();
    let ref_for = |text: &str| {
        let line = tree
            .lines()
            .find(|line| line.contains(&format!("{text:?}")))
            .unwrap_or_else(|| panic!("no snapshot line for {text}: {tree}"));
        let id: String = line
            .split("ref=")
            .nth(1)
            .unwrap_or_else(|| panic!("{text} has no ref: {tree}"))
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        assert!(!id.is_empty(), "{text} has no ref: {tree}");
        format!("@{id}")
    };
    let clickable = ref_for("Clickable div");
    ref_for("Pointer span");
    ref_for("Focusable div");
    ref_for("Sample card");

    assert_success(&firefox_act(&mut state, "click", &clickable, json!({})).await);
    assert_eq!(
        firefox_eval(&mut state, "window.__clicked === true").await,
        true
    );
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_role_selector_semantics() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;

    let html = concat!(
        "<html><body>",
        "<button>Save changes</button><button>Save</button>",
        "<img alt='Brand' width='20' height='20' src='data:image/gif;base64,R0lGODlhAQABAAD/ACwAAAAAAQABAAACADs=' onclick='window.imageClicks++'>",
        "<span role='mark none'>Wrong</span><span role='none mark'>Right</span>",
        "<div id='not-select'>Not a select</div>",
        "<script>window.imageClicks = 0</script>",
        "</body></html>"
    );
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    firefox_open(&mut state, &url).await;

    for (id, name, exact, expected) in [
        ("substring", "save", false, "Save changes"),
        ("exact", "Save", true, "Save"),
    ] {
        let resp = firefox_command(
            &mut state,
            json!({
                "id": id, "action": "getbyrole", "role": "button", "name": name,
                "exact": exact, "subaction": "text"
            }),
        )
        .await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["text"], expected);
    }

    for role in ["image", "img"] {
        let resp = firefox_command(
            &mut state,
            json!({
                "id": role, "action": "getbyrole", "role": role, "name": "Brand",
                "exact": true, "subaction": "click"
            }),
        )
        .await;
        assert_success(&resp);
    }
    assert_eq!(
        firefox_eval(&mut state, "window.imageClicks").await,
        json!(2)
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "presentational", "action": "getbyrole", "role": "none", "subaction": "text" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["text"], "Right");

    let resp = firefox_command(
        &mut state,
        json!({ "id": "select", "action": "select", "selector": "#not-select", "values": ["x"] }),
    )
    .await;
    let error = resp["error"].as_str().unwrap_or("");
    assert_eq!(resp["success"], false, "{resp}");
    assert!(error.contains("not a <select>"), "{error}");

    let resp = firefox_command(
        &mut state,
        json!({ "id": "xpath", "action": "getattribute", "selector": "xpath=[", "attribute": "id" }),
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    let error = resp["error"].as_str().unwrap_or("");
    assert!(error.contains("xpath=["), "{error}");
    assert!(!error.contains("XPath error"), "{error}");

    firefox_close(&mut state).await;
}

async fn firefox_eval(state: &mut DaemonState, script: &str) -> Value {
    let resp = firefox_command(
        state,
        json!({ "id": "eval", "action": "evaluate", "script": script }),
    )
    .await;
    assert_success(&resp);
    get_data(&resp)["result"].clone()
}

async fn firefox_title(state: &mut DaemonState) -> String {
    let resp = firefox_command(state, json!({ "id": "title", "action": "title" })).await;
    assert_success(&resp);
    get_data(&resp)["title"].as_str().unwrap().to_string()
}

async fn firefox_tab_list(state: &mut DaemonState) -> Vec<Value> {
    let resp = firefox_command(state, json!({ "id": "tabs", "action": "tab_list" })).await;
    assert_success(&resp);
    get_data(&resp)["tabs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

async fn firefox_switch_to_title(state: &mut DaemonState, title: &str) {
    let tabs = firefox_tab_list(state).await;
    let tab_id = tabs
        .iter()
        .find(|tab| tab["title"] == title)
        .and_then(|tab| tab["tabId"].as_str())
        .unwrap_or_else(|| panic!("tab_list must include {title:?}: {tabs:?}"));
    let resp = firefox_command(
        state,
        json!({ "id": "switch", "action": "tab_switch", "tabId": tab_id }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(firefox_title(state).await, title);
}

async fn firefox_ref(state: &mut DaemonState, name: &str) -> String {
    let resp = firefox_command(
        state,
        json!({ "id": "snap", "action": "snapshot", "interactive": true }),
    )
    .await;
    assert_success(&resp);
    get_data(&resp)["refs"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, node)| node["name"] == name)
        .map(|(ref_id, _)| format!("@{}", ref_id.trim_start_matches('@')))
        .unwrap_or_else(|| panic!("no ref named {name} in {resp}"))
}

async fn firefox_act(state: &mut DaemonState, action: &str, selector: &str, extra: Value) -> Value {
    let mut request = json!({ "id": action, "action": action, "selector": selector });
    request
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    firefox_command(state, request).await
}

async fn firefox_close(state: &mut DaemonState) {
    let resp = firefox_command(state, json!({ "id": "close", "action": "close" })).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["closed"], true);
}

/// A Firefox the test started itself, so the daemon has to attach rather than launch it.
struct RunningFirefox {
    child: bidi_firefox::Child,
    _profile: tempfile::TempDir,
    bidi_port: u16,
    devtools_port: u16,
}

impl RunningFirefox {
    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for RunningFirefox {
    fn drop(&mut self) {
        bidi_firefox::kill_tree(&mut self.child);
    }
}

fn available_port(exclude: Option<u16>) -> u16 {
    loop {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        if Some(port) != exclude {
            return port;
        }
    }
}

fn start_running_firefox(pages: &[&str]) -> RunningFirefox {
    let bidi_port = available_port(None);
    let devtools_port = available_port(Some(bidi_port));
    let profile = tempfile::tempdir().unwrap();
    std::fs::write(
        profile.path().join("user.js"),
        "user_pref(\"devtools.debugger.remote-enabled\", true);\n\
         user_pref(\"devtools.chrome.enabled\", true);\n\
         user_pref(\"devtools.debugger.prompt-connection\", false);\n",
    )
    .unwrap();
    let mut args = bidi_firefox::base_args(true);
    args.extend([
        "--remote-debugging-port".into(),
        bidi_port.to_string(),
        "--start-debugger-server".into(),
        devtools_port.to_string(),
        "-profile".into(),
        profile.path().to_string_lossy().into_owned(),
        "-no-remote".into(),
    ]);
    args.extend(pages.iter().map(|page| (*page).to_string()));
    let child = bidi_firefox::spawn(
        std::path::Path::new(&required_firefox_binary()),
        &args,
        true,
    )
    .expect("Firefox must start");
    let mut firefox = RunningFirefox {
        child,
        _profile: profile,
        bidi_port,
        devtools_port,
    };
    let stderr = firefox
        .child
        .stderr
        .take()
        .expect("Firefox stderr is piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        let mut ready = false;
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            if !ready && line.contains("WebDriver BiDi listening on") {
                ready = true;
                let _ = tx.send(());
            }
        }
    });
    rx.recv_timeout(std::time::Duration::from_secs(30))
        .expect("Firefox must report WebDriver BiDi readiness");
    firefox
}

/// Attaching over WebDriver BiDi uses the running Firefox, keeps refs and snapshots working, and never owns the process.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_attach_to_running_firefox() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let forms_url = format!("{}forms.html", fixture.url);
    let mut firefox = start_running_firefox(&[&fixture.url, &forms_url]);
    let mut state = DaemonState::new();

    let target = format!(
        "http://127.0.0.1:{}?devtools={}",
        firefox.bidi_port, firefox.devtools_port
    );
    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.engine, "firefox");

    for _ in 0..100 {
        let tabs = firefox_tab_list(&mut state).await;
        let titles: Vec<&str> = tabs
            .iter()
            .filter_map(|tab| tab["title"].as_str())
            .collect();
        if ["Firefox adapter probe", "Firefox forms"]
            .iter()
            .all(|title| titles.contains(title))
        {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }
    for title in ["Firefox forms", "Firefox adapter probe"] {
        firefox_switch_to_title(&mut state, title).await;
    }

    firefox_open(&mut state, &fixture.url).await;
    let name = firefox_ref(&mut state, "Name").await;
    let active_title = firefox_title(&mut state).await;
    let resp = firefox_command(
        &mut state,
        json!({ "id": "repeat-attach", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["reused"],
        true,
        "repeating the same Firefox target must reuse its live session: {resp}"
    );
    assert_eq!(firefox_title(&mut state).await, active_title);
    assert_success(&firefox_act(&mut state, "fill", &name, json!({ "value": "Ada" })).await);
    let go = firefox_ref(&mut state, "Go").await;
    assert_success(&firefox_act(&mut state, "click", &go, json!({})).await);
    assert_eq!(firefox_title(&mut state).await, "Hello Ada");

    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach-again", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["relaunchedBrowser"],
        false,
        "attaching to the same target must reuse the active session: {resp}"
    );
    firefox_switch_to_title(&mut state, "Hello Ada").await;

    firefox_close(&mut state).await;
    assert!(
        firefox.alive(),
        "close must not stop a Firefox the daemon attached to"
    );

    let ws_target = format!(
        "ws://127.0.0.1:{}/session?devtools={}",
        firefox.bidi_port, firefox.devtools_port
    );
    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach-ws", "action": "launch", "cdpUrl": ws_target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.engine, "firefox");
    firefox_switch_to_title(&mut state, "Hello Ada").await;
    firefox_close(&mut state).await;

    let wrong = available_port(Some(firefox.devtools_port));
    let target = format!("http://127.0.0.1:{}?devtools={}", firefox.bidi_port, wrong);
    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach-bad-devtools", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    let error = resp["error"].as_str().expect("attach must report an error");
    assert!(
        error.contains(&format!(
            "Firefox DevTools server is not reachable at 127.0.0.1:{wrong}"
        )),
        "{resp}"
    );
    assert!(
        error.contains(&format!("--start-debugger-server {wrong}")),
        "{resp}"
    );
    assert!(!error.contains("default"), "{resp}");
    assert!(!error.contains("?devtools="), "{resp}");

    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach-after-failure", "action": "launch", "cdpUrl": ws_target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.engine, "firefox");
    firefox_switch_to_title(&mut state, "Hello Ada").await;
    firefox_close(&mut state).await;
}

/// `--user-agent` reaches an attached Firefox in place, and `close` puts Firefox's own user agent back.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_attach_user_agent_applies_in_place_and_is_reset() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut firefox = start_running_firefox(&[&fixture.url]);
    let mut state = DaemonState::new();

    let target = format!(
        "http://127.0.0.1:{}?devtools={}",
        firefox.bidi_port, firefox.devtools_port
    );
    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    let own = firefox_eval(&mut state, "navigator.userAgent").await;
    assert!(
        own.as_str()
            .is_some_and(|user_agent| user_agent.contains("Firefox")),
        "the attached browser must report its own user agent: {own}"
    );
    let page =
        "[innerWidth, navigator.language, Intl.DateTimeFormat().resolvedOptions().timeZone].join()";
    let own_page = firefox_eval(&mut state, page).await;

    let resp = firefox_command(
        &mut state,
        json!({
            "id": "user-agent",
            "action": "launch",
            "cdpUrl": target,
            "settings": { "user-agent": "AttachedUA/1" },
        }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["relaunchedBrowser"],
        false,
        "an in-place setting must not reconnect: {resp}"
    );
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        json!("AttachedUA/1")
    );

    for step in [
        json!({ "id": "command", "action": "useragent", "userAgent": "CommandUA/1" }),
        json!({
            "id": "timeout",
            "action": "launch",
            "cdpUrl": target,
            "settings": { "default-timeout": 5000 },
        }),
    ] {
        assert_success(&firefox_command(&mut state, step).await);
    }
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        json!("CommandUA/1"),
        "changing an unrelated setting must keep the user agent a command set"
    );
    for step in [
        json!({ "id": "viewport", "action": "viewport", "width": 500, "height": 400 }),
        json!({ "id": "locale", "action": "locale", "locale": "fr-FR" }),
        json!({ "id": "timezone", "action": "timezone", "timezoneId": "Asia/Tokyo" }),
    ] {
        assert_success(&firefox_command(&mut state, step).await);
    }
    assert_eq!(
        firefox_eval(&mut state, page).await,
        json!("500,fr-FR,Asia/Tokyo")
    );

    firefox_close(&mut state).await;
    assert!(
        firefox.alive(),
        "close must leave the attached Firefox running"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "clear", "action": "session_info", "settings": { "user-agent": null } }),
    )
    .await;
    assert_success(&resp);

    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach-again", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        own,
        "close must put back the user agent Firefox had"
    );
    assert_eq!(
        firefox_eval(&mut state, page).await,
        own_page,
        "close must put back the viewport, locale and timezone Firefox had"
    );
    firefox_close(&mut state).await;
}

/// Overrides and grants reach the page tab of an attached Firefox and are put back on close, even
/// with a privileged tab open that refuses every command naming it.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_attach_undoes_changes_with_a_privileged_tab_open() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut firefox = start_running_firefox(&["about:home", &fixture.url]);
    let mut state = DaemonState::new();

    let target = format!(
        "http://127.0.0.1:{}?devtools={}",
        firefox.bidi_port, firefox.devtools_port
    );
    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);

    let bidi = state.firefox.as_ref().unwrap().bidi.clone();
    let tree = bidi
        .send("browsingContext.getTree", json!({"maxDepth": 0}))
        .await
        .unwrap();
    let contexts: Vec<Value> = tree["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|context| context["context"].clone())
        .collect();
    assert_eq!(
        contexts.len(),
        2,
        "the page must be open beside Firefox's own page: {tree}"
    );
    let refusal = bidi
        .send(
            "emulation.setUserAgentOverride",
            json!({ "userAgent": "Refused/1", "contexts": contexts }),
        )
        .await
        .expect_err("Firefox must refuse one user agent for every tab while about:home is open");
    assert!(refusal.contains("System access is required"), "{refusal}");

    firefox_switch_to_title(&mut state, "Firefox adapter probe").await;
    let own = firefox_eval(&mut state, "navigator.userAgent").await;
    let own_language = firefox_eval(&mut state, "navigator.language").await;

    let resp = firefox_command(
        &mut state,
        json!({
            "id": "user-agent",
            "action": "launch",
            "cdpUrl": target,
            "settings": { "user-agent": "AttachedUA/1" },
        }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        get_data(&resp)["relaunchedBrowser"],
        false,
        "an in-place setting must not reconnect: {resp}"
    );
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        json!("AttachedUA/1")
    );

    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "locale", "action": "locale", "locale": "fr-FR" }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "navigator.language").await,
        json!("fr-FR")
    );

    let permission_state = "navigator.permissions.query({name:'geolocation'}).then(p => p.state)";
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "permissions", "action": "permissions", "permissions": ["geolocation"] }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, permission_state).await,
        json!("granted")
    );

    firefox_close(&mut state).await;
    assert!(
        firefox.alive(),
        "close must leave the attached Firefox running"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "clear", "action": "session_info", "settings": { "user-agent": null } }),
    )
    .await;
    assert_success(&resp);

    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach-again", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    firefox_switch_to_title(&mut state, "Firefox adapter probe").await;
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        own,
        "close must put Firefox's own user agent back"
    );
    assert_eq!(
        firefox_eval(&mut state, "navigator.language").await,
        own_language,
        "close must put Firefox's own locale back"
    );
    assert_eq!(
        firefox_eval(&mut state, permission_state).await,
        json!("prompt"),
        "close must take the geolocation grant back out of the profile"
    );
    firefox_close(&mut state).await;
}

/// Attaching adopts the tab the user has selected, not the oldest one.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_attach_adopts_the_selected_tab() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let firefox = start_running_firefox(&[&fixture.url]);
    let target = format!(
        "http://127.0.0.1:{}?devtools={}",
        firefox.bidi_port, firefox.devtools_port
    );
    let selected = format!("{}two", fixture.url);
    let mut state = DaemonState::new();
    for step in [
        json!({ "id": "attach", "action": "launch", "cdpUrl": target }),
        json!({ "id": "tab", "action": "tab_new", "url": selected }),
    ] {
        assert_success(&firefox_command(&mut state, step).await);
    }
    firefox_close(&mut state).await;

    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach-again", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        firefox_eval(&mut state, "location.href").await,
        json!(selected),
        "attach must act on the tab that is selected in the browser"
    );
    firefox_close(&mut state).await;
}

/// Attaching to a fresh profile, whose only tab refuses scripts, opens a tab it can drive.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_attach_opens_a_tab_when_none_runs_scripts() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let firefox = start_running_firefox(&["about:home"]);
    let target = format!(
        "http://127.0.0.1:{}?devtools={}",
        firefox.bidi_port, firefox.devtools_port
    );
    let mut state = DaemonState::new();
    let resp = firefox_command(
        &mut state,
        json!({ "id": "attach", "action": "launch", "cdpUrl": target }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, json!(2));
    firefox_close(&mut state).await;
}

/// Fill replaces controlled values and reaches password inputs by ref, fill/type reach contenteditables and cross-origin frames, and inserttext and raw char succeed without editing when beforeinput is canceled, like Chrome.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_form_entry() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{}forms.html", fixture.url)).await;

    for (label, action, args) in [
        ("Issues", "fill", json!({ "value": "is:issue firefox" })),
        ("Editor", "fill", json!({ "value": "filled editor" })),
        ("Literal", "fill", json!({ "value": "one\ttwo\nthree" })),
        ("Password", "fill", json!({ "value": "hunter2" })),
        ("Email", "fill", json!({ "value": "user@example.test" })),
        ("Number", "fill", json!({ "value": "42" })),
        ("Frame fill", "fill", json!({ "value": "cross frame fill" })),
        ("Frame type", "type", json!({ "text": "cross frame type" })),
    ] {
        let target = firefox_ref(&mut state, label).await;
        assert_success(&firefox_act(&mut state, action, &target, args).await);
    }
    let guarded = firefox_ref(&mut state, "Guarded").await;
    assert_success(&firefox_act(&mut state, "click", &guarded, json!({})).await);
    let inserted = firefox_command(
        &mut state,
        json!({ "id": "insert", "action": "inserttext", "text": "BLOCKED" }),
    )
    .await;
    assert_success(&inserted);
    firefox_eval(
        &mut state,
        "document.body.insertAdjacentHTML('beforeend', '<input id=key-cancelled>'); document.querySelector('#key-cancelled').addEventListener('keydown', event => event.preventDefault()); true",
    )
    .await;
    assert_success(
        &firefox_act(
            &mut state,
            "type",
            "#key-cancelled",
            json!({ "text": "literal" }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "document.querySelector('#key-cancelled').value").await,
        "literal"
    );
    firefox_eval(
        &mut state,
        "document.body.insertAdjacentHTML('beforeend', '<input id=checkbox type=checkbox value=sentinel>'); document.querySelector('#checkbox').focus(); true",
    )
    .await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "checkbox-insert", "action": "inserttext", "text": "changed" }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(
            &mut state,
            "JSON.stringify([checkbox.checked, checkbox.value])"
        )
        .await,
        "[false,\"sentinel\"]"
    );
    let raw_char = firefox_command(
        &mut state,
        json!({ "id": "char", "action": "input_keyboard", "type": "char", "text": "!" }),
    )
    .await;
    assert_success(&raw_char);

    firefox_eval(
        &mut state,
        "const input = document.querySelector('#shadow-host').shadowRoot.querySelector('input'); input.focus(); input.setSelectionRange(3, 3); 0",
    )
    .await;
    let raw_char = firefox_command(
        &mut state,
        json!({ "id": "shadow-char", "action": "input_keyboard", "type": "char", "text": "!" }),
    )
    .await;
    assert_success(&raw_char);
    for (id, field, text) in [
        ("email-char", "email", ".org"),
        ("number-char", "number", "3"),
    ] {
        firefox_eval(&mut state, &format!("{field}.focus()")).await;
        assert_success(
            &firefox_command(
                &mut state,
                json!({ "id": id, "action": "input_keyboard", "type": "char", "text": text }),
            )
            .await,
        );
    }

    firefox_eval(&mut state, "document.getElementById('notes').focus()").await;
    let inserted = firefox_command(
        &mut state,
        json!({ "id": "insert-notes", "action": "inserttext", "text": "a\tb" }),
    )
    .await;
    assert_success(&inserted);
    assert_eq!(
        firefox_eval(&mut state, "document.getElementById('notes').value").await,
        "a\tb"
    );

    let nested_input = "document.getElementById('nest-host').shadowRoot.querySelector('div').shadowRoot.querySelector('input')";
    firefox_eval(&mut state, &format!("{nested_input}.focus()")).await;
    let inserted = firefox_command(
        &mut state,
        json!({ "id": "insert-nested", "action": "inserttext", "text": "shadow" }),
    )
    .await;
    assert_success(&inserted);
    assert_eq!(
        firefox_eval(&mut state, &format!("{nested_input}.value")).await,
        "shadow"
    );

    assert_eq!(
        firefox_eval(
            &mut state,
            "JSON.stringify([issues.value, editor.textContent, literal.value, password.value, guarded.value, email.value, number.value, document.querySelector('#shadow-host').shadowRoot.querySelector('input').value])"
        )
        .await,
        "[\"is:issue firefox\",\"filled editor\",\"one\\ttwo\\nthree\",\"hunter2\",\"\",\"user@example.test.org\",\"423\",\"nes!ted\"]"
    );
    for (label, expected) in [
        ("Frame fill", "cross frame fill"),
        ("Frame type", "cross frame type"),
    ] {
        let target = firefox_ref(&mut state, label).await;
        let target = resolve_target(&mut state, &target).await.unwrap();
        let value = state
            .firefox
            .as_ref()
            .unwrap()
            .bidi
            .call_function(
                &target.context,
                "function() { return this.value; }",
                Some(json!({ "sharedId": target.shared_id })),
                vec![],
            )
            .await
            .unwrap();
        assert_eq!(value, expected);
    }
    firefox_eval(&mut state, "literal.value = ''; literal.focus()").await;
    let inserted = firefox_command(
        &mut state,
        json!({ "id": "literal-insert", "action": "inserttext", "text": "a\tb" }),
    )
    .await;
    assert_success(&inserted);
    assert_eq!(firefox_eval(&mut state, "literal.value").await, "a\tb");
    firefox_close(&mut state).await;
}

/// History and reload report the document they land on, like Chrome's handlers.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_history_and_reload_land_on_the_traversed_document() {
    required_firefox_binary();
    let home = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    _env.set("HOME", home.path().to_str().unwrap());
    let fixture = firefox_fixture().await;
    let second = format!("{}/page", fixture.other);
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_open(&mut state, &second).await;

    for (action, expected) in [
        ("back", fixture.url.clone()),
        ("forward", second.clone()),
        ("reload", second.clone()),
    ] {
        let resp = firefox_command(&mut state, json!({ "id": action, "action": action })).await;
        assert_success(&resp);
        assert_eq!(get_data(&resp)["url"], expected, "{action}");
        assert_eq!(
            firefox_command(&mut state, json!({ "id": "url", "action": "url" })).await["data"]
                ["url"],
            expected,
            "{action} leaves the tab on the same document"
        );
    }
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_read_active_page() {
    required_firefox_binary();
    let home = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    _env.set("HOME", home.path().to_str().unwrap());
    _env.remove("AGENT_BROWSER_ENCRYPTION_KEY");
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        "document.body.innerHTML = '<h1>Active page read marker</h1>'",
    )
    .await;
    let resp = firefox_command(&mut state, json!({ "id": "read", "action": "read" })).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["finalUrl"], fixture.url);
    assert_eq!(get_data(&resp)["source"], "active-tab-html");
    assert!(get_data(&resp)["content"]
        .as_str()
        .unwrap()
        .contains("Active page read marker"));
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_review_pointer_motion_and_wheel() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(&mut state, r#"document.body.innerHTML = '<div id="left" style="position:fixed;left:10px;top:10px;width:250px;height:250px"></div><div id="right" style="position:fixed;left:320px;top:20px;width:200px;height:200px"></div>'; window.moves=[]; window.wheels=[]; document.addEventListener('pointermove', e => moves.push([e.clientX,e.clientY])); document.addEventListener('wheel', e => { wheels.push([e.clientX,e.clientY,e.target.id]); e.preventDefault(); }, {passive:false}); true"#).await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"start", "action":"mousemove", "x":40, "y":60}),
        )
        .await,
    );
    firefox_eval(&mut state, "moves=[]; true").await;
    let started = std::time::Instant::now();
    assert_success(&firefox_command(&mut state, json!({"id":"move", "action":"mousemove", "x":220, "y":60, "duration":180, "steps":6, "inputMode":"human", "seed":42})).await);
    assert!(started.elapsed() >= std::time::Duration::from_millis(140));
    let moves = firefox_eval(&mut state, "moves").await;
    let moves = moves.as_array().unwrap();
    assert!(moves.len() > 2, "{moves:?}");
    assert_eq!(moves.last().unwrap(), &json!([220, 60]));
    assert!(moves.iter().any(|point| point[1] != 60), "{moves:?}");
    for command in [
        json!({"id":"wheel", "action":"wheel", "deltaY":20}),
        json!({"id":"explicit", "action":"wheel", "x":420, "y":60, "deltaY":20}),
        json!({"id":"tracked", "action":"wheel", "deltaY":20}),
    ] {
        assert_success(&firefox_command(&mut state, command).await);
    }
    assert_eq!(
        firefox_eval(&mut state, "wheels").await,
        json!([[220, 60, "left"], [420, 60, "right"], [420, 60, "right"]])
    );
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"hover", "action":"hover", "selector":"#right"}),
        )
        .await,
    );
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"hover-wheel", "action":"wheel", "deltaY":20}),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "wheels.at(-1)").await,
        json!([420, 120, "right"])
    );
    firefox_eval(&mut state, "document.querySelector('#right').style.cssText='position:fixed;left:-100px;top:0;width:300px;height:200px'; moves=[]; true").await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"clipped-hover", "action":"hover", "selector":"#right"}),
        )
        .await,
    );
    let actual_pointer = firefox_eval(&mut state, "moves.at(-1)").await;
    assert_eq!(actual_pointer, json!([100, 100]));
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"clipped-wheel", "action":"wheel", "deltaY":20}),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "wheels.at(-1)").await,
        json!([100, 100, "right"])
    );
    firefox_open(&mut state, &fixture.url).await;
    for _ in 0..100 {
        if firefox_eval(&mut state, "document.readyState").await == json!("complete") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    firefox_eval(
        &mut state,
        "document.body.innerHTML = '<div style=\"height:4000px\">tall</div>'; window.scrollTo(0, 0); document.body.scrollHeight",
    )
    .await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"fractional-wheel", "action":"wheel", "x":100.5, "y":100.5, "deltaY":150.5}),
        )
        .await,
    );
    let mut scroll = firefox_eval(
        &mut state,
        "({ y: window.scrollY, height: document.body.scrollHeight })",
    )
    .await;
    for _ in 0..100 {
        if scroll["y"].as_f64().is_some_and(|scroll_y| scroll_y > 0.0) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        scroll = firefox_eval(
            &mut state,
            "({ y: window.scrollY, height: document.body.scrollHeight })",
        )
        .await;
    }
    assert!(
        scroll["y"].as_f64().is_some_and(|scroll_y| scroll_y > 0.0),
        "fractional wheel must scroll the page: {scroll}"
    );
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_frame_mouse_uses_local_coordinates() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(&mut state, r#"document.body.innerHTML = '<iframe id="f" style="position:fixed;left:150px;top:100px;width:300px;height:250px;border:10px solid" srcdoc="<button id=target style=width:100px;height:100px>Target</button>"></iframe>'; true"#).await;
    assert_success(&firefox_act(&mut state, "frame", "#f", json!({})).await);
    firefox_eval(&mut state, "window.events=[]; for (const t of ['mousedown','mouseup','wheel']) document.addEventListener(t, e => { events.push([t,e.clientX,e.clientY,e.target.id]); if (t === 'wheel') e.preventDefault(); }, {passive:false}); true").await;
    assert_success(&firefox_act(&mut state, "hover", "#target", json!({})).await);
    for command in [
        json!({"id":"down", "action":"mousedown"}),
        json!({"id":"up", "action":"mouseup"}),
        json!({"id":"wheel", "action":"wheel", "deltaY":10}),
        json!({"id":"raw", "action":"input_mouse", "type":"mousePressed"}),
        json!({"id":"raw-up", "action":"input_mouse", "type":"mouseReleased"}),
    ] {
        assert_success(&firefox_command(&mut state, command).await);
    }
    assert_eq!(
        firefox_eval(&mut state, "events").await,
        json!([
            ["mousedown", 58, 58, "target"],
            ["mouseup", 58, 58, "target"],
            ["wheel", 58, 58, "target"],
            ["mousedown", 58, 58, "target"],
            ["mouseup", 58, 58, "target"]
        ])
    );
    assert_success(&firefox_command(&mut state, json!({"id":"main", "action":"mainframe"})).await);
    firefox_eval(&mut state, &format!("new Promise(resolve => {{ document.body.innerHTML = '<iframe id=other style=\"position:fixed;left:150px;top:100px;width:300px;height:250px;border:10px solid\" src=\"{}/frame.html\"></iframe>'; document.querySelector('#other').onload = () => resolve(true); }})", fixture.other)).await;
    assert_success(&firefox_act(&mut state, "frame", "#other", json!({})).await);
    firefox_eval(&mut state, "document.body.innerHTML='<button id=target style=width:100px;height:100px>Target</button>'; window.events=[]; for (const t of ['mousedown','mouseup','wheel']) document.addEventListener(t, e => { events.push([t,e.clientX,e.clientY,e.target.id]); if (t === 'wheel') e.preventDefault(); }, {passive:false}); true").await;
    assert_success(&firefox_act(&mut state, "hover", "#target", json!({})).await);
    for command in [
        json!({"id":"down2", "action":"mousedown"}),
        json!({"id":"up2", "action":"mouseup"}),
        json!({"id":"wheel2", "action":"wheel", "deltaY":10}),
    ] {
        assert_success(&firefox_command(&mut state, command).await);
    }
    assert_eq!(
        firefox_eval(&mut state, "events").await,
        json!([
            ["mousedown", 58, 58, "target"],
            ["mouseup", 58, 58, "target"],
            ["wheel", 58, 58, "target"]
        ])
    );
    firefox_close(&mut state).await;
}

/// `frame` reaches a child frame by name and by url, and reports Chrome's not-found error.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_frame_by_name_and_url() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    let frame_url = format!("{}/frame.html", fixture.other);
    firefox_eval(
        &mut state,
        &format!(
            "new Promise(resolve => {{ document.body.innerHTML = '<iframe name=alpha srcdoc=\"<p>alpha text</p>\"></iframe><iframe name=beta src=\"{frame_url}\"></iframe>'; const frames = Array.from(document.querySelectorAll('iframe')); let left = frames.length; for (const frame of frames) frame.addEventListener('load', () => {{ if (--left === 0) resolve(true); }}); }})"
        ),
    )
    .await;

    let resp = firefox_command(
        &mut state,
        json!({ "id": "by-name", "action": "frame", "name": "alpha" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["frame"], json!("alpha"));
    assert_eq!(
        firefox_eval(&mut state, "document.body.textContent").await,
        json!("alpha text")
    );

    assert_success(
        &firefox_command(&mut state, json!({ "id": "main", "action": "mainframe" })).await,
    );
    let resp = firefox_command(
        &mut state,
        json!({ "id": "by-url", "action": "frame", "url": frame_url }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["frame"], json!(frame_url));
    assert_eq!(
        firefox_eval(&mut state, "document.querySelector('button').textContent").await,
        json!("Frame button")
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "missing", "action": "frame", "name": "gamma" }),
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    assert_eq!(resp["error"], json!("Frame not found"));
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_snapshot_tracks_active_window() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        "document.body.innerHTML='<button>First window</button>'; true",
    )
    .await;
    assert_success(
        &firefox_command(&mut state, json!({"id":"window", "action":"window_new"})).await,
    );
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        "document.body.innerHTML='<button>Second window</button>'; true",
    )
    .await;
    for (tab, text) in [
        ("t1", "First window"),
        ("t2", "Second window"),
        ("t1", "First window"),
    ] {
        assert_success(
            &firefox_command(
                &mut state,
                json!({"id":"switch", "action":"tab_switch", "tabId":tab}),
            )
            .await,
        );
        let reference = firefox_ref(&mut state, text).await;
        assert_success(&firefox_act(&mut state, "click", &reference, json!({})).await);
    }
    firefox_close(&mut state).await;
}

/// PR1: open, title, evaluate, Chrome-only error, and close leaves no process or profile.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_smoke() {
    required_firefox_binary();
    let home = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    _env.set("HOME", home.path().to_str().unwrap());
    _env.remove("AGENT_BROWSER_ENCRYPTION_KEY");
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({ "userAgent": "AgentBrowserTest/2.0" })).await;
    assert!(
        firefox_processes_using(tmp.path()),
        "Firefox verification must own a Firefox process with a profile under the temp dir"
    );
    firefox_open(&mut state, &fixture.url).await;
    assert_eq!(firefox_title(&mut state).await, "Firefox adapter probe");
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, json!(2));
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        json!("AgentBrowserTest/2.0")
    );
    let resp = firefox_command(&mut state, json!({ "id": "t", "action": "trace_start" })).await;
    assert_eq!(resp["success"], false, "{resp}");
    assert!(
        resp["error"]
            .as_str()
            .is_some_and(|error| error.contains("Chrome")),
        "{resp}"
    );
    firefox_eval(
        &mut state,
        "setTimeout(() => { document.body.innerHTML = '<p id=ready>Done</p>'; }, 300); 0",
    )
    .await;
    let resp = firefox_command(
        &mut state,
        json!({ "id": "w", "action": "wait", "function": "document.querySelector('#ready').textContent === 'Done'", "timeout": 5000 }),
    )
    .await;
    assert_success(&resp);
    let resp = firefox_command(
        &mut state,
        json!({ "id": "l", "action": "launch", "headless": true, "engine": "firefox", "initScripts": ["x.js"] }),
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    assert!(
        resp["error"]
            .as_str()
            .is_some_and(|e| e.contains("--init-script")),
        "{resp}"
    );
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(&mut state, "localStorage.setItem('visited', 'from-a')").await;
    let saved_path = {
        let ff = state.firefox.as_mut().unwrap();
        let path = super::bidi::storage::save_auto_state(ff, "firefox-smoke", "autosave")
            .await
            .unwrap();
        let block = "Object.defineProperty(window,'localStorage',{get(){throw 0}})";
        ff.eval(block).await.unwrap();
        super::bidi::storage::save_auto_state(ff, "firefox-smoke", "autosave")
            .await
            .unwrap();
        path
    };
    firefox_open(&mut state, &fixture.other).await;
    let ff = state.firefox.as_mut().unwrap();
    let first_path = super::bidi::storage::save_auto_state(ff, "firefox-first", "autosave")
        .await
        .unwrap();
    let first: Value =
        serde_json::from_str(&std::fs::read_to_string(&first_path).unwrap()).unwrap();
    assert!(first["origins"]
        .as_array()
        .unwrap()
        .iter()
        .any(|origin| origin["localStorage"] == json!([{"name": "visited", "value": "from-a"}])));
    super::bidi::storage::save_auto_state(ff, "firefox-smoke", "autosave")
        .await
        .unwrap();
    let state_data: Value =
        serde_json::from_str(&std::fs::read_to_string(&saved_path).unwrap()).unwrap();
    assert!(state_data["origins"]
        .as_array()
        .unwrap()
        .iter()
        .any(|origin| origin["localStorage"] == json!([{"name": "visited", "value": "from-a"}])));
    let current = firefox_command(&mut state, json!({"id": "url", "action": "url"})).await;
    assert_eq!(
        url::Url::parse(get_data(&current)["url"].as_str().unwrap()).unwrap(),
        url::Url::parse(&fixture.other).unwrap()
    );
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(&mut state, "localStorage.clear()").await;
    firefox_open(&mut state, &fixture.other).await;
    let saved = firefox_command(
        &mut state,
        json!({ "id": "save-b", "action": "state_save", "path": saved_path }),
    )
    .await;
    assert_success(&saved);
    let state_data: Value =
        serde_json::from_str(&std::fs::read_to_string(&saved_path).unwrap()).unwrap();
    assert_eq!(state_data["origins"], json!([]));
    firefox_close(&mut state).await;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    assert!(
        !firefox_processes_using(tmp.path()),
        "close must leave no Firefox process"
    );
    assert_eq!(
        std::fs::read_dir(tmp.path()).unwrap().count(),
        0,
        "close must remove the temp profile"
    );
}

/// Pointer motion follows the requested input mode and seed for click and drag.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_pointer_motion_follows_input_mode() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(&mut state, r#"document.body.innerHTML = '<button id="hit" style="position:fixed;left:300px;top:200px;width:80px;height:40px">Hit</button><div id="drag-source" style="position:fixed;left:40px;top:120px;width:60px;height:60px"></div><div id="drag-target" style="position:fixed;left:360px;top:300px;width:80px;height:60px"></div>'; window.moves = []; document.addEventListener('pointermove', event => moves.push([Math.round(event.clientX), Math.round(event.clientY), event.buttons]), true); true"#).await;

    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "instant", "action": "click", "selector": "#hit", "inputMode": "instant", "seed": 7 }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "moves.splice(0)").await,
        json!([[340, 220, 0]]),
        "instant input reaches the target in one move"
    );

    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "away", "action": "mousemove", "x": 40, "y": 40 }),
        )
        .await,
    );
    firefox_eval(&mut state, "moves = []; true").await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "human", "action": "click", "selector": "#hit", "inputMode": "human", "seed": 42 }),
        )
        .await,
    );
    let moves = firefox_eval(&mut state, "moves").await;
    let moves = moves.as_array().cloned().unwrap_or_default();
    assert!(moves.len() > 2, "human input samples the path: {moves:?}");
    assert_eq!(moves.last(), Some(&json!([340, 220, 0])), "{moves:?}");
    assert!(
        moves
            .iter()
            .any(|point| point[1].as_f64().is_some_and(|y| y > 40.0 && y < 220.0)),
        "human input visits intermediate points: {moves:?}"
    );

    firefox_eval(&mut state, "moves = []; true").await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "drag", "action": "drag", "source": "#drag-source", "target": "#drag-target", "inputMode": "human", "seed": 5 }),
        )
        .await,
    );
    let moves = firefox_eval(&mut state, "moves").await;
    let moves = moves.as_array().cloned().unwrap_or_default();
    assert_eq!(moves.last(), Some(&json!([400, 330, 1])), "{moves:?}");
    assert!(
        moves.iter().filter(|point| point[2] == 1).count() > 2,
        "drag moves through intermediate points while the button is held: {moves:?}"
    );
    assert!(
        moves.iter().any(|point| point[2] == 0),
        "the drag approaches the source before pressing: {moves:?}"
    );

    firefox_eval(&mut state, "window.downs = 0; document.addEventListener('pointerdown', () => downs++, true); document.querySelector('#hit').addEventListener('pointerover', () => confirm('moved'), { once: true }); true").await;
    let blocked = firefox_command(
        &mut state,
        json!({ "id": "dialog-move", "action": "click", "selector": "#hit" }),
    )
    .await;
    assert_success(&blocked);
    assert_eq!(get_data(&blocked)["dialogOpened"], true, "{blocked}");
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "dismiss", "action": "dialog", "response": "dismiss" }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "downs").await,
        json!(0),
        "a dialog opened on the way stops the press"
    );

    firefox_eval(&mut state, "window.ups = 0; document.addEventListener('pointerup', () => ups++, true); document.querySelector('#drag-target').addEventListener('pointerover', () => confirm('drop?'), { once: true }); true").await;
    let blocked = firefox_command(
        &mut state,
        json!({ "id": "dialog-drag", "action": "drag", "source": "#drag-source", "target": "#drag-target" }),
    )
    .await;
    assert_success(&blocked);
    assert_eq!(get_data(&blocked)["dialogOpened"], true, "{blocked}");
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "accept", "action": "dialog", "response": "accept" }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "ups").await,
        json!(1),
        "a drag a dialog interrupted releases the button once the dialog closes"
    );
    firefox_close(&mut state).await;
}

/// Refs inside open shadow roots resolve whether or not the host has an id.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_shadow_refs_without_host_ids() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(&mut state, r#"document.body.innerHTML = '<div id="withid"></div><div class="noid"></div><my-card></my-card>'; window.clicked = []; for (const [host, name] of [['#withid', 'A'], ['.noid', 'B'], ['my-card', 'C']]) { const button = document.createElement('button'); button.textContent = name + ' btn'; button.onclick = () => clicked.push(name); document.querySelector(host).attachShadow({ mode: 'open' }).append(button); } true"#).await;
    for name in ["A btn", "B btn", "C btn"] {
        let button = firefox_ref(&mut state, name).await;
        assert_success(&firefox_act(&mut state, "click", &button, json!({})).await);
    }
    assert_eq!(
        firefox_eval(&mut state, "clicked").await,
        json!(["A", "B", "C"])
    );
    firefox_close(&mut state).await;
}

/// PR2: fill, click by ref, title reflects the action, and refs survive DOM changes.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_core_flow_and_ref_stability() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let name = firefox_ref(&mut state, "Name").await;
    assert_success(&firefox_act(&mut state, "fill", &name, json!({ "value": "Ada" })).await);
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "mouse-before-touch", "action": "input_mouse", "type": "mouseMoved", "x": 20, "y": 20 }),
        )
        .await,
    );
    assert_success(&firefox_act(&mut state, "tap", &name, json!({})).await);
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "mouse-after-touch", "action": "input_mouse", "type": "mouseMoved", "x": 30, "y": 30 }),
        )
        .await,
    );
    let go = firefox_ref(&mut state, "Go").await;
    assert_success(&firefox_act(&mut state, "click", &go, json!({})).await);
    assert_eq!(firefox_title(&mut state).await, "Hello Ada");

    let second = firefox_ref(&mut state, "Second").await;
    firefox_eval(
        &mut state,
        "document.getElementById('list').prepend(document.createElement('div')); 0",
    )
    .await;
    assert_success(&firefox_act(&mut state, "click", &second, json!({})).await);
    assert_eq!(firefox_title(&mut state).await, "clicked-second");

    let covered = "data:text/html,<body style='margin:0'><button onclick=\"document.title='hit'\" style='position:absolute;top:-100px;left:50px;width:200px;height:400px'>Target</button><div style='position:fixed;top:140px;left:50px;width:200px;height:20px'></div>";
    firefox_open(&mut state, covered).await;
    let target = firefox_ref(&mut state, "Target").await;
    let refused = firefox_act(&mut state, "click", &target, json!({})).await;
    assert!(
        refused["error"].as_str().unwrap().contains("is covered by"),
        "{refused}"
    );
    assert_ne!(firefox_title(&mut state).await, "hit");

    firefox_close(&mut state).await;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    assert!(!firefox_processes_using(tmp.path()));
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_hidden_checkbox_and_disabled_hover() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{}forms.html", fixture.url)).await;
    firefox_eval(&mut state, "document.body.insertAdjacentHTML('afterbegin', `<input id=\"hidden-check\" type=\"checkbox\" style=\"display:none\"><button id=\"disabled-hover\" disabled>Disabled</button>`); window.__hovered = false; document.querySelector('#disabled-hover').addEventListener('pointerover', () => window.__hovered = true)").await;
    for (action, checked) in [("check", true), ("uncheck", false)] {
        assert_success(&firefox_act(&mut state, action, "#hidden-check", json!({})).await);
        assert_eq!(
            firefox_eval(
                &mut state,
                "document.querySelector('#hidden-check').checked"
            )
            .await,
            checked
        );
    }
    assert_success(&firefox_act(&mut state, "hover", "#disabled-hover", json!({})).await);
    assert_eq!(firefox_eval(&mut state, "window.__hovered").await, true);
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_html5_drag_drops_payload() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{}html5_drag_probe.html", fixture.url)).await;

    let drag = firefox_command(
        &mut state,
        json!({ "id": "drag", "action": "drag", "source": "#source", "target": "#dest" }),
    )
    .await;
    assert_success(&drag);

    let events = firefox_eval(&mut state, "window.__html5DragProbe.events").await;
    let events = events
        .as_array()
        .expect("html5 drag probe should expose events");
    assert!(events
        .iter()
        .any(|event| event["type"] == "dragover" && event["target"] == "dest"));
    assert!(events.iter().any(|event| {
        event["type"] == "drop" && event["target"] == "dest" && event["dropped"] == "probe"
    }));

    for (selector, event, handler, capture) in [
        ("#source", "dragstart", "event.preventDefault()", false),
        (
            "#dest",
            "dragover",
            "event.stopImmediatePropagation()",
            true,
        ),
    ] {
        firefox_eval(&mut state, &format!(
            "window.__html5DragProbe.events.length = 0; document.querySelector('{selector}').addEventListener('{event}', event => {{ {handler}; }}, {{once: true, capture: {capture}}})"
        )).await;
        assert_success(&firefox_command(
            &mut state,
            json!({ "id": "cancelled-drag", "action": "drag", "source": "#source", "target": "#dest" }),
        ).await);
        let events = firefox_eval(&mut state, "window.__html5DragProbe.events").await;
        assert!(!events
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["type"] == "drop"));
    }

    firefox_eval(&mut state, "dest.style.cssText = 'position:absolute;top:1500px;z-index:2'; source.addEventListener('dragstart', event => { const rect = source.getBoundingClientRect(); window.__dragOffset = event.clientY - rect.top - rect.height / 2; }, {once:true})").await;
    assert_success(&firefox_command(
        &mut state,
        json!({ "id": "offscreen-drag", "action": "drag", "source": "#source", "target": "#dest" }),
    ).await);
    assert_eq!(firefox_eval(&mut state, "window.__dragOffset").await, 0);

    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_drag_scrolls_destination_and_releases_on_failure() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{}forms.html", fixture.url)).await;
    firefox_eval(&mut state, "window.__dragTrace = []; document.addEventListener('pointerdown', e => __dragTrace.push(`down:${e.target.id}:${e.buttons}`), true); document.addEventListener('pointerup', e => __dragTrace.push(`up:${e.target.id}:${e.buttons}`), true)").await;

    let drag = firefox_command(
        &mut state,
        json!({ "id": "drag", "action": "drag", "source": "#drag-source", "target": "#drag-target" }),
    )
    .await;
    assert_success(&drag);
    assert_eq!(
        firefox_eval(&mut state, "window.__dragTrace.join('|')").await,
        "down:drag-source:1|up:drag-target:0"
    );

    firefox_eval(
        &mut state,
        "window.__dragTrace = []; document.querySelector('#drag-cover').style.display = 'block'",
    )
    .await;
    let failed_drag = firefox_command(
        &mut state,
        json!({ "id": "blocked-drag", "action": "drag", "source": "#drag-source", "target": "#drag-target" }),
    )
    .await;
    assert!(
        !failed_drag["success"].as_bool().unwrap_or(true),
        "{failed_drag}"
    );
    assert!(failed_drag["error"]
        .as_str()
        .unwrap_or("")
        .contains("drag-cover"));
    assert!(firefox_eval(&mut state, "window.__dragTrace.join('|')")
        .await
        .as_str()
        .unwrap_or("")
        .ends_with(":0"));
    firefox_close(&mut state).await;
}

/// Dashboard modifier masks hold Shift across keys and apply it before pointer input.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_stream_modifier_masks() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        r#"document.body.innerHTML = '<button style="position:fixed;left:0;top:0;width:100px;height:80px">Probe</button>';
        window.__streamKeys = [];
        for (const type of ['keydown', 'keyup', 'click']) {
            window.addEventListener(type, e => {
                window.__streamKeys.push([e.type, e.key || '', e.shiftKey]);
                e.preventDefault();
            });
        }"#,
    )
    .await;
    let ff = state.firefox.as_ref().unwrap();
    let live = super::bidi::stream::FirefoxLive {
        bidi: Arc::clone(&ff.bidi),
        context: ff.contexts.active_tab().to_string(),
        contexts: ff.contexts.clone(),
        modifiers: Default::default(),
    };
    for (key, modifiers) in [("K", 8), ("J", 8), ("a", 0)] {
        for event in ["keyDown", "keyUp"] {
            super::bidi::stream::dispatch_input(
                &live,
                "input_keyboard",
                &json!({"eventType": event, "key": key, "modifiers": modifiers}),
            )
            .await;
        }
    }
    for (event, modifiers) in [("mousePressed", 8), ("mouseReleased", 8), ("mouseMoved", 0)] {
        super::bidi::stream::dispatch_input(
            &live,
            "input_mouse",
            &json!({"eventType": event, "x": 20, "y": 20, "button": "left", "modifiers": modifiers}),
        )
        .await;
    }
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id": "input-ready", "action": "wait", "function": "window.__streamKeys.length >= 11", "timeout": 5000}),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "window.__streamKeys").await,
        json!([
            ["keydown", "Shift", true],
            ["keydown", "K", true],
            ["keyup", "K", true],
            ["keydown", "J", true],
            ["keyup", "J", true],
            ["keyup", "Shift", false],
            ["keydown", "a", false],
            ["keyup", "a", false],
            ["keydown", "Shift", true],
            ["click", "", true],
            ["keyup", "Shift", false],
        ])
    );
    firefox_close(&mut state).await;
}

/// Firefox publishes the same tab data to the dashboard as its tab commands return.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_stream_tabs_follow_tab_commands() {
    async fn next_tabs(
        ws: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        until: impl Fn(&Value) -> bool,
    ) -> Value {
        loop {
            let message = tokio::time::timeout(tokio::time::Duration::from_secs(10), ws.next())
                .await
                .expect("timed out waiting for the dashboard tab list")
                .expect("dashboard stream ended")
                .expect("dashboard stream error");
            let Ok(text) = message.to_text() else {
                continue;
            };
            let payload: Value = serde_json::from_str(text).unwrap();
            if payload["type"] == "tabs" && until(&payload["tabs"]) {
                return payload["tabs"].clone();
            }
        }
    }

    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    let (server, client_slot) = crate::native::stream::StreamServer::start_without_client(
        0,
        "firefox-tabs".to_string(),
        true,
        state.idle_activity.clone(),
    )
    .await
    .unwrap();
    let server = Arc::new(server);
    state.stream_client = Some(client_slot);
    state.stream_server = Some(Arc::clone(&server));
    launch_firefox(&mut state, json!({})).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}", server.port()))
        .await
        .unwrap();
    let tabs = next_tabs(&mut ws, Value::is_array).await;
    assert_eq!(tabs.as_array().unwrap().len(), 1);
    assert_eq!(tabs[0]["tabId"], "t1");
    assert_eq!(tabs[0]["active"], true);

    firefox_open(&mut state, &fixture.url).await;
    let created = firefox_command(
        &mut state,
        json!({"id":"new", "action":"tab_new", "label":"docs", "url":fixture.url}),
    )
    .await;
    assert_success(&created);
    let tabs = next_tabs(&mut ws, |tabs| tabs[1]["label"] == "docs").await;
    assert_eq!(tabs.as_array().unwrap().len(), 2);
    assert_eq!(tabs[1]["tabId"], get_data(&created)["tabId"]);
    assert_eq!(tabs[1]["active"], true);
    let listed = firefox_command(&mut state, json!({"id":"list", "action":"tab_list"})).await;
    assert_success(&listed);
    assert_eq!(get_data(&listed)["tabs"], tabs);

    let switched = firefox_command(
        &mut state,
        json!({"id":"switch", "action":"tab_switch", "tabId":"t1"}),
    )
    .await;
    assert_success(&switched);
    let tabs = next_tabs(&mut ws, |tabs| tabs[0]["active"] == true).await;
    assert_eq!(tabs[0]["active"], true);
    assert_eq!(tabs.as_array().unwrap().len(), 2);
    let closed = firefox_command(
        &mut state,
        json!({"id":"close-tab", "action":"tab_close", "tabId":"docs"}),
    )
    .await;
    assert_success(&closed);
    let tabs = next_tabs(&mut ws, |tabs| tabs.as_array().unwrap().len() == 1).await;
    assert_eq!(tabs.as_array().unwrap().len(), 1);
    firefox_close(&mut state).await;
    server.shutdown().await;
}

/// `click --new-tab` moves the Firefox live view to the opened tab, even when its navigation fails.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_click_new_tab_retargets_live_view() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    let (server, client_slot) = crate::native::stream::StreamServer::start_without_client(
        0,
        "firefox-live".to_string(),
        true,
        state.idle_activity.clone(),
    )
    .await
    .unwrap();
    let server = Arc::new(server);
    state.stream_client = Some(client_slot);
    state.stream_server = Some(Arc::clone(&server));
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    let first = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active_tab()
        .to_string();
    assert_eq!(
        server.firefox_live_context().await.as_deref(),
        Some(first.as_str())
    );

    firefox_eval(
        &mut state,
        "document.body.insertAdjacentHTML('beforeend', '<a href=\"http://127.0.0.1:1/\">Broken tab</a>')",
    )
    .await;
    let broken = firefox_ref(&mut state, "Broken tab").await;
    let failed = firefox_act(&mut state, "click", &broken, json!({ "newTab": true })).await;
    assert_eq!(failed["success"], false, "{failed}");
    let second = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active_tab()
        .to_string();
    assert_ne!(second, first);
    assert_eq!(
        server.firefox_live_context().await.as_deref(),
        Some(second.as_str())
    );

    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        "document.body.insertAdjacentHTML('beforeend', '<a href=\"/frame.html\">Live tab</a>')",
    )
    .await;
    let link = firefox_ref(&mut state, "Live tab").await;
    assert_success(&firefox_act(&mut state, "click", &link, json!({ "newTab": true })).await);
    let third = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active_tab()
        .to_string();
    assert_ne!(third, second);
    assert_eq!(
        server.firefox_live_context().await.as_deref(),
        Some(third.as_str())
    );

    firefox_close(&mut state).await;
    server.shutdown().await;
}

/// PR2: a confirm() dialog is resolved with `dialog accept`.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_dialog() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let ask = firefox_ref(&mut state, "Ask").await;
    let resp = firefox_act(&mut state, "click", &ask, json!({})).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["dialogOpened"], true, "{resp}");
    let resp = firefox_command(
        &mut state,
        json!({ "id": "d", "action": "dialog", "response": "accept" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(firefox_title(&mut state).await, "confirmed");

    firefox_eval(
        &mut state,
        "document.getElementById('go').addEventListener('mousedown', () => confirm('mousedown'), { once: true }); true",
    )
    .await;
    let go = firefox_ref(&mut state, "Go").await;
    let resp = firefox_act(&mut state, "click", &go, json!({})).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["dialogOpened"], true, "{resp}");
    let resp = firefox_command(
        &mut state,
        json!({ "id": "release", "action": "dialog", "response": "dismiss" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(state.mouse_state.buttons, 0);
    assert_success(&firefox_act(&mut state, "click", &go, json!({})).await);
    assert_eq!(firefox_title(&mut state).await, "Hello");

    firefox_eval(
        &mut state,
        "document.body.insertAdjacentHTML('beforeend', '<div id=drag-source draggable=true></div><div id=drag-target></div>'); document.querySelector('#drag-source').addEventListener('dragstart', () => confirm('drag')); true",
    )
    .await;
    let drag = firefox_command(
        &mut state,
        json!({"id":"dialog-drag", "action":"drag", "source":"#drag-source", "target":"#drag-target"}),
    )
    .await;
    assert_success(&drag);
    assert_eq!(get_data(&drag)["dragged"], false, "{drag}");
    assert_eq!(get_data(&drag)["dialogOpened"], true, "{drag}");
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "drag-dismiss", "action": "dialog", "response": "dismiss" }),
        )
        .await,
    );
    firefox_close(&mut state).await;
}

/// Alerts and beforeunload prompts are auto-accepted, so they never block the next command.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_auto_dialog() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let alert = firefox_ref(&mut state, "Alert").await;
    assert_success(&firefox_act(&mut state, "click", &alert, json!({})).await);
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, json!(2));

    firefox_eval(&mut state, "window.onbeforeunload = () => 'leave?'").await;
    firefox_open(&mut state, &fixture.other).await;
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, json!(2));

    let status = firefox_command(
        &mut state,
        json!({ "id": "d", "action": "dialog", "response": "status" }),
    )
    .await;
    assert_success(&status);
    assert_eq!(get_data(&status)["hasDialog"], false);
    firefox_close(&mut state).await;
}

/// PR2: refs inside a cross-origin iframe and an open shadow root are clickable.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_iframe_and_shadow_root_refs() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        "document.getElementById('host').removeAttribute('id'); 0",
    )
    .await;

    let shadow = firefox_ref(&mut state, "Shadow button").await;
    assert_success(&firefox_act(&mut state, "click", &shadow, json!({})).await);
    assert_eq!(firefox_title(&mut state).await, "shadow-clicked");

    let frame = firefox_ref(&mut state, "Frame button").await;
    assert_success(&firefox_act(&mut state, "click", &frame, json!({})).await);
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    assert_eq!(firefox_title(&mut state).await, "frame-clicked");

    let input = firefox_ref(&mut state, "Frame input").await;
    assert_success(&firefox_act(&mut state, "focus", &input, json!({})).await);
    firefox_eval(&mut state, "Object.defineProperty(document, 'activeElement', { configurable: true, get: () => document.body }); true").await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "insert", "action": "inserttext", "text": "focused" }),
        )
        .await,
    );
    let target = resolve_target(&mut state, &input).await.unwrap();
    let value = state
        .firefox
        .as_ref()
        .unwrap()
        .bidi
        .call_function(
            &target.context,
            "function() { return this.value; }",
            Some(target.node_ref()),
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(value, "focused");

    assert_success(&firefox_act(&mut state, "frame", "iframe", json!({})).await);
    let target_url = format!("{}/frame.html", fixture.other);
    let navigated = firefox_command(
        &mut state,
        json!({ "id": "navigate", "action": "navigate", "url": target_url }),
    )
    .await;
    assert_success(&navigated);
    assert_eq!(get_data(&navigated)["url"], target_url);
    let session = state.firefox.as_ref().unwrap();
    assert_eq!(session.contexts.active(), session.contexts.active_tab());
    assert_eq!(firefox_title(&mut state).await, "Frame");
    firefox_close(&mut state).await;
}

/// A selector shared by light and shadow trees still gives the shadow control a usable ref.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_shadow_ref_survives_a_shared_selector() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        r#"(() => {
  const markup = name => `<section class="mirror"><button id="shared">${name}</button></section>`;
  document.body.innerHTML = markup('Light button');
  const host = document.createElement('div');
  document.body.append(host);
  const shadow = host.attachShadow({ mode: 'open' });
  shadow.innerHTML = markup('Shadow button');
  document.querySelector('button').onclick = () => document.title = 'light-clicked';
  shadow.querySelector('button').onclick = () => document.title = 'shadow-clicked';
  return true;
})()"#,
    )
    .await;

    let light = firefox_ref(&mut state, "Light button").await;
    let shadow = firefox_ref(&mut state, "Shadow button").await;
    assert_ne!(light, shadow);
    assert_success(&firefox_act(&mut state, "click", &shadow, json!({})).await);
    assert_eq!(firefox_title(&mut state).await, "shadow-clicked");
    firefox_close(&mut state).await;
}

/// A confirm() opened inside a cross-origin iframe stops the click promptly and dismisses.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_frame_dialog() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let ask = firefox_ref(&mut state, "Frame ask").await;
    let resp = firefox_act(&mut state, "click", &ask, json!({})).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["dialogOpened"], true, "{resp}");
    let resp = firefox_command(
        &mut state,
        json!({ "id": "d", "action": "dialog", "response": "dismiss" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["accepted"], false, "{resp}");

    assert_success(&firefox_act(&mut state, "frame", "iframe", json!({})).await);
    firefox_eval(
        &mut state,
        "document.querySelector('#input').addEventListener('input', () => confirm('typed'), { once: true })",
    )
    .await;
    for command in [
        json!({ "id": "dispatch", "action": "dispatch", "selector": "button:nth-of-type(2)", "event": "click" }),
        json!({ "id": "type", "action": "type", "selector": "#input", "text": "abc", "delay": 10 }),
    ] {
        assert_success(&firefox_command(&mut state, command).await);
        let resp = firefox_command(
            &mut state,
            json!({ "id": "dismiss", "action": "dialog", "response": "dismiss" }),
        )
        .await;
        assert_success(&resp);
    }
    assert_eq!(
        firefox_eval(&mut state, "document.querySelector('#input').value").await,
        "a"
    );
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_frame_dialog_does_not_block_new_tab() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    let owner_context = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active_tab()
        .to_string();

    let ask = firefox_ref(&mut state, "Frame ask").await;
    let resp = firefox_act(&mut state, "click", &ask, json!({})).await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["dialogOpened"], true, "{resp}");

    let resp = firefox_command(&mut state, json!({ "id": "new", "action": "tab_new" })).await;
    assert_success(&resp);
    let dialog = state.pending_dialog.as_ref().unwrap();
    assert_ne!(dialog.session_id.as_deref(), Some(owner_context.as_str()));
    assert_eq!(
        dialog.owner_context.as_deref(),
        Some(owner_context.as_str())
    );
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, json!(2));

    let resp = firefox_command(
        &mut state,
        json!({ "id": "dismiss", "action": "dialog", "response": "dismiss" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["accepted"], false, "{resp}");
    let resp = firefox_command(&mut state, json!({ "id": "third", "action": "tab_new" })).await;
    assert_success(&resp);
    let opener_context = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active_tab()
        .to_string();
    assert_eq!(
        firefox_eval(&mut state, "window.open('about:blank'); 'opened'").await,
        "opened"
    );
    for _ in 0..100 {
        state.drain_cdp_events_background().await.unwrap();
        if state.firefox.as_ref().unwrap().contexts.active_tab() != opener_context {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    let popup_context = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active_tab()
        .to_string();
    assert_ne!(popup_context, opener_context);
    assert_eq!(
        state.firefox.as_ref().unwrap().contexts.active(),
        popup_context
    );

    assert_eq!(firefox_eval(&mut state, "window.close(); 1").await, 1);
    for _ in 0..100 {
        state.drain_cdp_events_background().await.unwrap();
        if state.firefox.as_ref().unwrap().contexts.active_tab() == opener_context {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        state.firefox.as_ref().unwrap().contexts.active_tab(),
        opener_context
    );
    assert_eq!(
        state.firefox.as_ref().unwrap().contexts.active(),
        opener_context
    );
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, 2);

    assert_eq!(firefox_eval(&mut state, "window.close(); 1").await, 1);
    for _ in 0..100 {
        state.drain_cdp_events_background().await.unwrap();
        if state.firefox.as_ref().unwrap().contexts.active_tab() != opener_context {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, 2);
    let ff = state.firefox.as_ref().unwrap();
    assert_ne!(ff.contexts.active_tab(), opener_context);
    assert_eq!(ff.contexts.active(), ff.contexts.active_tab());
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_smoke_full_page_annotations() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;

    let html = "<!doctype html><html><body style='margin:0'><div style='height:1000px'></div><button style='position:absolute;top:1200px;left:20px'>Scroll target</button><div style='height:500px'></div></body></html>";
    let url = format!("data:text/html;base64,{}", STANDARD.encode(html));
    firefox_open(&mut state, &url).await;
    let scroll_y = firefox_eval(&mut state, "window.scrollTo(0, 500); window.scrollY")
        .await
        .as_f64()
        .unwrap();
    let document_y = firefox_eval(
        &mut state,
        "document.querySelector('button').getBoundingClientRect().top + window.scrollY",
    )
    .await
    .as_f64()
    .unwrap();
    let path = tmp.path().join("annotated-full-page.png");
    let screenshot = firefox_command(
        &mut state,
        json!({
            "id": "annotated-full-page",
            "action": "screenshot",
            "fullPage": true,
            "annotate": true,
            "path": path,
        }),
    )
    .await;
    assert_success(&screenshot);
    let annotation_y = get_data(&screenshot)["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|annotation| annotation["name"] == "Scroll target")
        .and_then(|annotation| annotation["box"]["y"].as_i64());
    firefox_close(&mut state).await;

    assert!(scroll_y > 0.0, "page did not scroll");
    assert_eq!(annotation_y, Some(document_y.round() as i64));
    assert!(std::fs::metadata(path).unwrap().len() > 0);
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_element_screenshot_below_viewport() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;

    let url = format!(
        "data:text/html;base64,{}",
        STANDARD.encode(
            "<body style='margin:0'><div style='height:1600px'></div><button id='box' style='display:block;width:200px;height:200px;border:0;background:#c00'>box</button>"
        )
    );
    firefox_open(&mut state, &url).await;
    assert_eq!(firefox_eval(&mut state, "window.scrollY").await, json!(0));
    assert!(
        firefox_eval(
            &mut state,
            "document.querySelector('#box').getBoundingClientRect().top"
        )
        .await
        .as_f64()
        .unwrap()
            > 600.0
    );

    let path = tmp.path().join("below-viewport.png");
    let response = firefox_command(
        &mut state,
        json!({"id": "shot", "action": "screenshot", "selector": "#box", "path": path}),
    )
    .await;
    assert_success(&response);
    let (width, height, red) = red_fraction(path.to_str().unwrap());
    firefox_close(&mut state).await;

    assert_eq!((width, height), (200, 200));
    assert!(
        red > 0.99,
        "only {:.0}% of the clip is the element",
        red * 100.0
    );
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_screenshot_annotate_excludes_iframe_overlays() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    firefox_eval(
        &mut state,
        "window.__annotationLabels = []; new MutationObserver(records => { for (const record of records) for (const node of record.addedNodes) if (node.id === '__agent_browser_annotations__') window.__annotationLabels.push(...Array.from(node.children, box => box.firstElementChild.textContent)); }).observe(document.documentElement, { childList: true }); true",
    )
    .await;
    let screenshot = firefox_command(
        &mut state,
        json!({ "id": "annotated", "action": "screenshot", "annotate": true }),
    )
    .await;
    assert_success(&screenshot);
    let annotations = get_data(&screenshot)["annotations"]
        .as_array()
        .unwrap()
        .clone();
    let drawn = firefox_eval(&mut state, "window.__annotationLabels")
        .await
        .as_array()
        .unwrap()
        .iter()
        .map(|label| label.as_str().unwrap().parse::<u64>().unwrap())
        .collect::<Vec<_>>();
    let ff = state.firefox.as_ref().unwrap();
    let in_frame = |annotation: &Value| {
        annotation["ref"]
            .as_str()
            .and_then(|ref_id| state.ref_map.get(ref_id))
            .and_then(|entry| entry.backend_node_id)
            .and_then(|id| ff.refs.iter().find(|(_, value)| **value == id))
            .is_some_and(|((context, _), _)| *context != ff.contexts.active())
    };
    let top_document_numbers = annotations
        .iter()
        .filter(|annotation| !in_frame(annotation))
        .map(|annotation| annotation["number"].as_u64().unwrap())
        .collect::<Vec<_>>();
    let has_frame_annotation = annotations.iter().any(in_frame);
    firefox_close(&mut state).await;

    assert!(
        has_frame_annotation,
        "iframe refs remain in the annotation list"
    );
    assert!(!drawn.is_empty(), "no annotations were drawn");
    assert_eq!(drawn, top_document_numbers);
}

async fn firefox_text_of(state: &mut DaemonState, target: &Target) -> String {
    let ff = state.firefox.as_ref().unwrap();
    let this = json!({ "sharedId": target.shared_id });
    let text = ff
        .bidi
        .call_function(
            &target.context,
            "function() { return this.textContent; }",
            Some(this),
            vec![],
        )
        .await
        .unwrap();
    text.as_str().unwrap().to_string()
}

/// U5: refs, CSS and XPath resolve across shadow roots and frames; stale and recovery rules hold.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_resolve_target() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let first = firefox_ref(&mut state, "First").await;
    let second = firefox_ref(&mut state, "Second").await;
    let go = firefox_ref(&mut state, "Go").await;
    let shadow = firefox_ref(&mut state, "Shadow button").await;
    let frame = firefox_ref(&mut state, "Frame button").await;
    let top = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active()
        .to_string();

    for (selector, text) in [(&first, "First"), (&shadow, "Shadow button")] {
        let target = resolve_target(&mut state, selector).await.unwrap();
        assert_eq!(target.context, top);
        assert_eq!(firefox_text_of(&mut state, &target).await, text);
    }
    let framed = resolve_target(&mut state, &frame).await.unwrap();
    assert_ne!(
        framed.context, top,
        "cross-origin frame nodes live in their own context"
    );
    assert_eq!(firefox_text_of(&mut state, &framed).await, "Frame button");
    let by_ref = resolve_target(&mut state, &go).await.unwrap();
    assert_eq!(resolve_target(&mut state, "#go").await.unwrap(), by_ref);
    assert_eq!(
        resolve_target(&mut state, "xpath=//button[@id='go']")
            .await
            .unwrap(),
        by_ref
    );
    let missing = resolve_target(&mut state, "#nope").await.unwrap_err();
    assert_eq!(missing, "Element not found: #nope");

    firefox_eval(
        &mut state,
        "document.getElementById('list').prepend(document.createElement('button')); 0",
    )
    .await;
    assert_eq!(firefox_ref(&mut state, "First").await, first);
    assert_eq!(firefox_ref(&mut state, "Second").await, second);

    firefox_eval(
        &mut state,
        "document.getElementById('list').children[1].remove(); 0",
    )
    .await;
    let stale = resolve_target(&mut state, &first).await.unwrap_err();
    assert!(stale.contains("the ref is stale"), "{stale}");

    firefox_eval(
        &mut state,
        "const b = document.createElement('button'); b.textContent = 'First'; document.getElementById('list').append(b); 0",
    )
    .await;
    let recovered = resolve_target(&mut state, &first).await.unwrap();
    assert_eq!(firefox_text_of(&mut state, &recovered).await, "First");
    assert_eq!(resolve_target(&mut state, &first).await.unwrap(), recovered);

    firefox_eval(
        &mut state,
        "document.body.insertAdjacentHTML('beforeend', '<p id=twins><button>Twin</button><button>Twin</button></p>'); 0",
    )
    .await;
    let twin = firefox_ref(&mut state, "Twin").await;
    firefox_eval(
        &mut state,
        "document.getElementById('twins').innerHTML = '<button>Twin</button>'; 0",
    )
    .await;
    let refused = resolve_target(&mut state, &twin).await.unwrap_err();
    assert!(
        refused.contains("the snapshot had several matching elements"),
        "{refused}"
    );

    firefox_eval(
        &mut state,
        &format!("location.assign('{}/frame.html'); 0", fixture.other),
    )
    .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(800)).await;
    let gone = resolve_target(&mut state, &go).await.unwrap_err();
    assert!(
        gone.contains("the page changed since the snapshot"),
        "{gone}"
    );

    firefox_close(&mut state).await;
}

/// PR3: a route mocks `/api`, and an unrouted fetch reaches the server.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_route_mock() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let fetch = "fetch('/api').then(r => r.text())";
    assert_eq!(firefox_eval(&mut state, fetch).await, json!("real"));
    let resp = firefox_command(
        &mut state,
        json!({ "id": "r", "action": "route", "url": "**/api", "response": { "body": "mocked" } }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(firefox_eval(&mut state, fetch).await, json!("mocked"));
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_headers_requests_and_har() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let har_path = tmp.path().join("session.har");
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    for action in ["requests", "har_start"] {
        let cmd = json!({ "id": action, "action": action });
        assert_success(&firefox_command(&mut state, cmd).await);
    }
    assert_success(
        &firefox_command(
            &mut state,
            json!({
                "id": "open", "action": "navigate", "url": fixture.url,
                "headers": { "X-Scoped-Test": "first-only" }
            }),
        )
        .await,
    );
    let scoped = |value: &Value| {
        value["headers"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("x-scoped-test"))
            .map(|(_, value)| value.clone())
    };
    let same = firefox_eval(&mut state, "fetch('/echo').then(r => r.json())").await;
    assert_eq!(scoped(&same), Some(json!("first-only")), "{same}");
    let cross = format!("fetch('{}/echo').then(r => r.json())", fixture.other);
    let other = firefox_eval(&mut state, &cross).await;
    assert_eq!(scoped(&other), None, "{other}");

    firefox_eval(
        &mut state,
        "fetch('/har-post', {method: 'POST', body: 'payload'}).then(r => r.text()).then(text => { document.title = text; }); true",
    )
    .await;
    let body = firefox_command(
        &mut state,
        json!({ "id": "body", "action": "responsebody", "url": "har-post", "timeout": 5000 }),
    )
    .await;
    assert_success(&body);
    assert_eq!(get_data(&body)["body"], "ok");
    assert_eq!(get_data(&body)["status"], 200);
    assert_eq!(firefox_title(&mut state).await, "ok");

    let listed = firefox_command(
        &mut state,
        json!({ "id": "listed", "action": "requests", "filter": "/har-post" }),
    )
    .await;
    assert_success(&listed);
    let tracked = get_data(&listed)["requests"].as_array().unwrap();
    let posted = tracked.iter().find(|request| request["method"] == "POST");
    let posted = posted.unwrap_or_else(|| panic!("the POST is not listed: {listed}"));
    assert_eq!(posted["status"], 200);
    assert_eq!(posted["postData"], "payload");
    let detail = firefox_command(
        &mut state,
        json!({ "id": "detail", "action": "request_detail", "requestId": posted["requestId"] }),
    )
    .await;
    assert_success(&detail);
    assert_eq!(get_data(&detail)["method"], "POST");
    assert_eq!(get_data(&detail)["status"], 200);
    assert_eq!(get_data(&detail)["postData"], "payload");
    assert_eq!(get_data(&detail)["responseBody"], "ok");

    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "stop", "action": "har_stop", "path": har_path.to_str().unwrap() }),
        )
        .await,
    );
    let har: Value = serde_json::from_slice(&std::fs::read(&har_path).unwrap()).unwrap();
    let entry = har["log"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["request"]["url"] == format!("{}har-post", fixture.url))
        .unwrap_or_else(|| panic!("the POST is missing from the HAR: {har}"));
    assert_eq!(entry["request"]["method"], "POST");
    assert_eq!(entry["response"]["status"], 200);
    assert_eq!(entry["request"]["postData"]["text"], "payload");
    assert_eq!(entry["response"]["content"]["text"], "ok");
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_networkidle_is_scoped_to_the_active_tab() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let pending_url = format!("http://{}/pending", listener.local_addr().unwrap());
    let (started, accepted) = tokio::sync::oneshot::channel();
    let pending = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = [0; 4096];
        stream.read(&mut buf).await.unwrap();
        let _ = started.send(());
        while stream.read(&mut buf).await.unwrap_or(0) != 0 {}
    });
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        &format!("window.pollController = new AbortController(); fetch('{pending_url}', {{mode:'no-cors', signal:pollController.signal}}).catch(() => {{}}); true"),
    )
    .await;
    tokio::time::timeout(std::time::Duration::from_secs(5), accepted)
        .await
        .unwrap()
        .unwrap();
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"new", "action":"tab_new", "url":fixture.url}),
        )
        .await,
    );
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"idle", "action":"waitforloadstate", "state":"networkidle", "timeout":1500}),
        )
        .await,
    );
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"switch", "action":"tab_switch", "tabId":"t1"}),
        )
        .await,
    );
    let busy = firefox_command(
        &mut state,
        json!({"id":"busy", "action":"waitforloadstate", "state":"networkidle", "timeout":700}),
    )
    .await;
    assert_eq!(busy["success"], false, "{busy}");
    firefox_eval(&mut state, "pollController.abort(); true").await;
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"aborted", "action":"waitforloadstate", "state":"networkidle", "timeout":2000}),
        )
        .await,
    );
    firefox_close(&mut state).await;
    pending.abort();
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_navigate_networkidle_waits_for_delayed_request() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    let routed = firefox_command(
        &mut state,
        json!({
            "id": "route",
            "action": "route",
            "url": "**/networkidle",
            "response": {
                "contentType": "text/html",
                "body": "<script>fetch('/har-post', {method:'POST'}).then(() => { document.title = 'request-complete'; })</script>"
            }
        }),
    )
    .await;
    assert_success(&routed);
    let target = format!("{}networkidle", fixture.url);
    let resp = firefox_command(
        &mut state,
        json!({
            "id": "networkidle",
            "action": "navigate",
            "url": target,
            "waitUntil": "networkidle",
            "timeout": 5000
        }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(get_data(&resp)["title"], "request-complete");
    assert!(fixture.hit_count("/har-post") > 0);

    let resp = firefox_command(
        &mut state,
        json!({
            "id": "networkidle-timeout",
            "action": "navigate",
            "url": target,
            "waitUntil": "networkidle",
            "timeout": 50
        }),
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    let error = resp["error"].as_str().unwrap_or("").to_lowercase();
    assert!(
        error.contains("timeout") || error.contains("timed out"),
        "{resp}"
    );
    firefox_close(&mut state).await;
}

/// PR3: allowed-domains blocks a cross-origin worker fetch and a WebSocket.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_allowed_domains_block_worker_and_websocket() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({ "allowedDomains": ["localhost"] })).await;
    let file = firefox_command(
        &mut state,
        json!({ "id": "file", "action": "navigate", "url": "file:///etc/hosts" }),
    )
    .await;
    assert_eq!(file["success"], false, "{file}");
    let blank = firefox_command(
        &mut state,
        json!({ "id": "blank", "action": "navigate", "url": "about:blank" }),
    )
    .await;
    assert_eq!(blank["success"], true, "{blank}");
    let target = json!({ "id": "tab", "action": "tab_new", "url": &fixture.other });
    assert_eq!(firefox_command(&mut state, target).await["success"], false);
    firefox_open(&mut state, &fixture.url).await;

    let own = format!("runWorker('{}hit')", fixture.url);
    assert_eq!(firefox_eval(&mut state, &own).await, json!("ok"));
    assert!(
        fixture.hit_count("/hit") > 0,
        "allowed origin must be reachable"
    );

    let worker = format!("runWorker('{}/hit')", fixture.other);
    assert_eq!(firefox_eval(&mut state, &worker).await, json!("blocked"));
    let socket = format!(
        "runSocket('{}/ws')",
        fixture.other.replace("http://", "ws://")
    );
    assert_ne!(firefox_eval(&mut state, &socket).await, json!("open"));
    let other_port = fixture.other.rsplit(':').next().unwrap();
    assert_eq!(
        fixture.hit_count(&format!("{other_port}/hit"))
            + fixture.hit_count(&format!("{other_port}/ws")),
        0,
        "blocked requests must never reach the disallowed origin"
    );

    for url in [
        format!("{}/blocked-open", fixture.other),
        "file:///etc/hosts".to_string(),
    ] {
        let resp = firefox_command(
            &mut state,
            json!({ "id": "nav", "action": "navigate", "url": url }),
        )
        .await;
        assert!(resp["error"].as_str().is_some(), "{resp}");
        let resp = firefox_command(
            &mut state,
            json!({ "id": "tab", "action": "tab_new", "url": url }),
        )
        .await;
        assert!(resp["error"].as_str().is_some(), "{resp}");
    }
    assert_eq!(fixture.hit_count("/blocked-open"), 0);
    firefox_close(&mut state).await;
}

/// A save after visiting two origins keeps both origins' storage, and a load restores both.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_state_keeps_every_visited_origin() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let state_path = tmp.path().join("state.json").to_string_lossy().to_string();
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    for (url, value) in [(fixture.url.clone(), "A"), (fixture.other.clone(), "B")] {
        firefox_open(&mut state, &url).await;
        firefox_eval(
            &mut state,
            &format!("localStorage.setItem('k', '{value}'); 0"),
        )
        .await;
    }
    let saved = firefox_command(
        &mut state,
        json!({ "id": "save", "action": "state_save", "path": &state_path }),
    )
    .await;
    assert_success(&saved);
    firefox_close(&mut state).await;

    launch_firefox(&mut state, json!({})).await;
    let loaded = firefox_command(
        &mut state,
        json!({ "id": "load", "action": "state_load", "path": &state_path }),
    )
    .await;
    assert_success(&loaded);
    for (url, value) in [(fixture.url.clone(), "A"), (fixture.other.clone(), "B")] {
        firefox_open(&mut state, &url).await;
        assert_eq!(
            firefox_eval(&mut state, "localStorage.getItem('k')").await,
            json!(value)
        );
    }
    firefox_close(&mut state).await;
}

/// Restore must not expose secrets to page hooks, including after a cross-origin redirect.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_state_restore_isolated_from_page_hooks() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let path = tmp.path().join("state.json");
    let origin = fixture.url.trim_end_matches('/');
    std::fs::write(
        &path,
        json!({
            "cookies": [],
            "origins": [{
                "origin": origin,
                "localStorage": [{"name": "token", "value": "synthetic-local-secret"}],
                "sessionStorage": [{"name": "token", "value": "synthetic-session-secret"}]
            }]
        })
        .to_string(),
    )
    .unwrap();
    let hostile_page = r#"<!doctype html><script>
        window.stolen = [];
        const parse = JSON.parse;
        JSON.parse = function(payload) {
            window.stolen.push(payload);
            return parse(payload);
        };
        const setItem = Storage.prototype.setItem;
        Storage.prototype.setItem = function(key, value) {
            window.stolen.push(value);
            return setItem.call(this, key, value);
        };
        window.hooksInstalled = true;
    </script>"#;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;

    for redirect in [false, true] {
        assert_success(
            &firefox_command(&mut state, json!({"id": "unroute", "action": "unroute"})).await,
        );
        let destination = format!("{}/restore-hook.html", fixture.other);
        let response = if redirect {
            json!({"status": 302, "headers": {"Location": destination}, "body": ""})
        } else {
            json!({"contentType": "text/html", "body": hostile_page})
        };
        assert_success(
            &firefox_command(
                &mut state,
                json!({
                    "id": "route-origin", "action": "route", "url": fixture.url,
                    "response": response
                }),
            )
            .await,
        );
        assert_success(
            &firefox_command(
                &mut state,
                json!({
                    "id": "route-destination", "action": "route", "url": destination,
                    "response": {"contentType": "text/html", "body": hostile_page}
                }),
            )
            .await,
        );

        let loaded = firefox_command(
            &mut state,
            json!({
                "id": "load", "action": "state_load", "path": path.to_str().unwrap()
            }),
        )
        .await;
        assert_success(&loaded);
        assert_eq!(
            get_data(&loaded)["skippedOrigins"],
            if redirect { json!([origin]) } else { json!([]) }
        );
        assert_eq!(
            firefox_eval(&mut state, "window.hooksInstalled").await,
            json!(true)
        );
        assert_eq!(firefox_eval(&mut state, "window.stolen").await, json!([]));
        let storage = firefox_eval(
            &mut state,
            "[localStorage.getItem('token'), sessionStorage.getItem('token')]",
        )
        .await;
        assert_eq!(
            storage,
            if redirect {
                json!([null, null])
            } else {
                json!(["synthetic-local-secret", "synthetic-session-secret"])
            }
        );
    }
    firefox_close(&mut state).await;
}

/// PR3: proxy credentials are never sent to an origin that answers 401.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_proxy_credentials_not_sent_to_origin() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(
        &mut state,
        json!({ "proxy": {
            "server": "http://127.0.0.1:9",
            "bypass": "localhost,127.0.0.1",
            "username": "proxy-user",
            "password": "proxy-pass",
        } }),
    )
    .await;
    firefox_open(&mut state, &fixture.url).await;
    let request = format!(
        "let image = document.createElement('img'); image.src = '{}auth'; document.body.append(image); 'started'",
        fixture.url
    );
    assert_eq!(firefox_eval(&mut state, &request).await, json!("started"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while fixture.hit_count("/auth") == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(fixture.hit_count("/auth") > 0, "origin must be reached");
    assert_eq!(
        fixture.hit_count("#authorization"),
        0,
        "proxy credentials must not reach the origin"
    );
    firefox_close(&mut state).await;
}

async fn firefox_auth_proxy(accept_credentials: bool) -> (String, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).await.unwrap_or(0);
            let text = String::from_utf8_lossy(&request[..n]).to_string();
            if text.contains("http://example.com/") {
                count.fetch_add(1, Ordering::Relaxed);
            }
            let authorized =
                accept_credentials && text.to_ascii_lowercase().contains("proxy-authorization");
            let response = if authorized {
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            } else {
                "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"proxy\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            };
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    (url, attempts)
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_proxy_credentials() {
    required_firefox_binary();
    let tmp = tempfile::tempdir_in(".").unwrap();
    let _env = firefox_test_environment(tmp.path());
    let mut state = DaemonState::new();
    let (rejected_proxy, rejected_attempts) = firefox_auth_proxy(false).await;
    launch_firefox(
        &mut state,
        json!({ "proxy": { "server": rejected_proxy, "username": "proxy-user", "password": "wrong" } }),
    )
    .await;
    let started = std::time::Instant::now();
    let response = firefox_command(
        &mut state,
        json!({ "id": "nav", "action": "navigate", "url": "http://example.com/" }),
    )
    .await;
    let elapsed = started.elapsed();
    firefox_close(&mut state).await;

    assert_eq!(response["success"], false, "{response}");
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("Navigation failed:")),
        "navigation should report a clear error: {response}"
    );
    assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(rejected_attempts.load(Ordering::Relaxed), 3);
    let (accepted_proxy, _) = firefox_auth_proxy(true).await;
    launch_firefox(
        &mut state,
        json!({ "proxy": { "server": accepted_proxy, "username": "proxy-user", "password": "proxy-pass" } }),
    )
    .await;
    let response = firefox_command(
        &mut state,
        json!({ "id": "nav", "action": "navigate", "url": "http://example.com/" }),
    )
    .await;
    firefox_close(&mut state).await;

    assert_success(&response);
}

/// Relaunching through a different proxy must not offer the previous proxy's credentials.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_stale_proxy_credentials_not_reused() {
    required_firefox_binary();
    let tmp = tempfile::tempdir_in(".").unwrap();
    let env = firefox_test_environment(tmp.path());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = requests.clone();
    let proxy_server = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut request = [0u8; 4096];
            let n = stream.read(&mut request).await.unwrap_or(0);
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request[..n]).to_string());
            let _ = stream
                .write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"proxy\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    });
    let mut state = DaemonState::new();
    launch_firefox(
        &mut state,
        json!({ "proxy": {
            "server": "http://127.0.0.1:9",
            "username": "proxy-a-user",
            "password": "proxy-a-pass",
        } }),
    )
    .await;
    env.set("AGENT_BROWSER_PROXY_USERNAME", "proxy-a-user");
    env.set("AGENT_BROWSER_PROXY_PASSWORD", "proxy-a-pass");
    launch_firefox(&mut state, json!({ "proxy": { "server": proxy } })).await;
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        firefox_command(
            &mut state,
            json!({ "id": "nav", "action": "navigate", "url": "http://example.com/" }),
        ),
    )
    .await;
    firefox_close(&mut state).await;
    proxy_server.abort();
    let requests = requests.lock().unwrap().join("\n");

    assert!(!requests.is_empty(), "the second proxy must be used");
    assert!(
        !requests
            .to_ascii_lowercase()
            .contains("proxy-authorization"),
        "the second proxy received the first proxy's credentials: {requests}"
    );
}

/// PR3: clicking a download link saves the file to the requested path.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_download() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("download.txt");
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let resp = firefox_act(
        &mut state,
        "download",
        "#dl",
        json!({ "path": dest.to_str().unwrap() }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "downloaded");

    let wait_dest = out.path().join("waited.txt");
    let ff = state.firefox.as_ref().unwrap();
    let bidi = ff.bidi.clone();
    let context = ff.contexts.active_tab().to_string();
    let trigger = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        bidi.call_function(
            &context,
            "function() { document.querySelector('#dl').click(); return true; }",
            None,
            vec![],
        )
        .await
        .unwrap();
    });
    let resp = firefox_command(
        &mut state,
        json!({
            "id": "waitfordownload",
            "action": "waitfordownload",
            "path": wait_dest.to_str().unwrap(),
            "timeout": 5000,
        }),
    )
    .await;
    trigger.await.unwrap();
    assert_success(&resp);
    assert_eq!(get_data(&resp)["path"], json!(wait_dest.to_string_lossy()));
    assert_eq!(std::fs::read_to_string(&wait_dest).unwrap(), "downloaded");

    let repeated_dest = out.path().join("repeated.txt");
    let resp = firefox_act(
        &mut state,
        "download",
        "#dl",
        json!({ "path": repeated_dest.to_str().unwrap() }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        std::fs::read_to_string(&repeated_dest).unwrap(),
        "downloaded"
    );

    let inline_dest = out.path().join("inline.txt");
    let resp = firefox_act(
        &mut state,
        "download",
        "#dl-inline",
        json!({ "path": inline_dest.to_str().unwrap() }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(std::fs::read_to_string(&inline_dest).unwrap(), "real");
    firefox_close(&mut state).await;
}

/// A download never leaves a page-chosen file name in the directory the user asked for.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_download_never_writes_a_page_chosen_name() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("report.bin");
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        "const hostile = document.createElement('a'); hostile.id = 'dl-hostile'; hostile.href = '/payload';
         const stall = document.createElement('a'); stall.id = 'dl-stall'; stall.href = '/stall';
         document.body.append(hostile, stall); 0",
    )
    .await;

    let resp = firefox_act(
        &mut state,
        "download",
        "#dl-hostile",
        json!({ "path": dest.to_str().unwrap() }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        std::fs::read_to_string(&dest).unwrap(),
        "page chose this name"
    );
    assert_eq!(
        file_names(out.path()),
        ["report.bin"],
        "the destination holds only the requested file"
    );

    let stalled = out.path().join("stalled");
    let resp = firefox_act(
        &mut state,
        "download",
        "#dl-stall",
        json!({ "path": stalled.join("report.bin").to_str().unwrap() }),
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");
    assert_eq!(
        file_names(&stalled),
        Vec::<String>::new(),
        "a failed download leaves nothing in the destination"
    );
    firefox_close(&mut state).await;
}

fn file_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// PR3: console and page errors are recorded, a route serves its mock until unrouted, and offline blocks fetches.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_console_route_and_offline() {
    async fn send(state: &mut DaemonState, action: &str, args: Value) -> Value {
        let mut command = json!({ "id": action, "action": action });
        command
            .as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        firefox_command(state, command).await
    }
    let recorded = |response: &Value, key: &str, marker: &str| {
        get_data(response)[key].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.to_string().contains(marker))
        })
    };
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    firefox_eval(
        &mut state,
        "console.log('firefox-console-probe'); setTimeout(() => { throw new Error('firefox-error-probe'); }, 0); true",
    )
    .await;
    let (messages, errors) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let messages = send(&mut state, "console", json!({})).await;
            let errors = send(&mut state, "errors", json!({})).await;
            if recorded(&messages, "messages", "firefox-console-probe")
                && recorded(&errors, "errors", "firefox-error-probe")
            {
                break (messages, errors);
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("console and page errors must arrive");
    let logged = get_data(&messages)["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["text"]
                .as_str()
                .unwrap_or("")
                .contains("firefox-console-probe")
        })
        .unwrap_or_else(|| panic!("{messages}"));
    assert_eq!(logged["type"], "log");
    assert!(
        recorded(&errors, "errors", "firefox-error-probe"),
        "{errors}"
    );

    let fetch_api = "fetch('/api', {cache: 'no-store'}).then(r => r.text())";
    let served = fixture.hit_count("/api");
    assert_success(
        &send(
            &mut state,
            "route",
            json!({ "url": "**/api", "response": { "body": "mocked" } }),
        )
        .await,
    );
    assert_eq!(firefox_eval(&mut state, fetch_api).await, json!("mocked"));
    assert_eq!(
        fixture.hit_count("/api"),
        served,
        "a mock must not reach the server"
    );
    assert_success(&send(&mut state, "unroute", json!({ "url": "**/api" })).await);
    assert_eq!(firefox_eval(&mut state, fetch_api).await, json!("real"));
    assert!(
        fixture.hit_count("/api") > served,
        "an unrouted fetch must reach the server"
    );

    assert_success(&send(&mut state, "offline", json!({ "offline": true })).await);
    assert_eq!(
        firefox_eval(&mut state, "navigator.onLine").await,
        json!(false)
    );
    let served = fixture.hit_count("/api");
    let blocked = "fetch('/api', {cache: 'no-store'}).then(() => 'served', () => 'blocked')";
    assert_eq!(firefox_eval(&mut state, blocked).await, json!("blocked"));
    assert_eq!(
        fixture.hit_count("/api"),
        served,
        "an offline fetch must not reach the server"
    );
    assert_success(&send(&mut state, "offline", json!({ "offline": false })).await);
    assert_eq!(firefox_eval(&mut state, fetch_api).await, json!("real"));
    assert!(
        fixture.hit_count("/api") > served,
        "fetches resume once online"
    );
    firefox_close(&mut state).await;
}

/// PR3: timezone and color-scheme emulation are visible to page scripts.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_emulation() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    let resp = firefox_command(
        &mut state,
        json!({ "id": "tz", "action": "timezone", "timezoneId": "Asia/Tokyo" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        firefox_eval(
            &mut state,
            "Intl.DateTimeFormat().resolvedOptions().timeZone"
        )
        .await,
        json!("Asia/Tokyo")
    );
    let resp = firefox_command(
        &mut state,
        json!({ "id": "m", "action": "set_media", "colorScheme": "dark" }),
    )
    .await;
    assert_success(&resp);
    assert_eq!(
        firefox_eval(
            &mut state,
            "matchMedia('(prefers-color-scheme: dark)').matches"
        )
        .await,
        json!(true)
    );

    let original = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active_tab()
        .to_string();
    let created = state
        .firefox
        .as_ref()
        .unwrap()
        .bidi
        .send(
            "browsingContext.create",
            json!({"type": "tab", "background": true}),
        )
        .await
        .unwrap();
    let background = created["context"].as_str().unwrap().to_string();
    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    let tabs = state
        .firefox
        .as_ref()
        .unwrap()
        .rdp
        .request("root", "listTabs", json!({}))
        .await
        .unwrap();
    assert!(tabs["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tab| tab["selected"] == true && tab["url"] == fixture.url));
    assert_eq!(
        state.firefox.as_ref().unwrap().contexts.active_tab(),
        original
    );

    assert_success(
        &firefox_command(
            &mut state,
            json!({"id": "background", "action": "tab_switch", "tabId": background}),
        )
        .await,
    );
    firefox_open(&mut state, &fixture.other).await;
    assert_eq!(
        firefox_eval(
            &mut state,
            "matchMedia('(prefers-color-scheme: dark)').matches"
        )
        .await,
        json!(true)
    );
    firefox_close(&mut state).await;
}

/// Runtime user agent, locale, geolocation and permission grants reach page scripts and the wire.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_user_agent_locale_geolocation_and_permissions() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let (echo, _echo_server) = start_echo_server().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;

    assert_ne!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        json!("FirefoxCoverage/1.0")
    );
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "ua", "action": "useragent", "userAgent": "FirefoxCoverage/1.0" }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent").await,
        json!("FirefoxCoverage/1.0")
    );
    let echoed = firefox_eval(
        &mut state,
        &format!("fetch('{echo}/probe').then(r => r.json())"),
    )
    .await;
    assert_eq!(
        echoed["headers"]["User-Agent"],
        json!("FirefoxCoverage/1.0"),
        "{echoed}"
    );

    assert_ne!(
        firefox_eval(&mut state, "navigator.language").await,
        json!("fr-FR")
    );
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "locale", "action": "locale", "locale": "fr-FR" }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, "navigator.language").await,
        json!("fr-FR")
    );

    let permission_state = "navigator.permissions.query({name:'geolocation'}).then(p => p.state)";
    assert_ne!(
        firefox_eval(&mut state, permission_state).await,
        json!("granted")
    );
    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "permissions", "action": "permissions", "permissions": ["geolocation"] }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(&mut state, permission_state).await,
        json!("granted")
    );

    assert_success(
        &firefox_command(
            &mut state,
            json!({
                "id": "geo", "action": "geolocation",
                "latitude": 35.5, "longitude": 139.75, "accuracy": 12.0
            }),
        )
        .await,
    );
    assert_eq!(
        firefox_eval(
            &mut state,
            "new Promise((resolve, reject) => navigator.geolocation.getCurrentPosition(\
             p => resolve([p.coords.latitude, p.coords.longitude, p.coords.accuracy]), reject))",
        )
        .await,
        json!([35.5, 139.75, 12])
    );
    firefox_close(&mut state).await;
}

/// A page asking for geolocation after `geolocation` is set gets a prompt-free denial instead of hanging.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_geolocation_request_does_not_hang() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    let resp = firefox_command(
        &mut state,
        json!({ "id": "g", "action": "geolocation", "latitude": 37.7, "longitude": -122.4 }),
    )
    .await;
    assert_success(&resp);
    let result = firefox_eval(
        &mut state,
        "new Promise(r => navigator.geolocation.getCurrentPosition(() => r('ok'), e => r('err')))",
    )
    .await;
    assert_eq!(result, json!("err"));
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_relaunch_keeps_engine_when_options_change() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    let relaunch = execute_command(
        &json!({ "id": "relaunch", "action": "launch", "colorScheme": "dark" }),
        &mut state,
    )
    .await;
    assert_eq!(relaunch["success"], json!(true), "{relaunch}");
    assert_eq!(state.engine, "firefox");
    firefox_open(&mut state, &fixture.url).await;
    assert_eq!(
        firefox_eval(&mut state, "navigator.userAgent.includes('Firefox')").await,
        json!(true)
    );
    firefox_close(&mut state).await;
}

/// A grant made before the first page loads reaches that origin and every origin navigated to later.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_permissions_follow_navigation() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;

    assert_success(
        &firefox_command(
            &mut state,
            json!({ "id": "permissions", "action": "permissions", "permissions": ["geolocation"] }),
        )
        .await,
    );

    let permission_state = "navigator.permissions.query({name:'geolocation'}).then(p => p.state)";
    firefox_open(&mut state, &fixture.url).await;
    assert_eq!(
        firefox_eval(&mut state, permission_state).await,
        json!("granted")
    );

    firefox_open(&mut state, &fixture.other).await;
    assert_eq!(
        firefox_eval(&mut state, permission_state).await,
        json!("granted")
    );

    let unsupported = firefox_command(
        &mut state,
        json!({ "id": "permissions", "action": "permissions", "permissions": ["clipboard-read"] }),
    )
    .await;
    assert_eq!(
        unsupported.get("success").and_then(Value::as_bool),
        Some(false),
        "{unsupported}"
    );
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_full_page_screenshot_larger_than_64mb() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    firefox_eval(
        &mut state,
        "const c = document.createElement('canvas'); c.width = 1280; c.height = 16000; \
         const x = c.getContext('2d'); const d = x.createImageData(1280, 16000); \
         for (let i = 0; i < d.data.length; i += 65536) crypto.getRandomValues(d.data.subarray(i, i + 65536)); \
         for (let i = 3; i < d.data.length; i += 4) d.data[i] = 255; \
         x.putImageData(d, 0, 0); document.body.append(c); true",
    )
    .await;
    let path = tmp.path().join("full.png");
    let shot = execute_command(
        &json!({ "id": "full", "action": "screenshot", "fullPage": true, "path": path }),
        &mut state,
    )
    .await;
    assert_eq!(shot["success"], json!(true), "{shot}");
    assert!(std::fs::metadata(&path).unwrap().len() > 48 * 1024 * 1024);
    assert_eq!(firefox_eval(&mut state, "1 + 1").await, json!(2));
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_review_live_dialog_setting() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &fixture.url).await;
    let context = state
        .firefox
        .as_ref()
        .unwrap()
        .contexts
        .active()
        .to_string();
    let before = firefox_command(&mut state, json!({"id":"before", "action":"session_info"})).await;
    assert_success(&before);
    for disabled in [true, false] {
        let setting = if disabled { json!(true) } else { Value::Null };
        let changed = firefox_command(
            &mut state,
            json!({"id":"setting", "action":"url", "settings":{"no-auto-dialog":setting}}),
        )
        .await;
        assert_success(&changed);
        firefox_eval(
            &mut state,
            "setTimeout(() => alert('live setting'), 50); true",
        )
        .await;
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let status = firefox_command(
            &mut state,
            json!({"id":"status", "action":"dialog", "response":"status"}),
        )
        .await;
        assert_success(&status);
        assert_eq!(get_data(&status)["hasDialog"], disabled, "{status}");
        if disabled {
            assert_success(
                &firefox_command(
                    &mut state,
                    json!({"id":"enable", "action":"url", "settings":{"no-auto-dialog":null}}),
                )
                .await,
            );
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let status = firefox_command(
                        &mut state,
                        json!({"id":"accepted", "action":"dialog", "response":"status"}),
                    )
                    .await;
                    assert_success(&status);
                    if get_data(&status)["hasDialog"] == false {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        assert_eq!(firefox_eval(&mut state, "1 + 1").await, json!(2));
        let after =
            firefox_command(&mut state, json!({"id":"after", "action":"session_info"})).await;
        assert_eq!(
            get_data(&before)["launchHash"],
            get_data(&after)["launchHash"]
        );
        assert_eq!(state.firefox.as_ref().unwrap().contexts.active(), context);
    }
    firefox_eval(
        &mut state,
        "setTimeout(() => confirm('manual confirm'), 50); true",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    let status = firefox_command(
        &mut state,
        json!({"id":"confirm", "action":"dialog", "response":"status"}),
    )
    .await;
    assert_eq!(get_data(&status)["hasDialog"], true);
    assert_eq!(get_data(&status)["type"], "confirm");
    assert_success(
        &firefox_command(
            &mut state,
            json!({"id":"dismiss", "action":"dialog", "response":"dismiss"}),
        )
        .await,
    );
    firefox_close(&mut state).await;
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_review_session_page_count() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    for (expected, action) in [(1, Some("tab_new")), (2, Some("tab_close")), (1, None)] {
        let info = firefox_command(&mut state, json!({"id":"info", "action":"session_info"})).await;
        assert_success(&info);
        assert_eq!(get_data(&info)["browserLaunched"], true);
        assert_eq!(get_data(&info)["pageCount"], expected, "{info}");
        if let Some(action) = action {
            let result = firefox_command(&mut state, json!({"id":"tab", "action":action})).await;
            assert_success(&result);
        }
    }
    firefox_close(&mut state).await;
}

/// A cookie set without a path uses the RFC 6265 default-path; an explicit path stays scoped.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_window_new_starts_with_an_empty_cookie_jar() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;
    let resp = firefox_command(
        &mut state,
        json!({ "id": "c1", "action": "cookies_set", "name": "main_window", "value": "1" }),
    )
    .await;
    assert_success(&resp);
    firefox_eval(&mut state, "localStorage.setItem('jar', 'default')").await;

    let resp = firefox_command(&mut state, json!({ "id": "w1", "action": "window_new" })).await;
    assert_success(&resp);
    let window_tab = get_data(&resp)["tabId"].clone();
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;
    assert!(
        !firefox_eval(&mut state, "document.cookie")
            .await
            .as_str()
            .unwrap_or_default()
            .contains("main_window=1"),
        "a new window must not see the other window's cookies"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c2", "action": "cookies_set", "name": "isolated_window", "value": "2" }),
    )
    .await;
    assert_success(&resp);
    assert!(
        firefox_eval(&mut state, "document.cookie")
            .await
            .as_str()
            .unwrap_or_default()
            .contains("isolated_window=2"),
        "a cookie set in the new window must stay in its own jar"
    );

    firefox_eval(&mut state, "localStorage.setItem('jar', 'window')").await;
    firefox_open(&mut state, &format!("{}/two", fixture.other)).await;
    let state_path = tmp.path().join("window-state.json");
    let resp = firefox_command(
        &mut state,
        json!({ "id": "sv", "action": "state_save", "path": state_path.to_str().unwrap() }),
    )
    .await;
    assert_success(&resp);
    let saved: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    let origin = fixture.url.trim_end_matches('/');
    assert!(
        saved["origins"].as_array().unwrap().iter().any(|entry| {
            entry["origin"] == origin
                && entry["localStorage"] == json!([{ "name": "jar", "value": "window" }])
        }),
        "state save from an isolated window must read that window's storage: {saved}"
    );
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;

    firefox_eval(&mut state, "window.open(location.href); 'opened'").await;
    let tabs = firefox_command(&mut state, json!({ "id": "p1", "action": "tab_list" })).await;
    let popup = get_data(&tabs)["tabs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tab| tab["tabId"].clone())
        .find(|tab| *tab != "t1" && *tab != window_tab)
        .expect("the popup must be listed as a tab");
    let resp = firefox_command(
        &mut state,
        json!({ "id": "p2", "action": "tab_switch", "tabId": popup }),
    )
    .await;
    assert_success(&resp);
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        has_cookie(&cookies, "isolated_window") && !has_cookie(&cookies, "main_window"),
        "a popup must read the jar of the window that opened it: {cookies}"
    );
    let resp = firefox_command(
        &mut state,
        json!({ "id": "p3", "action": "tab_close", "tabId": popup }),
    )
    .await;
    assert_success(&resp);
    let resp = firefox_command(
        &mut state,
        json!({ "id": "p4", "action": "tab_switch", "tabId": window_tab }),
    )
    .await;
    assert_success(&resp);

    let resp = firefox_command(&mut state, json!({ "id": "t1", "action": "tab_new" })).await;
    assert_success(&resp);
    let second_tab = get_data(&resp)["tabId"].clone();
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;
    assert!(
        firefox_eval(&mut state, "document.cookie")
            .await
            .as_str()
            .unwrap_or_default()
            .contains("isolated_window=2"),
        "a tab opened from the new window must share that window's jar"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c3", "action": "cookies_set", "name": "second_tab", "value": "3" }),
    )
    .await;
    assert_success(&resp);
    assert!(
        firefox_eval(&mut state, "document.cookie")
            .await
            .as_str()
            .unwrap_or_default()
            .contains("second_tab=3"),
        "a cookie set in the tab must stay in the window's jar"
    );
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        has_cookie(&cookies, "second_tab") && has_cookie(&cookies, "isolated_window"),
        "cookies get in a tab opened from the window must read that window's jar: {cookies}"
    );
    assert!(
        !has_cookie(&cookies, "main_window"),
        "cookies get in a tab opened from the window must not read the default jar: {cookies}"
    );

    let resp = firefox_command(&mut state, json!({ "id": "t2", "action": "tab_new" })).await;
    assert_success(&resp);
    let third_tab = get_data(&resp)["tabId"].clone();
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        has_cookie(&cookies, "second_tab") && has_cookie(&cookies, "isolated_window"),
        "a third tab opened from the second must stay in the window's jar: {cookies}"
    );
    assert!(
        !has_cookie(&cookies, "main_window"),
        "a third tab opened from the second must not read the default jar: {cookies}"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c4", "action": "tab_close", "tabId": third_tab }),
    )
    .await;
    assert_success(&resp);
    let resp = firefox_command(
        &mut state,
        json!({ "id": "s1", "action": "tab_switch", "tabId": second_tab }),
    )
    .await;
    assert_success(&resp);
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        has_cookie(&cookies, "second_tab"),
        "the second tab must still read its cookie after the third closes: {cookies}"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c5", "action": "tab_close", "tabId": second_tab }),
    )
    .await;
    assert_success(&resp);
    let resp = firefox_command(
        &mut state,
        json!({ "id": "s2", "action": "tab_switch", "tabId": window_tab }),
    )
    .await;
    assert_success(&resp);
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        has_cookie(&cookies, "isolated_window") && has_cookie(&cookies, "second_tab"),
        "closing the second tab must keep the window's user context alive: {cookies}"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c6", "action": "tab_close", "tabId": window_tab }),
    )
    .await;
    assert_success(&resp);
    let cookies = firefox_eval(&mut state, "document.cookie")
        .await
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        cookies.contains("main_window=1"),
        "closing the window must leave the original cookies in place: {cookies}"
    );
    assert!(
        !cookies.contains("isolated_window=2"),
        "the isolated window must not write into the original jar: {cookies}"
    );
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        has_cookie(&cookies, "main_window") && !has_cookie(&cookies, "second_tab"),
        "the original window must not read the closed window's cookies: {cookies}"
    );
}

/// A cookie set without a path is stored at `/`, so it stays visible site-wide; an explicit path stays scoped.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_cookie_default_path() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{}app/page", fixture.url)).await;

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c1", "action": "cookies_set", "name": "default_path", "value": "1" }),
    )
    .await;
    assert_success(&resp);
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;
    assert!(
        firefox_eval(&mut state, "document.cookie")
            .await
            .as_str()
            .is_some_and(|cookies| cookies.contains("default_path=1")),
        "a cookie set without a path must be visible outside the page directory"
    );

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c2", "action": "cookies_set", "cookies": [
            { "name": "explicit_path", "value": "2", "path": "/one" },
            { "name": "path_boundary", "value": "3", "path": "/foo" },
            { "name": "secure_cookie", "value": "4", "secure": true }
        ] }),
    )
    .await;
    assert_success(&resp);
    assert!(
        !firefox_eval(&mut state, "document.cookie")
            .await
            .as_str()
            .is_some_and(|cookies| cookies.contains("explicit_path=2")),
        "an explicit cookie path must stay scoped"
    );
    firefox_open(&mut state, &format!("{}one", fixture.url)).await;
    assert!(
        firefox_eval(&mut state, "document.cookie")
            .await
            .as_str()
            .is_some_and(|cookies| cookies.contains("explicit_path=2")),
        "the explicit path cookie must be visible under its path"
    );

    let cookies =
        firefox_cookies_get(&mut state, Some(json!([format!("{}foobar", fixture.url)]))).await;
    assert!(has_cookie(&cookies, "default_path"));
    assert!(!has_cookie(&cookies, "path_boundary"));
    assert!(!has_cookie(&cookies, "secure_cookie"));

    let cookies = firefox_cookies_get(
        &mut state,
        Some(json!([fixture.url.replace("localhost", "sub.localhost")])),
    )
    .await;
    assert!(!has_cookie(&cookies, "default_path"));

    let cookies =
        firefox_cookies_get(&mut state, Some(json!([format!("{}foo/bar", fixture.url)]))).await;
    assert!(has_cookie(&cookies, "path_boundary"));

    let cookies = firefox_cookies_get(
        &mut state,
        Some(json!([format!("{}/frame.html", fixture.other)])),
    )
    .await;
    assert!(!has_cookie(&cookies, "default_path"));

    let resp = firefox_command(
        &mut state,
        json!({ "id": "c8", "action": "cookies_get", "urls": ["not a URL"] }),
    )
    .await;
    assert_eq!(resp["success"], false, "{resp}");

    firefox_open(&mut state, &format!("{}/frame.html", fixture.other)).await;
    let resp = firefox_command(
        &mut state,
        json!({ "id": "c9", "action": "cookies_set", "cookies": [
            { "name": "frame_cookie", "value": "5", "path": "/frame.html" },
            { "name": "unmatched_frame_cookie", "value": "6", "path": "/only-other" }
        ] }),
    )
    .await;
    assert_success(&resp);
    firefox_open(&mut state, &fixture.url).await;

    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(has_cookie(&cookies, "frame_cookie"));
    assert!(!has_cookie(&cookies, "unmatched_frame_cookie"));

    let export_path = tmp
        .path()
        .join("all-cookies.json")
        .to_string_lossy()
        .to_string();
    let resp = firefox_command(
        &mut state,
        json!({ "id": "c12", "action": "state_save", "path": export_path }),
    )
    .await;
    assert_success(&resp);
    let path = get_data(&resp)["path"].as_str().unwrap();
    let exported: Value =
        serde_json::from_str(&super::state::read_state_json(path).unwrap()).unwrap();
    for name in ["secure_cookie", "unmatched_frame_cookie"] {
        assert!(has_cookie(&exported["cookies"], name));
    }
    firefox_close(&mut state).await;
}

/// A state save records the origin and values the browser reports, whatever the page forges.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_state_save_ignores_page_forged_storage() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let (origin, server) = start_raw_http_server(html_response(
        "<title>storage</title>
        <script>
            localStorage.setItem('real-key', 'real-value');
            const apply = Function.prototype.apply;
            Function.prototype.apply = function (target, args, ...rest) {
                if (Array.isArray(args) && args.length === 0) {
                    return { origin: 'https://bank.example', localStorage: [{ name: 'forged-key', value: 'forged-value' }], sessionStorage: [] };
                }
                return apply.call(this, target, args, ...rest);
            };
            const getItem = Storage.prototype.getItem;
            Storage.prototype.getItem = function (key) {
                return key === 'real-key' ? 'forged-value' : getItem.call(this, key);
            };
        </script>",
    ))
    .await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{origin}/")).await;
    assert_eq!(
        firefox_eval(&mut state, "localStorage.getItem('real-key')").await,
        json!("forged-value"),
        "the page must be able to forge what a main-realm reader sees"
    );

    let path = tmp.path().join("forged-state.json");
    let resp = firefox_command(
        &mut state,
        json!({ "id": "save", "action": "state_save", "path": path.to_str().unwrap() }),
    )
    .await;
    assert_success(&resp);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        saved["origins"],
        json!([{
            "origin": origin,
            "localStorage": [{ "name": "real-key", "value": "real-value" }],
            "sessionStorage": []
        }]),
        "the saved state must hold the browser origin and the real entry: {saved}"
    );
    assert!(
        !saved["origins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["origin"] == "https://bank.example"),
        "a page must not save storage under another origin: {saved}"
    );
    firefox_close(&mut state).await;
    server.abort();
}

/// Uses a temporary HOME on Unix to keep autosave files out of the user profile.
#[cfg(unix)]
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_autosave_preserves_blocked_storage_and_clears_readable_empty_storage() {
    required_firefox_binary();
    let home = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    _env.set("HOME", home.path().to_str().unwrap());
    _env.remove("AGENT_BROWSER_ENCRYPTION_KEY");
    let (origin, server) = start_raw_http_server(html_response("<title>storage</title>")).await;
    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{origin}/")).await;
    firefox_eval(
        &mut state,
        "localStorage.setItem('local', 'saved'); sessionStorage.setItem('session', 'saved'); true",
    )
    .await;
    let path = super::bidi::storage::save_auto_state(
        state.firefox.as_mut().unwrap(),
        "blocked-storage",
        "autosave",
    )
    .await
    .unwrap();
    let before: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        before["origins"][0]["localStorage"],
        json!([{ "name": "local", "value": "saved" }])
    );
    assert_eq!(
        before["origins"][0]["sessionStorage"],
        json!([{ "name": "session", "value": "saved" }])
    );

    for storage in ["localStorage", "sessionStorage"] {
        firefox_eval(
            &mut state,
            &format!(
                "window.savedDescriptor = Object.getOwnPropertyDescriptor(window, '{storage}'); \
                 Object.defineProperty(window, '{storage}', {{ configurable: true, get() {{ throw new DOMException('Blocked', 'SecurityError'); }} }}); true"
            ),
        )
        .await;
        super::bidi::storage::save_auto_state(
            state.firefox.as_mut().unwrap(),
            "blocked-storage",
            "autosave",
        )
        .await
        .unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["origins"], before["origins"], "{storage}");
        firefox_eval(
            &mut state,
            &format!("Object.defineProperty(window, '{storage}', window.savedDescriptor); true"),
        )
        .await;
    }

    firefox_open(&mut state, "about:blank").await;
    super::bidi::storage::save_auto_state(
        state.firefox.as_mut().unwrap(),
        "blocked-storage",
        "autosave",
    )
    .await
    .unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["origins"], before["origins"]);
    firefox_open(&mut state, &format!("{origin}/")).await;

    firefox_eval(
        &mut state,
        "localStorage.clear(); sessionStorage.clear(); true",
    )
    .await;
    super::bidi::storage::save_auto_state(
        state.firefox.as_mut().unwrap(),
        "blocked-storage",
        "autosave",
    )
    .await
    .unwrap();
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(saved["origins"], json!([]));
    firefox_close(&mut state).await;
    server.abort();
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_restore_validation_failure_does_not_overwrite_state() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let restore_key = format!(
        "e2e-firefox-restore-validation-{}",
        &uuid::Uuid::new_v4().to_string()[..8]
    );

    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({ "restoreKey": restore_key })).await;
    firefox_open(&mut state, &fixture.url).await;
    let resp = firefox_command(
        &mut state,
        json!({ "id": "cookie", "action": "cookies_set", "name": "known_good", "value": "1" }),
    )
    .await;
    assert_success(&resp);
    let resp = firefox_command(&mut state, json!({ "id": "close", "action": "close" })).await;
    assert_eq!(get_data(&resp)["saveStatus"], "saved");

    let path = super::state::find_auto_state_file(&restore_key)
        .expect("first close should create Firefox restore state");
    let before = std::fs::read(&path).expect("restore state should be readable");

    let mut state = DaemonState::new();
    launch_firefox(
        &mut state,
        json!({
            "restoreKey": restore_key,
            "restoreCheckText": "text-that-does-not-exist-in-the-page"
        }),
    )
    .await;
    firefox_open(&mut state, &fixture.url).await;
    assert_eq!(state.restore_status, "loaded_but_invalid");
    firefox_eval(&mut state, "localStorage.setItem('uncommitted', '1')").await;
    let resp = firefox_command(&mut state, json!({ "id": "close", "action": "close" })).await;
    assert_eq!(get_data(&resp)["saveStatus"], "skipped_restore_failed");

    assert_eq!(std::fs::read(&path).unwrap(), before);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{path}.previous"));
}

#[tokio::test]
#[ignore]
async fn e2e_firefox_state_load_keeps_storage_away_from_page_scripts_and_redirects() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let (other_origin, other_server) =
        start_raw_http_server(html_response("<title>other</title>")).await;
    let (redirect_origin, redirect_server) = start_raw_http_server(format!(
        "HTTP/1.1 302 Found\r\nLocation: {}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        other_origin
    ))
    .await;
    let (hook_origin, hook_server) = start_raw_http_server(html_response(
        "<script>
            window.__captured = [];
            const parse = JSON.parse;
            JSON.parse = function (...args) { window.__captured.push(args); return parse.apply(this, args); };
            const setItem = Storage.prototype.setItem;
            Storage.prototype.setItem = function (...args) { window.__captured.push(args); return setItem.apply(this, args); };
        </script>",
    ))
    .await;

    let state_path = tmp.path().join("state.json");
    std::fs::write(
        &state_path,
        serde_json::to_vec(&json!({
            "cookies": [],
            "origins": [
                {
                    "origin": redirect_origin,
                    "localStorage": [{ "name": "redirect-token", "value": "redirect-secret" }],
                    "sessionStorage": [{ "name": "redirect-session", "value": "redirect-secret" }]
                },
                {
                    "origin": hook_origin,
                    "localStorage": [{ "name": "hook-token", "value": "hook-secret" }],
                    "sessionStorage": [{ "name": "hook-session", "value": "hook-secret" }]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    let load = firefox_command(
        &mut state,
        json!({ "id": "load", "action": "state_load", "path": state_path }),
    )
    .await;
    assert_success(&load);
    assert_eq!(get_data(&load)["skippedOrigins"], json!([redirect_origin]));

    assert_eq!(
        firefox_eval(&mut state, "window.__captured.length").await,
        json!(0)
    );
    assert_eq!(
        firefox_eval(
            &mut state,
            "[localStorage.getItem('hook-token'), sessionStorage.getItem('hook-session')]"
        )
        .await,
        json!(["hook-secret", "hook-secret"])
    );

    firefox_open(&mut state, &format!("{}/", other_origin)).await;
    assert_eq!(
        firefox_eval(&mut state, "localStorage.length + sessionStorage.length").await,
        json!(0)
    );

    firefox_close(&mut state).await;
    other_server.abort();
    redirect_server.abort();
    hook_server.abort();
}

/// A state load while an isolated window is active must restore cookies into that window's jar.
#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_state_load_restores_cookies_into_the_active_window() {
    required_firefox_binary();
    let tmp = tempfile::tempdir().unwrap();
    let _env = firefox_test_environment(tmp.path());
    let fixture = firefox_fixture().await;
    let state_path = tmp.path().join("state.json");
    std::fs::write(
        &state_path,
        serde_json::to_vec(&json!({
            "cookies": [{
                "name": "from_state",
                "value": "1",
                "domain": "localhost",
                "path": "/",
                "expires": -1,
                "httpOnly": false,
                "secure": false
            }],
            "origins": []
        }))
        .unwrap(),
    )
    .unwrap();

    let mut state = DaemonState::new();
    launch_firefox(&mut state, json!({})).await;
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;
    let resp = firefox_command(&mut state, json!({ "id": "w1", "action": "window_new" })).await;
    assert_success(&resp);
    firefox_open(&mut state, &format!("{}two", fixture.url)).await;

    let load = firefox_command(
        &mut state,
        json!({ "id": "l1", "action": "state_load", "path": state_path.to_str().unwrap() }),
    )
    .await;
    assert_success(&load);
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        has_cookie(&cookies, "from_state"),
        "a restored cookie must land in the active window's jar: {cookies}"
    );

    let resp = firefox_command(&mut state, json!({ "id": "c1", "action": "tab_close" })).await;
    assert_success(&resp);
    let cookies = firefox_cookies_get(&mut state, None).await;
    assert!(
        !has_cookie(&cookies, "from_state"),
        "a restored cookie must not land in the default jar: {cookies}"
    );
}

const FLOW_FIXTURE_PAGE: &str = r#"<!doctype html><html><body>
<input id="email" type="text" placeholder="Email">
<input id="code" autocomplete="one-time-code">
<input id="pw" type="password">
<button id="login" onclick="login()">Login</button>
<button id="session" onclick="session()">Session</button>
<button id="items" onclick="items()">Items</button>
<button id="gql" onclick="gql()">Gql</button>
<button id="add" onclick="add()">Add</button>
<button id="flaky" onclick="fetch('/api/flaky?_=' + Date.now())">Flaky</button>
<button id="cartadd" onclick="fetch('/cart/remove?id=7')">CartAdd</button>
<button id="slow" onclick="fetch('/api/slow?q=stove')">Slow</button>
<button id="stream" onclick="fetch('/api/stream')">Stream</button>
<button id="moved" onclick="fetch('/api/moved')">Moved</button>
<button id="hang" onclick="fetch('/api/hang?q=1')">Hang</button>
<button id="hold" onclick="fetch('/api/hold')">Hold</button>
<button id="both" onclick="login(); fetch('/api/hang?q=1')">Both</button>
<button id="hangs" onclick="fetch('/api/hangs/' + document.getElementById('pw').value)">Hangs</button>
<script>
let csrf = ''; let user = '';
async function login() { await fetch('/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ email: document.getElementById('email').value, pw: document.getElementById('pw').value }) }); }
async function session() { const r = await fetch('/api/session'); const j = await r.json(); csrf = j.csrf; user = j.user; }
async function items() { await fetch('/api/items?u=' + encodeURIComponent(user)); }
async function gql() { await fetch('/graphql', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ query: 'query Items { items { id name } }' }) }); }
async function add() { await fetch('/api/cart', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ id: 7, csrf }) }); }
</script></body></html>"#;

struct FlowFixture {
    site: String,
    other: String,
    log: Arc<Mutex<Vec<String>>>,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

impl FlowFixture {
    fn hits(&self, needle: &str) -> usize {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(needle))
            .count()
    }
}

/// Clicks that make the fixture page send a request, by the log line it leaves.
const FLOW_FIXTURE_FIRES: &[(&str, &str)] = &[
    ("#login", "POST /login"),
    ("#session", "GET /api/session"),
    ("#items", "GET /api/items"),
    ("#gql", "POST /graphql"),
    ("#add", "POST /api/cart"),
    ("#cartadd", "GET /cart/remove"),
    ("#flaky", "GET /api/flaky"),
];

impl FlowFixture {
    /// Wait until `done` holds, for as long as a replay waits for a response.
    async fn wait_until(&self, done: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !done() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// Run a command; for a click that fires a request, wait until the server
    /// has it and the browser has read the whole response.
    async fn run_settled(&self, state: &mut DaemonState, cmd: &Value) -> Value {
        let needle = (cmd["action"] == "click")
            .then(|| cmd["selector"].as_str())
            .flatten()
            .and_then(|sel| FLOW_FIXTURE_FIRES.iter().find(|(s, _)| *s == sel))
            .map(|(_, needle)| *needle);
        let before = needle.map(|n| self.hits(n)).unwrap_or(0);
        let resp = exec(cmd, state).await;
        if let Some(needle) = needle {
            self.wait_until(|| self.hits(needle) > before).await;
        }
        settle_responses(state).await;
        resp
    }
}

/// Wait until every request the browser has seen has finished loading.
async fn settle_responses(state: &mut DaemonState) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let _ = state.drain_cdp_events_background().await;
        let finished = state
            .har_entries
            .iter()
            .all(|e| e.loading_finished_timestamp.is_some());
        if finished || std::time::Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

impl Drop for FlowFixture {
    fn drop(&mut self) {
        for s in &self.servers {
            s.abort();
        }
    }
}

async fn flow_fixture() -> FlowFixture {
    let site_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let other_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let site_port = site_listener.local_addr().unwrap().port();
    let other_port = other_listener.local_addr().unwrap().port();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, String, String, String) {
        let mut data = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = stream.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            data.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&data).to_string();
            if let Some(head_end) = text.find("\r\n\r\n") {
                let length = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                    })
                    .unwrap_or(0);
                if data.len() >= head_end + 4 + length {
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&data).to_string();
        let head_end = text.find("\r\n\r\n").unwrap_or(text.len());
        let mut lines = text[..head_end].lines();
        let first = lines.next().unwrap_or("").to_string();
        let mut parts = first.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();
        let cookie = lines
            .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
            .unwrap_or("")
            .to_string();
        let body = text.get(head_end + 4..).unwrap_or("").to_string();
        (method, path, cookie, body)
    }

    async fn reply(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        ctype: &str,
        extra: &str,
        body: &str,
    ) {
        let response = format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n{}Connection: close\r\n\r\n{}",
            status, ctype, body.len(), extra, body
        );
        let _ = stream.write_all(response.as_bytes()).await;
    }

    let site_log = log.clone();
    let site = tokio::spawn(async move {
        loop {
            let (mut stream, _) = site_listener.accept().await.unwrap();
            let (method, path, cookie, body) = read_request(&mut stream).await;
            site_log
                .lock()
                .unwrap()
                .push(format!("site {} {} {}", method, path, body));
            let signed_in = cookie.contains("sid=abc");
            let route = path.split('?').next().unwrap_or("").to_string();
            match (method.as_str(), route.as_str()) {
                ("GET", "/") => {
                    reply(&mut stream, "200 OK", "text/html", "", FLOW_FIXTURE_PAGE).await
                }
                ("POST", "/login") => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "Set-Cookie: sid=abc; Path=/\r\n",
                        r#"{"ok":true}"#,
                    )
                    .await
                }
                ("GET", "/api/flaky") if path.contains("_=") => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"ok":true}"#,
                    )
                    .await
                }
                ("GET", "/api/flaky") => {
                    reply(
                        &mut stream,
                        "401 Unauthorized",
                        "application/json",
                        "",
                        "{}",
                    )
                    .await
                }
                ("GET", "/api/session") if signed_in => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"csrf":"tok-8d41","user":"ana"}"#,
                    )
                    .await
                }
                ("GET", "/api/items") if signed_in => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"items":[{"id":1,"name":"kettle"},{"id":2,"name":"toaster"}]}"#,
                    )
                    .await
                }
                ("GET", "/api/moved") => {
                    reply(
                        &mut stream,
                        "302 Found",
                        "text/plain",
                        "Location: /api/target\r\n",
                        "",
                    )
                    .await
                }
                ("GET", "/api/target") => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"items":[{"id":1,"name":"stove"}]}"#,
                    )
                    .await
                }
                ("GET", "/api/old") => {
                    reply(
                        &mut stream,
                        "302 Found",
                        "text/plain",
                        "Location: /\r\n",
                        "",
                    )
                    .await
                }
                ("GET", r) if r.starts_with("/api/hangs/") => {
                    tokio::spawn(async move {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n[")
                            .await;
                        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    });
                }
                ("GET", "/api/hold") => {
                    // A long poll: the server holds the request and writes nothing.
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                        drop(stream);
                    });
                }
                ("GET", "/api/hang") => {
                    // First-party JSON headers, then a body that never ends.
                    tokio::spawn(async move {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n[")
                            .await;
                        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    });
                }
                ("GET", "/api/stream") => {
                    // Headers go out and the body never ends.
                    tokio::spawn(async move {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nAccess-Control-Allow-Origin: *\r\n\r\ndata: hi\n\n")
                            .await;
                        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
                    });
                }
                ("GET", "/api/slow") => {
                    // The request is in flight when the click returns.
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"items":[{"id":1,"name":"stove"}]}"#,
                    )
                    .await
                }
                ("POST", "/graphql") => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"data":{"items":[{"id":1,"name":"kettle"}]}}"#,
                    )
                    .await
                }
                ("POST", "/api/cart") if body.contains("tok-8d41") => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"cart":[7]}"#,
                    )
                    .await
                }
                ("POST", "/api/cart") => {
                    reply(&mut stream, "403 Forbidden", "application/json", "", "{}").await
                }
                ("GET", p) if p.starts_with("/cart/remove") => {
                    reply(
                        &mut stream,
                        "200 OK",
                        "application/json",
                        "",
                        r#"{"added":true}"#,
                    )
                    .await
                }
                ("GET", "/go") => {
                    reply(
                        &mut stream,
                        "302 Found",
                        "text/plain",
                        &format!("Location: http://localhost:{}/data\r\n", other_port),
                        "",
                    )
                    .await
                }
                (_, _) if !signed_in && path.starts_with("/api/") => {
                    reply(
                        &mut stream,
                        "401 Unauthorized",
                        "application/json",
                        "",
                        "{}",
                    )
                    .await
                }
                _ => reply(&mut stream, "404 Not Found", "application/json", "", "{}").await,
            }
        }
    });
    let other_log = log.clone();
    let other = tokio::spawn(async move {
        loop {
            let (mut stream, _) = other_listener.accept().await.unwrap();
            let (method, path, _, _) = read_request(&mut stream).await;
            other_log
                .lock()
                .unwrap()
                .push(format!("other {} {}", method, path));
            reply(
                &mut stream,
                "200 OK",
                "application/json",
                "",
                r#"{"other":true}"#,
            )
            .await;
        }
    });
    FlowFixture {
        site: format!("http://127.0.0.1:{}", site_port),
        other: format!("http://localhost:{}", other_port),
        log,
        servers: vec![site, other],
    }
}

#[tokio::test]
#[ignore]
async fn e2e_flow_capture_save_run_and_guards() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&[
        "AGENT_BROWSER_FLOWS_DIR",
        "AGENT_BROWSER_ACTION_POLICY",
        "AGENT_BROWSER_CONFIRM_ACTIONS",
    ]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    guard.remove("AGENT_BROWSER_ACTION_POLICY");
    guard.remove("AGENT_BROWSER_CONFIRM_ACTIONS");
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    let mut n = 0;
    fn run(n: &mut u32, mut cmd: Value) -> Value {
        *n += 1;
        cmd["id"] = json!(format!("f{}", n));
        cmd
    }

    let launch = run(
        &mut n,
        json!({ "action": "launch", "headless": true, "capture": ["flow"] }),
    );
    assert_success(&exec(&launch, &mut state).await);
    assert!(state.flow.active());
    // Turning capture on for a session that has none relaunches it with capture.
    let mut plain = DaemonState::new();
    assert_success(
        &exec(
            &json!({ "id": "p1", "action": "launch", "headless": true }),
            &mut plain,
        )
        .await,
    );
    assert!(!plain.flow.active());
    let late = exec(
        &json!({ "id": "p2", "action": "launch", "headless": true, "capture": ["flow"] }),
        &mut plain,
    )
    .await;
    assert_success(&late);
    assert_eq!(
        late["data"]["lifecycle"]["relaunchedBrowser"], true,
        "{late}"
    );
    assert!(plain.flow.active());
    let _ = exec(&json!({ "id": "p3", "action": "close" }), &mut plain).await;

    let first = exec(
        &run(
            &mut n,
            json!({ "action": "navigate", "url": format!("{}/", fx.site) }),
        ),
        &mut state,
    )
    .await;
    assert_success(&first);
    assert_eq!(first["recorded"]["seq"], 1);
    for (action, selector, value) in [
        ("fill", "#email", Some("ana@example.com")),
        ("fill", "#code", Some("482913")),
        ("click", "#login", None),
        ("click", "#session", None),
        ("click", "#add", None),
        ("click", "#cartadd", None),
        ("click", "#pw", None),
    ] {
        let mut cmd = json!({ "action": action, "selector": selector });
        if let Some(v) = value {
            cmd["value"] = json!(v);
        }
        let resp = fx.run_settled(&mut state, &run(&mut n, cmd)).await;
        assert_success(&resp);
    }
    let typed = exec(
        &run(
            &mut n,
            json!({ "action": "keyboard", "subaction": "type", "text": "s3cret!" }),
        ),
        &mut state,
    )
    .await;
    assert_success(&typed);

    let failed = exec(
        &run(
            &mut n,
            json!({ "action": "click", "selector": "#missing", "timeout": 500 }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(failed["success"], false);
    assert!(
        failed["recorded"]["seq"].is_u64(),
        "failed commands are recorded: {failed}"
    );

    let steps = exec(&run(&mut n, json!({ "action": "flow_steps" })), &mut state).await;
    assert_success(&steps);
    assert_eq!(steps["data"]["steps"].as_array().unwrap().len(), 10);
    assert_eq!(steps["data"]["steps"][9]["ok"], false);

    // Save the first flow without the failed step, keep recording.
    let saved = exec(
        &run(
            &mut n,
            json!({ "action": "flow_save", "name": "cart", "to": 9, "description": "Add to cart" }),
        ),
        &mut state,
    )
    .await;
    assert_success(&saved);
    assert_eq!(saved["data"]["from"], 1);
    assert_eq!(saved["data"]["to"], 9);
    assert!(state.flow.active(), "save keeps the capture running");
    // The flow answers with the cart write, picked by request id.
    let add_id = saved["data"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["url"].as_str().unwrap().ends_with("/api/cart"))
        .and_then(|r| r["id"].as_str())
        .unwrap()
        .to_string();
    let resaved = exec(
        &run(
            &mut n,
            json!({ "action": "flow_save", "name": "cart", "from": 1, "to": 9, "output": add_id }),
        ),
        &mut state,
    )
    .await;
    assert_success(&resaved);
    assert_eq!(resaved["data"]["output"], add_id.as_str());
    let writes = saved["data"]["writes"].as_array().unwrap();
    assert!(writes.iter().any(|w| w["method"] == "POST"
        && w["url"].as_str().unwrap().ends_with("/api/cart")
        && w["kind"] == "explicit"));
    assert!(writes
        .iter()
        .any(|w| w["method"] == "POST" && w["url"].as_str().unwrap().ends_with("/login")));
    assert!(writes.iter().any(|w| w["method"] == "GET"
        && w["url"].as_str().unwrap().contains("/cart/remove?id=7")
        && w["kind"] == "suspected"));

    // A second flow in the same session, from the step after the first save.
    for selector in ["#items", "#gql"] {
        let resp = fx
            .run_settled(
                &mut state,
                &run(&mut n, json!({ "action": "click", "selector": selector })),
            )
            .await;
        assert_success(&resp);
    }
    let saved2 = exec(
        &run(&mut n, json!({ "action": "flow_save", "name": "browse" })),
        &mut state,
    )
    .await;
    assert_success(&saved2);
    assert_eq!(saved2["data"]["from"], 10);
    assert_eq!(
        saved2["data"]["writes"],
        json!([]),
        "a GraphQL query and a data GET are reads: {}",
        saved2["data"]
    );

    // Files: no typed secret, the email is a param, the CSRF value is a reference.
    let cart_path = dir.path().join("127.0.0.1").join("cart.json");
    let cart_text = std::fs::read_to_string(&cart_path).unwrap();
    assert!(!cart_text.contains("482913"));
    assert!(
        !cart_text.contains("s3cret!"),
        "keyboard text into a password field is redacted"
    );
    assert!(
        !cart_text.contains("ana@example.com"),
        "typed values are not stored as examples"
    );
    assert!(cart_text.contains(r#""source": "typed""#));
    assert!(cart_text.contains("{{email}}"));
    assert!(cart_text.contains("<redacted>"));
    assert!(dir.path().join("127.0.0.1").join("browse.json").exists());

    // Runs: a read flow needs no confirmation; a write flow asks first.
    let browse = exec(
        &run(
            &mut n,
            json!({ "action": "flow_run", "name": "browse", "params": {} }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(browse["data"]["ok"], true, "{browse}");
    let ask = exec(
        &run(
            &mut n,
            json!({ "action": "flow_run", "name": "cart", "params": { "email": "ana@example.com" } }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(ask["data"]["confirmation_required"], true, "{ask}");
    assert!(ask["data"]["action"]
        .as_str()
        .unwrap()
        .starts_with("flow_write:"));

    let posts_before = fx.hits("POST /api/cart");
    let sent = exec(
        &run(
            &mut n,
            json!({ "action": "flow_run", "name": "cart", "params": { "email": "ana@example.com" }, "allowWrite": "all" }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(sent["data"]["ok"], true, "{sent}");
    assert_eq!(
        fx.hits("POST /api/cart"),
        posts_before + 1,
        "the write is sent once: {sent}\n{:#?}",
        fx.log.lock().unwrap()
    );

    // An edited flow cannot reach another host, directly or by redirect.
    let browse_path = dir.path().join("127.0.0.1").join("browse.json");
    let original = std::fs::read_to_string(&browse_path).unwrap();
    let other_hits = fx.hits("other ");
    for target in [format!("{}/data", fx.other), format!("{}/go", fx.site)] {
        let mut flow: Value = serde_json::from_str(&original).unwrap();
        let at = flow["steps"]
            .as_array()
            .unwrap()
            .iter()
            .position(|s| !s["requests"].as_array().unwrap().is_empty())
            .unwrap();
        flow["steps"][at]["requests"][0]["url"] = json!(target);
        flow["steps"][at]["requests"][0]["tier"] = json!("http");
        std::fs::write(&browse_path, serde_json::to_string(&flow).unwrap()).unwrap();
        let resp = exec(
            &run(
                &mut n,
                json!({ "action": "flow_run", "name": "browse", "params": {} }),
            ),
            &mut state,
        )
        .await;
        assert_eq!(resp["success"], false, "{target}: {resp}");
        if target.starts_with(&fx.other) {
            assert!(
                resp["error"]
                    .as_str()
                    .unwrap()
                    .contains("outside the hosts"),
                "{resp}"
            );
        }
    }
    assert_eq!(
        fx.hits("other "),
        other_hits,
        "no request reached the other host"
    );

    // Runs are not recorded, and close reports nothing unsaved after the last save.
    let steps = exec(&run(&mut n, json!({ "action": "flow_steps" })), &mut state).await;
    assert_eq!(steps["data"]["steps"].as_array().unwrap().len(), 12);
    let closed = exec(&run(&mut n, json!({ "action": "close" })), &mut state).await;
    assert_success(&closed);
    assert!(closed["data"].get("unsaved").is_none(), "{closed}");
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_includes_requests_still_in_flight() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    assert_success(
        &exec(
            &json!({ "id": "1", "action": "launch", "headless": true, "capture": ["flow"] }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &exec(
            &json!({ "id": "2", "action": "navigate", "url": format!("{}/", fx.site) }),
            &mut state,
        )
        .await,
    );
    // Save as soon as the server has the request, before its response is read.
    for i in 0..3 {
        let before = fx.hits("GET /api/slow");
        assert_success(
            &exec(
                &json!({ "id": format!("c{i}"), "action": "click", "selector": "#slow" }),
                &mut state,
            )
            .await,
        );
        fx.wait_until(|| fx.hits("GET /api/slow") > before).await;
        let saved = exec(
            &json!({ "id": format!("s{i}"), "action": "flow_save", "name": format!("stoves{i}") }),
            &mut state,
        )
        .await;
        assert_success(&saved);
        assert_eq!(
            saved["data"]["requests"].as_array().unwrap().len(),
            1,
            "{saved}"
        );
    }
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_does_not_wait_for_streaming_responses() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    assert_success(
        &exec(
            &json!({ "id": "1", "action": "launch", "headless": true, "capture": ["flow"] }),
            &mut state,
        )
        .await,
    );
    assert_success(
        &exec(
            &json!({ "id": "2", "action": "navigate", "url": format!("{}/", fx.site) }),
            &mut state,
        )
        .await,
    );
    for (id, sel) in [("c1", "#stream"), ("c2", "#slow")] {
        assert_success(
            &exec(
                &json!({ "id": id, "action": "click", "selector": sel }),
                &mut state,
            )
            .await,
        );
    }
    fx.wait_until(|| fx.hits("GET /api/stream") > 0 && fx.hits("GET /api/slow") > 0)
        .await;
    let started = std::time::Instant::now();
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "streams" }),
        &mut state,
    )
    .await;
    let took = started.elapsed();
    assert_success(&saved);
    assert!(
        took < std::time::Duration::from_secs(5),
        "save took {took:?}"
    );
    assert_eq!(
        saved["data"]["requests"].as_array().unwrap().len(),
        1,
        "{saved}"
    );
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

async fn flow_fixture_session(fx: &FlowFixture, state: &mut DaemonState) {
    assert_success(
        &exec(
            &json!({ "id": "1", "action": "launch", "headless": true, "capture": ["flow"] }),
            state,
        )
        .await,
    );
    assert_success(
        &exec(
            &json!({ "id": "2", "action": "navigate", "url": format!("{}/", fx.site) }),
            state,
        )
        .await,
    );
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_does_not_wait_on_redirected_requests() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    assert_success(
        &exec(
            &json!({ "id": "c1", "action": "click", "selector": "#moved" }),
            &mut state,
        )
        .await,
    );
    fx.wait_until(|| fx.hits("GET /api/target") > 0).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let started = std::time::Instant::now();
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "moved", "timeout": 6000 }),
        &mut state,
    )
    .await;
    let took = started.elapsed();
    assert_success(&saved);
    assert!(
        took < std::time::Duration::from_secs(3),
        "save took {took:?}"
    );
    assert_eq!(
        saved["data"]["requests"].as_array().unwrap().len(),
        1,
        "{saved}"
    );
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_har_finalizes_a_redirect_hop() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    assert_success(
        &exec(
            &json!({ "id": "c1", "action": "click", "selector": "#moved" }),
            &mut state,
        )
        .await,
    );
    fx.wait_until(|| fx.hits("GET /api/target") > 0).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    state.drain_cdp_events_background().await.unwrap();
    let hop = state
        .har_entries
        .iter()
        .find(|e| e.url.ends_with("/api/moved"))
        .expect("redirect hop recorded");
    assert_eq!(hop.status, Some(302), "{:?}", hop.status);
    assert!(hop.loading_finished_timestamp.is_some());
    assert!(hop
        .response_headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("location") && v == "/api/target"));
    let target = state
        .har_entries
        .iter()
        .find(|e| e.url.ends_with("/api/target"))
        .expect("redirect target recorded");
    assert_eq!(target.status, Some(200));
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_does_not_wait_on_a_long_poll() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    for (id, sel) in [("c1", "#hang"), ("c2", "#slow")] {
        assert_success(
            &exec(
                &json!({ "id": id, "action": "click", "selector": sel }),
                &mut state,
            )
            .await,
        );
    }
    fx.wait_until(|| fx.hits("GET /api/hang") > 0 && fx.hits("GET /api/slow") > 0)
        .await;
    // The JSON long poll has its headers; only /api/slow is waited on.
    let started = std::time::Instant::now();
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "poll", "timeout": 10000 }),
        &mut state,
    )
    .await;
    let took = started.elapsed();
    assert_success(&saved);
    assert!(
        took < std::time::Duration::from_secs(3),
        "save took {took:?}"
    );
    assert_eq!(
        saved["data"]["requests"].as_array().unwrap().len(),
        1,
        "{saved}"
    );
    assert!(saved["data"]["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["reason"] == "still loading at save: GET /api/hang"));
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_does_not_wait_on_a_held_request_from_an_earlier_step() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    for (id, sel) in [("c1", "#hold"), ("c2", "#slow")] {
        assert_success(
            &exec(
                &json!({ "id": id, "action": "click", "selector": sel }),
                &mut state,
            )
            .await,
        );
    }
    fx.wait_until(|| fx.hits("GET /api/hold") > 0 && fx.hits("GET /api/slow") > 0)
        .await;
    // The held request has no headers, but a later step ran while it hung.
    let started = std::time::Instant::now();
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "held", "timeout": 10000 }),
        &mut state,
    )
    .await;
    let took = started.elapsed();
    assert_success(&saved);
    assert!(
        took < std::time::Duration::from_secs(3),
        "save took {took:?}"
    );
    assert_eq!(
        saved["data"]["requests"].as_array().unwrap().len(),
        1,
        "{saved}"
    );
    assert!(saved["data"]["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["reason"] == "still loading at save: GET /api/hold"));
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_failure_names_the_request_still_loading() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    assert_success(
        &exec(
            &json!({ "id": "c1", "action": "click", "selector": "#hang" }),
            &mut state,
        )
        .await,
    );
    fx.wait_until(|| fx.hits("GET /api/hang") > 0).await;
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "only-hang", "timeout": 400 }),
        &mut state,
    )
    .await;
    let err = saved["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("still loading at save: GET /api/hang"),
        "{saved}"
    );
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_reports_requests_still_loading_at_the_deadline() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    for (id, sel) in [("c1", "#hang"), ("c2", "#slow")] {
        assert_success(
            &exec(
                &json!({ "id": id, "action": "click", "selector": sel }),
                &mut state,
            )
            .await,
        );
    }
    fx.wait_until(|| fx.hits("GET /api/hang") > 0 && fx.hits("GET /api/slow") > 0)
        .await;
    let started = std::time::Instant::now();
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "hang", "timeout": 1500 }),
        &mut state,
    )
    .await;
    let took = started.elapsed();
    assert_success(&saved);
    assert!(
        took < std::time::Duration::from_secs(5),
        "save took {took:?}"
    );
    assert_eq!(
        saved["data"]["requests"].as_array().unwrap().len(),
        1,
        "{saved}"
    );
    let gaps = saved["data"]["gaps"].as_array().unwrap();
    // The step that started the request owns the gap, and nothing else is
    // said about that step.
    let for_step_2: Vec<&Value> = gaps.iter().filter(|g| g["step"] == 2).collect();
    assert_eq!(for_step_2.len(), 1, "{saved}");
    assert_eq!(for_step_2[0]["command"], "click #hang", "{saved}");
    assert_eq!(
        for_step_2[0]["reason"], "still loading at save: GET /api/hang",
        "{saved}"
    );
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_gap_for_a_loading_request_does_not_print_a_secret() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    for cmd in [
        json!({ "id": "f", "action": "fill", "selector": "#pw", "value": "tk-91ab-77" }),
        json!({ "id": "c1", "action": "click", "selector": "#hangs" }),
        json!({ "id": "c2", "action": "click", "selector": "#slow" }),
    ] {
        assert_success(&exec(&cmd, &mut state).await);
    }
    fx.wait_until(|| fx.hits("GET /api/hangs/") > 0 && fx.hits("GET /api/slow") > 0)
        .await;
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "secret", "timeout": 1500 }),
        &mut state,
    )
    .await;
    assert_success(&saved);
    assert!(!saved.to_string().contains("91ab"), "{saved}");
    let text = crate::output::format_flow_text(Some("flow_save"), &saved["data"]).unwrap();
    assert!(!text.contains("91ab"), "{text}");
    assert!(text.contains("still loading at save"), "{text}");
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_requests_finalizes_a_redirect_hop() {
    let fx = flow_fixture().await;
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    assert_success(&exec(&json!({ "id": "r0", "action": "requests" }), &mut state).await);
    assert_success(
        &exec(
            &json!({ "id": "c1", "action": "click", "selector": "#moved" }),
            &mut state,
        )
        .await,
    );
    fx.wait_until(|| fx.hits("GET /api/target") > 0).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    state.drain_cdp_events_background().await.unwrap();
    let listed = exec(&json!({ "id": "r1", "action": "requests" }), &mut state).await;
    assert_success(&listed);
    let requests = listed["data"]["requests"].as_array().unwrap();
    let hop = requests
        .iter()
        .find(|r| r["url"].as_str().unwrap().ends_with("/api/moved"))
        .expect("hop listed");
    assert_eq!(hop["status"], 302, "{listed}");
    let target = requests
        .iter()
        .find(|r| r["url"].as_str().unwrap().ends_with("/api/target"))
        .expect("target listed");
    assert_eq!(target["status"], 200, "{listed}");
    assert_eq!(target["mimeType"], "application/json", "{listed}");
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_save_keeps_the_secret_gap_next_to_the_loading_gap() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    for cmd in [
        json!({ "id": "f1", "action": "fill", "selector": "#email", "value": "ana@example.com" }),
        json!({ "id": "f2", "action": "fill", "selector": "#pw", "value": "tk-91ab-77" }),
        json!({ "id": "c1", "action": "click", "selector": "#both" }),
        json!({ "id": "c2", "action": "click", "selector": "#slow" }),
    ] {
        assert_success(&exec(&cmd, &mut state).await);
    }
    fx.wait_until(|| {
        fx.hits("POST /login") > 0 && fx.hits("GET /api/hang") > 0 && fx.hits("GET /api/slow") > 0
    })
    .await;
    let saved = exec(
        &json!({ "id": "s", "action": "flow_save", "name": "both", "timeout": 1500 }),
        &mut state,
    )
    .await;
    assert_success(&saved);
    let gaps = saved["data"]["gaps"].as_array().unwrap();
    let on_step: Vec<String> = gaps
        .iter()
        .filter(|g| g["command"] == "click #both")
        .map(|g| g["reason"].as_str().unwrap().to_string())
        .collect();
    assert!(
        on_step.iter().any(|r| r.contains("secret field")),
        "{saved}"
    );
    assert!(
        on_step.iter().any(|r| r.contains("still loading at save")),
        "{saved}"
    );
    assert!(!saved.to_string().contains("91ab"), "{saved}");
    assert!(gaps.iter().all(|g| g.get("kind").is_none()), "{saved}");
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_request_detail_returns_the_final_redirect_hop() {
    let fx = flow_fixture().await;
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    flow_fixture_session(&fx, &mut state).await;
    assert_success(&exec(&json!({ "id": "r0", "action": "requests" }), &mut state).await);
    assert_success(
        &exec(
            &json!({ "id": "c1", "action": "click", "selector": "#moved" }),
            &mut state,
        )
        .await,
    );
    fx.wait_until(|| fx.hits("GET /api/target") > 0).await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    state.drain_cdp_events_background().await.unwrap();
    let id = state
        .tracked_requests
        .iter()
        .find(|r| r.url.ends_with("/api/moved"))
        .unwrap()
        .request_id
        .clone();
    let detail = exec(
        &json!({ "id": "d", "action": "request_detail", "requestId": id }),
        &mut state,
    )
    .await;
    assert_success(&detail);
    assert_eq!(detail["data"]["status"], 200, "{detail}");
    assert!(
        detail["data"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/api/target"),
        "{detail}"
    );
    assert!(
        detail["data"]["responseBody"].to_string().contains("stove"),
        "{detail}"
    );
    let _ = exec(&json!({ "id": "99", "action": "close" }), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_keeps_vault_and_key_by_key_passwords_out_of_flows() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    let mut n = 0;
    fn run(n: &mut u32, mut cmd: Value) -> Value {
        *n += 1;
        cmd["id"] = json!(format!("v{}", n));
        cmd
    }
    let profile = unique_auth_profile_name("flow-capture");
    for cmd in [
        json!({ "action": "launch", "headless": true, "capture": ["flow"] }),
        json!({ "action": "navigate", "url": format!("{}/", fx.site) }),
        json!({ "action": "auth_save", "name": profile, "url": format!("{}/", fx.site),
            "username": "vault@example.test", "password": "v4ult-pw" }),
    ] {
        assert_success(&exec(&run(&mut n, cmd), &mut state).await);
    }
    // The vault login fills the password and submits; its requests are not steps.
    let before = fx.hits("POST /login");
    let login = exec(
        &run(
            &mut n,
            json!({ "action": "auth_login", "name": profile, "noNavigate": true }),
        ),
        &mut state,
    )
    .await;
    assert_success(&login);
    fx.wait_until(|| fx.hits("POST /login") > before).await;
    settle_responses(&mut state).await;
    // The field still holds the vault password, so this recorded click sends it.
    for cmd in [
        json!({ "action": "click", "selector": "#login" }),
        json!({ "action": "click", "selector": "#items" }),
    ] {
        assert_success(&fx.run_settled(&mut state, &run(&mut n, cmd)).await);
    }
    // A password typed key by key into a password field.
    for cmd in [
        json!({ "action": "evaluate", "script": "document.getElementById('pw').value = ''" }),
        json!({ "action": "click", "selector": "#pw" }),
        json!({ "action": "press", "key": "k" }),
        json!({ "action": "press", "key": "e" }),
        json!({ "action": "press", "key": "y" }),
        json!({ "action": "press", "key": "9" }),
        json!({ "action": "click", "selector": "#login" }),
    ] {
        assert_success(&fx.run_settled(&mut state, &run(&mut n, cmd)).await);
    }
    assert!(fx.hits("key9") >= 1, "the page did send the typed password");
    let saved = exec(
        &run(&mut n, json!({ "action": "flow_save", "name": "vault" })),
        &mut state,
    )
    .await;
    assert_success(&saved);
    let text = std::fs::read_to_string(dir.path().join("127.0.0.1").join("vault.json")).unwrap();
    assert!(
        !text.contains("v4ult-pw"),
        "vault password reached the flow file"
    );
    assert!(
        !text.contains("key9"),
        "typed password reached the flow file"
    );
    assert!(text.contains("<redacted>"));
    let _ = exec(
        &run(&mut n, json!({ "action": "auth_delete", "name": profile })),
        &mut state,
    )
    .await;
    let _ = exec(&run(&mut n, json!({ "action": "close" })), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_replay_runs_once_per_run() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    let mut n = 0;
    fn run(n: &mut u32, mut cmd: Value) -> Value {
        *n += 1;
        cmd["id"] = json!(format!("c{}", n));
        cmd
    }
    for cmd in [
        json!({ "action": "launch", "headless": true, "capture": ["flow"] }),
        json!({ "action": "navigate", "url": format!("{}/", fx.site) }),
        json!({ "action": "click", "selector": "#login" }),
        json!({ "action": "click", "selector": "#session" }),
        json!({ "action": "click", "selector": "#items" }),
        json!({ "action": "click", "selector": "#add" }),
    ] {
        assert_success(&fx.run_settled(&mut state, &run(&mut n, cmd)).await);
    }
    let saved = exec(
        &run(&mut n, json!({ "action": "flow_save", "name": "ci" })),
        &mut state,
    )
    .await;
    assert_success(&saved);
    let requests = saved["data"]["requests"].as_array().unwrap();
    let id_of = |suffix: &str| {
        requests
            .iter()
            .find(|r| r["url"].as_str().unwrap().contains(suffix))
            .and_then(|r| r["id"].as_str())
            .unwrap()
            .to_string()
    };
    let (session_id, items_id) = (id_of("/api/session"), id_of("/api/items"));
    let resaved = exec(
        &run(
            &mut n,
            json!({ "action": "flow_save", "name": "ci", "from": 1, "output": items_id }),
        ),
        &mut state,
    )
    .await;
    assert_success(&resaved);

    // The output reads the session response, and both fall back to the replay.
    let path = dir.path().join("127.0.0.1").join("ci.json");
    let mut flow: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for step in flow["steps"].as_array_mut().unwrap() {
        for req in step["requests"].as_array_mut().unwrap() {
            if req["id"] == items_id.as_str() {
                req["url"] = json!(format!(
                    "{}/api/items?u={{{{@{}.user}}}}",
                    fx.site, session_id
                ));
            }
            if req["id"] == items_id.as_str() || req["id"] == session_id.as_str() {
                req["tier"] = json!("replay");
            }
        }
    }
    std::fs::write(&path, serde_json::to_string(&flow).unwrap()).unwrap();
    assert!(
        dir.path().join("127.0.0.1").join("ci.json").exists()
            && flow["writes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["url"].as_str().unwrap().ends_with("/api/cart"))
    );

    let before = fx.hits("POST /api/cart");
    let sent = exec(
        &run(
            &mut n,
            json!({ "action": "flow_run", "name": "ci", "params": {}, "allowWrite": "all" }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(sent["data"]["ok"], true, "{sent}");
    assert_eq!(
        fx.hits("POST /api/cart"),
        before + 1,
        "an approved write is sent once per run: {sent}"
    );
    // The items click follows the session click at once; it must send the
    // user the session response set, never an empty one.
    let log_start = fx.log.lock().unwrap().len();
    for _ in 0..4 {
        let again = exec(
            &run(
                &mut n,
                json!({ "action": "flow_run", "name": "ci", "params": {}, "allowWrite": "all" }),
            ),
            &mut state,
        )
        .await;
        assert_eq!(again["data"]["ok"], true, "{again}");
    }
    let log = fx.log.lock().unwrap().clone();
    let items: Vec<&String> = log[log_start..]
        .iter()
        .filter(|l| l.contains("GET /api/items"))
        .collect();
    assert!(!items.is_empty(), "{log:#?}");
    assert!(
        items.iter().all(|l| l.contains("GET /api/items?u=ana ")),
        "an items request went out without the session value: {items:#?}"
    );
    let _ = exec(&run(&mut n, json!({ "action": "close" })), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_replay_leaves_the_session_capture_untouched() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    let mut n = 0;
    fn run(n: &mut u32, mut cmd: Value) -> Value {
        *n += 1;
        cmd["id"] = json!(format!("c{}", n));
        cmd
    }
    for cmd in [
        json!({ "action": "launch", "headless": true, "capture": ["flow"] }),
        json!({ "action": "navigate", "url": format!("{}/", fx.site) }),
        json!({ "action": "click", "selector": "#login" }),
        json!({ "action": "click", "selector": "#session" }),
        json!({ "action": "click", "selector": "#items" }),
    ] {
        assert_success(&fx.run_settled(&mut state, &run(&mut n, cmd)).await);
    }
    let saved = exec(
        &run(&mut n, json!({ "action": "flow_save", "name": "ci" })),
        &mut state,
    )
    .await;
    assert_success(&saved);
    let requests = saved["data"]["requests"].as_array().unwrap();
    let id_of = |suffix: &str| {
        requests
            .iter()
            .find(|r| r["url"].as_str().unwrap().contains(suffix))
            .and_then(|r| r["id"].as_str())
            .unwrap()
            .to_string()
    };
    let (session_id, items_id) = (id_of("/api/session"), id_of("/api/items"));
    let path = dir.path().join("127.0.0.1").join("ci.json");
    let mut flow: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for step in flow["steps"].as_array_mut().unwrap() {
        for req in step["requests"].as_array_mut().unwrap() {
            if req["id"] == items_id.as_str() || req["id"] == session_id.as_str() {
                req["tier"] = json!("replay");
            }
        }
    }
    std::fs::write(&path, serde_json::to_string(&flow).unwrap()).unwrap();

    let steps_before = exec(&run(&mut n, json!({ "action": "flow_steps" })), &mut state).await;
    assert_success(&steps_before);
    let har_before = state.har_entries.len();
    let ids_before: Vec<String> = state
        .har_entries
        .iter()
        .map(|e| e.request_id.clone())
        .collect();

    let hits_before = fx.hits("GET /api/items");
    let sent = exec(
        &run(
            &mut n,
            json!({ "action": "flow_run", "name": "ci", "params": {}, "allowWrite": "all" }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(sent["data"]["ok"], true, "{sent}");
    assert!(
        fx.hits("GET /api/items") > hits_before,
        "the run did not reach the replay tier"
    );
    settle_responses(&mut state).await;

    let steps_after = exec(&run(&mut n, json!({ "action": "flow_steps" })), &mut state).await;
    assert_success(&steps_after);
    assert_eq!(
        steps_after["data"]["steps"], steps_before["data"]["steps"],
        "a run added steps to the user's recording"
    );
    let leaked: Vec<String> = state
        .har_entries
        .iter()
        .filter(|e| !ids_before.contains(&e.request_id))
        .map(|e| e.url.clone())
        .collect();
    assert_eq!(
        state.har_entries.len(),
        har_before,
        "a run left entries in the session capture: {leaked:#?}"
    );

    // The next save holds only what the user did.
    assert_success(
        &fx.run_settled(
            &mut state,
            &run(&mut n, json!({ "action": "click", "selector": "#add" })),
        )
        .await,
    );
    let next = exec(
        &run(&mut n, json!({ "action": "flow_save", "name": "next" })),
        &mut state,
    )
    .await;
    assert_success(&next);
    let urls: Vec<String> = next["data"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["url"].as_str().unwrap().to_string())
        .collect();
    assert!(
        urls.iter().filter(|u| u.contains("/api/items")).count() <= 1,
        "the next save carries a run's requests: {urls:#?}"
    );
    let _ = exec(&run(&mut n, json!({ "action": "close" })), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_a_write_the_replay_sent_is_not_sent_again_by_a_later_request() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    let mut n = 0;
    fn run(n: &mut u32, mut cmd: Value) -> Value {
        *n += 1;
        cmd["id"] = json!(format!("w{}", n));
        cmd
    }
    for cmd in [
        json!({ "action": "launch", "headless": true, "capture": ["flow"] }),
        json!({ "action": "navigate", "url": format!("{}/", fx.site) }),
        json!({ "action": "click", "selector": "#login" }),
        json!({ "action": "click", "selector": "#session" }),
        json!({ "action": "click", "selector": "#add" }),
    ] {
        assert_success(&fx.run_settled(&mut state, &run(&mut n, cmd)).await);
    }
    let saved = exec(
        &run(&mut n, json!({ "action": "flow_save", "name": "cs" })),
        &mut state,
    )
    .await;
    assert_success(&saved);
    let requests = saved["data"]["requests"].as_array().unwrap();
    let id_of = |suffix: &str| {
        requests
            .iter()
            .find(|r| r["url"].as_str().unwrap().contains(suffix))
            .and_then(|r| r["id"].as_str())
            .unwrap()
            .to_string()
    };
    let (session_id, add_id) = (id_of("/api/session"), id_of("/api/cart"));
    assert_success(
        &exec(
            &run(
                &mut n,
                json!({ "action": "flow_save", "name": "cs", "from": 1, "output": add_id }),
            ),
            &mut state,
        )
        .await,
    );
    // The read falls back to the replay, which clicks Add; the write then
    // comes up on the HTTP tier and must not be sent a second time.
    let path = dir.path().join("127.0.0.1").join("cs.json");
    let mut flow: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for step in flow["steps"].as_array_mut().unwrap() {
        for req in step["requests"].as_array_mut().unwrap() {
            if req["id"] == session_id.as_str() {
                req["tier"] = json!("replay");
            }
        }
    }
    assert!(flow["steps"]
        .to_string()
        .contains(&format!("{{{{@{}.", session_id)));
    std::fs::write(&path, serde_json::to_string(&flow).unwrap()).unwrap();
    let before = fx.hits("POST /api/cart");
    let sent = exec(
        &run(
            &mut n,
            json!({ "action": "flow_run", "name": "cs", "params": {}, "allowWrite": "all" }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(sent["data"]["ok"], true, "{sent}");
    assert_eq!(
        fx.hits("POST /api/cart"),
        before + 1,
        "one POST per run: {sent}"
    );
    let _ = exec(&run(&mut n, json!({ "action": "close" })), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_the_page_tier_fetch_does_not_answer_for_the_replay() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    let mut n = 0;
    fn run(n: &mut u32, mut cmd: Value) -> Value {
        *n += 1;
        cmd["id"] = json!(format!("p{}", n));
        cmd
    }
    for cmd in [
        json!({ "action": "launch", "headless": true, "capture": ["flow"] }),
        json!({ "action": "navigate", "url": format!("{}/", fx.site) }),
        json!({ "action": "click", "selector": "#flaky" }),
    ] {
        assert_success(&fx.run_settled(&mut state, &run(&mut n, cmd)).await);
    }
    assert_success(
        &exec(
            &run(&mut n, json!({ "action": "flow_save", "name": "pf" })),
            &mut state,
        )
        .await,
    );
    // The page tier asks the exact URL, which the site rejects; the replay's
    // own request carries a cache-buster and is answered.
    let path = dir.path().join("127.0.0.1").join("pf.json");
    let mut flow: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for step in flow["steps"].as_array_mut().unwrap() {
        for req in step["requests"].as_array_mut().unwrap() {
            if req["url"].as_str().unwrap().contains("/api/flaky") {
                req["url"] = json!(format!("{}/api/flaky", fx.site));
                req["tier"] = json!("page");
                req["expect"]["strippedAuth"] = json!(true);
            }
        }
    }
    std::fs::write(&path, serde_json::to_string(&flow).unwrap()).unwrap();
    let out = exec(
        &run(
            &mut n,
            json!({ "action": "flow_run", "name": "pf", "params": {} }),
        ),
        &mut state,
    )
    .await;
    assert_eq!(out["data"]["ok"], true, "{out}");
    let _ = exec(&run(&mut n, json!({ "action": "close" })), &mut state).await;
}

#[tokio::test]
#[ignore]
async fn e2e_flow_a_route_limited_by_resource_type_applies_the_same_on_both_tiers() {
    let fx = flow_fixture().await;
    let dir = tempfile::tempdir().unwrap();
    let guard = EnvGuard::new(&["AGENT_BROWSER_FLOWS_DIR"]);
    guard.set("AGENT_BROWSER_FLOWS_DIR", dir.path().to_str().unwrap());
    let mut state = DaemonState::new();
    state.policy = None;
    state.confirm_actions = None;
    let mut n = 0;
    fn run(n: &mut u32, mut cmd: Value) -> Value {
        *n += 1;
        cmd["id"] = json!(format!("rt{}", n));
        cmd
    }
    for cmd in [
        json!({ "action": "launch", "headless": true, "capture": ["flow"] }),
        json!({ "action": "navigate", "url": format!("{}/", fx.site) }),
        json!({ "action": "click", "selector": "#login" }),
        json!({ "action": "click", "selector": "#session" }),
        json!({ "action": "click", "selector": "#add" }),
    ] {
        assert_success(&fx.run_settled(&mut state, &run(&mut n, cmd)).await);
    }
    let saved = exec(
        &run(&mut n, json!({ "action": "flow_save", "name": "rt" })),
        &mut state,
    )
    .await;
    assert_success(&saved);
    let add_id = saved["data"]["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["url"].as_str().unwrap().contains("/api/cart"))
        .and_then(|r| r["id"].as_str())
        .unwrap()
        .to_string();
    assert_success(
        &exec(
            &run(
                &mut n,
                json!({ "action": "flow_save", "name": "rt", "from": 1, "output": add_id }),
            ),
            &mut state,
        )
        .await,
    );
    let path = dir.path().join("127.0.0.1").join("rt.json");
    let original = std::fs::read_to_string(&path).unwrap();
    for tier in ["http", "page"] {
        let mut flow: Value = serde_json::from_str(&original).unwrap();
        for step in flow["steps"].as_array_mut().unwrap() {
            for req in step["requests"].as_array_mut().unwrap() {
                if req["id"] == add_id.as_str() {
                    req["tier"] = json!(tier);
                }
            }
        }
        std::fs::write(&path, serde_json::to_string(&flow).unwrap()).unwrap();
        // (route limited to, whether the mock answers the write); Chrome
        // reports the in-page fetch() as an XHR to request interception
        for (limit, mocked) in [("fetch", false), ("xhr", true)] {
            assert_success(
                &exec(
                    &run(
                        &mut n,
                        json!({
                            "action": "route",
                            "url": "*/api/cart",
                            "resourceType": limit,
                            "response": { "status": 200, "body": "{\"cart\":[7]}" },
                        }),
                    ),
                    &mut state,
                )
                .await,
            );
            let before = fx.hits("POST /api/cart");
            let out = exec(
                &run(
                    &mut n,
                    json!({ "action": "flow_run", "name": "rt", "params": {}, "allowWrite": "all" }),
                ),
                &mut state,
            )
            .await;
            assert_eq!(out["data"]["ok"], true, "{tier} {limit}: {out}");
            assert_eq!(
                fx.hits("POST /api/cart"),
                before + usize::from(!mocked),
                "{tier} tier, route limited to {limit}: {out}"
            );
            assert_success(
                &exec(
                    &run(&mut n, json!({ "action": "unroute", "url": "*/api/cart" })),
                    &mut state,
                )
                .await,
            );
        }
    }
    let _ = exec(&run(&mut n, json!({ "action": "close" })), &mut state).await;
}
