use serde_json::{json, Value};
use std::path::PathBuf;

use super::cdp::client::CdpClient;

const MAX_PROFILE_EVENTS: usize = 5_000_000;

const DEFAULT_PROFILER_CATEGORIES: &[&str] = &[
    "devtools.timeline",
    "disabled-by-default-devtools.timeline",
    "disabled-by-default-devtools.timeline.frame",
    "disabled-by-default-devtools.timeline.stack",
    "v8.execute",
    "disabled-by-default-v8.cpu_profiler",
    "disabled-by-default-v8.cpu_profiler.hires",
    "v8",
    "disabled-by-default-v8.runtime_stats",
    "blink",
    "blink.user_timing",
    "latencyInfo",
    "renderer.scheduler",
    "sequence_manager",
    "toplevel",
];

/// Which command owns the single active CDP tracing session. `trace` and
/// `profiler` both drive Chrome's `Tracing` domain (only one can run at a
/// time), so we record who started it and let only the matching `stop` end it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recorder {
    Trace,
    Profiler,
}

impl Recorder {
    /// Lowercase name used in messages, e.g. "trace" / "profiler".
    fn label(self) -> &'static str {
        match self {
            Recorder::Trace => "trace",
            Recorder::Profiler => "profiler",
        }
    }

    /// The command that stops this recorder, e.g. "trace stop".
    fn stop_command(self) -> &'static str {
        match self {
            Recorder::Trace => "trace stop",
            Recorder::Profiler => "profiler stop",
        }
    }

    /// Noun for "No <x> in progress", e.g. "tracing" / "profiling".
    fn progress_noun(self) -> &'static str {
        match self {
            Recorder::Trace => "tracing",
            Recorder::Profiler => "profiling",
        }
    }

    /// Capitalized noun for "<X> already active", e.g. "Tracing" / "Profiling".
    fn active_noun(self) -> &'static str {
        match self {
            Recorder::Trace => "Tracing",
            Recorder::Profiler => "Profiling",
        }
    }
}

/// Guard for a `start` command: is it safe to start `want`?
fn ensure_startable(active: Option<Recorder>, want: Recorder) -> Result<(), String> {
    match active {
        None => Ok(()),
        Some(a) if a == want => Err(format!("{} already active", want.active_noun())),
        Some(other) => Err(format!(
            "A {} recording is already active; stop it with '{}' before starting the {}",
            other.label(),
            other.stop_command(),
            want.label()
        )),
    }
}

/// Guard for a `stop` command: only the recorder that is actually running may
/// be stopped, so `profiler stop` no longer ends a trace (and vice versa).
fn ensure_stoppable(active: Option<Recorder>, want: Recorder) -> Result<(), String> {
    match active {
        Some(a) if a == want => Ok(()),
        Some(other) => Err(format!(
            "No {} recording in progress (a {} recording is active; use '{}')",
            want.label(),
            other.label(),
            other.stop_command()
        )),
        None => Err(format!("No {} in progress", want.progress_noun())),
    }
}

pub struct TracingState {
    /// `Some(recorder)` when a trace or profiler recording is active, else `None`.
    pub active: Option<Recorder>,
    pub events: Vec<Value>,
    pub events_dropped: bool,
}

impl TracingState {
    pub fn new() -> Self {
        Self {
            active: None,
            events: Vec::new(),
            events_dropped: false,
        }
    }
}

pub async fn trace_start(
    client: &CdpClient,
    session_id: &str,
    tracing_state: &mut TracingState,
) -> Result<Value, String> {
    ensure_startable(tracing_state.active, Recorder::Trace)?;

    client
        .send_command(
            "Tracing.start",
            Some(json!({
                "traceConfig": {
                    "recordMode": "recordContinuously",
                },
                "transferMode": "ReturnAsStream",
            })),
            Some(session_id),
        )
        .await?;

    tracing_state.active = Some(Recorder::Trace);
    tracing_state.events.clear();
    tracing_state.events_dropped = false;

    Ok(json!({ "started": true }))
}

