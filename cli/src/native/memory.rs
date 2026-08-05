use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::browser::BrowserManager;
use super::cdp::client::CdpClient;

pub const ERROR_UNSUPPORTED_ENGINE: &str = "memory_unsupported_engine";
pub const ERROR_CAPTURE_ACTIVE: &str = "memory_capture_active";
pub const ERROR_NO_CAPTURE: &str = "memory_no_capture";
pub const ERROR_TARGET_GONE: &str = "memory_target_gone";
pub const ERROR_CANCELLED: &str = "memory_capture_cancelled";
pub const ERROR_TIMEOUT: &str = "memory_capture_timeout";
pub const ERROR_SIZE_LIMIT: &str = "memory_size_limit";
pub const ERROR_INVALID_ARTIFACT: &str = "memory_invalid_artifact";
pub const API_VERSION: u64 = 1;

const DEFAULT_SAMPLING_INTERVAL: u64 = 32_768;
const DEFAULT_TOP_FUNCTIONS: usize = 20;
const DEFAULT_SNAPSHOT_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_MAX_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
const DEFAULT_ARTIFACT_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureKind {
    Sampling,
    Snapshot,
}

impl CaptureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sampling => "sampling",
            Self::Snapshot => "snapshot",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ActiveCapture {
    pub capture_id: String,
    pub kind: CaptureKind,
    pub browser_session: String,
    pub target_id: String,
    pub cdp_session_id: String,
    pub url: String,
    pub started_at: String,
    pub output_path: Option<PathBuf>,
    pub sampling_interval: Option<u64>,
    cancel_requested: Arc<AtomicBool>,
}

impl ActiveCapture {
    fn status(&self) -> Value {
        json!({
            "active": true,
            "captureId": self.capture_id,
            "captureType": self.kind.as_str(),
            "browserSession": self.browser_session,
            "targetId": self.target_id,
            "url": self.url,
            "startedAt": self.started_at,
            "outputPath": self.output_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            "samplingInterval": self.sampling_interval,
            "cancelRequested": self.cancel_requested.load(Ordering::SeqCst),
        })
    }

    fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Default)]
pub struct MemoryState {
    active: Arc<Mutex<Option<ActiveCapture>>>,
}

impl MemoryState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&self, capture: ActiveCapture) -> Result<(), String> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = active.as_ref() {
            return Err(format!(
                "A {} memory capture is already active for target {} (captureId: {})",
                existing.kind.as_str(),
                existing.target_id,
                existing.capture_id
            ));
        }
        *active = Some(capture);
        Ok(())
    }

    pub fn active_capture(&self) -> Option<ActiveCapture> {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn status(&self) -> Value {
        self.active_capture()
            .map(|capture| capture.status())
            .unwrap_or_else(|| json!({ "active": false }))
    }

    pub fn request_cancel(&self) -> Result<ActiveCapture, String> {
        let capture = self
            .active_capture()
            .ok_or_else(|| "No memory capture is active".to_string())?;
        capture.request_cancel();
        Ok(capture)
    }

    pub fn finish(&self, capture_id: &str) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active
            .as_ref()
            .is_some_and(|capture| capture.capture_id == capture_id)
        {
            *active = None;
        }
    }

    pub fn cancel_target(&self, target_id: &str) {
        if let Some(capture) = self.active_capture() {
            if capture.target_id == target_id {
                capture.request_cancel();
                if capture.kind == CaptureKind::Sampling {
                    self.finish(&capture.capture_id);
                }
            }
        }
    }

    pub fn cancel_all(&self) {
        if let Some(capture) = self.active_capture() {
            capture.request_cancel();
            if let Some(path) = capture.output_path.as_ref() {
                let _ = fs::remove_file(path);
            }
            self.finish(&capture.capture_id);
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_test_capture(&self, kind: CaptureKind) {
        self.begin(ActiveCapture {
            capture_id: "test-capture".to_string(),
            kind,
            browser_session: "test-session".to_string(),
            target_id: "test-target".to_string(),
            cdp_session_id: "test-cdp-session".to_string(),
            url: "https://example.test".to_string(),
            started_at: "now".to_string(),
            output_path: None,
            sampling_interval: None,
            cancel_requested: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
    }
}

pub fn error_code(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not supported") || lower.contains("only supported") {
        ERROR_UNSUPPORTED_ENGINE
    } else if lower.contains("already active") {
        ERROR_CAPTURE_ACTIVE
    } else if lower.contains("no memory capture") || lower.contains("no allocation sampling") {
        ERROR_NO_CAPTURE
    } else if lower.contains("target") && (lower.contains("closed") || lower.contains("left")) {
        ERROR_TARGET_GONE
    } else if lower.contains("cancel") {
        ERROR_CANCELLED
    } else if lower.contains("timed out") {
        ERROR_TIMEOUT
    } else if lower.contains("size limit") {
        ERROR_SIZE_LIMIT
    } else if lower.contains("invalid heap snapshot") || lower.contains("incomplete heap snapshot")
    {
        ERROR_INVALID_ARTIFACT
    } else {
        "memory_command_failed"
    }
}

pub fn with_api_version(mut data: Value) -> Value {
    if let Some(object) = data.as_object_mut() {
        object.insert("memoryApiVersion".to_string(), json!(API_VERSION));
    }
    data
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}

fn capture_id() -> String {
    format!("mem_{}", uuid::Uuid::new_v4().simple())
}

fn default_artifact_path(session: &str, capture_id: &str, extension: &str) -> PathBuf {
    let directory = std::env::temp_dir()
        .join("agent-browser-memory")
        .join(session);
    cleanup_expired_artifacts(&directory, DEFAULT_ARTIFACT_RETENTION);
    directory.join(format!("{}.{}", capture_id, extension))
}

fn cleanup_expired_artifacts(directory: &Path, retention: Duration) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let expired = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= retention);
        if expired {
            let _ = fs::remove_file(path);
        }
    }
}

