use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

use super::cdp::client::CdpClient;

const CAPTURE_FPS: u32 = 10;
// Keep only the tail of ffmpeg stderr so diagnostics remain available without
// allowing the pipe to block the encoder.
const FFMPEG_STDERR_TAIL_BYTES: usize = 8 * 1024;
// Bound stop latency so a wedged task does not hang the CLI forever.
const RECORDING_STOP_TIMEOUT_MS: u64 = 5_000;

pub struct RecordingState {
    pub active: bool,
    pub output_path: String,
    pub frame_count: u64,
    pub capture_task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    pub shared_frame_count: Option<Arc<AtomicU64>>,
    pub cancel_tx: Option<oneshot::Sender<()>>,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active: false,
            output_path: String::new(),
            frame_count: 0,
            capture_task: None,
            shared_frame_count: None,
            cancel_tx: None,
        }
    }
}

pub fn recording_start(state: &mut RecordingState, path: &str) -> Result<Value, String> {
    if state.active {
        return Err("Recording already active".to_string());
    }

    state.active = true;
    state.output_path = path.to_string();
    state.frame_count = 0;

    Ok(json!({ "started": true, "path": path }))
}

pub fn recording_stop(state: &mut RecordingState) -> Result<Value, String> {
    if !state.active {
        return Err("No recording in progress".to_string());
    }

    state.active = false;

    if state.frame_count == 0 {
        return Err("No frames captured".to_string());
    }

    Ok(json!({ "path": &state.output_path, "frames": state.frame_count }))
}

pub fn recording_restart(state: &mut RecordingState, path: &str) -> Result<Value, String> {
    let previous = if state.active {
        let stop_result = recording_stop(state);
        stop_result
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(String::from))
    } else {
        None
    };

    recording_start(state, path)?;

    Ok(json!({
        "restarted": true,
        "previousPath": previous,
        "path": path,
    }))
}

fn build_ffmpeg_command(output_path: &str) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("ffmpeg");

    cmd.args(["-y"])
        .args(["-avioflags", "direct"])
        .args([
            "-fpsprobesize",
            "0",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
        ])
        .args([
            "-f",
            "image2pipe",
            "-c:v",
            "mjpeg",
            "-framerate",
            &CAPTURE_FPS.to_string(),
            "-i",
            "pipe:0",
        ])
        .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);

    if output_path.ends_with(".webm") {
        cmd.args(["-c:v", "libvpx", "-crf", "30", "-b:v", "1M"]);
    } else {
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast"]);
    }

    cmd.args(["-pix_fmt", "yuv420p", "-threads", "1"])
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    cmd
}

/// Spawn a background task that records browser frames via `Page.startScreencast`.
///
/// Chrome pushes frames automatically as CDP events — no polling, no
/// `Page.captureScreenshot` commands competing with user commands for the
/// compositor. The only CDP write is `Page.screencastFrameAck`.
///
/// The previous `Page.captureScreenshot` approach sent 10 CDP commands/second
/// that contended with user commands (`eval`, `navigate`, `snapshot`) for
/// Chrome's compositor, causing intermittent timeouts on JS-heavy pages.
pub fn spawn_recording_task(
    client: Arc<CdpClient>,
    session_id: String,
    output_path: String,
    shared_count: Arc<AtomicU64>,
    cancel_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Result<(), String>> {
    tokio::spawn(async move {
        let mut cancel_rx = std::pin::pin!(cancel_rx);

        let mut ffmpeg = build_ffmpeg_command(&output_path).spawn().map_err(|e| {
            format!(
                "ffmpeg not found or failed to execute: {}. Install ffmpeg to enable recording.",
                e
            )
        })?;

        let mut stdin = ffmpeg
            .stdin
            .take()
            .ok_or_else(|| "Failed to open ffmpeg stdin".to_string())?;
        let stderr = ffmpeg
            .stderr
            .take()
            .ok_or_else(|| "Failed to open ffmpeg stderr".to_string())?;

        // Drain ffmpeg stderr in background to prevent pipe backpressure
        // from blocking the encoder (see PR #1197 for the original diagnosis).
        let stderr_task = tokio::spawn(async move {
            let mut stderr = stderr;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 2048];

            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        if buf.len() > FFMPEG_STDERR_TAIL_BYTES {
                            let overflow = buf.len() - FFMPEG_STDERR_TAIL_BYTES;
                            buf.drain(..overflow);
                        }
                    }
                    Err(_) => break,
                }
            }

            buf
        });

        // Subscribe to CDP events BEFORE starting screencast
        let mut event_rx = client.subscribe();

        // Start screencast — Chrome pushes JPEG frames as events
        let start_result = client
            .send_command(
                "Page.startScreencast",
                Some(json!({
                    "format": "jpeg",
                    "quality": 80,
                    "maxWidth": 1920,
                    "maxHeight": 1080,
                    "everyNthFrame": 1,
                })),
                Some(&session_id),
            )
            .await;

        if let Err(e) = start_result {
            drop(stdin);
            let _ = ffmpeg.kill().await;
            return Err(format!("Page.startScreencast failed: {}", e));
        }

        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,
                event = event_rx.recv() => {
                    match event {
                        Ok(evt) if evt.method == "Page.screencastFrame" => {
                            // Ack so Chrome sends the next frame.
                            // Use evt.session_id (not the closed-over session_id) — matches
                            // the cdp_loop.rs pattern and handles session reattach/target swaps.
                            if let Some(sid) = evt.params.get("sessionId").and_then(|v| v.as_i64()) {
                                let _ = client.send_command(
                                    "Page.screencastFrameAck",
                                    Some(json!({ "sessionId": sid })),
                                    evt.session_id.as_deref(),
                                ).await;
                            }

                            if let Some(data) = evt.params.get("data").and_then(|v| v.as_str()) {
                                let bytes = match base64::Engine::decode(
                                    &base64::engine::general_purpose::STANDARD,
                                    data,
                                ) {
                                    Ok(b) => b,
                                    Err(_) => continue,
                                };

                                if stdin.write_all(&bytes).await.is_err() {
                                    break;
                                }
                                shared_count.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Ok(_) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        }

        // Stop screencast
        let _ = client
            .send_command_no_params("Page.stopScreencast", Some(&session_id))
            .await;

        drop(stdin);

        let status = ffmpeg
            .wait()
            .await
            .map_err(|e| format!("ffmpeg wait failed: {}", e))?;
        let stderr = stderr_task
            .await
            .map_err(|e| format!("ffmpeg stderr task failed: {}", e))?;

        if !status.success() {
            let stderr = String::from_utf8_lossy(&stderr);
            let stderr = stderr.trim();
            if stderr.is_empty() {
                return Err(format!("ffmpeg exited with status {}", status));
            }
            return Err(format!("ffmpeg exited with status {}: {}", status, stderr));
        }

        Ok(())
    })
}

