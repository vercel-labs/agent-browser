use serde_json::{json, Value};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{broadcast, oneshot};

use super::cdp::client::CdpClient;
use super::cdp::types::CdpEvent;
use super::stream;

const TARGET_FPS: f64 = 25.0;
const ACK_INTERVAL_MS: u64 = 35;

pub struct RecordingState {
    pub active: bool,
    pub output_path: String,
    pub frame_count: u64,
    pub screencast_task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    pub shared_frame_count: Option<Arc<AtomicU64>>,
    pub cancel_tx: Option<oneshot::Sender<()>>,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active: false,
            output_path: String::new(),
            frame_count: 0,
            screencast_task: None,
            shared_frame_count: None,
            cancel_tx: None,
        }
    }

    pub fn has_background_task(&self) -> bool {
        self.screencast_task.is_some()
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
        .args(["-f", "image2pipe", "-c:v", "mjpeg", "-i", "pipe:0"])
        .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);

    if output_path.ends_with(".webm") {
        cmd.args([
            "-c:v",
            "libvpx-vp9",
            "-crf",
            "30",
            "-b:v",
            "0",
            "-deadline",
            "realtime",
            "-cpu-used",
            "4",
        ]);
    } else {
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast"]);
    }

    cmd.args(["-pix_fmt", "yuv420p", "-threads", "1"])
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    cmd
}

/// Spawn a background task that receives screencast frames from CDP,
/// pipes them to ffmpeg in real-time, and returns when cancelled or
/// the broadcast closes. Acks are throttled at 35ms intervals.
pub fn spawn_recording_task(
    mut event_rx: broadcast::Receiver<CdpEvent>,
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

        let mut pending_acks: VecDeque<i64> = VecDeque::new();
        let mut ack_interval = tokio::time::interval(Duration::from_millis(ACK_INTERVAL_MS));
        ack_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut last_frame: Option<Vec<u8>> = None;
        let mut last_frame_number: u64 = 0;
        let mut first_timestamp: Option<f64> = None;

        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,

                _ = ack_interval.tick() => {
                    if let Some(sid) = pending_acks.pop_front() {
                        let _ = stream::ack_screencast_frame(&client, &session_id, sid).await;
                    }
                }

                result = event_rx.recv() => {
                    let event = match result {
                        Ok(ev) => ev,
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            eprintln!("[recording] broadcast lagged, skipped {} events", n);
                            continue;
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    };

                    if event.method != "Page.screencastFrame" {
                        continue;
                    }

                    if let Some(sid) = event.params.get("sessionId").and_then(|v| v.as_i64()) {
                        pending_acks.push_back(sid);
                    }

                    let timestamp = event
                        .params
                        .get("metadata")
                        .and_then(|m| m.get("timestamp"))
                        .and_then(|t| t.as_f64())
                        .unwrap_or(0.0);

                    let relative_ts = if let Some(first) = first_timestamp {
                        timestamp - first
                    } else {
                        first_timestamp = Some(timestamp);
                        0.0
                    };

                    let frame_number = (relative_ts * TARGET_FPS).round().max(0.0) as u64;

                    let data = match event.params.get("data").and_then(|v| v.as_str()) {
                        Some(d) => d,
                        None => continue,
                    };
                    let bytes = match base64::Engine::decode(
                        &base64::engine::general_purpose::STANDARD,
                        data,
                    ) {
                        Ok(b) => b,
                        Err(_) => continue,
                    };

                    // Fill gap with repeated previous frame, then write current frame
                    if let Some(ref prev) = last_frame {
                        let gap = frame_number.saturating_sub(last_frame_number);
                        for _ in 0..gap.saturating_sub(1) {
                            if stdin.write_all(prev).await.is_err() {
                                break;
                            }
                            shared_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }

                    if stdin.write_all(&bytes).await.is_err() {
                        break;
                    }
                    shared_count.fetch_add(1, Ordering::Relaxed);

                    last_frame = Some(bytes);
                    last_frame_number = frame_number;
                }
            }
        }

        // Pad last frame for ~1 second
        if let Some(ref last) = last_frame {
            let pad_count = TARGET_FPS as u64;
            for _ in 0..pad_count {
                if stdin.write_all(last).await.is_err() {
                    break;
                }
                shared_count.fetch_add(1, Ordering::Relaxed);
            }
        }

        drop(stdin);

        let output = ffmpeg
            .wait_with_output()
            .await
            .map_err(|e| format!("ffmpeg wait failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "ffmpeg failed: {}",
                stderr.chars().take(300).collect::<String>()
            ));
        }

        Ok(())
    })
}

pub async fn stop_recording_task(state: &mut RecordingState) -> Result<(), String> {
    if let Some(tx) = state.cancel_tx.take() {
        let _ = tx.send(());
    }

    let counter = state.shared_frame_count.take();
    let handle = state.screencast_task.take();

    let result = if let Some(h) = handle {
        match h.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("Recording task panicked: {}", e)),
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
        assert!(args_str.contains(&"libvpx-vp9"));
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
}