fn prepare_output_path(path: Option<&str>, default: PathBuf) -> Result<PathBuf, String> {
    let output = path.map(PathBuf::from).unwrap_or(default);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create memory artifact directory {}: {}",
                parent.display(),
                e
            )
        })?;
    }
    Ok(output)
}

fn partial_artifact_path(output: &Path, capture_id: &str) -> PathBuf {
    let file_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memory-artifact");
    output.with_file_name(format!(".{}.{}.partial", file_name, capture_id))
}

fn finalize_artifact(partial: &Path, output: &Path) -> Result<(), String> {
    #[cfg(windows)]
    if output.exists() {
        fs::remove_file(output).map_err(|e| {
            format!(
                "Failed to replace existing memory artifact {}: {}",
                output.display(),
                e
            )
        })?;
    }
    fs::rename(partial, output).map_err(|e| {
        format!(
            "Failed to finalize memory artifact {}: {}",
            output.display(),
            e
        )
    })
}

fn active_page(mgr: &BrowserManager) -> Result<(String, String, String), String> {
    let target_id = mgr.active_target_id()?.to_string();
    let cdp_session_id = mgr.active_session_id()?.to_string();
    let url = mgr
        .pages_list()
        .into_iter()
        .find(|page| page.target_id == target_id)
        .map(|page| page.url)
        .unwrap_or_default();
    Ok((target_id, cdp_session_id, url))
}

pub async fn metrics(mgr: &BrowserManager, browser_session: &str) -> Result<Value, String> {
    let (target_id, session_id, url) = active_page(mgr)?;
    let _ = mgr
        .client
        .send_command_no_params("Performance.enable", Some(&session_id))
        .await;
    let performance = mgr
        .client
        .send_command_no_params("Performance.getMetrics", Some(&session_id))
        .await?;
    let dom = mgr
        .client
        .send_command_no_params("Memory.getDOMCounters", Some(&session_id))
        .await?;

    let metric = |name: &str| {
        performance
            .get("metrics")
            .and_then(Value::as_array)
            .and_then(|metrics| {
                metrics.iter().find_map(|entry| {
                    (entry.get("name").and_then(Value::as_str) == Some(name))
                        .then(|| entry.get("value").and_then(Value::as_f64))
                        .flatten()
                })
            })
    };

    Ok(json!({
        "browserSession": browser_session,
        "targetId": target_id,
        "url": url,
        "timestamp": now_rfc3339(),
        "jsHeapUsedSize": metric("JSHeapUsedSize"),
        "jsHeapTotalSize": metric("JSHeapTotalSize"),
        "documents": dom.get("documents").and_then(Value::as_u64),
        "nodes": dom.get("nodes").and_then(Value::as_u64),
        "jsEventListeners": dom.get("jsEventListeners").and_then(Value::as_u64),
    }))
}

