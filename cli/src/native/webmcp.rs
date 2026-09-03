use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TOOL_RECORD_BYTES: usize = 256 * 1024;
pub const MAX_TOOL_LIST_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TOOL_COUNT: usize = 512;
pub const MAX_INVOCATION_HISTORY: usize = 128;
pub const MAX_EARLY_RESPONSES: usize = 128;
const MAX_ERROR_BYTES: usize = 64 * 1024;

pub const ERR_UNSUPPORTED: &str = "webmcp_unsupported";
pub const ERR_TOOL_NOT_FOUND: &str = "webmcp_tool_not_found";
pub const ERR_AMBIGUOUS_TOOL: &str = "webmcp_ambiguous_tool";
pub const ERR_INVALID_INPUT: &str = "webmcp_invalid_input";
pub const ERR_INVALID_TOOL: &str = "webmcp_invalid_tool";
pub const ERR_INVOCATION_NOT_FOUND: &str = "webmcp_invocation_not_found";
pub const ERR_INVOCATION_NOT_ACTIVE: &str = "webmcp_invocation_not_active";
pub const ERR_INVOKE_FAILED: &str = "webmcp_invoke_failed";
pub const ERR_CANCEL_FAILED: &str = "webmcp_cancel_failed";
pub const ERR_CONTEXT_CHANGED: &str = "webmcp_context_changed";
pub const ERR_TOO_MANY_INVOCATIONS: &str = "webmcp_too_many_invocations";
pub const ERR_OUTPUT_TOO_LARGE: &str = "webmcp_output_too_large";

