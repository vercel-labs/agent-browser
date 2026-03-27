use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::types::BrowserVersionInfo;

/// Default timeout for CDP discovery HTTP requests.
const DEFAULT_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(2);

/// Discover the CDP WebSocket URL for the given host and port.
///
/// Tries three methods in order: `/json/version`, `/json/list`, and a direct
/// WebSocket connection to `/devtools/browser`. The returned URL has its
/// host/port rewritten to match the requested target.
///
/// An optional `query` string (without the leading `?`) is appended to the
/// final WebSocket URL so that user-supplied URL parameters (e.g.
/// `?mode=Hello`) are forwarded to the remote endpoint.
pub async fn discover_cdp_url(
    host: &str,
    port: u16,
    query: Option<&str>,
) -> Result<String, String> {
    discover_cdp_url_with_timeout(host, port, query, DEFAULT_DISCOVERY_TIMEOUT).await
}

/// Like [`discover_cdp_url`] but with a custom request timeout.
pub async fn discover_cdp_url_with_timeout(
    host: &str,
    port: u16,
    query: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    // Primary: /json/version (standard path)
    let version_err = match fetch_cdp_info(host, port, timeout).await {
        Ok(info) => {
            if let Some(ws_url) = info.web_socket_debugger_url {
                return Ok(append_query(&rewrite_ws_host(&ws_url, host, port), query));
            }
            format!(
                "No webSocketDebuggerUrl in /json/version at {}:{}",
                host, port
            )
        }
        Err(e) => e,
    };

    // Fallback: /json/list (returns target list; look for the browser target)
    let list_err = match fetch_cdp_list(host, port, timeout).await {
        Ok(ws_url) => return Ok(append_query(&rewrite_ws_host(&ws_url, host, port), query)),
        Err(e) => e,
    };

    // Final fallback: direct WebSocket at /devtools/browser.
    // Chrome 136+ with UI-based remote debugging (chrome://inspect) exposes
    // CDP over WebSocket but does not serve HTTP discovery endpoints.
    match discover_cdp_ws(host, port, timeout).await {
        Ok(ws_url) => Ok(append_query(&ws_url, query)),
        Err(ws_err) => Err(format!(
            "All CDP discovery methods failed for {}:{}: /json/version: {}; /json/list: {}; WebSocket: {}",
            host, port, version_err, list_err, ws_err
        )),
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

/// Append a query string to a URL, preserving any existing query parameters.
fn append_query(url: &str, query: Option<&str>) -> String {
    match query {
        Some(q) if !q.is_empty() => {
            if let Ok(mut parsed) = url::Url::parse(url) {
                {
                    let mut pairs = parsed.query_pairs_mut();
                    pairs.extend_pairs(url::form_urlencoded::parse(q.as_bytes()));
                }
                parsed.to_string()
            } else {
                // Fallback: raw string append
                if url.contains('?') {
                    format!("{}&{}", url, q)
                } else {
                    format!("{}?{}", url, q)
                }
            }
        }
        _ => url.to_string(),
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

/// Discover a CDP endpoint by connecting directly to `ws://host:port/devtools/browser`
/// and verifying it responds to `Browser.getVersion`.
/// Returns the WebSocket URL on success.
async fn discover_cdp_ws(host: &str, port: u16, timeout: Duration) -> Result<String, String> {
    let ws_url = format!("ws://{}:{}/devtools/browser", bracket_ipv6(host), port);

    tokio::time::timeout(timeout, async {
        let (mut ws_stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .map_err(|e| format!("WebSocket connect failed at {}: {}", ws_url, e))?;

        let cmd = r#"{"id":1,"method":"Browser.getVersion"}"#;
        ws_stream
            .send(Message::Text(cmd.into()))
            .await
            .map_err(|e| format!("Failed to send command: {}", e))?;

        #[derive(serde::Deserialize)]
        struct CdpReply {
            id: u64,
        }

        let mut result: Result<(), String> = Err("No valid CDP response received".to_string());
        while let Some(msg) = ws_stream.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if serde_json::from_str::<CdpReply>(&text).is_ok_and(|r| r.id == 1) {
                        result = Ok(());
                        break;
                    }
                }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => continue,
            }
        }

        let _ = ws_stream.close(None).await;
        result
    })
    .await
    .map_err(|_| format!("Timeout connecting to WebSocket at {}", ws_url))?
    .map(|()| ws_url)
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

    const HTTP_404: &str =
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    fn http_200(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(), body
        )
    }

    async fn accept_http(listener: &TcpListener, response: &str) {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = s.read(&mut buf).await;
        s.write_all(response.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    async fn discovers_ws_url_from_json_version() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            accept_http(
                &listener,
                &http_200(r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1234/"}"#),
            )
            .await;
        });

        let ws_url = discover_cdp_url("127.0.0.1", port, None).await.unwrap();
        assert_eq!(ws_url, format!("ws://127.0.0.1:{}/", port));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn returns_error_when_version_returns_invalid_json() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            accept_http(&listener, &http_200("not-json")).await;
            // /json/list and ws fallback both fail (server closes)
        });

        let err = discover_cdp_url("127.0.0.1", port, None).await.unwrap_err();
        assert!(err.contains("Invalid /json/version response"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn falls_back_to_json_list_on_version_404() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            accept_http(&listener, HTTP_404).await;
            accept_http(
                &listener,
                &http_200(r#"[{"type":"browser","webSocketDebuggerUrl":"ws://127.0.0.1:1234/devtools/browser/abc"}]"#),
            ).await;
        });

        let ws_url = discover_cdp_url("127.0.0.1", port, None).await.unwrap();
        assert!(ws_url.contains("/devtools/browser/abc"));
        assert!(ws_url.contains(&port.to_string()));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn falls_back_to_ws_when_http_returns_404() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            // /json/version -> 404, /json/list -> 404
            accept_http(&listener, HTTP_404).await;
            accept_http(&listener, HTTP_404).await;

            // WebSocket handshake + respond to Browser.getVersion
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            if let Some(Ok(Message::Text(text))) = ws.next().await {
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req.get("id").unwrap();
                let reply = format!(
                    r#"{{"id":{},"result":{{"protocolVersion":"1.3","product":"Chrome/136"}}}}"#,
                    id
                );
                ws.send(Message::Text(reply)).await.unwrap();
            }
            let _ = ws.close(None).await;
        });

        let ws_url = discover_cdp_url("127.0.0.1", port, None).await.unwrap();
        assert_eq!(ws_url, format!("ws://127.0.0.1:{}/devtools/browser", port));
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

    #[test]
    fn append_query_adds_params_to_url_without_query() {
        let url = "ws://127.0.0.1:9222/devtools/browser/abc";
        let result = append_query(url, Some("mode=Hello"));
        assert_eq!(
            result,
            "ws://127.0.0.1:9222/devtools/browser/abc?mode=Hello"
        );
    }

    #[test]
    fn append_query_merges_with_existing_query() {
        let url = "ws://127.0.0.1:9222/devtools/browser/abc?token=xyz";
        let result = append_query(url, Some("mode=Hello"));
        assert_eq!(
            result,
            "ws://127.0.0.1:9222/devtools/browser/abc?token=xyz&mode=Hello"
        );
    }

    #[test]
    fn append_query_noop_for_none() {
        let url = "ws://127.0.0.1:9222/devtools/browser/abc";
        let result = append_query(url, None);
        assert_eq!(result, url);
    }

    #[test]
    fn append_query_noop_for_empty() {
        let url = "ws://127.0.0.1:9222/devtools/browser/abc";
        let result = append_query(url, Some(""));
        assert_eq!(result, url);
    }

    #[test]
    fn append_query_handles_multiple_params() {
        let url = "ws://127.0.0.1:9222/devtools/browser/abc";
        let result = append_query(url, Some("mode=Hello&token=abc"));
        assert_eq!(
            result,
            "ws://127.0.0.1:9222/devtools/browser/abc?mode=Hello&token=abc"
        );
    }

    #[tokio::test]
    async fn discover_preserves_query_params() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            accept_http(
                &listener,
                &http_200(r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:1234/"}"#),
            )
            .await;
        });

        let ws_url = discover_cdp_url("127.0.0.1", port, Some("mode=Hello"))
            .await
            .unwrap();
        assert_eq!(ws_url, format!("ws://127.0.0.1:{}/?mode=Hello", port));
        server.await.unwrap();
    }
}
