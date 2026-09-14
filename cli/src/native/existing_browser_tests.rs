//! Regression tests for borrowed provider sessions. The WebSocket fixtures
//! exercise core CDP dispatch; they do not claim real extension compatibility.
use super::*;
use crate::test_utils::EnvGuard;
use futures_util::{SinkExt, StreamExt};
use std::sync::Mutex;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const ENV: &[&str] = &[
    "AGENT_BROWSER_SOCKET_DIR",
    "XDG_RUNTIME_DIR",
    "AGENT_BROWSER_SESSION",
    "AGENT_BROWSER_SESSION_NAME",
    "AGENT_BROWSER_RESTORE_SAVE",
    "AGENT_BROWSER_RESTORE_CHECK_URL",
    "AGENT_BROWSER_RESTORE_CHECK_TEXT",
    "AGENT_BROWSER_RESTORE_CHECK_FN",
    "AGENT_BROWSER_PIN_TAB",
    "AGENT_BROWSER_HEADED",
    "AGENT_BROWSER_PROFILE",
    "AGENT_BROWSER_STATE",
    "AGENT_BROWSER_PROXY",
    "AGENT_BROWSER_PROXY_BYPASS",
    "AGENT_BROWSER_PROXY_USERNAME",
    "AGENT_BROWSER_PROXY_PASSWORD",
    "AGENT_BROWSER_EXECUTABLE_PATH",
    "AGENT_BROWSER_ARGS",
    "AGENT_BROWSER_EXTENSIONS",
    "AGENT_BROWSER_ENGINE",
    "AGENT_BROWSER_ALLOW_FILE_ACCESS",
    "AGENT_BROWSER_ALLOWED_DOMAINS",
    "AGENT_BROWSER_IGNORE_HTTPS_ERRORS",
    "AGENT_BROWSER_DOWNLOAD_PATH",
    "AGENT_BROWSER_HIDE_SCROLLBARS",
    "AGENT_BROWSER_WEBGPU",
    "AGENT_BROWSER_NO_XVFB",
    "AGENT_BROWSER_USER_AGENT",
    "AGENT_BROWSER_COLOR_SCHEME",
    "AGENT_BROWSER_CDP",
    "AGENT_BROWSER_AUTO_CONNECT",
    "AGENT_BROWSER_PROVIDER",
    "AGENT_BROWSER_INIT_SCRIPTS",
    "AGENT_BROWSER_ENABLE",
];

fn clean_env(dir: &tempfile::TempDir) -> EnvGuard<'static> {
    let guard = EnvGuard::new(ENV);
    for name in ENV {
        guard.remove(name);
    }
    guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
    guard.set("AGENT_BROWSER_SESSION", "borrowed-test");
    guard
}

#[cfg(unix)]
fn provider_fixture(dir: &tempfile::TempDir, ws_url: &str) -> crate::plugins::PluginConfig {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("provider");
    fs::write(
        &path,
        r#"#!/bin/sh
request=$(cat)
printf '%s\n' "$request" >> "$1"
case "$request" in
  *'"type":"browser.close"'*) printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true}' ;;
  *) printf '%s' "$2" ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    crate::plugins::PluginConfig {
        name: "borrowed".to_string(),
        command: path.to_string_lossy().to_string(),
        args: vec![dir.path().join("requests.jsonl").to_string_lossy().to_string(),
            json!({ "protocol": crate::plugins::PROTOCOL_VERSION, "success": true,
                "browser": { "cdpUrl": ws_url, "existingBrowser": true, "cleanup": { "leaseId": "lease-test" } }
            }).to_string()],
        capabilities: vec![crate::plugins::CAPABILITY_BROWSER_PROVIDER.to_string()],
        ..Default::default()
    }
}