fn is_tool_bound_error(error: &str) -> bool {
    error.starts_with(&format!(
        "{}: Current page exposes more than ",
        ERR_OUTPUT_TOO_LARGE
    ))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRecord {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: Value,
    pub origin: String,
    pub frame_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend_node_id: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct InvocationRecord {
    pub invocation_id: String,
    pub tool_name: String,
    pub frame_id: String,
    pub origin: String,
    pub status: String,
    pub raw_status: Option<String>,
    pub result: Option<Value>,
    pub output_truncated: bool,
    pub original_output_bytes: Option<usize>,
    pub error: Option<String>,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
}

impl InvocationRecord {
    pub fn pending(
        invocation_id: String,
        tool_name: String,
        frame_id: String,
        origin: String,
    ) -> Self {
        Self {
            invocation_id,
            tool_name,
            frame_id,
            origin,
            status: "pending".to_string(),
            raw_status: None,
            result: None,
            output_truncated: false,
            original_output_bytes: None,
            error: None,
            started_at: Instant::now(),
            finished_at: None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.status != "pending"
    }

    fn apply_completion(&mut self, completion: InvocationCompletion) {
        self.raw_status = Some(completion.raw_status);
        self.status = completion.status;
        self.result = completion.result;
        self.output_truncated = completion.output_truncated;
        self.original_output_bytes = completion.original_output_bytes;
        self.error = completion.error;
        self.finished_at = Some(Instant::now());
    }

    pub fn mark_timed_out(&mut self) {
        self.status = "timed_out".to_string();
        self.error = Some("WebMCP invocation timed out".to_string());
        self.finished_at = Some(Instant::now());
    }

    pub fn mark_context_changed(&mut self) {
        self.status = "failed".to_string();
        self.error = Some(format!(
            "{}: The page context changed before the invocation completed",
            ERR_CONTEXT_CHANGED
        ));
        self.finished_at = Some(Instant::now());
    }

    pub fn to_json(&self) -> Value {
        let duration = self
            .finished_at
            .unwrap_or_else(Instant::now)
            .duration_since(self.started_at);
        let mut value = json!({
            "invocationId": self.invocation_id,
            "toolName": self.tool_name,
            "frameId": self.frame_id,
            "origin": self.origin,
            "status": self.status,
            "durationMs": duration.as_millis() as u64,
        });
        if let Some(raw_status) = &self.raw_status {
            value["rawStatus"] = json!(raw_status);
        }
        if let Some(result) = &self.result {
            value["output"] = result.clone();
        }
        if self.output_truncated {
            value["outputTruncated"] = json!(true);
        }
        if let Some(original_output_bytes) = self.original_output_bytes {
            value["originalOutputBytes"] = json!(original_output_bytes);
        }
        if let Some(error) = &self.error {
            value["error"] = json!(error);
        }
        value
    }
}

#[derive(Clone, Debug)]
struct InvocationCompletion {
    raw_status: String,
    status: String,
    result: Option<Value>,
    output_truncated: bool,
    original_output_bytes: Option<usize>,
    error: Option<String>,
}

impl InvocationCompletion {
    fn from_event(params: &Value) -> Self {
        let raw_status = params
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("Error")
            .to_string();
        let status = match raw_status.as_str() {
            "Completed" => "completed",
            "Canceled" => "canceled",
            _ => "failed",
        }
        .to_string();
        let (result, output_truncated, original_output_bytes) =
            if let Some(output) = params.get("output") {
                let (bounded, truncated, original_bytes) = bounded_value(output, MAX_OUTPUT_BYTES);
                (
                    Some(bounded),
                    truncated,
                    truncated.then_some(original_bytes),
                )
            } else {
                (None, false, None)
            };
        let error = params
            .get("errorText")
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| params.get("exception").map(Value::to_string))
            .map(|error| bounded_string(&error, MAX_ERROR_BYTES));
        Self {
            raw_status,
            status,
            result,
            output_truncated,
            original_output_bytes,
            error,
        }
    }
}

#[derive(Default)]
pub struct RuntimeState {
    pub invocations: HashMap<String, InvocationRecord>,
    order: VecDeque<String>,
    early_responses: HashMap<String, InvocationCompletion>,
    early_order: VecDeque<String>,
    tools: HashMap<String, HashMap<(String, String), ToolRecord>>,
    frame_origins: HashMap<String, HashMap<String, String>>,
    tool_errors: HashMap<String, String>,
}

impl RuntimeState {
    pub fn ensure_capacity(&mut self) -> Result<(), String> {
        while self.invocations.len() >= MAX_INVOCATION_HISTORY {
            let terminal = self.order.iter().find_map(|id| {
                self.invocations
                    .get(id)
                    .is_some_and(InvocationRecord::is_terminal)
                    .then(|| id.clone())
            });
            let Some(id) = terminal else {
                return Err(format!(
                    "{}: {} WebMCP invocations are still active; retrieve, cancel, or wait for one before starting another",
                    ERR_TOO_MANY_INVOCATIONS, MAX_INVOCATION_HISTORY
                ));
            };
            self.order.retain(|existing| existing != &id);
            self.invocations.remove(&id);
        }
        Ok(())
    }

    pub fn insert(&mut self, mut record: InvocationRecord) -> Result<(), String> {
        self.ensure_capacity()?;
        if let Some(response) = self.early_responses.remove(&record.invocation_id) {
            self.early_order
                .retain(|existing| existing != &record.invocation_id);
            record.apply_completion(response);
        }
        let id = record.invocation_id.clone();
        self.invocations.insert(id.clone(), record);
        self.order.retain(|existing| existing != &id);
        self.order.push_back(id);
        Ok(())
    }

    pub fn apply_response(&mut self, params: Value) {
        let Some(id) = params.get("invocationId").and_then(Value::as_str) else {
            return;
        };
        let id = id.to_string();
        let completion = InvocationCompletion::from_event(&params);
        if let Some(record) = self.invocations.get_mut(&id) {
            if !record.is_terminal() {
                record.apply_completion(completion);
            }
        } else {
            if !self.early_responses.contains_key(&id) {
                while self.early_responses.len() >= MAX_EARLY_RESPONSES {
                    let Some(oldest) = self.early_order.pop_front() else {
                        break;
                    };
                    self.early_responses.remove(&oldest);
                }
                self.early_order.push_back(id.clone());
            }
            self.early_responses.insert(id, completion);
        }
    }

    pub fn update_frame_origin(&mut self, session_id: &str, frame_id: &str, origin: &str) {
        self.frame_origins
            .entry(session_id.to_string())
            .or_default()
            .insert(frame_id.to_string(), origin.to_string());
        for tool in self
            .tools
            .entry(session_id.to_string())
            .or_default()
            .values_mut()
        {
            if tool.frame_id == frame_id {
                tool.origin = origin.to_string();
            }
        }
    }

    pub fn apply_tools_added(&mut self, session_id: &str, params: &Value, fallback_origin: &str) {
        let records = params
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for raw in records {
            let origin = raw
                .get("frameId")
                .and_then(Value::as_str)
                .and_then(|frame_id| {
                    self.frame_origins
                        .get(session_id)
                        .and_then(|origins| origins.get(frame_id))
                })
                .map(String::as_str)
                .unwrap_or(fallback_origin);
            let tool = match normalize_tool(&raw, origin) {
                Ok(tool) => tool,
                Err(error) => {
                    self.tool_errors.insert(session_id.to_string(), error);
                    continue;
                }
            };
            self.tools
                .entry(session_id.to_string())
                .or_default()
                .insert((tool.frame_id.clone(), tool.name.clone()), tool);
        }
        self.validate_tool_bounds(session_id);
    }

    pub fn apply_tools_removed(&mut self, session_id: &str, params: &Value) {
        for raw in params
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let (Some(frame_id), Some(name)) = (
                raw.get("frameId").and_then(Value::as_str),
                raw.get("name").and_then(Value::as_str),
            ) {
                if let Some(tools) = self.tools.get_mut(session_id) {
                    tools.remove(&(frame_id.to_string(), name.to_string()));
                }
            }
        }
        self.validate_tool_bounds(session_id);
    }

    pub fn tools(&self, session_id: &str) -> Result<Vec<ToolRecord>, String> {
        if let Some(error) = self.tool_errors.get(session_id) {
            return Err(error.clone());
        }
        let mut tools = self
            .tools
            .get(session_id)
            .into_iter()
            .flat_map(HashMap::values)
            .cloned()
            .collect::<Vec<_>>();
        tools.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.frame_id.cmp(&right.frame_id))
        });
        Ok(tools)
    }

    fn validate_tool_bounds(&mut self, session_id: &str) {
        let bound_error = self.tools.get(session_id).and_then(|tools| {
            if tools.len() > MAX_TOOL_COUNT {
                return Some(format!(
                    "{}: Current page exposes more than {} WebMCP tools",
                    ERR_OUTPUT_TOO_LARGE, MAX_TOOL_COUNT
                ));
            }
            let bytes = tools.values().fold(0usize, |total, tool| {
                total.saturating_add(serde_json::to_vec(tool).map_or(0, |encoded| encoded.len()))
            });
            (bytes > MAX_TOOL_LIST_BYTES).then(|| {
                format!(
                    "{}: Current page exposes more than {} bytes of WebMCP tool metadata",
                    ERR_OUTPUT_TOO_LARGE, MAX_TOOL_LIST_BYTES
                )
            })
        });

        if let Some(error) = bound_error {
            self.tool_errors.insert(session_id.to_string(), error);
        } else if self
            .tool_errors
            .get(session_id)
            .is_some_and(|error| is_tool_bound_error(error))
        {
            self.tool_errors.remove(session_id);
        }
    }

    pub fn clear_invocations(&mut self) {
        for record in self.invocations.values_mut() {
            if !record.is_terminal() {
                record.mark_context_changed();
            }
        }
        self.early_responses.clear();
        self.early_order.clear();
    }

    pub fn clear_page_scope(&mut self, session_id: &str) {
        self.clear_invocations();
        self.clear_page_tools(session_id);
    }

    pub fn clear_page_tools(&mut self, session_id: &str) {
        self.tools.remove(session_id);
        self.frame_origins.remove(session_id);
        self.tool_errors.remove(session_id);
    }

    pub fn clear_frame_scope(&mut self, session_id: &str, frame_id: &str) {
        for record in self.invocations.values_mut() {
            if !record.is_terminal() && record.frame_id == frame_id {
                record.mark_context_changed();
            }
        }
        if let Some(tools) = self.tools.get_mut(session_id) {
            tools.retain(|(id, _), _| id != frame_id);
        }
        if let Some(origins) = self.frame_origins.get_mut(session_id) {
            origins.remove(frame_id);
        }
        self.validate_tool_bounds(session_id);
    }

    pub fn clear_all(&mut self) {
        self.invocations.clear();
        self.order.clear();
        self.early_responses.clear();
        self.early_order.clear();
        self.tools.clear();
        self.frame_origins.clear();
        self.tool_errors.clear();
    }
}

