use base64::{engine::general_purpose::STANDARD, Engine};
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

pub struct TracingState {
    pub active: bool,
    pub events: Vec<Value>,
    pub events_dropped: bool,
}

impl TracingState {
    pub fn new() -> Self {
        Self {
            active: false,
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
    if tracing_state.active {
        return Err("Tracing already active".to_string());
    }

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

    tracing_state.active = true;
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
    if !tracing_state.active {
        return Err("No tracing in progress".to_string());
    }

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

    // Tracing has stopped even if reading its output fails.
    tracing_state.active = false;

    // If ReturnAsStream mode was used, read trace data from the IO stream.
    if let Some(handle) = stream_handle {
        let stream_data = if trace_events.is_empty() {
            read_io_stream(client, session_id, &handle).await.map(Some)
        } else {
            Ok(None)
        };
        // Release the handle before propagating an IO or decoding error.
        let _ = client
            .send_command(
                "IO.close",
                Some(json!({ "handle": handle })),
                Some(session_id),
            )
            .await;
        if let Some(stream_data) = stream_data? {
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
    }

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
    if tracing_state.active {
        return Err("Profiling/tracing already active".to_string());
    }

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

    tracing_state.active = true;
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
    if !tracing_state.active {
        return Err("No profiling in progress".to_string());
    }

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

    tracing_state.active = false;

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
    let mut data = Vec::new();
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

        data.extend(decode_io_chunk(&result)?);

        let eof = result.get("eof").and_then(|v| v.as_bool()).unwrap_or(true);
        if eof {
            break;
        }
    }
    String::from_utf8(data).map_err(|e| format!("Trace stream is not valid UTF-8: {}", e))
}

/// A CDP stream chunk may be base64 encoded, including when its boundary
/// falls inside a UTF-8 character. Decode bytes before joining the chunks.
fn decode_io_chunk(result: &Value) -> Result<Vec<u8>, String> {
    let chunk = result
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "Trace IO.read response has no data".to_string())?;
    if result.get("base64Encoded").and_then(Value::as_bool) == Some(true) {
        STANDARD
            .decode(chunk)
            .map_err(|e| format!("Invalid base64 trace chunk: {}", e))
    } else {
        Ok(chunk.as_bytes().to_vec())
    }
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
    if let Some(home) = std::env::home_dir() {
        home.join(".agent-browser").join("tmp").join("traces")
    } else {
        std::env::temp_dir().join("agent-browser").join("traces")
    }
}

fn get_profiles_dir() -> PathBuf {
    if let Some(home) = std::env::home_dir() {
        home.join(".agent-browser").join("tmp").join("profiles")
    } else {
        std::env::temp_dir().join("agent-browser").join("profiles")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    async fn trace_client(
        chunks: Vec<Value>,
        fail_read: bool,
    ) -> (CdpClient, Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let closed = Arc::new(AtomicBool::new(false));
        let seen_close = closed.clone();
        let task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut chunks = chunks.into_iter();
            while let Some(Ok(message)) = socket.next().await {
                let tokio_tungstenite::tungstenite::Message::Text(text) = message else {
                    break;
                };
                let command: Value = serde_json::from_str(&text).unwrap();
                let method = command["method"].as_str().unwrap();
                let mut response = json!({ "id": command["id"], "sessionId": "test" });
                if method == "IO.read" && fail_read {
                    response["error"] = json!({ "code": -32000, "message": "read failed" });
                } else {
                    response["result"] = match method {
                        "IO.read" => chunks.next().expect("an expected IO.read"),
                        "IO.close" => {
                            seen_close.store(true, Ordering::SeqCst);
                            json!({})
                        }
                        _ => json!({}),
                    };
                }
                socket.send(response.to_string().into()).await.unwrap();
                if method == "Tracing.end" {
                    socket
                        .send(
                            json!({
                                "method": "Tracing.tracingComplete", "sessionId": "test",
                                "params": { "stream": "trace-handle" },
                            })
                            .to_string()
                            .into(),
                        )
                        .await
                        .unwrap();
                }
            }
        });
        let client = CdpClient::connect(&format!("ws://{address}"))
            .await
            .unwrap();
        (client, closed, task)
    }

    #[tokio::test]
    async fn trace_stop_decodes_stream_bytes_and_closes_the_handle() {
        let text = json!({ "traceEvents": [{ "name": "中文" }] }).to_string();
        let split = text.find('中').unwrap() + 1;
        let chunks = vec![
            json!({ "data": STANDARD.encode(&text.as_bytes()[..split]), "base64Encoded": true, "eof": false }),
            json!({ "data": STANDARD.encode(&text.as_bytes()[split..]), "base64Encoded": true, "eof": true }),
        ];
        let (client, closed, task) = trace_client(chunks, false).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trace.json");
        let mut state = TracingState::new();
        state.active = true;
        let result = trace_stop(&client, "test", &mut state, path.to_str())
            .await
            .unwrap();
        assert_eq!(result["eventCount"], 1);
        let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["traceEvents"][0]["name"], "中文");
        assert!(closed.load(Ordering::SeqCst));
        assert!(!state.active);
        client.close().await;
        task.abort();
    }

    #[tokio::test]
    async fn trace_stop_releases_a_failed_stream_and_allows_a_new_recording() {
        let (client, closed, task) = trace_client(Vec::new(), true).await;
        let mut state = TracingState::new();
        state.active = true;
        let error = trace_stop(&client, "test", &mut state, None)
            .await
            .unwrap_err();
        assert!(error.contains("read failed"));
        assert!(closed.load(Ordering::SeqCst));
        assert!(!state.active);
        assert_eq!(
            trace_start(&client, "test", &mut state).await.unwrap()["started"],
            true
        );
        assert!(state.active);
        client.close().await;
        task.abort();
    }

    #[test]
    fn trace_chunks_preserve_encoded_and_plain_data() {
        let plain = json!({ "data": "{\"traceEvents\":[" });
        let encoded = json!({ "data": STANDARD.encode(b"]}"), "base64Encoded": true });
        let bytes = [
            decode_io_chunk(&plain).unwrap(),
            decode_io_chunk(&encoded).unwrap(),
        ]
        .concat();
        assert_eq!(String::from_utf8(bytes).unwrap(), "{\"traceEvents\":[]}");
    }

    #[test]
    fn trace_chunks_preserve_a_utf8_character_split_between_reads() {
        let bytes = "中".as_bytes();
        let first = json!({ "data": STANDARD.encode(&bytes[..1]), "base64Encoded": true });
        let second = json!({ "data": STANDARD.encode(&bytes[1..]), "base64Encoded": true });
        let joined = [
            decode_io_chunk(&first).unwrap(),
            decode_io_chunk(&second).unwrap(),
        ]
        .concat();
        assert_eq!(String::from_utf8(joined).unwrap(), "中");
    }

    #[test]
    fn trace_chunks_reject_malformed_data() {
        assert!(decode_io_chunk(&json!({ "data": "%%%", "base64Encoded": true })).is_err());
        assert!(decode_io_chunk(&json!({ "eof": true })).is_err());
    }
}
