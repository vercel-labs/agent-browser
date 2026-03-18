//! Tauri MCP backend — connects to a `tauri-plugin-agent-test` MCP server
//! running inside a Tauri desktop app and translates agent-browser commands
//! into MCP tool calls over HTTP+SSE.
//!
//! This backend enables AI agents to test Tauri apps on macOS WebKit (WKWebView)
//! which does not support CDP. The Tauri app must have the
//! `tauri-plugin-agent-test` plugin loaded (see https://github.com/coreyepstein/tauri-agent-browser).
//!
//! ## Transport
//!
//! 1. `GET /sse` → long-lived SSE stream; server sends `endpoint` event with session URL
//! 2. `POST /message?sessionId=<uuid>` → JSON-RPC 2.0 request body
//! 3. Server sends JSON-RPC response on the SSE stream as a `message` event

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use super::webdriver::backend::BrowserBackend;

// ---------------------------------------------------------------------------
// MCP protocol types (minimal set needed for the provider)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: &'static str,
    id: u64,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct CallToolResult {
    content: Vec<CallToolContent>,
    #[serde(rename = "isError")]
    is_error: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct CallToolContent {
    text: String,
}

// ---------------------------------------------------------------------------
// SSE event parser
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SseEvent {
    event_type: String,
    data: String,
}

fn parse_sse_block(block: &str) -> Option<SseEvent> {
    let mut event_type = "message".to_string();
    let mut data_lines: Vec<&str> = Vec::new();

    for line in block.lines() {
        if line.starts_with(':') {
            // SSE comment — ignore
        } else if let Some(rest) = line.strip_prefix("event:") {
            event_type = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim());
        }
    }

    if data_lines.is_empty() {
        return None;
    }

    Some(SseEvent {
        event_type,
        data: data_lines.join("\n"),
    })
}

// ---------------------------------------------------------------------------
// Tauri MCP backend
// ---------------------------------------------------------------------------

struct Session {
    post_url: String,
    event_rx: mpsc::UnboundedReceiver<SseEvent>,
}

/// Backend that connects to a Tauri app's MCP server for AI-driven UI testing.
pub struct TauriBackend {
    host: String,
    port: u16,
    client: reqwest::Client,
    session: Arc<Mutex<Option<Session>>>,
    next_id: Arc<AtomicU64>,
}

/// Actions not supported by the Tauri MCP backend.
pub const TAURI_UNSUPPORTED_ACTIONS: &[&str] = &[
    "screencast_start",
    "screencast_stop",
    "trace_start",
    "trace_stop",
    "profiler_start",
    "profiler_stop",
    "route",
    "unroute",
    "expose",
    "addscript",
    "addinitscript",
    "network",
    "har_start",
    "har_stop",
    "dblclick",
    "hover",
    "scroll",
    "select",
    "check",
    "uncheck",
    "wait",
    "type",
    "press",
    "evaluate",
    "gettext",
    "getattribute",
    "isvisible",
    "isenabled",
    "ischecked",
    "cookies",
    "storage",
];

impl TauriBackend {
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_string(),
            port,
            client: reqwest::Client::new(),
            session: Arc::new(Mutex::new(None)),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// Connect to the Tauri MCP server's SSE endpoint and initialize the session.
    pub async fn connect(&self) -> Result<(), String> {
        let sse_url = format!("{}/sse", self.base_url());

        let response = self
            .client
            .get(&sse_url)
            .send()
            .await
            .map_err(|e| {
                if e.is_connect() {
                    format!(
                        "Cannot connect to Tauri MCP server at {}:{}. \
                         Make sure the Tauri app is running with the \
                         tauri-plugin-agent-test plugin loaded.",
                        self.host, self.port
                    )
                } else {
                    format!("HTTP error connecting to Tauri MCP server: {e}")
                }
            })?;

        let (tx, mut rx) = mpsc::unbounded_channel::<SseEvent>();

        // Background task: stream SSE bytes → parsed events
        let mut byte_stream = response.bytes_stream();
        tokio::spawn(async move {
            let mut buf = String::new();
            while let Some(chunk) = byte_stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        if let Ok(text) = std::str::from_utf8(&bytes) {
                            buf.push_str(text);
                            while let Some(pos) = buf.find("\n\n") {
                                let block = buf[..pos].to_string();
                                buf = buf[pos + 2..].to_string();
                                if let Some(event) = parse_sse_block(&block) {
                                    let _ = tx.send(event);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Wait for the `endpoint` event
        let endpoint_path = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Some(ev) if ev.event_type == "endpoint" => return Ok(ev.data),
                    Some(_) => continue,
                    None => {
                        return Err("SSE stream closed before endpoint event".to_string());
                    }
                }
            }
        })
        .await
        .map_err(|_| {
            format!(
                "Timeout waiting for endpoint event from {}:{}",
                self.host, self.port
            )
        })??;

        let post_url = format!("{}{}", self.base_url(), endpoint_path);

        *self.session.lock().await = Some(Session {
            post_url,
            event_rx: rx,
        });

        // Send MCP initialize
        self.send_rpc("initialize", Some(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {
                "name": "agent-browser",
                "version": env!("CARGO_PKG_VERSION")
            }
        })))
        .await?;

