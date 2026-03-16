use std::time::Duration;

use super::types::BrowserVersionInfo;

/// Default timeout for CDP discovery HTTP requests.
const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Discover the CDP WebSocket URL by querying `/json/version` at the given host and port.
/// The returned `webSocketDebuggerUrl` has its host/port rewritten to match
/// the requested target, since Chrome always reports `127.0.0.1` regardless
/// of the interface it was reached through.
pub async fn discover_cdp_url(host: &str, port: u16) -> Result<String, String> {
    discover_cdp_url_with_timeout(host, port, DEFAULT_DISCOVERY_TIMEOUT).await
}

/// Like [`discover_cdp_url`] but with a custom request timeout.
pub async fn discover_cdp_url_with_timeout(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<String, String> {
    let info = fetch_cdp_info(host, port, timeout).await?;
    let ws_url = info.web_socket_debugger_url.ok_or_else(|| {
        format!(
            "No webSocketDebuggerUrl in /json/version at {}:{}",
            host, port
        )
    })?;
    Ok(rewrite_ws_host(&ws_url, host, port))
}

/// Bracket an IPv6 address for use in URLs. No-op for IPv4 or already-bracketed addresses.
fn bracket_ipv6(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{}]", host)
    } else {
        host.to_string()
    }
}

/// Fetch `/json/version` from the given host:port and parse the response.
async fn fetch_cdp_info(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<BrowserVersionInfo, String> {
    let url = format!("http://{}:{}/json/version", bracket_ipv6(host), port);

    let body = tokio::time::timeout(timeout, reqwest_get_string(&url))
        .await
        .map_err(|_| format!("Timeout connecting to CDP at {}:{}", host, port))?
        .map_err(|e| format!("Failed to connect to CDP at {}:{}: {}", host, port, e))?;

    serde_json::from_str(&body).map_err(|e| format!("Invalid /json/version response: {}", e))
}

/// Rewrite the host and port in a WebSocket URL to match the target we
/// actually connected to. Chrome's `/json/version` always returns
/// `ws://127.0.0.1:<local-port>/...` which is unreachable when the
/// browser is on a remote machine or behind a port-forward.
fn rewrite_ws_host(ws_url: &str, host: &str, port: u16) -> String {
    if let Ok(mut parsed) = url::Url::parse(ws_url) {
        let _ = parsed.set_host(Some(&bracket_ipv6(host)));
        let _ = parsed.set_port(Some(port));
        parsed.to_string()
    } else {
        ws_url.to_string()
    }
}

async fn reqwest_get_string(url: &str) -> Result<String, String> {
    let resp = reqwest::get(url).await.map_err(|e| e.to_string())?;
    resp.text().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn spawn_json_server(body: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (port, handle)
    }

    #[tokio::test]
    async fn discovers_ws_url_from_json_version() {
        let (port, server) =
            spawn_json_server(r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1234/"}"#).await;

        let ws_url = discover_cdp_url("127.0.0.1", port).await.unwrap();
        assert_eq!(ws_url, format!("ws://127.0.0.1:{}/", port));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_json_returns_parse_error() {
        let (port, server) = spawn_json_server("not-json").await;

        let err = discover_cdp_url("127.0.0.1", port).await.unwrap_err();
        assert!(err.contains("Invalid /json/version response"));
        server.await.unwrap();
    }

    #[test]
    fn rewrite_ws_host_replaces_host_and_port() {
        let original = "ws://127.0.0.1:9222/devtools/browser/abc";
        let rewritten = rewrite_ws_host(original, "10.211.55.12", 9223);
        assert_eq!(rewritten, "ws://10.211.55.12:9223/devtools/browser/abc");
    }

    #[test]
    fn rewrite_ws_host_handles_ipv6() {
        let original = "ws://127.0.0.1:9222/devtools/browser/abc";
        let rewritten = rewrite_ws_host(original, "::1", 9222);
        assert_eq!(rewritten, "ws://[::1]:9222/devtools/browser/abc");
    }
}