pub fn unsupported_error(error: &str) -> String {
    format!(
        "{}: This browser does not expose the experimental CDP WebMCP domain. Use a current agent-browser-managed Chrome session without --no-webmcp. Attached browsers and providers must enable WebMCP at launch. CDP detail: {}",
        ERR_UNSUPPORTED, error
    )
}

pub fn validate_input(input: &Value) -> Result<(), String> {
    if !input.is_object() {
        return Err(format!(
            "{}: WebMCP params must be a JSON object",
            ERR_INVALID_INPUT
        ));
    }
    let size = serde_json::to_vec(input)
        .map_err(|error| format!("{}: {}", ERR_INVALID_INPUT, error))?
        .len();
    if size > MAX_INPUT_BYTES {
        return Err(format!(
            "{}: WebMCP input is {} bytes; maximum is {} bytes",
            ERR_INVALID_INPUT, size, MAX_INPUT_BYTES
        ));
    }
    Ok(())
}

pub fn normalize_tool(raw: &Value, origin: &str) -> Result<ToolRecord, String> {
    let record_bytes = serde_json::to_vec(raw)
        .map_err(|error| format!("{}: {}", ERR_INVALID_TOOL, error))?
        .len();
    if record_bytes > MAX_TOOL_RECORD_BYTES {
        return Err(format!(
            "{}: WebMCP tool record is {} bytes; maximum is {} bytes",
            ERR_OUTPUT_TOO_LARGE, record_bytes, MAX_TOOL_RECORD_BYTES
        ));
    }
    Ok(ToolRecord {
        name: raw
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{}: WebMCP tool is missing name", ERR_INVALID_TOOL))?
            .to_string(),
        description: raw
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        input_schema: raw.get("inputSchema").cloned().unwrap_or_else(|| json!({})),
        annotations: raw.get("annotations").cloned().unwrap_or_else(|| json!({})),
        origin: origin.to_string(),
        frame_id: raw
            .get("frameId")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{}: WebMCP tool is missing frameId", ERR_INVALID_TOOL))?
            .to_string(),
        backend_node_id: raw.get("backendNodeId").and_then(Value::as_i64),
    })
}

