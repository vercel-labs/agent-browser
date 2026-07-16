use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use super::browser::BrowserManager;

pub const API_VERSION: u64 = 1;
const DEFAULT_MAX_SIZE_BYTES: u64 = 512 * 1024 * 1024;

pub fn error_code(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.contains("not supported") || lower.contains("only supported") {
        "coverage_unsupported_engine"
    } else if lower.contains("already active") {
        "coverage_capture_active"
    } else if lower.contains("no code coverage") {
        "coverage_no_capture"
    } else if lower.contains("target") && (lower.contains("closed") || lower.contains("left")) {
        "coverage_target_gone"
    } else if lower.contains("size limit") {
        "coverage_size_limit"
    } else {
        "coverage_command_failed"
    }
}

#[derive(Clone, Debug)]
pub struct ActiveCoverage {
    pub capture_id: String,
    pub browser_session: String,
    pub target_id: String,
    pub cdp_session_id: String,
    pub url: String,
    pub started_at: String,
    pub call_count: bool,
    pub checkpoint_count: u64,
}

impl ActiveCoverage {
    fn status(&self) -> Value {
        json!({
            "active": true,
            "captureId": self.capture_id,
            "browserSession": self.browser_session,
            "targetId": self.target_id,
            "url": self.url,
            "startedAt": self.started_at,
            "callCount": self.call_count,
            "checkpointCount": self.checkpoint_count,
        })
    }
}

#[derive(Clone, Default)]
pub struct CoverageState {
    active: Arc<Mutex<Option<ActiveCoverage>>>,
}

impl CoverageState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn begin(&self, capture: ActiveCoverage) -> Result<(), String> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(existing) = active.as_ref() {
            return Err(format!(
                "A code coverage capture is already active for target {} (captureId: {})",
                existing.target_id, existing.capture_id
            ));
        }
        *active = Some(capture);
        Ok(())
    }

    pub fn active_capture(&self) -> Option<ActiveCoverage> {
        self.active
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn status(&self) -> Value {
        self.active_capture()
            .map(|capture| capture.status())
            .unwrap_or_else(|| json!({ "active": false }))
    }

    fn next_checkpoint(&self, capture_id: &str) -> Result<ActiveCoverage, String> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let capture = active
            .as_mut()
            .filter(|capture| capture.capture_id == capture_id)
            .ok_or_else(|| "No code coverage capture is active".to_string())?;
        capture.checkpoint_count += 1;
        Ok(capture.clone())
    }

    pub fn finish(&self, capture_id: &str) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active
            .as_ref()
            .is_some_and(|capture| capture.capture_id == capture_id)
        {
            *active = None;
        }
    }

    pub fn cancel_target(&self, target_id: &str) {
        if self
            .active_capture()
            .is_some_and(|capture| capture.target_id == target_id)
        {
            let mut active = self
                .active
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            *active = None;
        }
    }

    pub fn cancel_all(&self) {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *active = None;
    }
}

pub fn with_api_version(mut data: Value) -> Value {
    if let Some(object) = data.as_object_mut() {
        object.insert("coverageApiVersion".to_string(), json!(API_VERSION));
    }
    data
}

pub async fn start(
    mgr: &BrowserManager,
    state: &CoverageState,
    browser_session: &str,
    call_count: bool,
) -> Result<Value, String> {
    let (target_id, cdp_session_id, url) = active_page(mgr)?;
    let capture = ActiveCoverage {
        capture_id: format!("cov_{}", uuid::Uuid::new_v4().simple()),
        browser_session: browser_session.to_string(),
        target_id,
        cdp_session_id,
        url,
        started_at: now_rfc3339(),
        call_count,
        checkpoint_count: 0,
    };
    state.begin(capture.clone())?;

    let result = async {
        mgr.client
            .send_command_no_params("Profiler.enable", Some(&capture.cdp_session_id))
            .await?;
        mgr.client
            .send_command(
                "Profiler.startPreciseCoverage",
                Some(json!({
                    "callCount": call_count,
                    "detailed": true,
                    "allowTriggeredUpdates": false,
                })),
                Some(&capture.cdp_session_id),
            )
            .await
    }
    .await;

    if let Err(error) = result {
        state.finish(&capture.capture_id);
        let _ = mgr
            .client
            .send_command_no_params("Profiler.disable", Some(&capture.cdp_session_id))
            .await;
        return Err(error);
    }

    Ok(capture.status())
}

