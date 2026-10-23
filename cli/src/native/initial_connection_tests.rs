//! Regression coverage for events emitted before CDP connection setup returns.
use super::*;
use crate::test_utils::EnvGuard;
use futures_util::{SinkExt, StreamExt};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::Message;

struct MockBrowser {
    url: String,
    commands: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockBrowser {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn target(id: &str, kind: &str) -> Value {
    json!({"targetId": id, "type": kind, "url": "http://example.test/", "title": id, "attached": true})
}

fn attachment(id: &str, kind: &str, parent: &str) -> Value {
    json!({"method": "Target.attachedToTarget", "sessionId": parent, "params": {
        "sessionId": format!("session-{id}"), "targetInfo": target(id, kind), "waitingForDebugger": true
    }})
}

async fn mock_browser(stall_child: bool) -> MockBrowser {
    mock_browser_with_event_burst(stall_child, 0).await
}

async fn mock_browser_with_event_burst(stall_child: bool, event_burst: usize) -> MockBrowser {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://{}/devtools/browser/test",
        listener.local_addr().unwrap()
    );
    let commands = Arc::new(Mutex::new(Vec::new()));
    let recorded = commands.clone();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut attached = HashSet::new();
        while let Some(Ok(message)) = ws.next().await {
            let Message::Text(text) = message else {
                continue;
            };
            let cmd: Value = serde_json::from_str(&text).unwrap();
            recorded.lock().unwrap().push(cmd.clone());
            let method = cmd["method"].as_str().unwrap();
            let sid = cmd["sessionId"].as_str().unwrap_or("");
            if stall_child && method == "Page.enable" && sid == "session-child" {
                continue;
            }
            // Events are deliberately sent before the command response. This
            // needs no scheduler sleeps to reproduce the initialization race.
            let mut events = Vec::new();
            let result = match method {
                "Target.getTargets" => {
                    json!({"targetInfos": [target("top", "page"), target("background", "page")]})
                }
                "Target.attachToTarget" => {
                    let id = cmd["params"]["targetId"].as_str().unwrap();
                    events.push(attachment(id, "page", ""));
                    json!({"sessionId": format!("session-{id}")})
                }
                "Target.setAutoAttach" if attached.insert(sid.to_string()) => {
                    match sid {
                        "session-top" => {
                            events.push(attachment("child", "iframe", sid));
                            events.push(attachment("detached", "iframe", sid));
                            events.push(json!({"method":"Target.detachedFromTarget", "params":{"sessionId":"session-detached"}}));
                            events.push(attachment("destroyed", "iframe", sid));
                            events.push(json!({"method":"Target.targetDestroyed", "params":{"targetId":"destroyed"}}));
                            events.push(attachment("worker", "worker", sid));
                        }
                        "session-child" => events.push(attachment("nested", "iframe", sid)),
                        _ => {}
                    }
                    json!({})
                }
                "Test.attach" => {
                    events.push(attachment("later", "iframe", "session-top"));
                    json!({})
                }
                "Test.destroy" => {
                    events.push(
                        json!({"method":"Target.targetDestroyed", "params":{"targetId":"later"}}),
                    );
                    json!({})
                }
                "Test.eventOrder" => {
                    let console = || json!({"method":"Runtime.consoleAPICalled", "sessionId":"session-top", "params":{"type":"log", "args":[]}});
                    events.push(console());
                    events.push(attachment("later", "iframe", "session-top"));
                    events.push(console());
                    events.push(
                        json!({"method":"Target.targetDestroyed", "params":{"targetId":"later"}}),
                    );
                    events.push(console());
                    json!({})
                }
                "Page.getFrameTree" => {
                    let id = sid.strip_prefix("session-").unwrap_or("top");
                    let mut frame =
                        json!({"id": id, "loaderId":"loader-1", "url":"http://example.test/"});
                    if id == "child" {
                        frame["parentId"] = json!("top");
                    }
                    if id == "nested" {
                        frame["parentId"] = json!("child");
                    }
                    let mut tree = json!({"frame": frame});
                    if id == "top" {
                        tree["childFrames"] = json!([{"frame":{"id":"child","parentId":"top","url":"http://example.test/"}}]);
                    }
                    if id == "child" {
                        tree["childFrames"] = json!([{"frame":{"id":"nested","parentId":"child","url":"http://example.test/"}}]);
                    }
                    json!({"frameTree":tree})
                }
                "Accessibility.getFullAXTree" => {
                    if sid == "session-top" && cmd["params"]["frameId"] == "child" {
                        ws.send(Message::Text(json!({"id":cmd["id"],"error":{"code":-32000,"message":"Frame with the given frameId is not found"}}).to_string())).await.unwrap();
                        continue;
                    }
                    let (role, name, bid) = if sid == "session-top" {
                        ("Iframe", "Report", 10)
                    } else {
                        ("button", "Choose metrics", 20)
                    };
                    json!({"nodes":[{"nodeId":"1","role":{"type":"role","value":role},"name":{"type":"string","value":name},"backendDOMNodeId":bid}]})
                }
                "DOM.describeNode" => {
                    json!({"node":{"nodeName":"IFRAME","frameId":"child","backendNodeId":10}})
                }
                "Runtime.evaluate" => json!({"result":{"type":"object","value":[]}}),
                _ => json!({}),
            };
            let emitted_events = !events.is_empty();
            for event in events {
                ws.send(Message::Text(event.to_string())).await.unwrap();
            }
            if emitted_events {
                // Overflow the ordinary broadcast before the command returns,
                // after the target events that initialization must retain.
                for n in 0..event_burst {
                    ws.send(Message::Text(
                        json!({
                            "method": "Network.dataReceived", "sessionId": sid,
                            "params": {"requestId": "noise", "dataLength": n}
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
                }
            }
            if ws
                .send(Message::Text(
                    json!({"id":cmd["id"],"result":result}).to_string(),
                ))
                .await
                .is_err()
            {
                break;
            }
        }
    });
    MockBrowser {
        url,
        commands,
        task,
    }
}

fn isolated_env(dir: &std::path::Path) -> EnvGuard<'static> {
    let guard = EnvGuard::new(&[
        "AGENT_BROWSER_CDP",
        "AGENT_BROWSER_AUTO_CONNECT",
        "AGENT_BROWSER_PROVIDER",
        "AGENT_BROWSER_SESSION",
        "AGENT_BROWSER_SESSION_NAME",
        "AGENT_BROWSER_RESTORE",
        "AGENT_BROWSER_SOCKET_DIR",
        "AGENT_BROWSER_NO_WEBMCP",
        "AGENT_BROWSER_PIN_TAB",
        "AGENT_BROWSER_ALLOWED_DOMAINS",
    ]);
    for name in [
        "AGENT_BROWSER_CDP",
        "AGENT_BROWSER_AUTO_CONNECT",
        "AGENT_BROWSER_PROVIDER",
        "AGENT_BROWSER_SESSION_NAME",
        "AGENT_BROWSER_RESTORE",
        "AGENT_BROWSER_PIN_TAB",
        "AGENT_BROWSER_ALLOWED_DOMAINS",
    ] {
        guard.remove(name);
    }
    guard.set("AGENT_BROWSER_SESSION", "initial-connection-test");
    guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_str().unwrap());
    guard.set("AGENT_BROWSER_NO_WEBMCP", "1");
    guard
}

#[tokio::test]
async fn first_snapshot_after_auto_connect_uses_initial_iframe_session() {
    let dir = tempfile::tempdir().unwrap();
    let guard = isolated_env(dir.path());
    let mock = mock_browser(false).await;
    guard.set("AGENT_BROWSER_CDP", &mock.url);
    let mut state = DaemonState::new();
    let response = execute_command(&json!({"action":"snapshot"}), &mut state).await;
    assert_eq!(response["success"], true, "{response}");
    assert!(
        response["data"]["snapshot"]
            .as_str()
            .unwrap()
            .contains("Choose metrics"),
        "first snapshot must include the iframe: {response}"
    );
    assert_eq!(
        state.iframe_sessions.get("nested").map(String::as_str),
        Some("session-nested")
    );
    assert_eq!(
        state.iframe_sessions.len(),
        2,
        "stale attachments must not survive"
    );
    assert_eq!(
        state.browser.as_ref().unwrap().active_target_id().unwrap(),
        "top"
    );
    let commands = mock.commands.lock().unwrap().clone();
    assert!(!commands.iter().any(|cmd| [
        "session-detached",
        "session-destroyed",
        "session-background"
    ]
    .contains(&cmd["sessionId"].as_str().unwrap_or(""))
        && cmd["method"] == "Page.enable"));
    assert!(commands
        .iter()
        .any(|cmd| cmd["method"] == "Runtime.runIfWaitingForDebugger"
            && cmd["sessionId"] == "session-worker"));
    close_current_browser(&mut state).await.unwrap();
}

#[tokio::test]
async fn target_events_keep_wire_order_without_broadcast_duplicates() {
    let mock = mock_browser(false).await;
    let mut browser = BrowserManager::connect_cdp(&mock.url).await.unwrap();
    let mut events = browser.take_initial_events().unwrap();
    events.drain();
    browser
        .client
        .send_command("Test.eventOrder", None, None)
        .await
        .unwrap();
    let methods: Vec<_> = events
        .drain()
        .into_iter()
        .map(|event| event.unwrap().method)
        .collect();
    assert_eq!(
        methods,
        [
            "Runtime.consoleAPICalled",
            "Target.attachedToTarget",
            "Runtime.consoleAPICalled",
            "Target.targetDestroyed",
            "Runtime.consoleAPICalled",
        ]
    );
    browser.close().await.unwrap();
}

#[tokio::test]
async fn first_snapshot_retains_initial_targets_when_broadcast_overflows() {
    let dir = tempfile::tempdir().unwrap();
    let guard = isolated_env(dir.path());
    let mock = mock_browser_with_event_burst(false, 5000).await;
    guard.set("AGENT_BROWSER_CDP", &mock.url);
    let mut state = DaemonState::new();
    let response = execute_command(&json!({"action":"snapshot"}), &mut state).await;
    assert_eq!(response["success"], true, "{response}");
    assert!(
        response["data"]["snapshot"]
            .as_str()
            .unwrap()
            .contains("Choose metrics"),
        "initial target events must survive unrelated event bursts: {response}"
    );
    assert_eq!(state.iframe_sessions.len(), 2);
    assert_eq!(
        state.iframe_sessions.get("nested").map(String::as_str),
        Some("session-nested")
    );
    assert_eq!(
        state.browser.as_ref().unwrap().active_target_id().unwrap(),
        "top"
    );
    let commands = mock.commands.lock().unwrap().clone();
    for sid in ["session-child", "session-nested"] {
        assert_eq!(
            commands
                .iter()
                .filter(|cmd| cmd["sessionId"] == sid && cmd["method"] == "Page.enable")
                .count(),
            1,
            "target events must not be applied twice"
        );
    }
    close_current_browser(&mut state).await.unwrap();
}

#[tokio::test]
async fn reconnect_retains_live_target_events_when_broadcast_overflows() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = isolated_env(dir.path());
    let mut state = DaemonState::new();
    for _ in 0..2 {
        let mock = mock_browser_with_event_burst(false, 5000).await;
        let response =
            execute_command(&json!({"action":"launch", "cdpUrl":mock.url}), &mut state).await;
        assert_eq!(response["success"], true, "{response}");
        assert_eq!(state.iframe_sessions.len(), 2);
        state
            .browser
            .as_ref()
            .unwrap()
            .client
            .send_command("Test.attach", None, None)
            .await
            .unwrap();
        state.drain_cdp_events_background().await.unwrap();
        assert!(state.iframe_sessions.contains_key("later"));
        state
            .browser
            .as_ref()
            .unwrap()
            .client
            .send_command("Test.destroy", None, None)
            .await
            .unwrap();
        state.drain_cdp_events_background().await.unwrap();
        assert!(!state.iframe_sessions.contains_key("later"));
        close_current_browser(&mut state).await.unwrap();
        assert!(state.event_rx.is_none());
    }
}