        Ok(())
    }

    async fn send_rpc(&self, method: &str, params: Option<Value>) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method: method.to_string(),
            params,
        };

        // Lock scope 1: get the POST URL and send the request.
        let post_url = {
            let session = self.session.lock().await;
            let sess = session
                .as_ref()
                .ok_or("Not connected — call connect() first")?;
            sess.post_url.clone()
        };

        self.client
            .post(&post_url)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        // Lock scope 2: read the SSE stream for the matching response.
        let response_value = {
            let mut session = self.session.lock().await;
            let sess = session
                .as_mut()
                .ok_or("Not connected — session dropped")?;

            tokio::time::timeout(
                std::time::Duration::from_secs(30),
                async {
                    let expected_id = serde_json::json!(id);
                    loop {
                        match sess.event_rx.recv().await {
                            Some(ev) if ev.event_type == "message" => {
                                if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                    if v.get("id") == Some(&expected_id) {
                                        return Ok(v);
                                    }
                                }
                            }
                            Some(_) => continue,
                            None => {
                                return Err(
                                    "SSE stream closed while waiting for response".to_string(),
                                );
                            }
                        }
                    }
                },
            )
            .await
            .map_err(|_| format!("Timeout waiting for response to '{method}'"))??
        };

        Ok(response_value)
    }

    async fn call_tool(&self, name: &str, arguments: Value) -> Result<CallToolResult, String> {
        let response = self
            .send_rpc(
                "tools/call",
                Some(serde_json::json!({ "name": name, "arguments": arguments })),
            )
            .await?;

        if let Some(error) = response.get("error") {
            return Err(format!(
                "MCP error: {}",
                error
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown")
            ));
        }

        let result = response
            .get("result")
            .ok_or("No result in tools/call response")?;

        serde_json::from_value::<CallToolResult>(result.clone())
            .map_err(|e| format!("Failed to parse CallToolResult: {e}"))
    }

    /// Call a tool and return the text content, or an error.
    async fn call_tool_text(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self.call_tool(name, arguments).await?;
        if result.is_error {
            return Err(result
                .content
                .first()
                .map(|c| c.text.clone())
                .unwrap_or_else(|| "Unknown MCP error".to_string()));
        }
        Ok(result
            .content
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default())
    }
}

#[async_trait]
impl BrowserBackend for TauriBackend {
    async fn navigate(&self, url: &str) -> Result<(), String> {
        self.call_tool_text("navigate", serde_json::json!({ "url": url }))
            .await?;
        Ok(())
    }

    async fn get_url(&self) -> Result<String, String> {
        // MCP plugin doesn't have a dedicated url tool; return empty for now
        Ok(String::new())
    }

    async fn get_title(&self) -> Result<String, String> {
        // MCP plugin doesn't have a dedicated title tool
        Ok(String::new())
    }

    async fn get_content(&self) -> Result<String, String> {
        // Snapshot returns the DOM tree — use that as "content"
        self.call_tool_text("snapshot", serde_json::json!({})).await
    }

    async fn evaluate(&self, _script: &str) -> Result<Value, String> {
        Err("evaluate is not supported on the Tauri MCP backend".to_string())
    }

    async fn screenshot(&self) -> Result<String, String> {
        self.call_tool_text("screenshot", serde_json::json!({}))
            .await
    }