pub async fn take(
    mgr: &BrowserManager,
    state: &CoverageState,
    path: Option<&str>,
    label: Option<&str>,
    max_size_bytes: Option<u64>,
) -> Result<Value, String> {
    let capture = state
        .active_capture()
        .ok_or_else(|| "No code coverage capture is active".to_string())?;
    if !mgr.has_target(&capture.target_id) {
        state.finish(&capture.capture_id);
        return Err(format!(
            "The target bound to code coverage capture {} has left or closed",
            capture.capture_id
        ));
    }

    let result = mgr
        .client
        .send_command_no_params(
            "Profiler.takePreciseCoverage",
            Some(&capture.cdp_session_id),
        )
        .await?;
    let checkpoint = state.next_checkpoint(&capture.capture_id)?;
    let scripts = result.get("result").cloned().unwrap_or_else(|| json!([]));
    let script_count = scripts.as_array().map(|items| items.len()).unwrap_or(0);
    let function_count = scripts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|script| script.get("functions").and_then(Value::as_array))
        .map(Vec::len)
        .sum::<usize>();
    let range_count = scripts
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|script| script.get("functions").and_then(Value::as_array))
        .flatten()
        .filter_map(|function| function.get("ranges").and_then(Value::as_array))
        .map(Vec::len)
        .sum::<usize>();
    let captured_at = now_rfc3339();
    let artifact = json!({
        "schemaVersion": 1,
        "captureId": checkpoint.capture_id,
        "checkpoint": checkpoint.checkpoint_count,
        "label": label,
        "browserSession": checkpoint.browser_session,
        "targetId": checkpoint.target_id,
        "url": checkpoint.url,
        "startedAt": checkpoint.started_at,
        "capturedAt": captured_at,
        "timestamp": result.get("timestamp"),
        "scripts": scripts,
    });
    let output_path = prepare_output_path(path, &checkpoint, label)?;
    write_artifact(&output_path, &artifact, max_size_bytes)?;
    let file_size = fs::metadata(&output_path)
        .map_err(|error| format!("Failed to inspect code coverage artifact: {}", error))?
        .len();

    Ok(json!({
        "captureId": checkpoint.capture_id,
        "checkpoint": checkpoint.checkpoint_count,
        "label": label,
        "browserSession": checkpoint.browser_session,
        "targetId": checkpoint.target_id,
        "url": checkpoint.url,
        "capturedAt": captured_at,
        "path": output_path.to_string_lossy(),
        "fileSize": file_size,
        "scriptCount": script_count,
        "functionCount": function_count,
        "rangeCount": range_count,
    }))
}

pub async fn stop(
    mgr: &BrowserManager,
    state: &CoverageState,
    path: Option<&str>,
    label: Option<&str>,
    max_size_bytes: Option<u64>,
) -> Result<Value, String> {
    let capture = state
        .active_capture()
        .ok_or_else(|| "No code coverage capture is active".to_string())?;
    let result = take(mgr, state, path, label, max_size_bytes).await;
    let _ = mgr
        .client
        .send_command_no_params(
            "Profiler.stopPreciseCoverage",
            Some(&capture.cdp_session_id),
        )
        .await;
    let _ = mgr
        .client
        .send_command_no_params("Profiler.disable", Some(&capture.cdp_session_id))
        .await;
    state.finish(&capture.capture_id);
    result
}

pub async fn cancel(mgr: &BrowserManager, state: &CoverageState) -> Result<Value, String> {
    let capture = state
        .active_capture()
        .ok_or_else(|| "No code coverage capture is active".to_string())?;
    let _ = mgr
        .client
        .send_command_no_params(
            "Profiler.stopPreciseCoverage",
            Some(&capture.cdp_session_id),
        )
        .await;
    let _ = mgr
        .client
        .send_command_no_params("Profiler.disable", Some(&capture.cdp_session_id))
        .await;
    state.finish(&capture.capture_id);
    Ok(json!({
        "cancelled": true,
        "captureId": capture.capture_id,
        "targetId": capture.target_id,
    }))
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

fn prepare_output_path(
    path: Option<&str>,
    capture: &ActiveCoverage,
    label: Option<&str>,
) -> Result<PathBuf, String> {
    let suffix = label.map(sanitize_label).filter(|value| !value.is_empty());
    let default_name = suffix
        .map(|value| {
            format!(
                "{}-{}-{}.coverage.json",
                capture.capture_id, capture.checkpoint_count, value
            )
        })
        .unwrap_or_else(|| {
            format!(
                "{}-{}.coverage.json",
                capture.capture_id, capture.checkpoint_count
            )
        });
    let output = path.map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir()
            .join("agent-browser-coverage")
            .join(&capture.browser_session)
            .join(default_name)
    });
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "Failed to create code coverage artifact directory {}: {}",
                parent.display(),
                error
            )
        })?;
    }
    Ok(output)
}

fn write_artifact(
    path: &Path,
    artifact: &Value,
    max_size_bytes: Option<u64>,
) -> Result<(), String> {
    let partial = path.with_extension("coverage.json.partial");
    let file = File::create(&partial)
        .map_err(|error| format!("Failed to create {}: {}", partial.display(), error))?;
    let mut writer = BufWriter::new(file);
    if let Err(error) = serde_json::to_writer(&mut writer, artifact) {
        drop(writer);
        let _ = fs::remove_file(&partial);
        return Err(format!("Failed to write code coverage artifact: {}", error));
    }
    writer
        .flush()
        .map_err(|error| format!("Failed to finish code coverage artifact: {}", error))?;
    drop(writer);
    let file_size = fs::metadata(&partial)
        .map_err(|error| format!("Failed to inspect code coverage artifact: {}", error))?
        .len();
    let max_size = max_size_bytes.unwrap_or(DEFAULT_MAX_SIZE_BYTES);
    if file_size > max_size {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "Code coverage artifact exceeded the size limit of {} bytes",
            max_size
        ));
    }
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("Failed to replace {}: {}", path.display(), error))?;
    }
    fs::rename(&partial, path)
        .map_err(|error| format!("Failed to finalize {}: {}", path.display(), error))
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string())
}
