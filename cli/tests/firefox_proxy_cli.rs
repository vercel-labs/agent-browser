#![cfg(unix)]

use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const TARGET: &str = "http://example.com/";

fn cli(directory: &Path) -> Command {
    let mut command = Command::new(BIN);
    command
        .kill_on_drop(true)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", directory)
        .env("TMPDIR", directory)
        .env("AGENT_BROWSER_SOCKET_DIR", directory.join("sock"))
        .env("AGENT_BROWSER_SESSION", "proxy")
        .current_dir(directory);
    command
}

#[tokio::test]
#[ignore = "requires FIREFOX_BIN; run serially"]
async fn e2e_firefox_authenticated_proxy_env_and_config_credentials() {
    let firefox = std::env::var("FIREFOX_BIN").expect("requires FIREFOX_BIN");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let proxy = tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut socket = BufReader::new(socket);
                let _ = tokio::time::timeout(Duration::from_secs(5), async {
                    let mut line = String::new();
                    socket.read_line(&mut line).await?;
                    let expected_page = line.starts_with(&format!("GET {TARGET} "));
                    let mut authenticated = false;
                    loop {
                        line.clear();
                        if socket.read_line(&mut line).await? == 0 || line == "\r\n" {
                            break;
                        }
                        if let Some((name, value)) = line.split_once(':') {
                            authenticated |= name.eq_ignore_ascii_case("Proxy-Authorization")
                                && value.trim() == "Basic dXNlcjpzZWNyZXQ=";
                        }
                    }
                    let response = if expected_page && authenticated {
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n<title>proxy-auth-ok</title>"
                    } else {
                        "HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"test\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    };
                    socket.get_mut().write_all(response.as_bytes()).await
                }).await;
            });
        }
    });

    for use_config in [false, true] {
        let directory = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::create_dir(directory.path().join("sock")).unwrap();
        let mut command = cli(directory.path());
        if use_config {
            let config = directory.path().join("proxy.json");
            std::fs::write(&config, json!({"proxy": proxy_url}).to_string()).unwrap();
            command.arg("--config").arg(config);
        } else {
            command.env("AGENT_BROWSER_PROXY", &proxy_url);
        }
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            command
                .args([
                    "--json",
                    "--engine",
                    "firefox",
                    "--executable-path",
                    &firefox,
                    "open",
                    TARGET,
                ])
                .env("AGENT_BROWSER_PROXY_USERNAME", "user")
                .env("AGENT_BROWSER_PROXY_PASSWORD", "secret")
                .output(),
        )
        .await;
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            cli(directory.path()).arg("close").output(),
        )
        .await;
        let output = output.expect("Firefox proxy navigation timed out").unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "config={use_config}: {stdout} {stderr}"
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            response["data"]["title"], "proxy-auth-ok",
            "config={use_config}: {response}"
        );
    }
    proxy.abort();
}