pub async fn collect_garbage(mgr: &BrowserManager, browser_session: &str) -> Result<Value, String> {
    let (target_id, session_id, url) = active_page(mgr)?;
    mgr.client
        .send_command_no_params("HeapProfiler.collectGarbage", Some(&session_id))
        .await?;
    Ok(json!({
        "collected": true,
        "browserSession": browser_session,
        "targetId": target_id,
        "url": url,
        "timestamp": now_rfc3339(),
    }))
}

pub async fn sampling_start(
    mgr: &BrowserManager,
    state: &MemoryState,
    browser_session: &str,
    sampling_interval: Option<u64>,
) -> Result<Value, String> {
    let (target_id, cdp_session_id, url) = active_page(mgr)?;
    let interval = sampling_interval.unwrap_or(DEFAULT_SAMPLING_INTERVAL);
    if interval == 0 {
        return Err("Sampling interval must be greater than zero".to_string());
    }

    let capture = ActiveCapture {
        capture_id: capture_id(),
        kind: CaptureKind::Sampling,
        browser_session: browser_session.to_string(),
        target_id,
        cdp_session_id,
        url,
        started_at: now_rfc3339(),
        output_path: None,
        sampling_interval: Some(interval),
        cancel_requested: Arc::new(AtomicBool::new(false)),
    };
    state.begin(capture.clone())?;

    let start_result = async {
        mgr.client
            .send_command_no_params("HeapProfiler.enable", Some(&capture.cdp_session_id))
            .await?;
        mgr.client
            .send_command(
                "HeapProfiler.startSampling",
                Some(json!({ "samplingInterval": interval })),
                Some(&capture.cdp_session_id),
            )
            .await
    }
    .await;

    if let Err(error) = start_result {
        state.finish(&capture.capture_id);
        let _ = mgr
            .client
            .send_command_no_params("HeapProfiler.disable", Some(&capture.cdp_session_id))
            .await;
        return Err(error);
    }

    Ok(capture.status())
}

#[derive(Debug)]
struct FunctionSummary {
    function_name: String,
    url: String,
    line_number: i64,
    column_number: i64,
    self_size: u64,
}

fn collect_function_summaries(node: &Value, output: &mut Vec<FunctionSummary>) {
    let self_size = node.get("selfSize").and_then(Value::as_u64).unwrap_or(0);
    if self_size > 0 {
        let frame = node.get("callFrame").unwrap_or(&Value::Null);
        output.push(FunctionSummary {
            function_name: frame
                .get("functionName")
                .and_then(Value::as_str)
                .unwrap_or("(anonymous)")
                .to_string(),
            url: frame
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            line_number: frame.get("lineNumber").and_then(Value::as_i64).unwrap_or(0),
            column_number: frame
                .get("columnNumber")
                .and_then(Value::as_i64)
                .unwrap_or(0),
            self_size,
        });
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_function_summaries(child, output);
        }
    }
}