fn requests(dir: &tempfile::TempDir) -> Vec<Value> {
    fs::read_to_string(dir.path().join("requests.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn cdp_fixture(
    fail_method: Option<&'static str>,
) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    cdp_fixture_with_frame_node(fail_method, json!({ "nodeName": "IFRAME", "contentDocument": { "frameId": "selected-child" }, "attributes": [] }), None).await
}

async fn cdp_fixture_with_frame_node(
    fail_method: Option<&'static str>,
    frame_node: Value,
    early_event_method: Option<&'static str>,
) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let captured = calls.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut early_event_sent = false;
        while let Some(Ok(Message::Text(text))) = socket.next().await {
            let command: Value = serde_json::from_str(&text).unwrap();
            captured.lock().unwrap().push(command.clone());
            let method = command["method"].as_str().unwrap();
            if method == "Runtime.enable" && command["sessionId"] == "authorized-session" {
                for (context_id, is_default) in [(777, true), (888, false)] {
                    socket.send(Message::Text(json!({ "method": "Runtime.executionContextCreated", "sessionId": "authorized-session", "params": {
                        "context": { "id": context_id, "auxData": { "frameId": "selected-child", "isDefault": is_default } }
                    } }).to_string())).await.unwrap();
                }
            }
            if !early_event_sent && Some(method) == early_event_method {
                early_event_sent = true;
                socket.send(Message::Text(json!({ "method": "Target.attachedToTarget", "sessionId": "authorized-session", "params": {
                    "sessionId": "child-session", "targetInfo": { "targetId": "selected-child", "type": "iframe", "title": "Child", "url": "https://other.example" }, "waitingForDebugger": true
                } }).to_string())).await.unwrap();
            }
            let mut response = json!({ "id": command["id"] });
            if Some(method) == fail_method {
                response["error"] = json!({ "code": -32000, "message": "page option unsupported" });
            } else {
                response["result"] = match method {
                    "Target.getTargets" => {
                        json!({ "targetInfos": [{ "targetId": "authorized-tab", "type": "page", "title": "Fixture", "url": "https://example.com" }] })
                    }
                    "Target.attachToTarget" => json!({ "sessionId": "authorized-session" }),
                    "Page.getFrameTree" => {
                        json!({ "frameTree": { "frame": { "id": "main", "url": "https://example.com" }, "childFrames": [{ "frame": { "id": "different-child", "name": "", "url": "https://example.com/frame" } }, { "frame": { "id": "selected-child", "name": "", "url": "https://example.com/frame" } }] } })
                    }
                    "Runtime.evaluate" if command["params"]["returnByValue"] == false => {
                        json!({ "result": { "type": "object", "objectId": "frame-owner" } })
                    }
                    "DOM.describeNode" => json!({ "node": frame_node }),
                    "Runtime.evaluate" if command["params"]["expression"] == "location.href" => {
                        let url = if command["params"]["contextId"] == 777
                            || command["sessionId"] == "child-session"
                        {
                            "https://example.com/frame"
                        } else {
                            "https://example.com"
                        };
                        json!({ "result": { "type": "string", "value": url } })
                    }
                    "Runtime.evaluate"
                        if command["params"]["expression"]
                            == "throw new Error('frame failure')" =>
                    {
                        json!({ "result": { "type": "undefined" }, "exceptionDetails": { "text": "Uncaught", "exception": { "type": "object", "description": "Error: frame failure" } } })
                    }
                    "Runtime.evaluate" => json!({ "result": { "type": "number", "value": 1 } }),
                    _ => json!({}),
                };
            }
            if socket
                .send(Message::Text(response.to_string()))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    (url, calls, task)
}

#[test]
fn existing_browser_preflight_distinguishes_defaults_and_explicit_choices() {
    let dir = tempfile::tempdir().unwrap();
    let guard = clean_env(&dir);
    let state = DaemonState::new();
    let defaults = effective_launch_options(&Value::Null);
    assert!(
        defaults.headless,
        "ordinary local launch remains headless by default"
    );
    assert!(validate_existing_browser_options(&json!({}), &state, &defaults, None, &[]).is_ok());
    assert!(validate_existing_browser_options(
        &json!({ "headless": false }),
        &state,
        &effective_launch_options(&json!({ "headless": false })),
        Some("chrome"),
        &[]
    )
    .is_ok());
    assert!(validate_existing_browser_options(
        &json!({ "headless": true }),
        &state,
        &defaults,
        None,
        &[]
    )
    .unwrap_err()
    .contains("--headed false"));
    guard.set("AGENT_BROWSER_HEADED", "false");
    assert!(validate_existing_browser_options(&json!({}), &state, &defaults, None, &[]).is_err());
    assert!(validate_existing_browser_options(
        &json!({ "headless": false }),
        &state,
        &effective_launch_options(&json!({ "headless": false })),
        None,
        &[]
    )
    .is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn existing_browser_preflight_releases_lease_before_any_cdp_attach() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    // A listening socket proves preflight never starts a CDP connection.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let plugin = provider_fixture(&dir, &format!("ws://{}", listener.local_addr().unwrap()));
    for (field, value, expected) in [
        ("profile", json!("private-profile"), "--profile"),
        ("storageState", json!("private-state.json"), "--state"),
        (
            "proxy",
            json!({ "server": "http://proxy", "username": "user", "password": "secret" }),
            "--proxy",
        ),
        (
            "executablePath",
            json!("/private/chrome"),
            "--executable-path",
        ),
        ("args", json!(["--some-process-flag"]), "--args"),
        ("extensions", json!(["/private/extension"]), "--extension"),
        ("headless", json!(true), "--headed false"),
        ("engine", json!("lightpanda"), "--engine"),
        (
            "allowedDomains",
            json!(["example.com"]),
            "--allowed-domains",
        ),
        ("pinTab", json!(true), "--pin-tab"),
        ("restoreKey", json!("private-restore"), "--restore"),
        ("ignoreHTTPSErrors", json!(true), "--ignore-https-errors"),
        (
            "downloadPath",
            json!("/private/downloads"),
            "--download-path",
        ),
        ("hideScrollbars", json!(true), "--hide-scrollbars"),
    ] {
        let mut state = DaemonState::new();
        let mut cmd = json!({ "action": "launch", "provider": "borrowed", "plugins": [plugin] });
        cmd[field] = value;
        let error = handle_launch(&cmd, &mut state).await.unwrap_err();
        assert!(error.contains(expected), "{field}: {error}");
        assert!(!error.contains("secret"));
        assert!(state.browser.is_none());
        assert!(state.proxy_credentials.read().await.is_none());
        assert!(state.domain_filter.read().await.is_none());
        let requests = requests(&dir);
        assert_eq!(requests[requests.len() - 2]["type"], "browser.launch");
        assert_eq!(requests.last().unwrap()["type"], "browser.close");
        assert_eq!(requests.last().unwrap()["request"]["leaseId"], "lease-test");
        let forwarded = &requests[requests.len() - 2]["request"]["launchOptions"];
        assert!(forwarded.get("proxy").is_none());
        assert!(forwarded.get("storageState").is_none());
        assert!(forwarded.get("args").is_none());
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn existing_browser_auto_and_explicit_launch_share_effective_env_preflight() {
    let dir = tempfile::tempdir().unwrap();
    let guard = clean_env(&dir);
    let plugin = provider_fixture(&dir, "ws://127.0.0.1:1");
    guard.set("AGENT_BROWSER_PROVIDER", "borrowed");
    for (name, value, expected) in [
        ("AGENT_BROWSER_PROFILE", "private-profile", "--profile"),
        ("AGENT_BROWSER_HIDE_SCROLLBARS", "true", "--hide-scrollbars"),
        ("AGENT_BROWSER_STATE", "private-state.json", "--state"),
        ("AGENT_BROWSER_PROXY", "http://proxy", "--proxy"),
        ("AGENT_BROWSER_ARGS", "--flag", "--args"),
        ("AGENT_BROWSER_ENGINE", "lightpanda", "--engine"),
        ("AGENT_BROWSER_SESSION_NAME", "saved", "--restore"),
        ("AGENT_BROWSER_PIN_TAB", "true", "--pin-tab"),
        (
            "AGENT_BROWSER_ALLOWED_DOMAINS",
            "example.com",
            "--allowed-domains",
        ),
    ] {
        guard.set(name, value);
        let auto = auto_launch(
            &mut DaemonState::new(),
            vec![plugin.clone()],
            &json!({ "action": "snapshot" }),
        )
        .await
        .unwrap_err();
        let explicit = handle_launch(
            &json!({ "action": "launch", "provider": "borrowed", "plugins": [plugin] }),
            &mut DaemonState::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(auto, explicit);
        assert!(auto.contains(expected), "{name}: {auto}");
        guard.remove(name);
    }
    assert_eq!(
        requests(&dir)
            .iter()
            .filter(|request| request["type"] == "browser.close")
            .count(),
        18
    );
}

#[tokio::test]
async fn existing_browser_active_preflight_precedes_configuration_and_state_writes() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    for cmd in [
        json!({ "action": "snapshot", "pinTab": true }),
        json!({ "action": "snapshot", "restoreKey": "new-restore" }),
        json!({ "action": "snapshot", "restoreSave": "always" }),
        json!({ "action": "state_save", "path": "private-state.json" }),
        json!({ "action": "state_load", "path": "private-state.json" }),
        json!({ "action": "launch", "storageState": "private-state.json" }),
        json!({ "action": "launch", "allowedDomains": ["example.com"] }),
    ] {
        let mut state = DaemonState::new();
        remember_active_provider_session(&mut state, None, &[], true);
        state.launch_hash = Some(123);
        let response = execute_command(&cmd, &mut state).await;
        assert_eq!(response["success"], false, "{cmd}: {response}");
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("existing browser"));
        assert!(state.active_existing_browser);
        assert!(!state.pin_tab);
        assert!(state.session_name.is_none());
        assert!(state.domain_filter.read().await.is_none());
        assert_eq!(state.launch_hash, Some(123));
        assert!(!tab_binding::binding_path(&state.session_id).exists());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn existing_browser_page_options_idle_and_close_use_borrowed_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    let (url, calls, task) = cdp_fixture(None).await;
    let plugin = provider_fixture(&dir, &url);
    let mut state = DaemonState::new();
    let cmd = json!({ "action": "launch", "provider": "borrowed", "plugins": [plugin], "userAgent": "fixture-agent", "colorScheme": "dark" });
    handle_launch(&cmd, &mut state).await.unwrap();
    assert!(state.active_existing_browser);
    assert!(state.blocks_default_idle_shutdown());
    assert!(maybe_persist_tab_binding(&mut state).is_none());
    assert!(!tab_binding::binding_path(&state.session_id).exists());
    let initial_calls = calls.lock().unwrap().clone();
    for method in [
        "Emulation.setUserAgentOverride",
        "Emulation.setEmulatedMedia",
    ] {
        let call = initial_calls
            .iter()
            .find(|call| call["method"] == method)
            .unwrap();
        assert_eq!(call["sessionId"], "authorized-session");
    }
    assert_eq!(
        initial_calls
            .iter()
            .find(|call| call["method"] == "Emulation.setUserAgentOverride")
            .unwrap()["params"]["userAgent"],
        "fixture-agent"
    );
    assert_eq!(
        handle_launch(&cmd, &mut state).await.unwrap()["reused"],
        true
    );
    handle_user_agent(&json!({ "userAgent": "runtime-agent" }), &mut state)
        .await
        .unwrap();
    state.session_setup.timezone = Some("UTC".to_string());
    assert_eq!(
        handle_launch(&cmd, &mut state).await.unwrap()["reused"],
        true
    );
    assert_eq!(
        state.session_setup.user_agent.as_deref(),
        Some("runtime-agent")
    );
    assert_eq!(state.session_setup.timezone.as_deref(), Some("UTC"));
    let mut changed = cmd.clone();
    changed["userAgent"] = json!("changed-agent");
    changed["colorScheme"] = json!("light");
    assert_eq!(
        handle_launch(&changed, &mut state).await.unwrap()["reused"],
        true
    );
    assert_eq!(
        state.session_setup.user_agent.as_deref(),
        Some("changed-agent")
    );
    assert_eq!(
        requests(&dir)
            .iter()
            .filter(|request| request["type"] == "browser.launch")
            .count(),
        1
    );
    // Internal save paths remain inert even if an older caller left a key.
    state.session_name = Some("legacy-key".to_string());
    assert!(auto_save_restore_state(&mut state).await.unwrap().is_none());
    close_current_browser(&mut state).await.unwrap();
    close_current_browser(&mut state).await.unwrap();
    assert!(!state.active_existing_browser);
    assert_eq!(
        requests(&dir)
            .iter()
            .filter(|request| request["type"] == "browser.close")
            .count(),
        1
    );
    assert!(!calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call["method"] == "Browser.close"));
    task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn existing_browser_page_option_failure_releases_lease() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    let (url, calls, task) = cdp_fixture(Some("Emulation.setEmulatedMedia")).await;
    let plugin = provider_fixture(&dir, &url);
    let mut state = DaemonState::new();
    let error = handle_launch(&json!({ "action": "launch", "provider": "borrowed", "plugins": [plugin], "colorScheme": "dark" }), &mut state).await.unwrap_err();
    assert!(error.contains("page option unsupported"));
    assert!(state.browser.is_none());
    assert!(!state.active_existing_browser);
    assert_eq!(requests(&dir).last().unwrap()["type"], "browser.close");
    assert!(!calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call["method"] == "Browser.close"));
    task.abort();
}

#[tokio::test]
async fn existing_browser_css_frame_selects_unnamed_dom_owner_and_releases_object() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    for node in [
        json!({ "nodeName": "IFRAME", "contentDocument": { "frameId": "selected-child" }, "attributes": [] }),
        json!({ "nodeName": "IFRAME", "frameId": "selected-child", "attributes": ["src", "https://example.com/frame"] }),
    ] {
        let (url, calls, task) = cdp_fixture_with_frame_node(None, node, None).await;
        let mut state = DaemonState::new();
        state.browser = Some(BrowserManager::connect_cdp(&url).await.unwrap());
        handle_frame(&json!({ "selector": "iframe:last-of-type" }), &mut state)
            .await
            .unwrap();
        assert_eq!(state.active_frame_id.as_deref(), Some("selected-child"));
        let calls = calls.lock().unwrap().clone();
        assert!(calls.iter().any(|call| call["method"] == "DOM.describeNode"
            && call["params"]["objectId"] == "frame-owner"));
        assert!(calls
            .iter()
            .any(|call| call["method"] == "Runtime.releaseObject"
                && call["params"]["objectId"] == "frame-owner"));
        close_current_browser(&mut state).await.unwrap();
        task.abort();
    }
}

#[tokio::test]
async fn existing_browser_css_frame_releases_object_when_describe_fails() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    let (url, calls, task) = cdp_fixture(Some("DOM.describeNode")).await;
    let mut state = DaemonState::new();
    state.browser = Some(BrowserManager::connect_cdp(&url).await.unwrap());
    assert!(handle_frame(&json!({ "selector": "iframe" }), &mut state)
        .await
        .is_err());
    assert!(state.active_frame_id.is_none());
    assert!(calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call["method"] == "Runtime.releaseObject"));
    close_current_browser(&mut state).await.unwrap();
    task.abort();
}

