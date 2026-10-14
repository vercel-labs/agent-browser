//! Moli adapter regression tests and opt-in tests against a real MOLI_BIN.

use super::actions::{execute_command, DaemonState};
use crate::test_utils::EnvGuard;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{timeout, Duration};

fn moli_test_environment() -> EnvGuard<'static> {
    let keys = &[
        "AGENT_BROWSER_CDP",
        "AGENT_BROWSER_AUTO_CONNECT",
        "AGENT_BROWSER_PROVIDER",
        "AGENT_BROWSER_ENGINE",
        "AGENT_BROWSER_EXECUTABLE_PATH",
        "AGENT_BROWSER_PROXY",
        "AGENT_BROWSER_PROXY_BYPASS",
        "AGENT_BROWSER_PROXY_USERNAME",
        "AGENT_BROWSER_PROXY_PASSWORD",
        "AGENT_BROWSER_PROFILE",
        "AGENT_BROWSER_STATE",
        "AGENT_BROWSER_EXTENSIONS",
        "AGENT_BROWSER_ARGS",
        "AGENT_BROWSER_HEADED",
        "AGENT_BROWSER_WEBGPU",
        "AGENT_BROWSER_CA_CERT",
        "AGENT_BROWSER_ALLOWED_DOMAINS",
        "AGENT_BROWSER_USER_AGENT",
        "AGENT_BROWSER_IGNORE_HTTPS_ERRORS",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "MOLI_BLOCK_PRIVATE_NETWORKS",
        "MOLI_LAYOUT",
        "MOLI_RESOURCE",
    ];
    let env = EnvGuard::new(keys);
    for key in keys {
        env.remove(key);
    }
    env
}

async fn moli_command(state: &mut DaemonState, command: Value) -> Value {
    timeout(
        Duration::from_secs(30),
        Box::pin(execute_command(&command, state)),
    )
    .await
    .expect("Moli command must not hang")
}

fn assert_success(response: &Value) {
    assert_eq!(response["success"], true, "{response}");
}

struct MoliFixture {
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for MoliFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn moli_fixture() -> MoliFixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request).await;
            let body = include_str!("test_fixtures/moli-probe.html");
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    MoliFixture { url, server }
}

async fn verify_moli_page(auto_launch: bool) {
    let binary = std::env::var("MOLI_BIN")
        .expect("MOLI_BIN is required for explicitly invoked Moli E2E verification");
    assert!(!binary.trim().is_empty(), "MOLI_BIN must not be empty");
    assert!(
        std::path::Path::new(&binary).is_file(),
        "MOLI_BIN must point to an existing executable"
    );
    let env = moli_test_environment();
    let fixture = moli_fixture().await;
    let mut state = DaemonState::new();
    if auto_launch {
        env.set("AGENT_BROWSER_ENGINE", "moli");
        env.set("AGENT_BROWSER_EXECUTABLE_PATH", &binary);
        env.set("AGENT_BROWSER_ARGS", "--timeout=30");
        env.set("AGENT_BROWSER_USER_AGENT", "Moli adapter probe");
        env.set("AGENT_BROWSER_PROXY", "http://127.0.0.1:1");
        env.set("AGENT_BROWSER_PROXY_USERNAME", "probe");
        env.set("AGENT_BROWSER_PROXY_PASSWORD", "pass");
        env.set("AGENT_BROWSER_PROXY_BYPASS", "127.0.0.1");
    } else {
        let response = moli_command(
            &mut state,
            json!({
                "id": "launch", "action": "launch", "headless": true,
            "engine": "moli", "executablePath": binary,
            "args": ["--timeout=30"],
            "userAgent": "Moli adapter probe", "hideScrollbars": false,
            "proxy": {"server": "http://127.0.0.1:1", "username": "probe", "password": "pass", "bypass": "127.0.0.1"},
            }),
        )
        .await;
        assert_success(&response);
    }
    let response = moli_command(
        &mut state,
        json!({
            "id": "navigate", "action": "navigate", "url": fixture.url,
        }),
    )
    .await;
    assert_success(&response);
    assert_eq!(response["data"]["title"], "Moli adapter probe");
    assert_eq!(response["data"]["url"], fixture.url);
    assert_eq!(state.engine, "moli");
    assert_eq!(
        *state.proxy_credentials.read().await,
        Some(("probe".into(), "pass".into()))
    );
    assert!(
        state
            .browser
            .as_ref()
            .is_some_and(|manager| manager.owns_moli_process()),
        "verification must own a Moli process rather than attach to an inherited CDP endpoint"
    );

    let response = moli_command(&mut state, json!({
        "id": "js", "action": "evaluate", "script": "[document.querySelector('#message').textContent, navigator.userAgent]",
    })).await;
    assert_success(&response);
    assert_eq!(
        response["data"]["result"],
        json!(["JavaScript ready", "Moli adapter probe"])
    );

    let response = moli_command(
        &mut state,
        json!({
            "id": "snapshot", "action": "snapshot", "interactive": true,
        }),
    )
    .await;
    assert_success(&response);
    let refs = response["data"]["refs"].as_object().unwrap();
    let button_ref = refs
        .iter()
        .find(|(_, value)| value["role"] == "button" && value["name"] == "Submit")
        .map(|(key, _)| format!("@{key}"))
        .expect("snapshot must expose the submit button");
    let response = moli_command(
        &mut state,
        json!({
            "id": "fill", "action": "fill", "selector": "#name", "value": "Moli",
        }),
    )
    .await;
    assert_success(&response);
    let response = moli_command(
        &mut state,
        json!({
            "id": "click", "action": "click", "selector": button_ref,
        }),
    )
    .await;
    assert_success(&response);
    let response = moli_command(&mut state, json!({
        "id": "result", "action": "evaluate", "script": "document.querySelector('#message').textContent",
    })).await;
    assert_success(&response);
    assert_eq!(response["data"]["result"], "Hello Moli");

    let output = tempfile::tempdir().unwrap();
    let mut viewport_height = 0;
    for full_page in [false, true] {
        let path = output.path().join(format!("screenshot-{full_page}.png"));
        let response = moli_command(
            &mut state,
            json!({
                "id": "screenshot", "action": "screenshot", "path": path,
                "fullPage": full_page,
            }),
        )
        .await;
        assert_success(&response);
        let bitmap = image::open(&path).expect("Moli screenshot must be a decodable image");
        assert!(bitmap.width() > 0 && bitmap.height() > 0);
        assert!(
            bitmap
                .to_rgb8()
                .pixels()
                .any(|pixel| pixel.0 != [255, 255, 255]),
            "Moli must render the fixture content, rather than an empty white image"
        );
        if full_page {
            assert!(
                bitmap.height() > viewport_height,
                "full-page capture must extend below the viewport"
            );
        } else {
            viewport_height = bitmap.height();
        }
    }
    let ws_url = state.browser.as_ref().unwrap().get_cdp_url().to_string();
    let port = url::Url::parse(&ws_url).unwrap().port().unwrap();
    let response = moli_command(&mut state, json!({"id": "close", "action": "close"})).await;
    assert_success(&response);
    assert_eq!(response["data"]["closed"], true);
    assert!(state.browser.is_none());
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "closing the session must stop its owned Moli server"
    );
}

#[tokio::test]
#[ignore = "requires MOLI_BIN; run serially"]
async fn e2e_moli_explicit_launch() {
    verify_moli_page(false).await;
}

#[tokio::test]
#[ignore = "requires MOLI_BIN; run serially"]
async fn e2e_moli_auto_launch() {
    verify_moli_page(true).await;
}
