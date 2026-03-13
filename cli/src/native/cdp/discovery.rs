use std::time::Duration;

use super::types::BrowserVersionInfo;

pub async fn discover_cdp_url(port: u16) -> Result<String, String> {
    discover_cdp_url_with_request_timeout(port, Duration::from_secs(2)).await
}

pub async fn discover_cdp_url_with_request_timeout(
    port: u16,
    request_timeout: Duration,
) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}/json/version", port);

    let body = tokio::time::timeout(request_timeout, async { reqwest_get_string(&url).await })
        .await
        .map_err(|_| format!("Timeout connecting to CDP on port {}", port))?
        .map_err(|e| format!("Failed to connect to CDP on port {}: {}", port, e))?;

    let info: BrowserVersionInfo = serde_json::from_str(&body)
        .map_err(|e| format!("Invalid /json/version response: {}", e))?;

    info.web_socket_debugger_url
        .ok_or_else(|| format!("No webSocketDebuggerUrl in /json/version on port {}", port))
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

        let ws_url = discover_cdp_url(port).await.unwrap();
        assert_eq!(ws_url, "ws://127.0.0.1:1234/");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn invalid_json_returns_parse_error() {
        let (port, server) = spawn_json_server("not-json").await;

        let err = discover_cdp_url(port).await.unwrap_err();
        assert!(err.contains("Invalid /json/version response"));
        server.await.unwrap();
    }
}