pub async fn trace_stop(
    client: &CdpClient,
    session_id: &str,
    tracing_state: &mut TracingState,
    path: Option<&str>,
) -> Result<Value, String> {
    ensure_stoppable(tracing_state.active, Recorder::Trace)?;

    // Subscribe to events before stopping
    let mut rx = client.subscribe();

    client
        .send_command_no_params("Tracing.end", Some(session_id))
        .await?;

    // Collect trace data with timeout
    let mut trace_events: Vec<Value> = Vec::new();
    let mut stream_handle: Option<String> = None;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

    loop {
        let result = tokio::time::timeout_at(deadline, rx.recv()).await;

        match result {
            Ok(Ok(event)) => {
                if event.session_id.as_deref() != Some(session_id) {
                    continue;
                }
                match event.method.as_str() {
                    "Tracing.dataCollected" => {
                        if let Some(arr) = event.params.get("value").and_then(|v| v.as_array()) {
                            trace_events.extend(arr.iter().cloned());
                        }
                    }
                    "Tracing.tracingComplete" => {
                        stream_handle = event
                            .params
                            .get("stream")
                            .and_then(|v| v.as_str())
                            .map(String::from);
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Err(_)) => break,
            Err(_) => {
                return Err("Tracing stop timed out after 30s".to_string());
            }
        }
    }

    // If ReturnAsStream mode was used, read trace data from the IO stream
    if let Some(handle) = stream_handle {
        if trace_events.is_empty() {
            let stream_data = read_io_stream(client, session_id, &handle).await?;
            if let Ok(parsed) = serde_json::from_str::<Value>(&stream_data) {
                if let Some(events) = parsed.get("traceEvents").and_then(|v| v.as_array()) {
                    trace_events.extend(events.iter().cloned());
                }
            } else {
                // Try parsing as newline-delimited JSON
                for line in stream_data.lines() {
                    if let Ok(val) = serde_json::from_str::<Value>(line) {
                        if let Some(events) = val.get("traceEvents").and_then(|v| v.as_array()) {
                            trace_events.extend(events.iter().cloned());
                        } else {
                            trace_events.push(val);
                        }
                    }
                }
            }
        }
        // Close the IO stream
        let _ = client
            .send_command(
                "IO.close",
                Some(json!({ "handle": handle })),
                Some(session_id),
            )
            .await;
    }

    tracing_state.active = None;

    let save_path = match path {
        Some(p) => p.to_string(),
        None => {
            let dir = get_traces_dir();
            let _ = std::fs::create_dir_all(&dir);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            dir.join(format!("trace-{}.json", timestamp))
                .to_string_lossy()
                .to_string()
        }
    };

    let trace_json = json!({ "traceEvents": trace_events });
    let json_str = serde_json::to_string(&trace_json)
        .map_err(|e| format!("Failed to serialize trace: {}", e))?;
    std::fs::write(&save_path, json_str)
        .map_err(|e| format!("Failed to write trace to {}: {}", save_path, e))?;

    Ok(json!({ "path": save_path, "eventCount": trace_events.len() }))
}

pub async fn profiler_start(
    client: &CdpClient,
    session_id: &str,
    tracing_state: &mut TracingState,
    categories: Option<Vec<String>>,
) -> Result<Value, String> {
    ensure_startable(tracing_state.active, Recorder::Profiler)?;

    let cats: Vec<String> = categories.unwrap_or_else(|| {
        DEFAULT_PROFILER_CATEGORIES
            .iter()
            .map(|s| s.to_string())
            .collect()
    });

    client
        .send_command(
            "Tracing.start",
            Some(json!({
                "traceConfig": {
                    "includedCategories": cats,
                    "enableSampling": true,
                },
                "transferMode": "ReportEvents",
            })),
            Some(session_id),
        )
        .await?;

    tracing_state.active = Some(Recorder::Profiler);
    tracing_state.events.clear();
    tracing_state.events_dropped = false;

    Ok(json!({ "started": true }))
}

pub async fn profiler_stop(
    client: &CdpClient,
    session_id: &str,
    tracing_state: &mut TracingState,
    path: Option<&str>,
) -> Result<Value, String> {
    ensure_stoppable(tracing_state.active, Recorder::Profiler)?;

    let mut rx = client.subscribe();

    client
        .send_command_no_params("Tracing.end", Some(session_id))
        .await?;

    let mut events: Vec<Value> = Vec::new();
    let mut dropped = false;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(30);

    loop {
        let result = tokio::time::timeout_at(deadline, rx.recv()).await;

        match result {
            Ok(Ok(event)) => {
                if event.session_id.as_deref() != Some(session_id) {
                    continue;
                }
                match event.method.as_str() {
                    "Tracing.dataCollected" => {
                        if let Some(arr) = event.params.get("value").and_then(|v| v.as_array()) {
                            if events.len() + arr.len() > MAX_PROFILE_EVENTS {
                                dropped = true;
                            } else {
                                events.extend(arr.iter().cloned());
                            }
                        }
                    }
                    "Tracing.tracingComplete" => {
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Err(_)) => break,
            Err(_) => {
                return Err("Profiler stop timed out after 30s".to_string());
            }
        }
    }

    tracing_state.active = None;

    let save_path = match path {
        Some(p) => p.to_string(),
        None => {
            let dir = get_profiles_dir();
            let _ = std::fs::create_dir_all(&dir);
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            dir.join(format!("profile-{}.json", timestamp))
                .to_string_lossy()
                .to_string()
        }
    };

    let clock_domain = get_clock_domain();
    let mut profile = json!({ "traceEvents": events });
    if let Some(cd) = clock_domain {
        profile
            .as_object_mut()
            .unwrap()
            .insert("metadata".to_string(), json!({ "clock-domain": cd }));
    }

    let json_str = serde_json::to_string(&profile)
        .map_err(|e| format!("Failed to serialize profile: {}", e))?;
    std::fs::write(&save_path, json_str)
        .map_err(|e| format!("Failed to write profile to {}: {}", save_path, e))?;

    let event_count = events.len();
    let mut result = json!({ "path": save_path, "eventCount": event_count });
    if dropped {
        result.as_object_mut().unwrap().insert(
            "warning".to_string(),
            Value::String(format!(
                "Events exceeded {} limit; some dropped",
                MAX_PROFILE_EVENTS
            )),
        );
    }

    Ok(result)
}

/// Read all data from a CDP IO stream handle.
async fn read_io_stream(
    client: &CdpClient,
    session_id: &str,
    handle: &str,
) -> Result<String, String> {
    let mut data = String::new();
    loop {
        let result = client
            .send_command(
                "IO.read",
                Some(json!({
                    "handle": handle,
                    "size": 1024 * 1024,
                })),
                Some(session_id),
            )
            .await?;

        if let Some(chunk) = result.get("data").and_then(|v| v.as_str()) {
            data.push_str(chunk);
        }

        let eof = result.get("eof").and_then(|v| v.as_bool()).unwrap_or(true);
        if eof {
            break;
        }
    }
    Ok(data)
}

fn get_clock_domain() -> Option<&'static str> {
    if cfg!(target_os = "linux") {
        Some("LINUX_CLOCK_MONOTONIC")
    } else if cfg!(target_os = "macos") {
        Some("MAC_MACH_ABSOLUTE_TIME")
    } else {
        None
    }
}

fn get_traces_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser").join("tmp").join("traces")
    } else {
        std::env::temp_dir().join("agent-browser").join("traces")
    }
}

fn get_profiles_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser").join("tmp").join("profiles")
    } else {
        std::env::temp_dir().join("agent-browser").join("profiles")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression for #1313: a stop command must act only on its own recorder,
    // never on the other type that happens to share the CDP tracing session.

    #[test]
    fn stop_refuses_the_other_recorder() {
        // trace running, `profiler stop` -> clear error, does NOT stop the trace.
        let err = ensure_stoppable(Some(Recorder::Trace), Recorder::Profiler).unwrap_err();
        assert_eq!(
            err,
            "No profiler recording in progress (a trace recording is active; use 'trace stop')"
        );

        // profiler running, `trace stop` -> clear error, does NOT stop the profiler.
        let err = ensure_stoppable(Some(Recorder::Profiler), Recorder::Trace).unwrap_err();
        assert_eq!(
            err,
            "No trace recording in progress (a profiler recording is active; use 'profiler stop')"
        );
    }

    #[test]
    fn stop_allows_the_matching_recorder() {
        assert!(ensure_stoppable(Some(Recorder::Trace), Recorder::Trace).is_ok());
        assert!(ensure_stoppable(Some(Recorder::Profiler), Recorder::Profiler).is_ok());
    }

    #[test]
    fn stop_with_nothing_active_keeps_the_original_message() {
        assert_eq!(
            ensure_stoppable(None, Recorder::Trace).unwrap_err(),
            "No tracing in progress"
        );
        assert_eq!(
            ensure_stoppable(None, Recorder::Profiler).unwrap_err(),
            "No profiling in progress"
        );
    }

    #[test]
    fn start_refuses_while_the_other_recorder_is_active() {
        let err = ensure_startable(Some(Recorder::Profiler), Recorder::Trace).unwrap_err();
        assert_eq!(
            err,
            "A profiler recording is already active; stop it with 'profiler stop' before starting the trace"
        );
        let err = ensure_startable(Some(Recorder::Trace), Recorder::Profiler).unwrap_err();
        assert_eq!(
            err,
            "A trace recording is already active; stop it with 'trace stop' before starting the profiler"
        );
    }

    #[test]
    fn start_refuses_a_duplicate_of_the_same_recorder() {
        assert_eq!(
            ensure_startable(Some(Recorder::Trace), Recorder::Trace).unwrap_err(),
            "Tracing already active"
        );
        assert_eq!(
            ensure_startable(Some(Recorder::Profiler), Recorder::Profiler).unwrap_err(),
            "Profiling already active"
        );
    }

    #[test]
    fn start_allows_when_idle() {
        assert!(ensure_startable(None, Recorder::Trace).is_ok());
        assert!(ensure_startable(None, Recorder::Profiler).is_ok());
    }
}
