//! MCP Tasks support across both protocol eras:
//! - `2026-07-28`: `io.modelcontextprotocol/tasks` extension (server-directed,
//!   `resultType: "task"`, `tasks/get`, `tasks/update`, `tasks/cancel`, `subscriptions/listen`).
//! - `2025-11-25`: core Tasks (requestor-driven `params.task`, `execution.taskSupport`,
//!   `tasks/get`, `tasks/result`, `tasks/list`, `tasks/cancel`, `notifications/tasks/status`).

use chrono::{SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::mcp_transport::{
    error_response_from, id_key, wrap_response_result, ProtocolEra, ProtocolError, RequestContext,
    StdioWriter,
};

pub(crate) const TASKS_EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";
pub(crate) const META_RELATED_TASK: &str = "io.modelcontextprotocol/related-task";
pub(crate) const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";

pub(crate) const DEFAULT_TASK_GRACE_MS: u64 = 250;
pub(crate) const DEFAULT_TASK_TTL_MS: u64 = 300_000;
pub(crate) const MAX_TASK_TTL_MS: u64 = 3_600_000;
const INITIAL_POLL_INTERVAL_MS: u64 = 500;
const SLOW_POLL_INTERVAL_MS: u64 = 2_000;
const MAX_CONCURRENT_TASKS: usize = 16;
const TASK_LIST_PAGE_SIZE: usize = 64;

/// Tools eligible for asynchronous task execution (`execution.taskSupport: "optional"`
/// in `2025-11-25` and server-directed task execution in `2026-07-28`).
pub(crate) const TASK_ELIGIBLE_TOOLS: &[&str] = &[
    // Waits
    "agent_browser_wait_ms",
    "agent_browser_wait_for_selector",
    "agent_browser_wait_for_text",
    "agent_browser_wait_for_url",
    "agent_browser_wait_for_load",
    "agent_browser_wait_for_function",
    "agent_browser_wait_for_download",
    // Navigation & browser launch
    "agent_browser_open",
    "agent_browser_reload",
    "agent_browser_read",
    "agent_browser_connect",
    "agent_browser_tab_new",
    // Transfers, captures, and audits
    "agent_browser_download",
    "agent_browser_pdf",
    "agent_browser_diff_url",
    "agent_browser_a11y",
    // Multi-step workflows and maintenance
    "agent_browser_batch",
    "agent_browser_flow_run",
    "agent_browser_auth_login",
    "agent_browser_webmcp_invoke",
    "agent_browser_install",
    "agent_browser_upgrade",
    "agent_browser_doctor",
    "agent_browser_chat",
    "agent_browser_plugin_run",
    "agent_browser_plugin_add",
    // Capture finalization
    "agent_browser_record_stop",
    "agent_browser_record_restart",
    "agent_browser_trace_stop",
    "agent_browser_profiler_stop",
    "agent_browser_network_har_stop",
];

pub(crate) fn is_task_eligible_tool(name: &str) -> bool {
    TASK_ELIGIBLE_TOOLS.contains(&name)
}

pub(crate) fn legacy_tasks_capability() -> Value {
    json!({
        "list": {},
        "cancel": {},
        "requests": {
            "tools": {
                "call": {}
            }
        }
    })
}

pub(crate) fn missing_tasks_capability_error() -> ProtocolError {
    ProtocolError::missing_required_client_capability(
        "Client must declare the io.modelcontextprotocol/tasks extension capability",
        json!({
            "extensions": {
                TASKS_EXTENSION_ID: {}
            }
        }),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum TaskOutcome {
    Result(Value),
    Error(ProtocolError),
}

/// Result of waiting through the modern task fast-path grace window.
pub(crate) enum GraceWaitResult {
    /// Tool finished before the grace window elapsed; return directly.
    Finished(Option<TaskOutcome>),
    /// Grace window elapsed or task needs input; task is now exposed via `CreateTaskResult`.
    Exposed(Value),
    /// Original `tools/call` was cancelled via `notifications/cancelled` during the grace window.
    Cancelled,
}

struct TaskRecord {
    task_id: String,
    era: ProtocolEra,
    status: TaskStatus,
    status_message: Option<String>,
    created_at_iso: String,
    last_updated_at_iso: String,
    created_at: Instant,
    ttl_ms: u64,
    cancel: Arc<AtomicBool>,
    outcome: Option<TaskOutcome>,
    input_requests: Map<String, Value>,
    input_waiters: HashMap<String, mpsc::Sender<Value>>,
    answered_input_keys: HashSet<String>,
    /// True once the task handle has been returned to the client (false during
    /// the modern fast-path grace window before returning `CreateTaskResult`).
    exposed: bool,
}

impl TaskRecord {
    fn is_expired(&self) -> bool {
        self.created_at.elapsed() >= Duration::from_millis(self.ttl_ms)
    }

    fn poll_interval_ms(&self) -> u64 {
        if self.created_at.elapsed() >= Duration::from_secs(5) {
            SLOW_POLL_INTERVAL_MS
        } else {
            INITIAL_POLL_INTERVAL_MS
        }
    }

    fn touch(&mut self) {
        self.last_updated_at_iso = iso_now();
    }

    /// `2025-11-25` Task object (`ttl`, `pollInterval`).
    fn legacy_task_json(&self) -> Value {
        let mut obj = json!({
            "taskId": self.task_id,
            "status": self.status.as_str(),
            "createdAt": self.created_at_iso,
            "lastUpdatedAt": self.last_updated_at_iso,
            "ttl": self.ttl_ms,
        });
        if !self.status.is_terminal() {
            obj["pollInterval"] = json!(self.poll_interval_ms());
        }
        if let Some(msg) = &self.status_message {
            obj["statusMessage"] = json!(msg);
        }
        obj
    }

    /// `2026-07-28` base `Task` fields (used by `CreateTaskResult` with `resultType: "task"`).
    fn modern_create_result_json(&self) -> Value {
        let mut obj = json!({
            "resultType": "task",
            "taskId": self.task_id,
            "status": self.status.as_str(),
            "createdAt": self.created_at_iso,
            "lastUpdatedAt": self.last_updated_at_iso,
            "ttlMs": self.ttl_ms,
            "pollIntervalMs": self.poll_interval_ms(),
        });
        if let Some(msg) = &self.status_message {
            obj["statusMessage"] = json!(msg);
        }
        obj
    }

    /// `2026-07-28` `DetailedTask` fields (used by `tasks/get` and `notifications/tasks`).
    fn modern_detailed_json(&self) -> Value {
        let mut obj = json!({
            "taskId": self.task_id,
            "status": self.status.as_str(),
            "createdAt": self.created_at_iso,
            "lastUpdatedAt": self.last_updated_at_iso,
            "ttlMs": self.ttl_ms,
        });
        if !self.status.is_terminal() {
            obj["pollIntervalMs"] = json!(self.poll_interval_ms());
        }
        if let Some(msg) = &self.status_message {
            obj["statusMessage"] = json!(msg);
        }
        match self.status {
            TaskStatus::Working | TaskStatus::Cancelled => {}
            TaskStatus::InputRequired => {
                obj["inputRequests"] = Value::Object(self.input_requests.clone());
            }
            TaskStatus::Completed => {
                if let Some(TaskOutcome::Result(res)) = &self.outcome {
                    obj["result"] = wrap_response_result(ProtocolEra::Modern, res.clone());
                }
            }
            TaskStatus::Failed => {
                if let Some(TaskOutcome::Error(err)) = &self.outcome {
                    let mut error_obj = json!({
                        "code": err.code,
                        "message": err.message,
                    });
                    if let Some(data) = &err.data {
                        error_obj["data"] = data.clone();
                    }
                    obj["error"] = error_obj;
                } else {
                    obj["error"] = json!({
                        "code": -32603,
                        "message": self.status_message.clone().unwrap_or_else(|| "Task failed".to_string()),
                    });
                }
            }
        }
        obj
    }
}

struct TaskSubscription {
    subscription_id: Value,
    task_ids: HashSet<String>,
}

struct TaskStore {
    tasks: HashMap<String, TaskRecord>,
    order: Vec<String>,
    subscriptions: HashMap<String, TaskSubscription>,
}

/// Manages active and recently completed MCP tasks across both protocol eras.
pub(crate) struct TaskManager {
    store: Mutex<TaskStore>,
    state_changed: Condvar,
    writer: StdioWriter,
}

impl TaskManager {
    pub(crate) fn new(writer: StdioWriter) -> Self {
        Self {
            store: Mutex::new(TaskStore {
                tasks: HashMap::new(),
                order: Vec::new(),
                subscriptions: HashMap::new(),
            }),
            state_changed: Condvar::new(),
            writer,
        }
    }

    fn purge_expired_locked(store: &mut TaskStore) {
        let expired: Vec<String> = store
            .tasks
            .iter()
            .filter(|(_, rec)| rec.is_expired())
            .map(|(id, _)| id.clone())
            .collect();
        if expired.is_empty() {
            return;
        }
        for id in &expired {
            if let Some(rec) = store.tasks.remove(id) {
                rec.cancel.store(true, Ordering::SeqCst);
            }
        }
        let expired_set: HashSet<&str> = expired.iter().map(String::as_str).collect();
        store.order.retain(|id| !expired_set.contains(id.as_str()));
    }

    /// Create a durable task record in `working` status.
    pub(crate) fn create_task(
        &self,
        era: ProtocolEra,
        tool_name: &str,
        requested_ttl_ms: Option<u64>,
        exposed: bool,
    ) -> Result<(String, Arc<AtomicBool>, Value), ProtocolError> {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired_locked(&mut store);

        let active_count = store
            .tasks
            .values()
            .filter(|rec| !rec.status.is_terminal())
            .count();
        if active_count >= MAX_CONCURRENT_TASKS {
            return Err(ProtocolError::internal_error(format!(
                "Maximum concurrent tasks ({}) reached; wait for existing tasks to finish or cancel them",
                MAX_CONCURRENT_TASKS
            )));
        }

        let task_id = Uuid::new_v4().to_string();
        let ttl_ms = requested_ttl_ms
            .unwrap_or(DEFAULT_TASK_TTL_MS)
            .clamp(1_000, MAX_TASK_TTL_MS);
        let now_iso = iso_now();
        let cancel = Arc::new(AtomicBool::new(false));
        let rec = TaskRecord {
            task_id: task_id.clone(),
            era,
            status: TaskStatus::Working,
            status_message: Some(format!("Running {}", tool_name)),
            created_at_iso: now_iso.clone(),
            last_updated_at_iso: now_iso,
            created_at: Instant::now(),
            ttl_ms,
            cancel: cancel.clone(),
            outcome: None,
            input_requests: Map::new(),
            input_waiters: HashMap::new(),
            answered_input_keys: HashSet::new(),
            exposed,
        };
        let create_result = match era {
            ProtocolEra::Modern => rec.modern_create_result_json(),
            ProtocolEra::Legacy => json!({ "task": rec.legacy_task_json() }),
        };
        store.tasks.insert(task_id.clone(), rec);
        store.order.push(task_id.clone());
        Ok((task_id, cancel, create_result))
    }

    /// Wake any thread waiting on task state changes (for example, when an
    /// unexposed task is cancelled via `notifications/cancelled` during its grace window).
    pub(crate) fn notify_state_change(&self) {
        let _guard = self.store.lock().unwrap_or_else(|e| e.into_inner());
        self.state_changed.notify_all();
    }

    /// Wait up to `grace` for a newly started modern task to finish synchronously.
    /// Removes the unexposed task if it finishes or is cancelled during the grace
    /// window; otherwise marks it exposed and returns its `CreateTaskResult`.
    pub(crate) fn wait_or_expose_modern(&self, task_id: &str, grace: Duration) -> GraceWaitResult {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + grace;
        loop {
            if let Some(rec) = store.tasks.get(task_id) {
                if rec.cancel.load(Ordering::SeqCst) {
                    store.tasks.remove(task_id);
                    store.order.retain(|id| id != task_id);
                    return GraceWaitResult::Cancelled;
                }
            }
            let done = store.tasks.get(task_id).is_some_and(|rec| {
                rec.status.is_terminal() || rec.status == TaskStatus::InputRequired
            });
            if done {
                if let Some(rec) = store.tasks.get(task_id) {
                    if rec.status == TaskStatus::InputRequired {
                        let create_res = rec.modern_create_result_json();
                        if let Some(rec_mut) = store.tasks.get_mut(task_id) {
                            rec_mut.exposed = true;
                        }
                        return GraceWaitResult::Exposed(create_res);
                    }
                }
                let rec = store.tasks.remove(task_id);
                store.order.retain(|id| id != task_id);
                return GraceWaitResult::Finished(rec.and_then(|r| r.outcome));
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                let Some(rec) = store.tasks.get_mut(task_id) else {
                    return GraceWaitResult::Finished(None);
                };
                if rec.cancel.load(Ordering::SeqCst) {
                    store.tasks.remove(task_id);
                    store.order.retain(|id| id != task_id);
                    return GraceWaitResult::Cancelled;
                }
                rec.exposed = true;
                return GraceWaitResult::Exposed(rec.modern_create_result_json());
            };
            let (next_store, wait_res) = self
                .state_changed
                .wait_timeout(store, remaining)
                .unwrap_or_else(|e| e.into_inner());
            store = next_store;
            if wait_res.timed_out() {
                let Some(rec) = store.tasks.get_mut(task_id) else {
                    return GraceWaitResult::Finished(None);
                };
                if rec.cancel.load(Ordering::SeqCst) {
                    store.tasks.remove(task_id);
                    store.order.retain(|id| id != task_id);
                    return GraceWaitResult::Cancelled;
                }
                if rec.status.is_terminal() {
                    let rec = store.tasks.remove(task_id);
                    store.order.retain(|id| id != task_id);
                    return GraceWaitResult::Finished(rec.and_then(|r| r.outcome));
                }
                rec.exposed = true;
                return GraceWaitResult::Exposed(rec.modern_create_result_json());
            }
        }
    }

    /// Record completion of a task worker.
    pub(crate) fn complete_task(&self, task_id: &str, outcome: TaskOutcome) {
        let mut notifications: Vec<(String, Value)> = Vec::new();
        {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let Some(rec) = store.tasks.get_mut(task_id) else {
                return;
            };
            if rec.status.is_terminal() {
                // Cancelled or already finalized: terminal states never transition.
                return;
            }
            match &outcome {
                TaskOutcome::Result(res) => {
                    let is_tool_error = res["isError"].as_bool().unwrap_or(false);
                    match rec.era {
                        ProtocolEra::Modern => {
                            // ext-tasks: tool-level isError stays status "completed".
                            rec.status = TaskStatus::Completed;
                            rec.status_message = Some(if is_tool_error {
                                "Tool returned an error result".to_string()
                            } else {
                                "Completed".to_string()
                            });
                        }
                        ProtocolEra::Legacy => {
                            // 2025-11-25: tool isError transitions task to "failed".
                            if is_tool_error {
                                rec.status = TaskStatus::Failed;
                                rec.status_message = first_text_summary(res)
                                    .or_else(|| Some("Tool execution failed".to_string()));
                            } else {
                                rec.status = TaskStatus::Completed;
                                rec.status_message = Some("Completed".to_string());
                            }
                        }
                    }
                }
                TaskOutcome::Error(err) => {
                    rec.status = TaskStatus::Failed;
                    rec.status_message = Some(err.message.clone());
                }
            }
            rec.outcome = Some(outcome);
            rec.input_requests.clear();
            rec.input_waiters.clear();
            rec.touch();

            if rec.exposed {
                collect_status_notifications(&store, task_id, &mut notifications);
            }
            self.state_changed.notify_all();
        }
        for (method, params) in notifications {
            let _ = self.writer.send_notification(&method, params);
        }
    }

    /// Update a working task's status message (optional progress visibility on `tasks/get`).
    #[allow(dead_code)]
    pub(crate) fn update_status_message(&self, task_id: &str, message: impl Into<String>) {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        let Some(rec) = store.tasks.get_mut(task_id) else {
            return;
        };
        if !rec.status.is_terminal() {
            rec.status_message = Some(message.into());
            rec.touch();
        }
    }

    /// Transition an active task into `input_required` and wait for a matching
    /// `tasks/update` response (used by `2026-07-28` mid-flight elicitation).
    #[allow(dead_code)]
    pub(crate) fn request_task_input(
        &self,
        task_id: &str,
        key: String,
        request: Value,
        message: String,
        timeout: Duration,
    ) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        let mut notifications = Vec::new();
        {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let Some(rec) = store.tasks.get_mut(task_id) else {
                return Err("Task not found".to_string());
            };
            if rec.status.is_terminal() {
                return Err("Task already terminated".to_string());
            }
            rec.status = TaskStatus::InputRequired;
            rec.status_message = Some(message);
            rec.input_requests.insert(key.clone(), request);
            rec.input_waiters.insert(key.clone(), tx);
            rec.touch();
            if rec.exposed {
                collect_status_notifications(&store, task_id, &mut notifications);
            }
            self.state_changed.notify_all();
        }
        for (method, params) in notifications {
            let _ = self.writer.send_notification(&method, params);
        }

        let reply = rx.recv_timeout(timeout);
        {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(rec) = store.tasks.get_mut(task_id) {
                rec.input_requests.remove(&key);
                rec.input_waiters.remove(&key);
                if rec.status == TaskStatus::InputRequired && rec.input_requests.is_empty() {
                    rec.status = TaskStatus::Working;
                    rec.status_message = Some("Resuming after input".to_string());
                    rec.touch();
                    let mut resume_notes = Vec::new();
                    if rec.exposed {
                        collect_status_notifications(&store, task_id, &mut resume_notes);
                    }
                    drop(store);
                    for (method, params) in resume_notes {
                        let _ = self.writer.send_notification(&method, params);
                    }
                }
            }
        }

        reply.map_err(|_| "Timed out waiting for tasks/update input response".to_string())
    }

    /// Transition a `2025-11-25` task into `input_required` and back to `working`.
    #[allow(dead_code)]
    pub(crate) fn set_legacy_input_required(&self, task_id: &str, waiting: bool, message: &str) {
        let mut notifications = Vec::new();
        {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            let Some(rec) = store.tasks.get_mut(task_id) else {
                return;
            };
            if rec.status.is_terminal() {
                return;
            }
            rec.status = if waiting {
                TaskStatus::InputRequired
            } else {
                TaskStatus::Working
            };
            rec.status_message = Some(message.to_string());
            rec.touch();
            if rec.exposed {
                collect_status_notifications(&store, task_id, &mut notifications);
            }
            self.state_changed.notify_all();
        }
        for (method, params) in notifications {
            let _ = self.writer.send_notification(&method, params);
        }
    }

    /// Handle `tasks/get` for both `2026-07-28` and `2025-11-25`.
    pub(crate) fn handle_get(
        &self,
        params: Option<&Value>,
        ctx: &RequestContext,
    ) -> Result<Value, ProtocolError> {
        if ctx.is_modern() && !ctx.has_extension(TASKS_EXTENSION_ID) {
            return Err(missing_tasks_capability_error());
        }
        let task_id = required_task_id(params, "tasks/get")?;
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired_locked(&mut store);
        let Some(rec) = store.tasks.get(task_id) else {
            return Err(ProtocolError::invalid_params(format!(
                "Task not found: {}",
                task_id
            )));
        };
        if ctx.is_modern() {
            let mut detailed = rec.modern_detailed_json();
            detailed["resultType"] = json!("complete");
            Ok(detailed)
        } else {
            Ok(rec.legacy_task_json())
        }
    }

    /// Handle `tasks/update` (`2026-07-28` ext-tasks).
    pub(crate) fn handle_update(
        &self,
        params: Option<&Value>,
        ctx: &RequestContext,
    ) -> Result<Value, ProtocolError> {
        if ctx.is_modern() && !ctx.has_extension(TASKS_EXTENSION_ID) {
            return Err(missing_tasks_capability_error());
        }
        let task_id = required_task_id(params, "tasks/update")?;
        let responses = params
            .and_then(|p| p.get("inputResponses"))
            .and_then(Value::as_object)
            .ok_or_else(|| {
                ProtocolError::invalid_params("tasks/update requires object params.inputResponses")
            })?;

        let mut deliveries: Vec<(mpsc::Sender<Value>, Value)> = Vec::new();
        {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            Self::purge_expired_locked(&mut store);
            let Some(rec) = store.tasks.get_mut(task_id) else {
                return Err(ProtocolError::invalid_params(format!(
                    "Task not found: {}",
                    task_id
                )));
            };
            for (key, value) in responses {
                if rec.answered_input_keys.contains(key) {
                    continue;
                }
                if let Some(tx) = rec.input_waiters.remove(key) {
                    rec.input_requests.remove(key);
                    rec.answered_input_keys.insert(key.clone());
                    deliveries.push((tx, value.clone()));
                }
            }
            if !deliveries.is_empty() {
                rec.touch();
            }
        }
        for (tx, val) in deliveries {
            let _ = tx.send(val);
        }

        Ok(json!({ "resultType": "complete" }))
    }

    /// Handle `tasks/cancel` for both `2026-07-28` and `2025-11-25`.
    pub(crate) fn handle_cancel(
        &self,
        params: Option<&Value>,
        ctx: &RequestContext,
    ) -> Result<Value, ProtocolError> {
        if ctx.is_modern() && !ctx.has_extension(TASKS_EXTENSION_ID) {
            return Err(missing_tasks_capability_error());
        }
        let task_id = required_task_id(params, "tasks/cancel")?;
        let mut notifications = Vec::new();
        let response = {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            Self::purge_expired_locked(&mut store);
            let Some(rec) = store.tasks.get_mut(task_id) else {
                return Err(ProtocolError::invalid_params(format!(
                    "Task not found: {}",
                    task_id
                )));
            };
            if rec.status.is_terminal() {
                if ctx.is_modern() {
                    // ext-tasks: cancellation is cooperative; empty ack is valid.
                    return Ok(json!({ "resultType": "complete" }));
                }
                return Err(ProtocolError::invalid_params(format!(
                    "Cannot cancel task: already in terminal status '{}'",
                    rec.status.as_str()
                )));
            }
            rec.cancel.store(true, Ordering::SeqCst);
            rec.status = TaskStatus::Cancelled;
            rec.status_message = Some("The task was cancelled by request.".to_string());
            rec.input_requests.clear();
            rec.input_waiters.clear();
            rec.touch();
            let exposed = rec.exposed;
            let reply = if ctx.is_modern() {
                json!({ "resultType": "complete" })
            } else {
                rec.legacy_task_json()
            };
            if exposed {
                collect_status_notifications(&store, task_id, &mut notifications);
            }
            self.state_changed.notify_all();
            reply
        };
        for (method, params) in notifications {
            let _ = self.writer.send_notification(&method, params);
        }
        Ok(response)
    }

    /// Handle `tasks/list` (`2025-11-25` core tasks).
    pub(crate) fn handle_list(
        &self,
        params: Option<&Value>,
        ctx: &RequestContext,
    ) -> Result<Value, ProtocolError> {
        if ctx.is_modern() {
            return Err(ProtocolError::method_not_found("tasks/list"));
        }
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        Self::purge_expired_locked(&mut store);
        let tasks: Vec<Value> = store
            .order
            .iter()
            .filter_map(|id| store.tasks.get(id))
            .filter(|rec| rec.exposed)
            .map(TaskRecord::legacy_task_json)
            .collect();

        let start = parse_list_cursor(params, tasks.len())?;
        let end = (start + TASK_LIST_PAGE_SIZE).min(tasks.len());
        let mut result = json!({
            "tasks": tasks[start..end].to_vec(),
        });
        if end < tasks.len() {
            result["nextCursor"] = json!(end.to_string());
        }
        Ok(result)
    }

    /// Blocking `tasks/result` handler for `2025-11-25` (invoked on a worker thread).
    pub(crate) fn wait_for_legacy_result(&self, task_id: &str) -> Result<Value, ProtocolError> {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            Self::purge_expired_locked(&mut store);
            let Some(rec) = store.tasks.get(task_id) else {
                return Err(ProtocolError::invalid_params(format!(
                    "Task not found: {}",
                    task_id
                )));
            };
            if rec.status.is_terminal() {
                return match &rec.outcome {
                    Some(TaskOutcome::Result(res)) => {
                        let mut out = res.clone();
                        if let Some(obj) = out.as_object_mut() {
                            let meta = obj.entry("_meta".to_string()).or_insert_with(|| json!({}));
                            if let Some(meta_obj) = meta.as_object_mut() {
                                meta_obj.insert(
                                    META_RELATED_TASK.to_string(),
                                    json!({ "taskId": task_id }),
                                );
                            }
                        }
                        Ok(out)
                    }
                    Some(TaskOutcome::Error(err)) => Err(err.clone()),
                    None => Err(ProtocolError::invalid_params(format!(
                        "Task '{}' was cancelled before producing a result",
                        task_id
                    ))),
                };
            }
            let (next_store, _) = self
                .state_changed
                .wait_timeout(store, Duration::from_millis(250))
                .unwrap_or_else(|e| e.into_inner());
            store = next_store;
        }
    }

    /// Handle `subscriptions/listen` for `2026-07-28` `notifications/tasks`.
    pub(crate) fn handle_subscribe(
        &self,
        id: &Value,
        params: Option<&Value>,
        ctx: &RequestContext,
    ) -> Result<(), ProtocolError> {
        if !ctx.is_modern() {
            return Err(ProtocolError::method_not_found("subscriptions/listen"));
        }
        let task_ids_val = params
            .and_then(|p| p.get("notifications"))
            .and_then(|n| n.get("taskIds"))
            .and_then(Value::as_array)
            .ok_or_else(|| {
                ProtocolError::invalid_params(
                    "subscriptions/listen requires params.notifications.taskIds array",
                )
            })?;
        if !ctx.has_extension(TASKS_EXTENSION_ID) {
            return Err(missing_tasks_capability_error());
        }

        let mut accepted: Vec<String> = Vec::new();
        let mut initial_snapshots: Vec<Value> = Vec::new();
        {
            let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
            Self::purge_expired_locked(&mut store);
            for raw in task_ids_val {
                let Some(tid) = raw.as_str() else {
                    return Err(ProtocolError::invalid_params(
                        "params.notifications.taskIds must contain strings",
                    ));
                };
                accepted.push(tid.to_string());
                if let Some(rec) = store.tasks.get(tid) {
                    if rec.status.is_terminal() || rec.status == TaskStatus::InputRequired {
                        let mut payload = rec.modern_detailed_json();
                        attach_subscription_meta(&mut payload, id);
                        initial_snapshots.push(payload);
                    }
                }
            }
            store.subscriptions.insert(
                id_key(id),
                TaskSubscription {
                    subscription_id: id.clone(),
                    task_ids: accepted.iter().cloned().collect(),
                },
            );
        }

        let _ = self.writer.send_notification(
            "notifications/subscriptions/acknowledged",
            json!({
                "_meta": {
                    META_SUBSCRIPTION_ID: id
                },
                "notifications": {
                    "taskIds": accepted
                }
            }),
        );
        for snapshot in initial_snapshots {
            let _ = self
                .writer
                .send_notification("notifications/tasks", snapshot);
        }
        Ok(())
    }

    pub(crate) fn unsubscribe(&self, subscription_id: &Value) -> bool {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        store
            .subscriptions
            .remove(&id_key(subscription_id))
            .is_some()
    }

    pub(crate) fn cancel_all(&self) {
        let mut store = self.store.lock().unwrap_or_else(|e| e.into_inner());
        for rec in store.tasks.values_mut() {
            rec.cancel.store(true, Ordering::SeqCst);
            if !rec.status.is_terminal() {
                rec.status = TaskStatus::Cancelled;
                rec.touch();
            }
        }
        self.state_changed.notify_all();
    }

    /// Write an error response for a failed `subscriptions/listen` call.
    pub(crate) fn send_subscribe_error(&self, id: Value, err: &ProtocolError) -> bool {
        self.writer
            .send_json(&error_response_from(id, err))
            .is_err()
    }
}

fn collect_status_notifications(store: &TaskStore, task_id: &str, out: &mut Vec<(String, Value)>) {
    let Some(rec) = store.tasks.get(task_id) else {
        return;
    };
    match rec.era {
        ProtocolEra::Legacy => {
            out.push((
                "notifications/tasks/status".to_string(),
                rec.legacy_task_json(),
            ));
        }
        ProtocolEra::Modern => {
            for sub in store.subscriptions.values() {
                if sub.task_ids.contains(task_id) {
                    let mut payload = rec.modern_detailed_json();
                    attach_subscription_meta(&mut payload, &sub.subscription_id);
                    out.push(("notifications/tasks".to_string(), payload));
                }
            }
        }
    }
}

fn attach_subscription_meta(payload: &mut Value, subscription_id: &Value) {
    if let Some(obj) = payload.as_object_mut() {
        let meta = obj.entry("_meta".to_string()).or_insert_with(|| json!({}));
        if let Some(meta_obj) = meta.as_object_mut() {
            meta_obj.insert(META_SUBSCRIPTION_ID.to_string(), subscription_id.clone());
        }
    }
}

fn required_task_id<'a>(params: Option<&'a Value>, method: &str) -> Result<&'a str, ProtocolError> {
    params
        .and_then(|p| p.get("taskId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            ProtocolError::invalid_params(format!(
                "{} requires non-empty string params.taskId",
                method
            ))
        })
}