#[tokio::test]
async fn existing_browser_retains_auto_attach_event_emitted_before_attach_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    for direct in [false, true] {
        let (url, calls, task) = cdp_fixture_with_frame_node(
            None,
            json!({}),
            Some(if direct {
                "Page.enable"
            } else {
                "Target.setAutoAttach"
            }),
        )
        .await;
        let mgr = if direct {
            BrowserManager::connect_cdp_direct(&url).await.unwrap()
        } else {
            BrowserManager::connect_cdp_with_headers(
                &url,
                Some(vec![("X-Fixture".to_string(), "true".to_string())]),
            )
            .await
            .unwrap()
        };
        let mut state = DaemonState::new();
        state.browser = Some(mgr);
        state.subscribe_to_browser_events();
        state.drain_cdp_events_background().await.unwrap();
        assert_eq!(
            state
                .iframe_sessions
                .get("selected-child")
                .map(String::as_str),
            Some("child-session")
        );
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "Runtime.runIfWaitingForDebugger"
                && call["sessionId"] == "child-session"));
        close_current_browser(&mut state).await.unwrap();
        task.abort();
    }
}

#[tokio::test]
async fn existing_browser_eval_uses_selected_frame_default_world() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    let (url, calls, task) = cdp_fixture(None).await;
    let mut state = DaemonState::new();
    state.browser = Some(BrowserManager::connect_cdp(&url).await.unwrap());
    state.subscribe_to_browser_events();
    state.drain_cdp_events_background().await.unwrap();
    handle_frame(&json!({ "selector": "iframe" }), &mut state)
        .await
        .unwrap();
    let cmd = json!({ "script": "location.href" });
    let result = handle_evaluate(&cmd, &state).await.unwrap();
    assert_eq!(result["result"], "https://example.com/frame");
    assert_eq!(result["origin"], "https://example.com/frame");
    let eval_call = calls
        .lock()
        .unwrap()
        .iter()
        .find(|call| call["method"] == "Runtime.evaluate" && call["params"]["contextId"] == 777)
        .unwrap()
        .clone();
    assert_eq!(eval_call["sessionId"], "authorized-session");
    assert_eq!(eval_call["params"]["awaitPromise"], true);
    assert_eq!(eval_call["params"]["returnByValue"], true);
    assert!(handle_evaluate(
        &json!({ "script": "throw new Error('frame failure')" }),
        &state
    )
    .await
    .unwrap_err()
    .contains("Evaluation error: Error: frame failure"));

    // An OOPIF can use its dedicated session even before its default-world event
    // is drained. An unavailable same-process context must never fall back to top.
    state.frame_execution_contexts.clear();
    assert!(handle_evaluate(&cmd, &state)
        .await
        .unwrap_err()
        .contains("context is unavailable"));
    state
        .iframe_sessions
        .insert("selected-child".to_string(), "child-session".to_string());
    assert_eq!(
        handle_evaluate(&cmd, &state).await.unwrap()["result"],
        "https://example.com/frame"
    );
    state.active_frame_id = None;
    assert_eq!(
        handle_evaluate(&cmd, &state).await.unwrap()["result"],
        "https://example.com"
    );
    state.frame_execution_contexts.insert(
        "selected-child".to_string(),
        FrameExecutionContext {
            session_id: "child-session".to_string(),
            context_id: 9,
        },
    );
    close_current_browser(&mut state).await.unwrap();
    assert!(state.frame_execution_contexts.is_empty());
    task.abort();
}

