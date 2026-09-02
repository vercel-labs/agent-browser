use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use super::cdp::client::CdpClient;
use super::cdp::types::{CaptureScreenshotParams, CaptureScreenshotResult};

/// Capture rate used when the caller does not ask for one. 30 fps reads as
/// smooth motion, so scrolls, hovers, and CSS transitions survive the
/// recording instead of turning into a slideshow.
pub const DEFAULT_FPS: u32 = 30;

/// Highest capture rate the recorder accepts. 60 fps is worth asking for on
/// short, motion-heavy clips (drag interactions, animation, scroll polish
/// work) where the extra temporal detail is the point.
pub const MAX_FPS: u32 = 60;

/// Rate above which the encoder switches to its high-frame-rate profile:
/// twice the bitrate budget and a second encoder thread, so the pipe does not
/// become the bottleneck and stall the capture loop.
const HIGH_FPS_THRESHOLD: u32 = 30;

/// Bitrate budget for WebM at [`HIGH_FPS_THRESHOLD`], scaled linearly with
/// the requested rate. VP8 at 60 fps needs roughly twice the bits to hold the
/// same per-frame quality.
const WEBM_BITRATE_KBPS_AT_BASE_FPS: u32 = 1000;

/// Longest stall a single lagging capture may backfill with held frames, in
/// seconds. Bounds file growth when a page hangs for minutes.
const MAX_BACKFILL_SECS: u64 = 5;

/// Reject frame rates the pipeline cannot honor.
pub fn validate_fps(fps: u32) -> Result<u32, String> {
    if fps == 0 || fps > MAX_FPS {
        return Err(format!(
            "Invalid fps: {} is out of range (valid range: 1-{})",
            fps, MAX_FPS
        ));
    }
    Ok(fps)
}

/// Wall-clock duration of one frame at `fps`.
fn frame_period(fps: u32) -> Duration {
    Duration::from_micros(1_000_000 / fps.clamp(1, MAX_FPS) as u64)
}

/// Number of frames to emit for a capture that landed `elapsed` into the
/// recording, given `written` frames already in the stream.
///
/// The capture loop can fall behind: `Page.captureScreenshot` on a busy page
/// takes longer than the frame budget, and at 60 fps that budget is under
/// 17ms. ffmpeg reads the pipe as a constant-rate stream, so a dropped frame
/// shortens playback rather than holding the picture, and a ten second
/// interaction plays back in five. Repeating the frame that was on screen
/// during the gap keeps playback aligned with wall clock at any rate.
fn frames_due(elapsed: Duration, period: Duration, written: u64, max_frames: u64) -> u64 {
    let period_us = period.as_micros().max(1);
    let slot = (elapsed.as_micros() / period_us) as u64;
    let due = slot.saturating_add(1).saturating_sub(written);
    due.clamp(1, max_frames.max(1))
}

pub struct RecordingState {
    pub active: bool,
    pub output_path: String,
    /// Capture rate for the active (or most recent) recording.
    pub fps: u32,
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
            fps: DEFAULT_FPS,
            frame_count: 0,
            capture_task: None,
            shared_frame_count: None,
            cancel_tx: None,
        }
    }
}

pub fn recording_start(
    state: &mut RecordingState,
    path: &str,
    fps: Option<u32>,
) -> Result<Value, String> {
    if state.active {
        return Err("Recording already active".to_string());
    }

    let fps = validate_fps(fps.unwrap_or(DEFAULT_FPS))?;

    state.active = true;
    state.output_path = path.to_string();
    state.fps = fps;
    state.frame_count = 0;

    Ok(json!({ "started": true, "path": path, "fps": fps }))
}

pub fn recording_stop(state: &mut RecordingState) -> Result<Value, String> {
    if !state.active {
        return Err("No recording in progress".to_string());
    }

    state.active = false;

    if state.frame_count == 0 {
        return Err("No frames captured".to_string());
    }

    Ok(json!({
        "path": &state.output_path,
        "frames": state.frame_count,
        "fps": state.fps,
    }))
}

pub fn recording_restart(
    state: &mut RecordingState,
    path: &str,
    fps: Option<u32>,
) -> Result<Value, String> {
    // Validate before tearing down the active recording so a bad rate cannot
    // stop a good take.
    let fps = validate_fps(fps.unwrap_or(DEFAULT_FPS))?;

    let previous = if state.active {
        let stop_result = recording_stop(state);
        stop_result
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(String::from))
    } else {
        None
    };

    recording_start(state, path, Some(fps))?;

    Ok(json!({
        "restarted": true,
        "previousPath": previous,
        "path": path,
        "fps": fps,
    }))
}