pub async fn sampling_stop(
    mgr: &BrowserManager,
    state: &MemoryState,
    path: Option<&str>,
    top: Option<usize>,
    max_size_bytes: Option<u64>,
) -> Result<Value, String> {
    let capture = state
        .active_capture()
        .filter(|capture| capture.kind == CaptureKind::Sampling)
        .ok_or_else(|| "No allocation sampling capture is active".to_string())?;
    if !mgr.has_target(&capture.target_id) {
        state.finish(&capture.capture_id);
        return Err(format!(
            "The target bound to capture {} has left or closed",
            capture.capture_id
        ));
    }
    if capture.is_cancelled() {
        state.finish(&capture.capture_id);
        return Err(format!(
            "Memory capture {} was cancelled",
            capture.capture_id
        ));
    }

    let stop_result = mgr
        .client
        .send_command_no_params("HeapProfiler.stopSampling", Some(&capture.cdp_session_id))
        .await;
    let _ = mgr
        .client
        .send_command_no_params("HeapProfiler.disable", Some(&capture.cdp_session_id))
        .await;
    state.finish(&capture.capture_id);
    let result = stop_result?;
    let profile = result.get("profile").cloned().unwrap_or(result);

    let output_path = prepare_output_path(
        path,
        default_artifact_path(&capture.browser_session, &capture.capture_id, "heapprofile"),
    )?;
    let partial_path = partial_artifact_path(&output_path, &capture.capture_id);
    let file = File::create(&partial_path)
        .map_err(|e| format!("Failed to create {}: {}", partial_path.display(), e))?;
    let mut writer = BufWriter::new(file);
    if let Err(error) = serde_json::to_writer(&mut writer, &profile) {
        drop(writer);
        let _ = fs::remove_file(&partial_path);
        return Err(format!("Failed to write allocation profile: {}", error));
    }
    if let Err(error) = writer.flush() {
        drop(writer);
        let _ = fs::remove_file(&partial_path);
        return Err(format!("Failed to finish allocation profile: {}", error));
    }
    drop(writer);
    let file_size = fs::metadata(&partial_path)
        .map_err(|e| format!("Failed to inspect allocation profile: {}", e))?
        .len();
    let max_size = max_size_bytes.unwrap_or(DEFAULT_MAX_SIZE_BYTES);
    if file_size > max_size {
        let _ = fs::remove_file(&partial_path);
        return Err(format!(
            "Allocation profile exceeded the size limit of {} bytes",
            max_size
        ));
    }

    let mut summaries = Vec::new();
    if let Some(head) = profile.get("head") {
        collect_function_summaries(head, &mut summaries);
    }
    summaries.sort_by(|a, b| b.self_size.cmp(&a.self_size));
    let allocation_bytes: u64 = summaries.iter().map(|entry| entry.self_size).sum();
    let top_functions: Vec<Value> = summaries
        .into_iter()
        .take(top.unwrap_or(DEFAULT_TOP_FUNCTIONS))
        .map(|entry| {
            json!({
                "functionName": entry.function_name,
                "url": entry.url,
                "lineNumber": entry.line_number,
                "columnNumber": entry.column_number,
                "selfSize": entry.self_size,
            })
        })
        .collect();
    if let Err(error) = finalize_artifact(&partial_path, &output_path) {
        let _ = fs::remove_file(&partial_path);
        return Err(error);
    }

    Ok(json!({
        "captureId": capture.capture_id,
        "captureType": "sampling",
        "browserSession": capture.browser_session,
        "targetId": capture.target_id,
        "url": capture.url,
        "startedAt": capture.started_at,
        "finishedAt": now_rfc3339(),
        "samplingInterval": capture.sampling_interval,
        "path": output_path.to_string_lossy(),
        "fileSize": file_size,
        "allocationBytes": allocation_bytes,
        "topFunctions": top_functions,
    }))
}

#[derive(Default)]
struct SnapshotValidation {
    snapshot: bool,
    nodes: bool,
    edges: bool,
    strings: bool,
    starts_with_object: bool,
    ends_with_object: bool,
    saw_content: bool,
    tail: String,
}

impl SnapshotValidation {
    fn observe(&mut self, chunk: &str) {
        if !self.saw_content {
            if let Some(first) = chunk.chars().find(|character| !character.is_whitespace()) {
                self.starts_with_object = first == '{';
                self.saw_content = true;
            }
        }
        if let Some(last) = chunk
            .chars()
            .rev()
            .find(|character| !character.is_whitespace())
        {
            self.ends_with_object = last == '}';
        }
        let joined = format!("{}{}", self.tail, chunk);
        self.snapshot |= joined.contains("\"snapshot\"");
        self.nodes |= joined.contains("\"nodes\"");
        self.edges |= joined.contains("\"edges\"");
        self.strings |= joined.contains("\"strings\"");
        self.tail = joined
            .chars()
            .rev()
            .take(32)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
    }