#[tokio::test]
async fn reconnect_transfers_a_fresh_initial_event_receiver() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = isolated_env(dir.path());
    let mut state = DaemonState::new();
    for _ in 0..2 {
        let mock = mock_browser(false).await;
        let response =
            execute_command(&json!({"action":"launch", "cdpUrl":mock.url}), &mut state).await;
        assert_eq!(response["success"], true, "{response}");
        assert_eq!(state.iframe_sessions.len(), 2);
        // The transferred receiver must also keep receiving live events.
        state
            .browser
            .as_ref()
            .unwrap()
            .client
            .send_command("Test.attach", None, None)
            .await
            .unwrap();
        let response = execute_command(&json!({"action":"snapshot"}), &mut state).await;
        assert!(response["data"]["snapshot"]
            .as_str()
            .unwrap()
            .contains("Choose metrics"));
        assert!(state.iframe_sessions.contains_key("later"));
        state
            .browser
            .as_ref()
            .unwrap()
            .client
            .send_command("Test.destroy", None, None)
            .await
            .unwrap();
        state.drain_cdp_events_background().await.unwrap();
        assert!(!state.iframe_sessions.contains_key("later"));
        close_current_browser(&mut state).await.unwrap();
        assert!(state.iframe_sessions.is_empty());
        assert!(
            state.event_rx.is_none(),
            "old connection events must be discarded"
        );
    }
}