fn build_ffmpeg_command(output_path: &str, fps: u32) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    let high_fps = fps > HIGH_FPS_THRESHOLD;

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
            &fps.to_string(),
            "-i",
            "pipe:0",
        ])
        .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);

    if output_path.ends_with(".webm") {
        let bitrate = WEBM_BITRATE_KBPS_AT_BASE_FPS
            .max(WEBM_BITRATE_KBPS_AT_BASE_FPS.saturating_mul(fps) / HIGH_FPS_THRESHOLD.max(1));
        cmd.args(["-c:v", "libvpx", "-crf", "30"])
            .args(["-b:v", &format!("{}k", bitrate)]);
    } else {
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast"]);
    }

    // One encoder thread keeps CPU away from the browser at ordinary rates;
    // above 30 fps the encoder needs a second one to drain the pipe in time.
    cmd.args(["-pix_fmt", "yuv420p"])
        .args(["-threads", if high_fps { "2" } else { "1" }])
        .arg(output_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    cmd
}

/// Spawn a background task that captures screenshots at `fps` and pipes them
/// to ffmpeg in real-time.
///
/// Frames are paced against wall clock rather than against the tick count, so
/// a capture that overruns its budget holds the previous picture instead of
/// speeding up playback. That is what makes 30 fps (and 60 fps on shorter
/// clips) produce a video whose duration matches the automation it recorded.
pub fn spawn_recording_task(
    client: Arc<CdpClient>,
    session_id: String,
    output_path: String,
    fps: u32,
    shared_count: Arc<AtomicU64>,
    cancel_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Result<(), String>> {
    tokio::spawn(async move {
        let mut cancel_rx = std::pin::pin!(cancel_rx);

        let fps = validate_fps(fps)?;
        let period = frame_period(fps);
        let max_frames_per_capture = MAX_BACKFILL_SECS * fps as u64 + 1;

        let mut command = build_ffmpeg_command(&output_path, fps);
        let mut ffmpeg = command.spawn().map_err(|e| {
            format!(
                "ffmpeg not found or failed to execute: {}. Install ffmpeg to enable recording.",
                e
            )
        })?;

        let mut stdin = ffmpeg
            .stdin
            .take()
            .ok_or_else(|| "Failed to open ffmpeg stdin".to_string())?;

        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let params = CaptureScreenshotParams {
            format: Some("jpeg".to_string()),
            quality: Some(80),
            clip: None,
            from_surface: Some(true),
            capture_beyond_viewport: None,
        };

        let started = tokio::time::Instant::now();
        let mut written: u64 = 0;
        // Last frame written, repeated to cover gaps left by slow captures.
        let mut held: Option<Vec<u8>> = None;

        loop {
            tokio::select! {
                _ = &mut cancel_rx => break,
                _ = interval.tick() => {}
            }

            let result: Result<CaptureScreenshotResult, _> = client
                .send_command_typed("Page.captureScreenshot", &params, Some(&session_id))
                .await;

            let screenshot = match result {
                Ok(s) => s,
                Err(e) => {
                    if e.contains("Target closed") || e.contains("not found") {
                        break;
                    }
                    continue;
                }
            };

            let bytes = match base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &screenshot.data,
            ) {
                Ok(b) => b,
                Err(_) => continue,
            };

            let frames = frames_due(started.elapsed(), period, written, max_frames_per_capture);

            let mut write_failed = false;
            for _ in 1..frames {
                let filler: &[u8] = held.as_deref().unwrap_or(&bytes);
                if stdin.write_all(filler).await.is_err() {
                    write_failed = true;
                    break;
                }
            }
            if write_failed || stdin.write_all(&bytes).await.is_err() {
                break;
            }

            written += frames;
            shared_count.fetch_add(frames, Ordering::Relaxed);
            held = Some(bytes);
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
    let handle = state.capture_task.take();

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
        assert_eq!(state.fps, DEFAULT_FPS);
    }

    #[test]
    fn test_recording_start_sets_active() {
        let mut state = RecordingState::new();
        let result = recording_start(&mut state, "/tmp/test.mp4", None);
        assert!(result.is_ok());
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/test.mp4");
        assert_eq!(state.frame_count, 0);
        assert_eq!(state.fps, 30);
        assert_eq!(result.unwrap()["fps"], 30);
    }

    #[test]
    fn test_recording_start_honors_requested_fps() {
        let mut state = RecordingState::new();
        let result = recording_start(&mut state, "/tmp/test.webm", Some(60)).unwrap();
        assert_eq!(state.fps, 60);
        assert_eq!(result["fps"], 60);
    }

    #[test]
    fn test_recording_start_rejects_out_of_range_fps() {
        let mut state = RecordingState::new();
        let too_high = recording_start(&mut state, "/tmp/test.webm", Some(61));
        assert!(too_high.unwrap_err().contains("valid range: 1-60"));
        assert!(!state.active);

        let zero = recording_start(&mut state, "/tmp/test.webm", Some(0));
        assert!(zero.is_err());
        assert!(!state.active);
    }

    #[test]
    fn test_recording_start_while_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test1.mp4", None).unwrap();
        let result = recording_start(&mut state, "/tmp/test2.mp4", None);
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
        recording_start(&mut state, "/tmp/test.mp4", None).unwrap();
        let result = recording_stop(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No frames"));
        assert!(!state.active);
    }

    #[test]
    fn test_recording_stop_reports_fps() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test.webm", Some(60)).unwrap();
        state.frame_count = 120;
        let result = recording_stop(&mut state).unwrap();
        assert_eq!(result["frames"], 120);
        assert_eq!(result["fps"], 60);
    }

    #[test]
    fn test_recording_restart_while_inactive() {
        let mut state = RecordingState::new();
        let result = recording_restart(&mut state, "/tmp/new.webm", None);
        assert!(result.is_ok());
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/new.webm");
        assert_eq!(state.fps, DEFAULT_FPS);
    }

    #[test]
    fn test_recording_restart_while_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/old.webm", None).unwrap();
        state.frame_count = 10;
        let result = recording_restart(&mut state, "/tmp/new.webm", Some(60)).unwrap();
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/new.webm");
        assert_eq!(state.frame_count, 0);
        assert_eq!(state.fps, 60);
        assert_eq!(result["previousPath"], "/tmp/old.webm");
        assert_eq!(result["fps"], 60);
    }

    #[test]
    fn test_recording_restart_rejects_bad_fps_without_stopping() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/old.webm", None).unwrap();
        state.frame_count = 10;
        let result = recording_restart(&mut state, "/tmp/new.webm", Some(120));
        assert!(result.is_err());
        // The in-flight take survives an invalid rate.
        assert!(state.active);
        assert_eq!(state.output_path, "/tmp/old.webm");
        assert_eq!(state.frame_count, 10);
    }

    #[test]
    fn test_validate_fps_range() {
        assert_eq!(validate_fps(1).unwrap(), 1);
        assert_eq!(validate_fps(DEFAULT_FPS).unwrap(), 30);
        assert_eq!(validate_fps(MAX_FPS).unwrap(), 60);
        assert!(validate_fps(0).is_err());
        assert!(validate_fps(MAX_FPS + 1).is_err());
    }

    #[test]
    fn test_frame_period_matches_fps() {
        assert_eq!(frame_period(1), Duration::from_millis(1000));
        assert_eq!(frame_period(30), Duration::from_micros(33333));
        assert_eq!(frame_period(60), Duration::from_micros(16666));
    }

    #[test]
    fn test_frames_due_on_schedule_emits_one_frame() {
        let period = frame_period(30);
        for slot in 0..5u64 {
            let elapsed = period * slot as u32;
            assert_eq!(frames_due(elapsed, period, slot, 151), 1);
        }
    }

    #[test]
    fn test_frames_due_backfills_a_lagging_capture() {
        let period = frame_period(60);
        // Capture landed in slot 3 but only slot 0 has been written, so the
        // two skipped slots are held before the fresh frame.
        assert_eq!(frames_due(period * 3, period, 1, 301), 3);
    }

    #[test]
    fn test_frames_due_clamps_long_stalls() {
        let period = frame_period(30);
        let max_frames = MAX_BACKFILL_SECS * 30 + 1;
        let stall = Duration::from_secs(60);
        assert_eq!(frames_due(stall, period, 0, max_frames), max_frames);
    }

    #[test]
    fn test_frames_due_never_returns_zero() {
        let period = frame_period(30);
        // A capture that arrives early (or a clock that has not advanced)
        // still contributes exactly one frame.
        assert_eq!(frames_due(Duration::ZERO, period, 10, 151), 1);
    }

    #[test]
    fn test_build_ffmpeg_command_webm() {
        let cmd = build_ffmpeg_command("/tmp/out.webm", DEFAULT_FPS);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libvpx"));
        assert!(args_str.contains(&"/tmp/out.webm"));
        assert!(args_str.contains(&"1000k"));
    }

    #[test]
    fn test_build_ffmpeg_command_mp4() {
        let cmd = build_ffmpeg_command("/tmp/out.mp4", DEFAULT_FPS);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libx264"));
        assert!(args_str.contains(&"/tmp/out.mp4"));
    }

    #[test]
    fn test_build_ffmpeg_command_passes_framerate() {
        let cmd = build_ffmpeg_command("/tmp/out.webm", 60);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .filter_map(|a| a.to_str().map(String::from))
            .collect();
        let framerate = args
            .iter()
            .position(|a| a == "-framerate")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        assert_eq!(framerate, Some("60"));
        // 60 fps doubles the VP8 bitrate budget and adds an encoder thread.
        assert!(args.iter().any(|a| a == "2000k"));
        let threads = args
            .iter()
            .position(|a| a == "-threads")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        assert_eq!(threads, Some("2"));
    }

    #[test]
    fn test_build_ffmpeg_command_single_thread_at_default_fps() {
        let cmd = build_ffmpeg_command("/tmp/out.mp4", DEFAULT_FPS);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .filter_map(|a| a.to_str().map(String::from))
            .collect();
        let threads = args
            .iter()
            .position(|a| a == "-threads")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        assert_eq!(threads, Some("1"));
    }
}
