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
///
/// Tries `/json/version` first (standard CDP HTTP endpoint). If that fails
/// (e.g., Chrome's UI-based remote debugging only exposes a WebSocket
/// endpoint), falls back to `/json/list` to discover the browser target.
pub async fn discover_cdp_url_with_timeout(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<String, String> {
    // Primary: /json/version (standard path)
    let version_err = match fetch_cdp_info(host, port, timeout).await {
        Ok(info) => {
            if let Some(ws_url) = info.web_socket_debugger_url {
                return Ok(rewrite_ws_host(&ws_url, host, port));
            }
            format!(
                "No webSocketDebuggerUrl in /json/version at {}:{}",
                host, port
            )
        }
        Err(e) => e,
    };

    // Fallback: /json/list (returns target list; look for the browser target)
    match fetch_cdp_list(host, port, timeout).await {
        Ok(ws_url) => Ok(rewrite_ws_host(&ws_url, host, port)),
        Err(_) => {
            // Return the original /json/version error since that's the primary path
            Err(version_err)
        }
    }
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

/// Fetch `/json/list` and extract the `webSocketDebuggerUrl` from the first
/// target with `type == "browser"`, or the first target if none has that type.
async fn fetch_cdp_list(host: &str, port: u16, timeout: Duration) -> Result<String, String> {
    let url = format!("http://{}:{}/json/list", bracket_ipv6(host), port);

    let body = tokio::time::timeout(timeout, reqwest_get_string(&url))
        .await
        .map_err(|_| format!("Timeout connecting to /json/list at {}:{}", host, port))?
        .map_err(|e| {
            format!(
                "Failed to connect to /json/list at {}:{}: {}",
                host, port, e
            )
        })?;

    let targets: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("Invalid /json/list response: {}", e))?;

    // Prefer targets with type "browser", fall back to first target with a ws URL
    let browser_target = targets
        .iter()
        .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("browser"));

    let target = browser_target.or_else(|| targets.first());

    target
        .and_then(|t| t.get("webSocketDebuggerUrl"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "No webSocketDebuggerUrl found in /json/list targets".to_string())
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
    async fn invalid_json_falls_through_to_list_fallback() {
        let (port, server) = spawn_json_server("not-json").await;

        let err = discover_cdp_url("127.0.0.1", port).await.unwrap_err();
        // /json/version returns invalid JSON; /json/list also fails (server
        // closed), so the original /json/version error is returned
        assert!(err.contains("Invalid /json/version response"));
        server.await.unwrap();
    }

    async fn spawn_json_list_server(body: &'static str) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            // First request: /json/version -> 404
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let not_found =
                "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            socket.write_all(not_found.as_bytes()).await.unwrap();
            drop(socket);

            // Second request: /json/list -> 200
            let (mut socket2, _) = listener.accept().await.unwrap();
            let _ = socket2.read(&mut buf).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{}",
                body.len(),
                body
            );
            socket2.write_all(response.as_bytes()).await.unwrap();
        });
        (port, handle)
    }

    #[tokio::test]
    async fn falls_back_to_json_list_on_version_404() {
        let (port, server) = spawn_json_list_server(
            r#"[{"type":"browser","webSocketDebuggerUrl":"ws://127.0.0.1:1234/devtools/browser/abc"}]"#,
        )
        .await;

        let ws_url = discover_cdp_url("127.0.0.1", port).await.unwrap();
        assert!(ws_url.contains("/devtools/browser/abc"));
        assert!(ws_url.contains(&port.to_string()));
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