    fn is_valid(&self) -> bool {
        self.starts_with_object
            && self.ends_with_object
            && self.snapshot
            && self.nodes
            && self.edges
            && self.strings
    }
}

fn write_snapshot_chunk<W: Write>(
    writer: &mut W,
    validation: &mut SnapshotValidation,
    bytes_written: &mut u64,
    chunk_count: &mut u64,
    chunk: &str,
    max_size_bytes: u64,
) -> Result<(), String> {
    let new_size = bytes_written.saturating_add(chunk.len() as u64);
    if new_size > max_size_bytes {
        return Err(format!(
            "Heap snapshot exceeded the size limit of {} bytes",
            max_size_bytes
        ));
    }
    writer
        .write_all(chunk.as_bytes())
        .map_err(|e| format!("Failed to write heap snapshot: {}", e))?;
    validation.observe(chunk);
    *bytes_written = new_size;
    *chunk_count += 1;
    Ok(())
}

pub async fn snapshot(
    mgr: &BrowserManager,
    state: &MemoryState,
    browser_session: &str,
    path: Option<&str>,
    collect_garbage_first: bool,
    timeout_ms: Option<u64>,
    max_size_bytes: Option<u64>,
) -> Result<Value, String> {
    let (target_id, cdp_session_id, url) = active_page(mgr)?;
    let id = capture_id();
    let output_path = prepare_output_path(
        path,
        default_artifact_path(browser_session, &id, "heapsnapshot"),
    )?;
    let partial_path = partial_artifact_path(&output_path, &id);
    let capture = ActiveCapture {
        capture_id: id,
        kind: CaptureKind::Snapshot,
        browser_session: browser_session.to_string(),
        target_id,
        cdp_session_id,
        url,
        started_at: now_rfc3339(),
        output_path: Some(output_path.clone()),
        sampling_interval: None,
        cancel_requested: Arc::new(AtomicBool::new(false)),
    };
    state.begin(capture.clone())?;

    let result = snapshot_inner(
        &mgr.client,
        &capture,
        &partial_path,
        collect_garbage_first,
        timeout_ms.unwrap_or(DEFAULT_SNAPSHOT_TIMEOUT_MS),
        max_size_bytes.unwrap_or(DEFAULT_MAX_SIZE_BYTES),
    )
    .await;
    let _ = mgr
        .client
        .send_command_no_params("HeapProfiler.disable", Some(&capture.cdp_session_id))
        .await;
    state.finish(&capture.capture_id);

    match result {
        Ok((file_size, chunk_count, duration)) => {
            if let Err(error) = finalize_artifact(&partial_path, &output_path) {
                let _ = fs::remove_file(&partial_path);
                return Err(error);
            }
            Ok(json!({
                "captureId": capture.capture_id,
                "captureType": "snapshot",
                "browserSession": capture.browser_session,
                "targetId": capture.target_id,
                "url": capture.url,
                "startedAt": capture.started_at,
                "finishedAt": now_rfc3339(),
                "path": output_path.to_string_lossy(),
                "fileSize": file_size,
                "chunkCount": chunk_count,
                "durationMs": duration.as_millis() as u64,
                "garbageCollectedFirst": collect_garbage_first,
                "valid": true,
            }))
        }
        Err(error) => {
            let _ = fs::remove_file(&partial_path);
            Err(error)
        }
    }
}