    async fn click(&self, selector: &str) -> Result<(), String> {
        // In Tauri MCP, "click" takes a @ref, not a CSS selector.
        // The ref is passed as the selector by the upstream dispatch.
        self.call_tool_text("click", serde_json::json!({ "ref": selector }))
            .await?;
        Ok(())
    }

    async fn fill(&self, selector: &str, value: &str) -> Result<(), String> {
        self.call_tool_text(
            "fill",
            serde_json::json!({ "ref": selector, "value": value }),
        )
        .await?;
        Ok(())
    }

    async fn close(&mut self) -> Result<(), String> {
        // Try to send close tool call, ignore errors (server may already be down)
        let _ = self.call_tool_text("close", serde_json::json!({})).await;
        *self.session.lock().await = None;
        Ok(())
    }

    async fn back(&self) -> Result<(), String> {
        Err("back is not supported on the Tauri MCP backend".to_string())
    }

    async fn forward(&self) -> Result<(), String> {
        Err("forward is not supported on the Tauri MCP backend".to_string())
    }

    async fn reload(&self) -> Result<(), String> {
        Err("reload is not supported on the Tauri MCP backend".to_string())
    }

    async fn get_cookies(&self) -> Result<Value, String> {
        Err("get_cookies is not supported on the Tauri MCP backend".to_string())
    }

    fn backend_type(&self) -> &str {
        "tauri"
    }
}

// ---------------------------------------------------------------------------
// Snapshot helper — returns the snapshot text for actions.rs
// ---------------------------------------------------------------------------

impl TauriBackend {
    /// Take a snapshot of the Tauri webview DOM.
    pub async fn snapshot(&self, interactive: bool) -> Result<String, String> {
        self.call_tool_text(
            "snapshot",
            serde_json::json!({ "interactive_only": interactive }),
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sse_block_endpoint() {
        let block = "event: endpoint\ndata: /message?sessionId=abc-123";
        let event = parse_sse_block(block).unwrap();
        assert_eq!(event.event_type, "endpoint");
        assert_eq!(event.data, "/message?sessionId=abc-123");
    }

    #[test]
    fn test_parse_sse_block_message() {
        let block = "event: message\ndata: {\"id\":1,\"result\":{}}";
        let event = parse_sse_block(block).unwrap();
        assert_eq!(event.event_type, "message");
        assert!(event.data.contains("\"id\":1"));
    }

    #[test]
    fn test_parse_sse_block_comment() {
        let block = ": keep-alive";
        assert!(parse_sse_block(block).is_none());
    }

    #[test]
    fn test_parse_sse_block_default_event_type() {
        let block = "data: hello";
        let event = parse_sse_block(block).unwrap();
        assert_eq!(event.event_type, "message");
        assert_eq!(event.data, "hello");
    }

    #[test]
    fn test_tauri_backend_type() {
        let backend = TauriBackend::new("127.0.0.1", 9876);
        assert_eq!(backend.backend_type(), "tauri");
    }

    #[test]
    fn test_tauri_unsupported_actions_includes_screencast() {
        assert!(TAURI_UNSUPPORTED_ACTIONS.contains(&"screencast_start"));
        assert!(TAURI_UNSUPPORTED_ACTIONS.contains(&"har_start"));
    }

    #[test]
    fn test_tauri_unsupported_actions_excludes_core() {
        // Core actions should NOT be in the unsupported list
        assert!(!TAURI_UNSUPPORTED_ACTIONS.contains(&"snapshot"));
        assert!(!TAURI_UNSUPPORTED_ACTIONS.contains(&"click"));
        assert!(!TAURI_UNSUPPORTED_ACTIONS.contains(&"fill"));
        assert!(!TAURI_UNSUPPORTED_ACTIONS.contains(&"screenshot"));
        assert!(!TAURI_UNSUPPORTED_ACTIONS.contains(&"navigate"));
        assert!(!TAURI_UNSUPPORTED_ACTIONS.contains(&"close"));
    }

    #[test]
    fn test_base_url() {
        let backend = TauriBackend::new("127.0.0.1", 9876);
        assert_eq!(backend.base_url(), "http://127.0.0.1:9876");
    }

    #[test]
    fn test_base_url_custom_port() {
        let backend = TauriBackend::new("localhost", 3000);
        assert_eq!(backend.base_url(), "http://localhost:3000");
    }
}
