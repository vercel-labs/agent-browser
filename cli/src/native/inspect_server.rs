use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

use super::cdp::client::InspectProxyHandle;

/// Counter for unique attach IDs so concurrent connections don't collide.
static ATTACH_ID: AtomicI64 = AtomicI64::new(-1000);

/// Lightweight HTTP + WebSocket server for `agent-browser inspect`.
///
/// Serves two purposes:
/// - `GET /` redirects to Chrome's built-in DevTools frontend with `ws=` pointing to this server
/// - WebSocket connections create a dedicated CDP session via `Target.attachToTarget` and proxy
///   CDP messages through the daemon's existing browser-level connection, injecting/stripping
///   `sessionId` so the DevTools frontend sees a page-level view
pub struct InspectServer {
    port: u16,
    _handle: tokio::task::JoinHandle<()>,
}

impl InspectServer {
    /// Start the inspect proxy server.
    ///
    /// - `proxy_handle`: lightweight handle for sending/receiving raw CDP messages
    /// - `target_id`: the CDP target ID of the page to inspect
    /// - `chrome_host_port`: the Chrome debug server address (e.g. "127.0.0.1:9222")
    pub async fn start(
        proxy_handle: InspectProxyHandle,
        target_id: String,
        chrome_host_port: String,
    ) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("Failed to bind inspect server: {}", e))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("Failed to get local addr: {}", e))?
            .port();

        let proxy = Arc::new(proxy_handle);

        let handle = tokio::spawn(accept_loop(
            listener,
            proxy,
            target_id,
            chrome_host_port,
            port,
        ));

        Ok(Self {
            port,
            _handle: handle,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn shutdown(self) {
        self._handle.abort();
    }
}

async fn accept_loop(
    listener: TcpListener,
    proxy: Arc<InspectProxyHandle>,
    target_id: String,
    chrome_host_port: String,
    proxy_port: u16,
) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let proxy = proxy.clone();
        let tid = target_id.clone();
        let chp = chrome_host_port.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, proxy, tid, chp, proxy_port).await {
                eprintln!("[inspect] connection error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    proxy: Arc<InspectProxyHandle>,
    target_id: String,
    chrome_host_port: String,
    proxy_port: u16,
) -> Result<(), String> {
    // Peek at the request line to determine routing WITHOUT consuming bytes.
    // This is critical: tokio_tungstenite::accept_async needs to read the full
    // HTTP upgrade request itself, so we must not consume anything for WS paths.
    let mut peek_buf = [0u8; 32];
    let n = stream
        .peek(&mut peek_buf)
        .await
        .map_err(|e| e.to_string())?;
    let peek = String::from_utf8_lossy(&peek_buf[..n]);

    if peek.starts_with("GET /ws") {
        return handle_ws_proxy(stream, proxy, target_id).await;
    }

    if peek.starts_with("GET / ") {
        let buf_reader = BufReader::new(stream);
        return handle_http_redirect(buf_reader, chrome_host_port, proxy_port).await;
    }

    // Unknown request -- consume and respond 404
    let mut stream = stream;
    let mut discard = [0u8; 4096];
    let _ = stream.read(&mut discard).await;
    let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    stream
        .write_all(resp.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

const MAX_HEADER_BYTES: usize = 8192;

async fn handle_http_redirect(
    buf_reader: BufReader<tokio::net::TcpStream>,
    chrome_host_port: String,
    proxy_port: u16,
) -> Result<(), String> {
    let mut br = buf_reader;
    let mut total_bytes = 0usize;
    loop {
        let mut line = String::new();
        let n = br.read_line(&mut line).await.map_err(|e| e.to_string())?;
        total_bytes += n;
        if line == "\r\n" || line == "\n" || line.is_empty() || total_bytes > MAX_HEADER_BYTES {
            break;
        }
    }

    let location = format!(
        "http://{}/devtools/devtools_app.html?ws=127.0.0.1:{}/ws",
        chrome_host_port, proxy_port
    );
    let body = format!(
        "<html><body>Redirecting to <a href=\"{url}\">{url}</a></body></html>",
        url = location
    );
    let resp = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        location,
        body.len(),
        body
    );
    let mut stream = br.into_inner();
    stream
        .write_all(resp.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

async fn handle_ws_proxy(
    stream: tokio::net::TcpStream,
    proxy: Arc<InspectProxyHandle>,
    target_id: String,
) -> Result<(), String> {
    let ws_stream = tokio_tungstenite::accept_async(stream)
        .await
        .map_err(|e| format!("WebSocket handshake failed: {}", e))?;

    // Create a dedicated CDP session for this DevTools connection.
    // Each connection gets its own session so domain enablements (DOM.enable, etc.)
    // always trigger fresh initial state dumps from Chrome.
    let attach_id = ATTACH_ID.fetch_sub(1, Ordering::SeqCst);
    let attach_cmd = format!(
        r#"{{"id":{},"method":"Target.attachToTarget","params":{{"targetId":"{}","flatten":true}}}}"#,
        attach_id, target_id
    );

    // Subscribe BEFORE sending so we don't miss the response (tokio broadcast
    // receivers only deliver messages to receivers that already exist).
    let mut raw_rx = proxy.subscribe_raw();

    proxy
        .send_raw(attach_cmd)
        .await
        .map_err(|e| format!("Failed to send attachToTarget: {}", e))?;

    // Wait for the attachToTarget response to extract the session ID
    let session_id = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while let Ok(raw_msg) = raw_rx.recv().await {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw_msg.text) {
                if val.get("id").and_then(|v| v.as_i64()) == Some(attach_id) {
                    if let Some(sid) = val
                        .get("result")
                        .and_then(|r| r.get("sessionId"))
                        .and_then(|s| s.as_str())
                    {
                        return Ok(sid.to_string());
                    }
                    return Err("attachToTarget failed".to_string());
                }
            }
        }
        Err("raw message channel closed".to_string())
    })
    .await
    .map_err(|_| "Timed out waiting for attachToTarget response".to_string())?
    .map_err(|e| format!("Failed to create DevTools session: {}", e))?;

    let (ws_tx, mut ws_rx) = ws_stream.split();
    let ws_tx = Arc::new(Mutex::new(ws_tx));

    let mut raw_rx = proxy.subscribe_raw();
    let ws_tx_clone = ws_tx.clone();
    let session_id_clone = session_id.clone();

    // Chrome -> DevTools: forward messages matching our session, strip sessionId
    let mut chrome_to_devtools = tokio::spawn(async move {
        loop {
            let raw_msg = match raw_rx.recv().await {
                Ok(msg) => msg,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!(
                        "[inspect] warning: dropped {} CDP messages (channel lag)",
                        n
                    );
                    continue;
                }
                Err(_) => break,
            };

            if raw_msg.session_id.as_deref() != Some(&session_id_clone) {
                continue;
            }

            let stripped = strip_session_id(&raw_msg.text);

            let mut tx = ws_tx_clone.lock().await;
            if tx.send(Message::Text(stripped)).await.is_err() {
                break;
            }
        }
    });

    // DevTools -> Chrome: inject sessionId and forward
    let proxy_for_send = proxy.clone();
    let session_id_for_send = session_id.clone();
    let mut devtools_to_chrome = tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => break,
                _ => continue,
            };

            let injected = inject_session_id(&text, &session_id_for_send);
            if proxy_for_send.send_raw(injected).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = &mut chrome_to_devtools => {
            devtools_to_chrome.abort();
        },
        _ = &mut devtools_to_chrome => {
            chrome_to_devtools.abort();
        },
    }

    // Clean up the CDP session so Chrome doesn't leak attached targets
    let detach_cmd = format!(
        r#"{{"id":{},"method":"Target.detachFromTarget","params":{{"sessionId":"{}"}}}}"#,
        ATTACH_ID.fetch_sub(1, Ordering::SeqCst),
        session_id
    );
    let _ = proxy.send_raw(detach_cmd).await;

    Ok(())
}