#[test]
fn frame_eval_contexts_invalidate_on_navigation_and_detachment() {
    let mut contexts = HashMap::new();
    let event = |method: &str, session: &str, params: Value| CdpEvent {
        method: method.to_string(),
        session_id: Some(session.to_string()),
        params,
    };
    let created = |frame: &str, id: i64| json!({ "context": { "id": id, "auxData": { "frameId": frame, "isDefault": true } } });
    track_frame_execution_context(
        &mut contexts,
        &event(
            "Runtime.executionContextCreated",
            "page",
            created("child", 7),
        ),
    );
    track_frame_execution_context(
        &mut contexts,
        &event(
            "Runtime.executionContextCreated",
            "other-page",
            created("other-child", 7),
        ),
    );
    track_frame_execution_context(
        &mut contexts,
        &event(
            "Runtime.executionContextDestroyed",
            "page",
            json!({ "executionContextId": 7 }),
        ),
    );
    assert!(!contexts.contains_key("child"));
    assert!(contexts.contains_key("other-child"));
    track_frame_execution_context(
        &mut contexts,
        &event(
            "Runtime.executionContextCreated",
            "page",
            created("child", 8),
        ),
    );
    assert_eq!(contexts["child"].context_id, 8);
    track_frame_execution_context(
        &mut contexts,
        &event("Runtime.executionContextsCleared", "page", json!({})),
    );
    assert!(!contexts.contains_key("child"));
    assert!(contexts.contains_key("other-child"));
    track_frame_execution_context(
        &mut contexts,
        &event(
            "Runtime.executionContextCreated",
            "child-session",
            created("child", 9),
        ),
    );
    // A delayed removal from the old process cannot delete the replacement world.
    track_frame_execution_context(
        &mut contexts,
        &event("Page.frameDetached", "page", json!({ "frameId": "child" })),
    );
    assert_eq!(contexts["child"].context_id, 9);
    track_frame_execution_context(
        &mut contexts,
        &event(
            "Page.frameDetached",
            "child-session",
            json!({ "frameId": "child" }),
        ),
    );
    assert!(!contexts.contains_key("child"));
    let mut detached = event(
        "Target.detachedFromTarget",
        "page",
        json!({ "sessionId": "other-page" }),
    );
    detached.session_id = None;
    track_frame_execution_context(&mut contexts, &detached);
    assert!(contexts.is_empty());
    let mut direct = event(
        "Runtime.executionContextCreated",
        "",
        created("direct-child", 10),
    );
    direct.session_id = None;
    track_frame_execution_context(&mut contexts, &direct);
    assert_eq!(contexts["direct-child"].session_id, "");
    assert_eq!(contexts["direct-child"].context_id, 10);
}

