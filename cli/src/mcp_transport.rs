//! Concurrent stdio transport and dual-era (`2026-07-28` + legacy `initialize`)
//! protocol layer for `agent-browser mcp`.

use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

pub(crate) const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
pub(crate) const LEGACY_DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";
pub(crate) const SUPPORTED_LEGACY_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub(crate) const SUPPORTED_ALL_PROTOCOL_VERSIONS: &[&str] = &[
    MODERN_PROTOCOL_VERSION,
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

pub(crate) const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub(crate) const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub(crate) const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";
pub(crate) const META_PROGRESS_TOKEN: &str = "progressToken";

pub(crate) const DISCOVER_TTL_MS: u64 = 3_600_000;
pub(crate) const LIST_TTL_MS: u64 = 300_000;
pub(crate) const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
pub(crate) const CANCELLED_ERROR_SENTINEL: &str = "__agent_browser_mcp_cancelled__";

/// JSON-RPC error returned by MCP request handlers.
#[derive(Debug, Clone)]
pub(crate) struct ProtocolError {
    pub(crate) code: i64,
    pub(crate) message: String,
    pub(crate) data: Option<Value>,
}

impl ProtocolError {
    pub(crate) fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("Method not found: {}", method),
            data: None,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn internal_error(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    pub(crate) fn unsupported_protocol_version(requested: &str) -> Self {
        Self {
            code: -32022,
            message: "Unsupported protocol version".to_string(),
            data: Some(json!({
                "supported": SUPPORTED_ALL_PROTOCOL_VERSIONS,
                "requested": requested,
            })),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn missing_required_client_capability(
        message: impl Into<String>,
        required_capabilities: Value,
    ) -> Self {
        Self {
            code: -32021,
            message: message.into(),
            data: Some(json!({
                "requiredCapabilities": required_capabilities,
            })),
        }
    }
}

/// Protocol era selected for an incoming request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtocolEra {
    /// Stateless per-request `_meta` era (`2026-07-28` and later).
    Modern,
    /// `initialize` handshake era (`2025-11-25` and earlier).
    Legacy,
}

/// Negotiated per-request metadata resolved by the transport layer.
#[derive(Debug, Clone)]
pub(crate) struct RequestContext {
    pub(crate) era: ProtocolEra,
    #[allow(dead_code)]
    pub(crate) protocol_version: String,
    #[allow(dead_code)]
    pub(crate) client_capabilities: Value,
    pub(crate) progress_token: Option<Value>,
    #[allow(dead_code)]
    pub(crate) request_id: Value,
}

impl RequestContext {
    #[allow(dead_code)]
    pub(crate) fn is_modern(&self) -> bool {
        self.era == ProtocolEra::Modern
    }

    #[allow(dead_code)]
    pub(crate) fn has_extension(&self, extension_id: &str) -> bool {
        self.client_capabilities
            .get("extensions")
            .and_then(Value::as_object)
            .and_then(|exts| exts.get(extension_id))
            .is_some_and(Value::is_object)
    }

    #[allow(dead_code)]
    pub(crate) fn supports_elicitation_form(&self) -> bool {
        let Some(elicit) = self
            .client_capabilities
            .get("elicitation")
            .and_then(Value::as_object)
        else {
            return false;
        };
        if elicit.is_empty() {
            return true;
        }
        elicit.get("form").is_some_and(Value::is_object)
    }

    #[allow(dead_code)]
    pub(crate) fn supports_elicitation_url(&self) -> bool {
        self.client_capabilities
            .get("elicitation")
            .and_then(Value::as_object)
            .and_then(|elicit| elicit.get("url"))
            .is_some_and(Value::is_object)
    }
}

/// Thread-safe newline-delimited JSON-RPC writer for stdio.
#[derive(Clone)]
pub(crate) struct StdioWriter {
    inner: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl StdioWriter {
    pub(crate) fn stdout() -> Self {
        Self::from_writer(io::stdout())
    }

    pub(crate) fn from_writer<W: Write + Send + 'static>(writer: W) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Box::new(writer))),
        }
    }

    pub(crate) fn send_json(&self, value: &Value) -> io::Result<()> {
        let line = serde_json::to_string(value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.write_all(line.as_bytes())?;
        guard.write_all(b"\n")?;
        guard.flush()
    }

    pub(crate) fn send_notification(&self, method: &str, params: Value) -> io::Result<()> {
        self.send_json(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    pub(crate) fn send_progress(
        &self,
        token: &Value,
        progress: u64,
        message: &str,
    ) -> io::Result<()> {
        self.send_notification(
            "notifications/progress",
            json!({
                "progressToken": token,
                "progress": progress,
                "message": message,
            }),
        )
    }
}

#[derive(Debug, Clone)]
struct LegacySessionState {
    protocol_version: String,
    client_capabilities: Value,
}

impl Default for LegacySessionState {
    fn default() -> Self {
        Self {
            protocol_version: LEGACY_DEFAULT_PROTOCOL_VERSION.to_string(),
            client_capabilities: json!({}),
        }
    }
}

/// Shared state across the stdio reader loop and worker threads.
pub(crate) struct McpServerState {
    pub(crate) writer: StdioWriter,
    session: Mutex<LegacySessionState>,
    in_flight: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pending_client_responses: Mutex<HashMap<String, mpsc::Sender<Value>>>,
    next_server_request_id: AtomicU64,
    active_workers: Mutex<usize>,
    workers_done: Condvar,
    shutdown: AtomicBool,
}

impl McpServerState {
    pub(crate) fn new(writer: StdioWriter) -> Self {
        Self {
            writer,
            session: Mutex::new(LegacySessionState::default()),
            in_flight: Mutex::new(HashMap::new()),
            pending_client_responses: Mutex::new(HashMap::new()),
            next_server_request_id: AtomicU64::new(1),
            active_workers: Mutex::new(0),
            workers_done: Condvar::new(),
            shutdown: AtomicBool::new(false),
        }
    }

    pub(crate) fn record_initialize(&self, protocol_version: &str, client_capabilities: Value) {
        let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        session.protocol_version = protocol_version.to_string();
        session.client_capabilities = if client_capabilities.is_object() {
            client_capabilities
        } else {
            json!({})
        };
    }

    pub(crate) fn resolve_request_context(
        &self,
        method: &str,
        id: &Value,
        params: Option<&Value>,
    ) -> Result<RequestContext, ProtocolError> {
        let meta = params
            .and_then(|p| p.get("_meta"))
            .and_then(Value::as_object);
        let progress_token = meta.and_then(|m| m.get(META_PROGRESS_TOKEN)).and_then(|t| {
            if t.is_string() || t.is_i64() || t.is_u64() {
                Some(t.clone())
            } else {
                None
            }
        });

        let has_modern_meta = meta.is_some_and(|m| {
            m.contains_key(META_PROTOCOL_VERSION) || m.contains_key(META_CLIENT_CAPABILITIES)
        });

        if method == "initialize" {
            let session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            return Ok(RequestContext {
                era: ProtocolEra::Legacy,
                protocol_version: session.protocol_version.clone(),
                client_capabilities: session.client_capabilities.clone(),
                progress_token,
                request_id: id.clone(),
            });
        }

        if method == "server/discover" || has_modern_meta {
            if id.is_null() {
                return Err(ProtocolError::invalid_request(
                    "Invalid request: request id must not be null",
                ));
            }
            let Some(meta) = meta else {
                return Err(ProtocolError::invalid_params(format!(
                    "{} requires _meta with {} and {}",
                    method, META_PROTOCOL_VERSION, META_CLIENT_CAPABILITIES
                )));
            };
            let Some(requested_version) = meta.get(META_PROTOCOL_VERSION).and_then(Value::as_str)
            else {
                return Err(ProtocolError::invalid_params(format!(
                    "Missing required _meta field: {}",
                    META_PROTOCOL_VERSION
                )));
            };
            if requested_version != MODERN_PROTOCOL_VERSION {
                return Err(ProtocolError::unsupported_protocol_version(
                    requested_version,
                ));
            }
            let Some(client_capabilities) =
                meta.get(META_CLIENT_CAPABILITIES).filter(|v| v.is_object())
            else {
                return Err(ProtocolError::invalid_params(format!(
                    "Missing required _meta field: {}",
                    META_CLIENT_CAPABILITIES
                )));
            };
            return Ok(RequestContext {
                era: ProtocolEra::Modern,
                protocol_version: requested_version.to_string(),
                client_capabilities: client_capabilities.clone(),
                progress_token,
                request_id: id.clone(),
            });
        }

        let session = self.session.lock().unwrap_or_else(|e| e.into_inner());
        Ok(RequestContext {
            era: ProtocolEra::Legacy,
            protocol_version: session.protocol_version.clone(),
            client_capabilities: session.client_capabilities.clone(),
            progress_token,
            request_id: id.clone(),
        })
    }

    pub(crate) fn register_in_flight(&self, id: &Value) -> Arc<AtomicBool> {
        let cancel = Arc::new(AtomicBool::new(false));
        self.register_in_flight_flag(id, cancel.clone());
        cancel
    }

    pub(crate) fn register_in_flight_flag(&self, id: &Value, cancel: Arc<AtomicBool>) {
        let key = id_key(id);
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, cancel);
    }

    pub(crate) fn unregister_in_flight(&self, id: &Value) {
        let key = id_key(id);
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }

    pub(crate) fn cancel_in_flight(&self, request_id: &Value) -> bool {
        let key = id_key(request_id);
        let guard = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flag) = guard.get(&key) {
            flag.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    #[allow(dead_code)]
    pub(crate) fn cancel_all_in_flight(&self) {
        let guard = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
        for flag in guard.values() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// Send a server-initiated JSON-RPC request and wait for the client's
    /// matching response (used by legacy `2025-11-25` / `2025-06-18` elicitation).
    #[allow(dead_code)]
    pub(crate) fn send_server_request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let seq = self.next_server_request_id.fetch_add(1, Ordering::SeqCst);
        let id = Value::String(format!("srv-{}", seq));
        let key = id_key(&id);
        let (tx, rx) = mpsc::channel();
        self.pending_client_responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), tx);

        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(err) = self.writer.send_json(&req) {
            self.pending_client_responses
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
            return Err(format!("Failed to write server request: {}", err));
        }

        let response = rx.recv_timeout(timeout);
        self.pending_client_responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        response.map_err(|_| format!("Timed out waiting for client response to {}", method))
    }

    /// Route an incoming JSON-RPC response (a message with `id` and no `method`)
    /// to a pending `send_server_request` waiter.
    pub(crate) fn deliver_client_response(&self, message: Value) -> bool {
        let Some(id) = message.get("id") else {
            return false;
        };
        let key = id_key(id);
        let sender = self
            .pending_client_responses
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
        if let Some(tx) = sender {
            let _ = tx.send(message);
            true
        } else {
            false
        }
    }

    pub(crate) fn worker_enter(&self) {
        let mut count = self
            .active_workers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *count += 1;
    }

    pub(crate) fn worker_exit(&self) {
        let mut count = self
            .active_workers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.workers_done.notify_all();
        }
    }

    pub(crate) fn wait_for_workers(&self, timeout: Duration) {
        let mut count = self
            .active_workers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let deadline = std::time::Instant::now() + timeout;
        while *count > 0 {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                break;
            };
            let (next_count, result) = self
                .workers_done
                .wait_timeout(count, remaining)
                .unwrap_or_else(|e| e.into_inner());
            count = next_count;
            if result.timed_out() {
                break;
            }
        }
    }

    pub(crate) fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub(crate) fn mark_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

/// Active cancellation and progress hooks bound to the current worker thread.
#[derive(Clone)]
pub(crate) struct ToolRunControl {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) progress_token: Option<Value>,
    pub(crate) writer: Option<StdioWriter>,
    pub(crate) tool_name: String,
}

thread_local! {
    static ACTIVE_TOOL_CONTROL: RefCell<Option<ToolRunControl>> = const { RefCell::new(None) };
}

/// RAII guard that binds a `ToolRunControl` for the current thread and restores
/// the previous control on drop.
pub(crate) struct ToolControlGuard {
    prev: Option<ToolRunControl>,
}

impl ToolControlGuard {
    pub(crate) fn enter(control: ToolRunControl) -> Self {
        let prev = ACTIVE_TOOL_CONTROL.with(|slot| slot.borrow_mut().replace(control));
        Self { prev }
    }
}

impl Drop for ToolControlGuard {
    fn drop(&mut self) {
        let prev = self.prev.take();
        ACTIVE_TOOL_CONTROL.with(|slot| {
            *slot.borrow_mut() = prev;
        });
    }
}

pub(crate) fn current_tool_control() -> Option<ToolRunControl> {
    ACTIVE_TOOL_CONTROL.with(|slot| slot.borrow().clone())
}

pub(crate) fn server_info_value() -> Value {
    json!({
        "name": "agent-browser",
        "title": "agent-browser",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Wrap a handler result with `2026-07-28` envelope fields (`resultType` and
/// `_meta["io.modelcontextprotocol/serverInfo"]`) when serving a modern request.
/// Legacy results are returned unchanged.
pub(crate) fn wrap_response_result(era: ProtocolEra, mut result: Value) -> Value {
    if era != ProtocolEra::Modern {
        return result;
    }
    if let Some(obj) = result.as_object_mut() {
        obj.entry("resultType".to_string())
            .or_insert_with(|| json!("complete"));
        let meta = obj.entry("_meta".to_string()).or_insert_with(|| json!({}));
        if let Some(meta_obj) = meta.as_object_mut() {
            meta_obj
                .entry(META_SERVER_INFO.to_string())
                .or_insert_with(server_info_value);
        }
    }
    result
}

pub(crate) fn error_response_from(id: Value, err: &ProtocolError) -> Value {
    let mut error = json!({
        "code": err.code,
        "message": err.message,
    });
    if let Some(data) = &err.data {
        error["data"] = data.clone();
    }
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": error,
    })
}

pub(crate) fn result_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

pub(crate) fn id_key(id: &Value) -> String {
    serde_json::to_string(id).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::thread;

    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn buffer_writer() -> (StdioWriter, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        (StdioWriter::from_writer(SharedBuffer(buf.clone())), buf)
    }

    #[test]
    fn resolve_context_detects_modern_discover_and_meta() {
        let (writer, _) = buffer_writer();
        let state = McpServerState::new(writer);
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {
                    "extensions": {
                        "io.modelcontextprotocol/tasks": {}
                    },
                    "elicitation": {
                        "form": {}
                    }
                },
                "progressToken": "tok-1"
            }
        });
        let ctx = state
            .resolve_request_context("server/discover", &json!("d1"), Some(&params))
            .unwrap();
        assert!(ctx.is_modern());
        assert_eq!(ctx.protocol_version, "2026-07-28");
        assert!(ctx.has_extension("io.modelcontextprotocol/tasks"));
        assert!(!ctx.has_extension("io.modelcontextprotocol/skills"));
        assert!(ctx.supports_elicitation_form());
        assert!(!ctx.supports_elicitation_url());
        assert_eq!(ctx.progress_token, Some(json!("tok-1")));
    }

    #[test]
    fn resolve_context_rejects_unsupported_modern_version_with_32022() {
        let (writer, _) = buffer_writer();
        let state = McpServerState::new(writer);
        let params = json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2099-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        });
        let err = state
            .resolve_request_context("server/discover", &json!(1), Some(&params))
            .unwrap_err();
        assert_eq!(err.code, -32022);
        let data = err.data.unwrap();
        assert_eq!(data["requested"], "2099-01-01");
        assert!(data["supported"]
            .as_array()
            .unwrap()
            .contains(&json!("2026-07-28")));
    }

    #[test]
    fn resolve_context_rejects_missing_modern_meta_and_null_id() {
        let (writer, _) = buffer_writer();
        let state = McpServerState::new(writer);

        let missing_meta = state
            .resolve_request_context("server/discover", &json!(1), None)
            .unwrap_err();
        assert_eq!(missing_meta.code, -32602);

        let missing_caps = state
            .resolve_request_context(
                "tools/list",
                &json!(2),
                Some(&json!({
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28"
                    }
                })),
            )
            .unwrap_err();
        assert_eq!(missing_caps.code, -32602);

        let null_id = state
            .resolve_request_context(
                "server/discover",
                &Value::Null,
                Some(&json!({
                    "_meta": {
                        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                        "io.modelcontextprotocol/clientCapabilities": {}
                    }
                })),
            )
            .unwrap_err();
        assert_eq!(null_id.code, -32600);
    }

    #[test]
    fn resolve_context_uses_legacy_session_state_after_initialize() {
        let (writer, _) = buffer_writer();
        let state = McpServerState::new(writer);
        state.record_initialize("2025-11-25", json!({ "elicitation": {} }));
        let ctx = state
            .resolve_request_context("tools/call", &json!(3), Some(&json!({"name": "x"})))
            .unwrap();
        assert!(!ctx.is_modern());
        assert_eq!(ctx.protocol_version, "2025-11-25");
        assert!(ctx.supports_elicitation_form());
        assert!(!ctx.supports_elicitation_url());
    }

    #[test]
    fn wrap_response_result_adds_modern_envelope_and_leaves_legacy_untouched() {
        let legacy = wrap_response_result(ProtocolEra::Legacy, json!({ "tools": [] }));
        assert!(legacy.get("resultType").is_none());
        assert!(legacy.get("_meta").is_none());

        let modern = wrap_response_result(ProtocolEra::Modern, json!({ "tools": [] }));
        assert_eq!(modern["resultType"], "complete");
        assert_eq!(
            modern["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "agent-browser"
        );

        let custom = wrap_response_result(
            ProtocolEra::Modern,
            json!({ "resultType": "task", "taskId": "t1" }),
        );
        assert_eq!(custom["resultType"], "task");
    }

    #[test]
    fn server_request_correlates_client_response_across_threads() {
        let (writer, buf) = buffer_writer();
        let state = Arc::new(McpServerState::new(writer));
        let sender_state = Arc::clone(&state);

        let handle = thread::spawn(move || {
            sender_state.send_server_request(
                "elicitation/create",
                json!({ "mode": "form", "message": "Confirm?" }),
                Duration::from_secs(2),
            )
        });

        let request_id = loop {
            let bytes = buf.lock().unwrap().clone();
            if !bytes.is_empty() {
                let line = String::from_utf8(bytes).unwrap();
                let parsed: Value = serde_json::from_str(line.trim()).unwrap();
                assert_eq!(parsed["method"], "elicitation/create");
                break parsed["id"].clone();
            }
            thread::sleep(Duration::from_millis(5));
        };

        assert!(state.deliver_client_response(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": { "action": "accept", "content": { "approve": true } }
        })));

        let reply = handle.join().unwrap().unwrap();
        assert_eq!(reply["result"]["action"], "accept");
    }

    #[test]
    fn stdio_writer_serializes_concurrent_writes_as_valid_lines() {
        let (writer, buf) = buffer_writer();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let writer = writer.clone();
                thread::spawn(move || {
                    for j in 0..25 {
                        writer.send_json(&json!({ "thread": i, "seq": j })).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let bytes = buf.lock().unwrap().clone();
        let mut count = 0;
        for line in std::io::BufRead::lines(Cursor::new(bytes)) {
            let parsed: Value = serde_json::from_str(&line.unwrap()).unwrap();
            assert!(parsed["thread"].is_number());
            assert!(parsed["seq"].is_number());
            count += 1;
        }
        assert_eq!(count, 200);
    }
}