pub fn frame_origin(frame: &Value) -> Option<String> {
    if let Some(origin) = frame
        .get("securityOrigin")
        .and_then(Value::as_str)
        .filter(|origin| !origin.is_empty() && *origin != "://")
    {
        return Some(origin.to_string());
    }
    frame.get("url").and_then(Value::as_str).map(|url| {
        url::Url::parse(url)
            .map(|parsed| parsed.origin().ascii_serialization())
            .unwrap_or_else(|_| url.to_string())
    })
}

pub fn resolve_tool(
    tools: &[ToolRecord],
    name: &str,
    frame_id: Option<&str>,
) -> Result<ToolRecord, String> {
    let matches = tools
        .iter()
        .filter(|tool| tool.name == name && frame_id.is_none_or(|id| tool.frame_id == id))
        .cloned()
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(format!(
            "{}: No WebMCP tool named '{}'{}",
            ERR_TOOL_NOT_FOUND,
            name,
            frame_id
                .map(|id| format!(" in frame '{}'", id))
                .unwrap_or_default()
        )),
        [tool] => Ok(tool.clone()),
        _ => Err(format!(
            "{}: WebMCP tool '{}' exists in multiple frames. Retry with --frame <frame-id>. Matching frames: {}",
            ERR_AMBIGUOUS_TOOL,
            name,
            matches
                .iter()
                .map(|tool| tool.frame_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

pub fn invoke_error(error: &str) -> String {
    if method_is_unsupported(error, "WebMCP.invokeTool") {
        unsupported_error(error)
    } else {
        format!("{}: {}", ERR_INVOKE_FAILED, error)
    }
}

pub fn cancel_error(error: &str) -> String {
    if method_is_unsupported(error, "WebMCP.cancelInvocation") {
        unsupported_error(error)
    } else if error.to_ascii_lowercase().contains("no pending execution") {
        format!("{}: {}", ERR_INVOCATION_NOT_ACTIVE, error)
    } else {
        format!("{}: {}", ERR_CANCEL_FAILED, error)
    }
}

fn method_is_unsupported(error: &str, method: &str) -> bool {
    error.contains(method)
        && (error.contains("wasn't found")
            || error.contains("Method not found")
            || error.contains("-32601"))
}

pub fn timeout_duration(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms.max(1))
}

fn bounded_string(value: &str, cap: usize) -> String {
    if value.len() <= cap {
        return value.to_string();
    }
    let mut boundary = cap;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!(
        "{}\n[truncated: showing {} of {} bytes]",
        &value[..boundary],
        boundary,
        value.len()
    )
}

fn bounded_value(value: &Value, cap: usize) -> (Value, bool, usize) {
    let encoded = serde_json::to_vec(value).unwrap_or_default();
    if encoded.len() <= cap {
        return (value.clone(), false, encoded.len());
    }
    let preview_cap = cap.min(64 * 1024);
    let preview = String::from_utf8_lossy(&encoded[..preview_cap]).into_owned();
    (
        json!({
            "truncated": true,
            "preview": preview,
        }),
        true,
        encoded.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_duplicate_names_with_frame() {
        let tools = vec![
            normalize_tool(&json!({"name":"search","frameId":"a"}), "https://a.test").unwrap(),
            normalize_tool(&json!({"name":"search","frameId":"b"}), "https://a.test").unwrap(),
        ];
        assert!(resolve_tool(&tools, "search", None)
            .unwrap_err()
            .starts_with(ERR_AMBIGUOUS_TOOL));
        assert_eq!(
            resolve_tool(&tools, "search", Some("b")).unwrap().frame_id,
            "b"
        );
    }

    #[test]
    fn buffers_early_response() {
        let mut state = RuntimeState::default();
        state.apply_response(json!({
            "invocationId": "i1",
            "status": "Completed",
            "output": {"ok": true}
        }));
        state
            .insert(InvocationRecord::pending(
                "i1".to_string(),
                "search".to_string(),
                "f1".to_string(),
                "https://example.test".to_string(),
            ))
            .unwrap();
        assert_eq!(state.invocations["i1"].status, "completed");
        assert_eq!(state.invocations["i1"].result, Some(json!({"ok": true})));
    }

    #[test]
    fn bounds_history_and_marks_page_change() {
        let mut state = RuntimeState::default();
        for index in 0..MAX_INVOCATION_HISTORY {
            let mut record = InvocationRecord::pending(
                format!("i{}", index),
                "tool".to_string(),
                "frame".to_string(),
                "https://example.test".to_string(),
            );
            record.status = "completed".to_string();
            state.insert(record).unwrap();
        }
        state
            .insert(InvocationRecord::pending(
                "latest".to_string(),
                "tool".to_string(),
                "frame".to_string(),
                "https://example.test".to_string(),
            ))
            .unwrap();
        assert_eq!(state.invocations.len(), MAX_INVOCATION_HISTORY);
        state.clear_invocations();
        assert!(state
            .invocations
            .values()
            .all(InvocationRecord::is_terminal));
    }

    #[test]
    fn refuses_to_evict_active_invocations() {
        let mut state = RuntimeState::default();
        for index in 0..MAX_INVOCATION_HISTORY {
            state
                .insert(InvocationRecord::pending(
                    format!("i{}", index),
                    "tool".to_string(),
                    "frame".to_string(),
                    "https://example.test".to_string(),
                ))
                .unwrap();
        }
        assert!(state
            .ensure_capacity()
            .unwrap_err()
            .starts_with(ERR_TOO_MANY_INVOCATIONS));
        assert_eq!(state.invocations.len(), MAX_INVOCATION_HISTORY);
    }

    #[test]
    fn bounds_early_responses_and_output_storage() {
        let mut state = RuntimeState::default();
        for index in 0..=MAX_EARLY_RESPONSES {
            state.apply_response(json!({
                "invocationId": format!("i{index}"),
                "status": "Completed",
                "output": {"value": "x".repeat(MAX_OUTPUT_BYTES)}
            }));
        }
        assert_eq!(state.early_responses.len(), MAX_EARLY_RESPONSES);
        assert!(!state.early_responses.contains_key("i0"));

        state
            .insert(InvocationRecord::pending(
                format!("i{MAX_EARLY_RESPONSES}"),
                "tool".to_string(),
                "frame".to_string(),
                "https://example.test".to_string(),
            ))
            .unwrap();
        let record = &state.invocations[&format!("i{MAX_EARLY_RESPONSES}")];
        assert!(record.output_truncated);
        assert!(
            serde_json::to_vec(record.result.as_ref().unwrap())
                .unwrap()
                .len()
                < MAX_OUTPUT_BYTES
        );
    }

    #[test]
    fn validates_input_shape_and_size() {
        assert!(validate_input(&json!([]))
            .unwrap_err()
            .starts_with(ERR_INVALID_INPUT));
        assert!(validate_input(&json!({"value": "x"})).is_ok());
        assert!(
            validate_input(&json!({"value": "x".repeat(MAX_INPUT_BYTES)}))
                .unwrap_err()
                .starts_with(ERR_INVALID_INPUT)
        );
    }

    #[test]
    fn maps_protocol_errors_by_operation() {
        let unsupported = "CDP error (WebMCP.invokeTool): 'WebMCP.invokeTool' wasn't found";
        assert!(invoke_error(unsupported).starts_with(ERR_UNSUPPORTED));
        assert!(
            cancel_error("CDP error (WebMCP.cancelInvocation): No pending execution")
                .starts_with(ERR_INVOCATION_NOT_ACTIVE)
        );
        assert!(
            cancel_error("CDP error (WebMCP.cancelInvocation): internal failure")
                .starts_with(ERR_CANCEL_FAILED)
        );
    }

    #[test]
    fn clears_only_pending_invocations_for_a_detached_frame() {
        let mut state = RuntimeState::default();
        for frame in ["a", "b"] {
            state
                .insert(InvocationRecord::pending(
                    frame.to_string(),
                    "tool".to_string(),
                    frame.to_string(),
                    "https://example.test".to_string(),
                ))
                .unwrap();
        }
        state.clear_frame_scope("session", "a");
        assert_eq!(state.invocations["a"].status, "failed");
        assert_eq!(state.invocations["b"].status, "pending");
    }

    #[test]
    fn tracks_tools_by_frame_and_clears_their_scope() {
        let mut state = RuntimeState::default();
        state.update_frame_origin("session", "main", "https://example.test");
        state.update_frame_origin("session", "child", "https://frame.test");
        state.apply_tools_added(
            "session",
            &json!({
                "tools": [
                    {"name": "search", "frameId": "main"},
                    {"name": "search", "frameId": "child"}
                ]
            }),
            "null",
        );

        let tools = state.tools("session").unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(
            tools
                .iter()
                .find(|tool| tool.frame_id == "child")
                .unwrap()
                .origin,
            "https://frame.test"
        );

        state.apply_tools_removed(
            "session",
            &json!({
                "tools": [{"name": "search", "frameId": "main"}]
            }),
        );
        assert_eq!(state.tools("session").unwrap().len(), 1);

        state.clear_frame_scope("session", "child");
        assert!(state.tools("session").unwrap().is_empty());
    }

    #[test]
    fn frame_origin_prefers_cdp_security_origin() {
        assert_eq!(
            frame_origin(&json!({
                "url": "about:blank",
                "securityOrigin": "https://parent.example"
            })),
            Some("https://parent.example".to_string())
        );
        assert_eq!(
            frame_origin(&json!({ "url": "https://example.test/path" })),
            Some("https://example.test".to_string())
        );
    }

    #[test]
    fn bounds_the_page_tool_registry() {
        let mut state = RuntimeState::default();
        let tools = (0..=MAX_TOOL_COUNT)
            .map(|index| json!({"name": format!("tool_{index}"), "frameId": "main"}))
            .collect::<Vec<_>>();
        state.apply_tools_added(
            "session",
            &json!({ "tools": tools }),
            "https://example.test",
        );
        assert!(state
            .tools("session")
            .unwrap_err()
            .starts_with(ERR_OUTPUT_TOO_LARGE));

        state.apply_tools_removed(
            "session",
            &json!({
                "tools": [{"name": format!("tool_{}", MAX_TOOL_COUNT), "frameId": "main"}]
            }),
        );
        assert_eq!(state.tools("session").unwrap().len(), MAX_TOOL_COUNT);

        state.apply_tools_added(
            "session",
            &json!({
                "tools": [{
                    "name": "oversized",
                    "description": "x".repeat(MAX_TOOL_RECORD_BYTES),
                    "frameId": "main"
                }]
            }),
            "https://example.test",
        );
        state.apply_tools_removed(
            "session",
            &json!({
                "tools": [{"name": "tool_0", "frameId": "main"}]
            }),
        );
        assert!(state
            .tools("session")
            .unwrap_err()
            .starts_with(ERR_OUTPUT_TOO_LARGE));
    }

    #[test]
    fn isolates_tools_by_page_session() {
        let mut state = RuntimeState::default();
        state.apply_tools_added(
            "page-a",
            &json!({"tools": [{"name": "alpha", "frameId": "a"}]}),
            "https://a.test",
        );
        state.apply_tools_added(
            "page-b",
            &json!({"tools": [{"name": "beta", "frameId": "b"}]}),
            "https://b.test",
        );

        assert_eq!(state.tools("page-a").unwrap()[0].name, "alpha");
        assert_eq!(state.tools("page-b").unwrap()[0].name, "beta");

        state.clear_page_tools("page-a");
        assert!(state.tools("page-a").unwrap().is_empty());
        assert_eq!(state.tools("page-b").unwrap()[0].name, "beta");
    }
}