fn inject_session_id(json: &str, session_id: &str) -> String {
    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(json) {
        if let Some(obj) = val.as_object_mut() {
            obj.insert(
                "sessionId".to_string(),
                serde_json::Value::String(session_id.to_string()),
            );
        }
        serde_json::to_string(&val).unwrap_or_else(|_| json.to_string())
    } else {
        json.to_string()
    }
}

fn strip_session_id(json: &str) -> String {
    if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(json) {
        if let Some(obj) = val.as_object_mut() {
            obj.remove("sessionId");
        }
        serde_json::to_string(&val).unwrap_or_else(|_| json.to_string())
    } else {
        json.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_session_id() {
        let input = r#"{"id":1,"method":"DOM.getDocument"}"#;
        let result = inject_session_id(input, "abc123");
        let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert_eq!(parsed["sessionId"], "abc123");
        assert_eq!(parsed["method"], "DOM.getDocument");
        assert_eq!(parsed["id"], 1);
    }

    #[test]
    fn test_inject_session_id_empty_object() {
        let result = inject_session_id("{}", "abc");
        let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert_eq!(parsed["sessionId"], "abc");
    }

    #[test]
    fn test_strip_session_id() {
        let input = r#"{"id":1,"result":{},"sessionId":"abc123"}"#;
        let result = strip_session_id(input);
        let parsed: serde_json::Value = serde_json::from_str(&result).expect("valid JSON");
        assert!(parsed.get("sessionId").is_none());
        assert_eq!(parsed["id"], 1);
    }

    #[test]
    fn test_inject_then_strip_roundtrip() {
        let input = r#"{"id":42,"method":"Runtime.evaluate"}"#;
        let injected = inject_session_id(input, "sess1");
        let stripped = strip_session_id(&injected);
        let original: serde_json::Value = serde_json::from_str(input).unwrap();
        let result: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(original, result);
    }
}