#[tokio::test]
async fn initialization_timeout_closes_the_incomplete_connection() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = isolated_env(dir.path());
    let mock = mock_browser(true).await;
    let mut state = DaemonState::new();
    state.default_timeout_ms = 100;
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        execute_command(&json!({"action":"launch", "cdpUrl":mock.url}), &mut state),
    )
    .await
    .expect("initialization must be bounded");
    assert_eq!(response["success"], false, "{response}");
    assert!(
        response.to_string().contains("Timed out initializing"),
        "{response}"
    );
    assert!(state.browser.is_none());
    assert!(state.iframe_sessions.is_empty());
    assert!(state.event_rx.is_none());
    assert!(state.fetch_handler_task.is_none());
    assert!(state.dialog_handler_task.is_none());
}

#[tokio::test]
async fn initial_iframe_controls_are_installed_before_resume() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = isolated_env(dir.path());
    let mock = mock_browser(false).await;
    let mut state = DaemonState::new();
    state.browser = Some(BrowserManager::connect_cdp(&mock.url).await.unwrap());
    state.subscribe_to_browser_events();
    *state.domain_filter.write().await = Some(DomainFilter::new("example.test"));
    state.start_fetch_handler();
    let response = execute_command(&json!({"action":"snapshot"}), &mut state).await;
    assert_eq!(response["success"], true, "{response}");
    // A round trip ensures the server has observed the fire-and-forget resume.
    state
        .browser
        .as_ref()
        .unwrap()
        .client
        .send_command("Target.getTargets", None, None)
        .await
        .unwrap();
    let commands = mock.commands.lock().unwrap().clone();
    for sid in ["session-child", "session-nested"] {
        let fetch = commands
            .iter()
            .position(|c| c["sessionId"] == sid && c["method"] == "Fetch.enable")
            .expect("Fetch must be installed");
        let resume = commands
            .iter()
            .position(|c| c["sessionId"] == sid && c["method"] == "Runtime.runIfWaitingForDebugger")
            .expect("child must be resumed");
        assert!(fetch < resume, "controls must precede resume for {sid}");
    }
    close_current_browser(&mut state).await.unwrap();
}