async fn snapshot_inner(
    client: &CdpClient,
    capture: &ActiveCapture,
    output_path: &Path,
    collect_garbage_first: bool,
    timeout_ms: u64,
    max_size_bytes: u64,
) -> Result<(u64, u64, Duration), String> {
    let event_matches_session = |event_session: Option<&str>| {
        if capture.cdp_session_id.is_empty() {
            event_session.is_none() || event_session == Some("")
        } else {
            event_session == Some(capture.cdp_session_id.as_str())
        }
    };
    if collect_garbage_first {
        client
            .send_command_no_params("HeapProfiler.collectGarbage", Some(&capture.cdp_session_id))
            .await?;
    }
    client
        .send_command_no_params("HeapProfiler.enable", Some(&capture.cdp_session_id))
        .await?;

    let file = File::create(output_path)
        .map_err(|e| format!("Failed to create {}: {}", output_path.display(), e))?;
    let mut writer = BufWriter::new(file);
    let mut events = client.subscribe();
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let command = client.send_command_with_timeout(
        "HeapProfiler.takeHeapSnapshot",
        Some(json!({ "reportProgress": true, "captureNumericValue": true })),
        Some(&capture.cdp_session_id),
        Duration::from_millis(timeout_ms),
    );
    tokio::pin!(command);

    let mut command_done = false;
    let mut bytes_written = 0_u64;
    let mut chunk_count = 0_u64;
    let mut validation = SnapshotValidation::default();

    loop {
        if capture.is_cancelled() {
            let _ = client
                .send_command_no_params("HeapProfiler.disable", Some(&capture.cdp_session_id))
                .await;
            return Err(format!(
                "Memory capture {} was cancelled",
                capture.capture_id
            ));
        }

        tokio::select! {
            command_result = &mut command, if !command_done => {
                command_result?;
                command_done = true;
            }
            event_result = events.recv() => {
                match event_result {
                    Ok(event) if event_matches_session(event.session_id.as_deref()) && event.method == "HeapProfiler.addHeapSnapshotChunk" => {
                        let chunk = event.params.get("chunk").and_then(Value::as_str).ok_or_else(|| "Heap snapshot chunk was missing data".to_string())?;
                        write_snapshot_chunk(&mut writer, &mut validation, &mut bytes_written, &mut chunk_count, chunk, max_size_bytes)?;
                    }
                    Ok(event) if event.method == "Target.targetDestroyed" && event.params.get("targetId").and_then(Value::as_str) == Some(capture.target_id.as_str()) => {
                        return Err(format!("The target bound to capture {} has left or closed", capture.capture_id));
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        return Err("Heap snapshot event stream overflowed; snapshot is incomplete".to_string());
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        return Err("Heap snapshot event stream closed before completion".to_string());
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(format!("Heap snapshot timed out after {}ms", timeout_ms));
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }

        if command_done {
            while let Ok(event) = events.try_recv() {
                if event_matches_session(event.session_id.as_deref())
                    && event.method == "HeapProfiler.addHeapSnapshotChunk"
                {
                    let chunk = event
                        .params
                        .get("chunk")
                        .and_then(Value::as_str)
                        .ok_or_else(|| "Heap snapshot chunk was missing data".to_string())?;
                    write_snapshot_chunk(
                        &mut writer,
                        &mut validation,
                        &mut bytes_written,
                        &mut chunk_count,
                        chunk,
                        max_size_bytes,
                    )?;
                }
            }
            break;
        }
    }

    writer
        .flush()
        .map_err(|e| format!("Failed to finish heap snapshot: {}", e))?;
    let _ = client
        .send_command_no_params("HeapProfiler.disable", Some(&capture.cdp_session_id))
        .await;

    if bytes_written == 0 || chunk_count == 0 {
        return Err("Heap snapshot is incomplete because no chunks were received".to_string());
    }
    if !validation.is_valid() {
        return Err(
            "Invalid heap snapshot: required snapshot, nodes, edges, or strings data is missing"
                .to_string(),
        );
    }

    Ok((bytes_written, chunk_count, start.elapsed()))
}

pub async fn cancel_sampling(mgr: &BrowserManager, state: &MemoryState) -> Result<Value, String> {
    let capture = state.request_cancel()?;
    if capture.kind == CaptureKind::Snapshot {
        return Ok(json!({
            "cancelled": true,
            "captureId": capture.capture_id,
            "captureType": capture.kind.as_str(),
        }));
    }

    if mgr.has_target(&capture.target_id) {
        let _ = mgr
            .client
            .send_command_no_params("HeapProfiler.stopSampling", Some(&capture.cdp_session_id))
            .await;
        let _ = mgr
            .client
            .send_command_no_params("HeapProfiler.disable", Some(&capture.cdp_session_id))
            .await;
    }
    if let Some(path) = capture.output_path.as_ref() {
        let _ = fs::remove_file(path);
    }
    state.finish(&capture.capture_id);
    Ok(json!({
        "cancelled": true,
        "captureId": capture.capture_id,
        "captureType": capture.kind.as_str(),
    }))
}

pub fn snapshot_cancel_response(state: &MemoryState) -> Result<Value, String> {
    let capture = state.request_cancel()?;
    Ok(json!({
        "cancelled": true,
        "captureId": capture.capture_id,
        "captureType": capture.kind.as_str(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(kind: CaptureKind, id: &str) -> ActiveCapture {
        ActiveCapture {
            capture_id: id.to_string(),
            kind,
            browser_session: "test".to_string(),
            target_id: "target-1".to_string(),
            cdp_session_id: "session-1".to_string(),
            url: "https://example.test".to_string(),
            started_at: "now".to_string(),
            output_path: None,
            sampling_interval: Some(DEFAULT_SAMPLING_INTERVAL),
            cancel_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn memory_state_enforces_one_active_capture() {
        let state = MemoryState::new();
        state
            .begin(capture(CaptureKind::Sampling, "first"))
            .unwrap();
        let error = state
            .begin(capture(CaptureKind::Snapshot, "second"))
            .unwrap_err();
        assert!(error.contains("already active"));
        assert_eq!(state.status()["captureId"], "first");
    }

    #[test]
    fn cancel_and_finish_are_stable_transitions() {
        let state = MemoryState::new();
        state
            .begin(capture(CaptureKind::Snapshot, "snapshot"))
            .unwrap();
        let cancelled = state.request_cancel().unwrap();
        assert!(cancelled.is_cancelled());
        assert_eq!(state.status()["cancelRequested"], true);
        state.finish("another");
        assert_eq!(state.status()["active"], true);
        state.finish("snapshot");
        assert_eq!(state.status()["active"], false);
    }

    #[test]
    fn snapshot_validation_accepts_split_keys() {
        let mut validation = SnapshotValidation::default();
        validation.observe("{\"snap");
        validation.observe("shot\":{},\"nodes\":[],\"ed");
        validation.observe("ges\":[],\"strings\":[]}");
        assert!(validation.is_valid());
    }

    #[test]
    fn memory_error_codes_are_stable() {
        assert_eq!(
            error_code("Memory is not supported"),
            ERROR_UNSUPPORTED_ENGINE
        );
        assert_eq!(error_code("capture already active"), ERROR_CAPTURE_ACTIVE);
        assert_eq!(error_code("No memory capture is active"), ERROR_NO_CAPTURE);
        assert_eq!(error_code("target has left or closed"), ERROR_TARGET_GONE);
        assert_eq!(error_code("snapshot timed out"), ERROR_TIMEOUT);
    }

    #[test]
    fn snapshot_chunks_are_written_incrementally_and_limited() {
        let mut output = Vec::new();
        let mut validation = SnapshotValidation::default();
        let mut bytes = 0;
        let mut chunks = 0;
        write_snapshot_chunk(
            &mut output,
            &mut validation,
            &mut bytes,
            &mut chunks,
            "{\"snapshot\":{},\"nodes\":[]",
            100,
        )
        .unwrap();
        write_snapshot_chunk(
            &mut output,
            &mut validation,
            &mut bytes,
            &mut chunks,
            ",\"edges\":[],\"strings\":[]}",
            100,
        )
        .unwrap();
        assert_eq!(chunks, 2);
        assert_eq!(bytes as usize, output.len());
        assert!(validation.is_valid());
        let limit = bytes;
        assert!(write_snapshot_chunk(
            &mut output,
            &mut validation,
            &mut bytes,
            &mut chunks,
            "overflow",
            limit,
        )
        .unwrap_err()
        .contains("size limit"));
    }

    #[test]
    fn target_and_daemon_cleanup_clear_capture_state() {
        let state = MemoryState::new();
        state
            .begin(capture(CaptureKind::Sampling, "sampling"))
            .unwrap();
        state.cancel_target("another-target");
        assert_eq!(state.status()["active"], true);
        state.cancel_target("target-1");
        assert_eq!(state.status()["active"], false);

        state
            .begin(capture(CaptureKind::Snapshot, "snapshot"))
            .unwrap();
        state.cancel_all();
        assert_eq!(state.status()["active"], false);
    }
}