pub async fn stop_recording_task(state: &mut RecordingState) -> Result<(), String> {
    if let Some(tx) = state.cancel_tx.take() {
        let _ = tx.send(());
    }

    let counter = state.shared_frame_count.take();
    let handle = state.capture_task.take();

    let result = if let Some(mut h) = handle {
        match tokio::time::timeout(Duration::from_millis(RECORDING_STOP_TIMEOUT_MS), &mut h).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(e)) => Err(format!("Recording task panicked: {}", e)),
            Err(_) => {
                h.abort();
                let _ = h.await;
                Err("Timed out stopping recording task".to_string())
            }
        }
    } else {
        Ok(())
    };

    if let Some(c) = counter {
        state.frame_count = c.load(Ordering::Relaxed);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::pending;

    #[test]
    fn test_recording_state_new() {
        let state = RecordingState::new();
        assert!(!state.active);
        assert!(state.output_path.is_empty());
        assert_eq!(state.frame_count, 0);
    }

    #[test]
    fn test_recording_start_sets_active() {
        let mut state = RecordingState::new();
        let result = recording_start(&mut state, "/tmp/test.mp4");
        assert!(result.is_ok());
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/test.mp4");
        assert_eq!(state.frame_count, 0);
    }

    #[test]
    fn test_recording_start_while_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test1.mp4").unwrap();
        let result = recording_start(&mut state, "/tmp/test2.mp4");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already active"));
    }

    #[test]
    fn test_recording_stop_not_active() {
        let mut state = RecordingState::new();
        let result = recording_stop(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No recording"));
    }

    #[test]
    fn test_recording_stop_no_frames() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test.mp4").unwrap();
        let result = recording_stop(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No frames"));
        assert!(!state.active);
    }

    #[test]
    fn test_recording_restart_while_inactive() {
        let mut state = RecordingState::new();
        let result = recording_restart(&mut state, "/tmp/new.webm");
        assert!(result.is_ok());
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/new.webm");
    }

    #[test]
    fn test_recording_restart_while_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/old.webm").unwrap();
        state.frame_count = 10;
        let result = recording_restart(&mut state, "/tmp/new.webm").unwrap();
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/new.webm");
        assert_eq!(state.frame_count, 0);
        assert_eq!(result["previousPath"], "/tmp/old.webm");
    }

    #[test]
    fn test_build_ffmpeg_command_webm() {
        let cmd = build_ffmpeg_command("/tmp/out.webm");
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libvpx"));
        assert!(args_str.contains(&"/tmp/out.webm"));
    }

    #[test]
    fn test_build_ffmpeg_command_mp4() {
        let cmd = build_ffmpeg_command("/tmp/out.mp4");
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libx264"));
        assert!(args_str.contains(&"/tmp/out.mp4"));
    }

    #[tokio::test]
    async fn test_stop_recording_task_times_out_and_aborts_hung_task() {
        let mut state = RecordingState::new();
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let shared_count = Arc::new(AtomicU64::new(7));

        state.cancel_tx = Some(cancel_tx);
        state.shared_frame_count = Some(shared_count);
        state.capture_task = Some(tokio::spawn(async move {
            pending::<Result<(), String>>().await
        }));

        let result = stop_recording_task(&mut state).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Timed out"));
        assert_eq!(state.frame_count, 7);
        assert!(state.capture_task.is_none());
    }
}