#[tokio::test]
#[ignore = "requires installed Chrome"]
async fn e2e_frame_eval_keeps_default_world_and_main_frame() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = clean_env(&dir);
    let mut state = DaemonState::new();
    let commands = [
        json!({ "action": "launch", "headless": true }),
        json!({ "action": "evaluate", "script": "new Promise(resolve => { const f = document.createElement('iframe'); f.srcdoc = '<h1>Child realm</h1><script>globalThis.frameSecret = 41;</script>'; f.onload = () => resolve(true); document.body.append(f); })" }),
        json!({ "action": "snapshot" }),
        json!({ "action": "frame", "selector": "iframe" }),
        json!({ "action": "evaluate", "script": "Promise.resolve(frameSecret + 1)" }),
        json!({ "action": "mainframe" }),
        json!({ "action": "evaluate", "script": "typeof frameSecret" }),
    ];
    for (index, command) in commands.iter().enumerate() {
        let response = execute_command(command, &mut state).await;
        assert_eq!(response["success"], true, "command {index}: {response}");
        if index == 2 {
            assert!(response["data"]["snapshot"]
                .as_str()
                .unwrap()
                .contains("Child realm"));
        } else if index == 4 {
            assert_eq!(response["data"]["result"], 42);
            assert_eq!(response["data"]["origin"], "about:srcdoc");
        } else if index == 6 {
            assert_eq!(response["data"]["result"], "undefined");
            assert_eq!(response["data"]["origin"], "about:blank");
        }
    }
    close_current_browser(&mut state).await.unwrap();
}