/// Serve three OOPIFs (including a nested one) and an in-process srcdoc frame.
async fn frame_fixture() -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0; 4096];
                let n = stream.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..n]);
                let path = request.split_whitespace().nth(1).unwrap_or("/");
                let body = match path {
                    "/child" => format!(
                        r#"<button id="metrics" onclick="this.textContent='Selected metrics'">Choose metrics</button><iframe name="nested" src="http://127.0.0.1:{port}/nested"></iframe>"#
                    ),
                    "/nested" => "<button>Nested button</button>".to_string(),
                    "/sibling" => "<button>Sibling button</button>".to_string(),
                    _ => format!(
                        r#"<h1>Portal</h1><iframe name="child" title="Child" src="http://localhost:{port}/child"></iframe><iframe name="sibling" src="http://localhost:{port}/sibling"></iframe><iframe name="same" srcdoc="<button>Same process button</button>"></iframe>"#
                    ),
                };
                let body =
                    format!("<!doctype html><title>Initial connection fixture</title>{body}");
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}/"), task)
}

#[tokio::test]
#[ignore = "requires Chrome"]
async fn e2e_initial_connection_reads_and_clicks_preloaded_iframes() {
    use super::super::cdp::chrome::{launch_chrome, LaunchOptions};
    let dir = tempfile::tempdir().unwrap();
    let guard = isolated_env(dir.path());
    let (url, server) = frame_fixture().await;
    let chrome = tokio::task::spawn_blocking(|| {
        launch_chrome(&LaunchOptions {
            headless: true,
            args: vec![
                "--site-per-process".to_string(),
                "--no-proxy-server".to_string(),
            ],
            ..Default::default()
        })
    })
    .await
    .unwrap()
    .unwrap();
    // Load the fixture using plain CDP, without BrowserManager or auto-attach.
    // This guarantees all iframe targets exist before the tested connection.
    let producer = CdpClient::connect(&chrome.ws_url).await.unwrap();
    let targets = producer
        .send_command("Target.getTargets", None, None)
        .await
        .unwrap();
    let page = targets["targetInfos"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["type"] == "page")
        .unwrap();
    let attached = producer
        .send_command(
            "Target.attachToTarget",
            Some(json!({"targetId":page["targetId"],"flatten":true})),
            None,
        )
        .await
        .unwrap();
    let sid = attached["sessionId"].as_str().unwrap();
    producer
        .send_command("Page.enable", None, Some(sid))
        .await
        .unwrap();
    let mut events = producer.subscribe();
    producer
        .send_command("Page.navigate", Some(json!({"url":url})), Some(sid))
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.method == "Page.loadEventFired" && event.session_id.as_deref() == Some(sid) {
                break;
            }
        }
    })
    .await
    .expect("fixture must finish loading before attach");
    let targets = producer
        .send_command("Target.getTargets", None, None)
        .await
        .unwrap();
    assert_eq!(
        targets["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["type"] == "iframe")
            .count(),
        3,
        "fixture must contain three real OOPIF targets: {targets}"
    );
    producer.close().await;

    guard.set("AGENT_BROWSER_CDP", &chrome.ws_url);
    let mut state = DaemonState::new();
    for attempt in 0..2 {
        let snapshot = execute_command(&json!({"action":"snapshot"}), &mut state).await;
        assert_eq!(snapshot["success"], true, "{snapshot}");
        let text = snapshot["data"]["snapshot"].as_str().unwrap();
        let label = if attempt == 0 {
            "Choose metrics"
        } else {
            "Selected metrics"
        };
        for expected in [label, "Sibling button", "Same process button"] {
            assert!(
                text.contains(expected),
                "first snapshot after connection {attempt} must contain {expected}: {snapshot}"
            );
        }
        assert_eq!(
            state.iframe_sessions.len(),
            3,
            "nested attachment must be initialized before the first action"
        );
        if attempt == 0 {
            let reference = snapshot["data"]["refs"]
                .as_object()
                .unwrap()
                .iter()
                .find(|(_, entry)| entry["name"] == "Choose metrics")
                .unwrap()
                .0;
            let clicked = execute_command(
                &json!({"action":"click","selector":format!("@{reference}")}),
                &mut state,
            )
            .await;
            assert_eq!(clicked["success"], true, "{clicked}");
        }
        // Select the child through the public frame command, then read it on
        // the first attempt. This was the failing sequence in the CLI repro.
        let child_ref = snapshot["data"]["refs"]
            .as_object()
            .unwrap()
            .iter()
            .find(|(_, entry)| entry["role"] == "Iframe" && entry["name"] == "Child")
            .expect("snapshot must expose the child iframe")
            .0;
        let selected = execute_command(
            &json!({"action":"frame","selector":format!("@{child_ref}")}),
            &mut state,
        )
        .await;
        assert_eq!(selected["success"], true, "{selected}");
        let child = execute_command(&json!({"action":"snapshot"}), &mut state).await;
        assert!(
            child["data"]["snapshot"]
                .as_str()
                .unwrap()
                .contains("Selected metrics"),
            "{child}"
        );
        // The snapshot format deliberately expands only one iframe level.
        // Verify that the nested renderer was nevertheless registered/resumed.
        let nested = state
            .browser
            .as_ref()
            .unwrap()
            .client
            .send_command("Target.getTargets", None, None)
            .await
            .unwrap();
        let nested_id = nested["targetInfos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| {
                t["url"]
                    .as_str()
                    .is_some_and(|url| url.ends_with("/nested"))
            })
            .unwrap()["targetId"]
            .as_str()
            .unwrap();
        let nested_sid = state.iframe_sessions.get(nested_id).unwrap();
        let ready = state.browser.as_ref().unwrap().client.send_command("Runtime.evaluate", Some(json!({"expression":"document.querySelector('button').textContent","returnByValue":true})), Some(nested_sid)).await.unwrap();
        assert_eq!(ready["result"]["value"], "Nested button");
        close_current_browser(&mut state).await.unwrap();
        state.active_frame_id = None;
    }
    server.abort();
    drop(chrome);
}