fn parse_list_cursor(params: Option<&Value>, total: usize) -> Result<usize, ProtocolError> {
    let Some(cursor) = params.and_then(|p| p.get("cursor")) else {
        return Ok(0);
    };
    let cursor = cursor
        .as_str()
        .ok_or_else(|| ProtocolError::invalid_params("tasks/list cursor must be a string"))?;
    let index = cursor
        .parse::<usize>()
        .map_err(|_| ProtocolError::invalid_params("Invalid tasks/list cursor"))?;
    if index > total {
        return Err(ProtocolError::invalid_params("Invalid tasks/list cursor"));
    }
    Ok(index)
}

fn first_text_summary(call_result: &Value) -> Option<String> {
    call_result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .map(|s| s.lines().next().unwrap_or(s).chars().take(200).collect())
}

fn iso_now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
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

    fn test_manager() -> (Arc<TaskManager>, Arc<Mutex<Vec<u8>>>) {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let writer = StdioWriter::from_writer(SharedBuffer(buf.clone()));
        (Arc::new(TaskManager::new(writer)), buf)
    }

    fn modern_ctx(with_tasks: bool) -> RequestContext {
        let caps = if with_tasks {
            json!({ "extensions": { TASKS_EXTENSION_ID: {} } })
        } else {
            json!({})
        };
        RequestContext {
            era: ProtocolEra::Modern,
            protocol_version: "2026-07-28".to_string(),
            client_capabilities: caps,
            progress_token: None,
            request_id: json!(1),
        }
    }

    fn legacy_ctx() -> RequestContext {
        RequestContext {
            era: ProtocolEra::Legacy,
            protocol_version: "2025-11-25".to_string(),
            client_capabilities: json!({}),
            progress_token: None,
            request_id: json!(1),
        }
    }

    #[test]
    fn modern_task_requires_capability_and_keeps_tool_errors_completed() {
        let (mgr, _) = test_manager();
        let (task_id, _, created) = mgr
            .create_task(ProtocolEra::Modern, "agent_browser_wait_ms", None, true)
            .unwrap();
        assert_eq!(created["resultType"], "task");
        assert_eq!(created["status"], "working");
        assert_eq!(created["taskId"], task_id);

        // Missing capability returns -32021.
        let err = mgr
            .handle_get(Some(&json!({ "taskId": task_id })), &modern_ctx(false))
            .unwrap_err();
        assert_eq!(err.code, -32021);
        assert!(
            err.data.unwrap()["requiredCapabilities"]["extensions"][TASKS_EXTENSION_ID].is_object()
        );

        // Tool-level isError: true must stay "completed" in 2026-07-28 ext-tasks.
        mgr.complete_task(
            &task_id,
            TaskOutcome::Result(json!({
                "content": [{ "type": "text", "text": "Element not found" }],
                "isError": true
            })),
        );
        let polled = mgr
            .handle_get(Some(&json!({ "taskId": task_id })), &modern_ctx(true))
            .unwrap();
        assert_eq!(polled["resultType"], "complete");
        assert_eq!(polled["status"], "completed");
        assert_eq!(polled["result"]["isError"], true);
    }

    #[test]
    fn legacy_task_maps_tool_is_error_to_failed_and_blocks_tasks_result() {
        let (mgr, _) = test_manager();
        let (task_id, _, created) = mgr
            .create_task(
                ProtocolEra::Legacy,
                "agent_browser_open",
                Some(60_000),
                true,
            )
            .unwrap();
        assert_eq!(created["task"]["status"], "working");
        assert_eq!(created["task"]["ttl"], 60_000);

        let waiter_mgr = Arc::clone(&mgr);
        let waiter_id = task_id.clone();
        let waiter = thread::spawn(move || waiter_mgr.wait_for_legacy_result(&waiter_id));

        thread::sleep(Duration::from_millis(50));
        mgr.complete_task(
            &task_id,
            TaskOutcome::Result(json!({
                "content": [{ "type": "text", "text": "Navigation timed out" }],
                "isError": true
            })),
        );

        let result = waiter.join().unwrap().unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(result["_meta"][META_RELATED_TASK]["taskId"], task_id);

        let polled = mgr
            .handle_get(Some(&json!({ "taskId": task_id })), &legacy_ctx())
            .unwrap();
        assert_eq!(polled["status"], "failed");
        assert_eq!(polled["statusMessage"], "Navigation timed out");

        // Cancelling a terminal legacy task returns -32602.
        let cancel_err = mgr
            .handle_cancel(Some(&json!({ "taskId": task_id })), &legacy_ctx())
            .unwrap_err();
        assert_eq!(cancel_err.code, -32602);
    }

    #[test]
    fn modern_task_input_required_and_tasks_update_round_trip() {
        let (mgr, _) = test_manager();
        let (task_id, _, _) = mgr
            .create_task(ProtocolEra::Modern, "agent_browser_download", None, true)
            .unwrap();

        let worker_mgr = Arc::clone(&mgr);
        let worker_id = task_id.clone();
        let worker = thread::spawn(move || {
            worker_mgr.request_task_input(
                &worker_id,
                "confirm_1".to_string(),
                json!({
                    "method": "elicitation/create",
                    "params": { "mode": "form", "message": "Approve download?" }
                }),
                "Waiting for user approval".to_string(),
                Duration::from_secs(2),
            )
        });

        thread::sleep(Duration::from_millis(40));
        let waiting = mgr
            .handle_get(Some(&json!({ "taskId": task_id })), &modern_ctx(true))
            .unwrap();
        assert_eq!(waiting["status"], "input_required");
        assert!(waiting["inputRequests"]["confirm_1"].is_object());

        let ack = mgr
            .handle_update(
                Some(&json!({
                    "taskId": task_id,
                    "inputResponses": {
                        "confirm_1": { "action": "accept", "content": { "approve": true } }
                    }
                })),
                &modern_ctx(true),
            )
            .unwrap();
        assert_eq!(ack["resultType"], "complete");

        let received = worker.join().unwrap().unwrap();
        assert_eq!(received["action"], "accept");
    }

    #[test]
    fn modern_subscriptions_listen_emits_ack_and_task_updates() {
        let (mgr, buf) = test_manager();
        let (task_id, _, _) = mgr
            .create_task(ProtocolEra::Modern, "agent_browser_wait_ms", None, true)
            .unwrap();

        mgr.handle_subscribe(
            &json!("sub-1"),
            Some(&json!({ "notifications": { "taskIds": [task_id] } })),
            &modern_ctx(true),
        )
        .unwrap();

        mgr.complete_task(
            &task_id,
            TaskOutcome::Result(
                json!({ "content": [{ "type": "text", "text": "ok" }], "isError": false }),
            ),
        );

        let raw = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        let msgs: Vec<Value> = raw
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(msgs.len(), 2);
        assert_eq!(
            msgs[0]["method"],
            "notifications/subscriptions/acknowledged"
        );
        assert_eq!(msgs[0]["params"]["_meta"][META_SUBSCRIPTION_ID], "sub-1");
        assert_eq!(msgs[1]["method"], "notifications/tasks");
        assert_eq!(msgs[1]["params"]["status"], "completed");
        assert_eq!(msgs[1]["params"]["_meta"][META_SUBSCRIPTION_ID], "sub-1");
    }

    #[test]
    fn modern_grace_window_returns_cancelled_and_removes_unexposed_task() {
        let (mgr, _) = test_manager();
        let (task_id, cancel, _) = mgr
            .create_task(ProtocolEra::Modern, "agent_browser_wait_ms", None, false)
            .unwrap();

        let waiter_mgr = Arc::clone(&mgr);
        let waiter_id = task_id.clone();
        let waiter = thread::spawn(move || {
            waiter_mgr.wait_or_expose_modern(&waiter_id, Duration::from_secs(5))
        });

        thread::sleep(Duration::from_millis(30));
        cancel.store(true, Ordering::SeqCst);
        mgr.notify_state_change();

        let res = waiter.join().unwrap();
        assert!(matches!(res, GraceWaitResult::Cancelled));

        // Task was never exposed and must be cleaned up.
        let err = mgr
            .handle_get(Some(&json!({ "taskId": task_id })), &modern_ctx(true))
            .unwrap_err();
        assert_eq!(err.code, -32602);
    }
}
