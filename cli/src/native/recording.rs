use serde_json::{json, Value};
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};

use super::cdp::client::CdpClient;
use super::cdp::types::{AttachToTargetParams, AttachToTargetResult};

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

/// Longest gap the recorder fills with held frames, in seconds. A page that
/// produces no frames for longer than this (a hang, or a tab left in the
/// background) is held for this long and the remainder is dropped from the
/// timeline, so a stalled page cannot inflate the file.
const MAX_BACKFILL_SECS: u64 = 5;

/// Screencast frames buffered ahead of the ticker. Two absorbs the jitter
/// between Chrome's frame clock and the recorder's without letting a lower
/// recording rate fall behind the page.
const MAX_PENDING_FRAMES: usize = 2;

/// JPEG quality requested from `Page.startScreencast`. Matches the quality the
/// recorder used to request from `Page.captureScreenshot`.
const SCREENCAST_QUALITY: u32 = 80;

/// Upper bound on waiting for Chrome to acknowledge screencast teardown.
/// The page may already be gone by the time a recording stops.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Characters of ffmpeg's stderr kept in a failure message. ffmpeg prints
/// the cause last, after the banner and stream summaries, so the tail is
/// the part worth showing.
const FFMPEG_STDERR_TAIL_CHARS: usize = 400;

/// Lower-cased extension of `path`, if it has one.
fn output_extension(path: &str) -> Option<String> {
    std::path::Path::new(path)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
}

/// Reject output paths with no extension. ffmpeg picks the container from
/// the extension, so a bare name fails at `record stop` with the take
/// already lost. Anything with an extension is handed to ffmpeg as-is:
/// `.webm` gets libvpx, everything else libx264 in whatever container
/// ffmpeg knows for that extension.
pub fn validate_output_path(path: &str) -> Result<(), String> {
    match output_extension(path) {
        Some(ext) if !ext.is_empty() => Ok(()),
        _ => Err(format!(
            "Invalid output path: '{}' has no extension (use .webm or .mp4)",
            path
        )),
    }
}

/// The last [`FFMPEG_STDERR_TAIL_CHARS`] of `stderr`, cut at a line boundary
/// where one falls inside the window, with trailing whitespace removed.
fn ffmpeg_error_tail(stderr: &str) -> String {
    let trimmed = stderr.trim_end();
    let total = trimmed.chars().count();
    if total <= FFMPEG_STDERR_TAIL_CHARS {
        return trimmed.to_string();
    }
    let tail: String = trimmed
        .chars()
        .skip(total - FFMPEG_STDERR_TAIL_CHARS)
        .collect();
    // Drop the partial first line so the message starts on a whole one.
    match tail.find(['\n', '\r']) {
        Some(cut) if cut + 1 < tail.len() => tail[cut + 1..].trim_start().to_string(),
        _ => tail,
    }
}

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

/// Frames owed to the constant-rate stream at `elapsed` into the recording,
/// given `written` frames already sent. Zero when the current slot is filled.
fn frames_due(elapsed: Duration, period: Duration, written: u64) -> u64 {
    let period_us = period.as_micros().max(1);
    let slot = (elapsed.as_micros() / period_us) as u64;
    slot.saturating_add(1).saturating_sub(written)
}

/// The CDP session a recording attaches to its page target for its screencast.
///
/// Chrome keeps one screencast per session and the live stream already runs
/// one on the page session, so the recorder attaches a second flattened
/// session to the same target. The daemon's event handlers use this to avoid
/// treating that attachment as a tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureSession {
    pub target_id: String,
    /// `None` while `Target.attachToTarget` is in flight: Chrome emits
    /// `Target.attachedToTarget` before it answers, so in that window the
    /// attachment is recognised by target instead.
    pub session_id: Option<String>,
}

impl CaptureSession {
    /// Whether a `Target.attachedToTarget` event is this recorder's own
    /// attachment rather than a tab the daemon should track.
    pub fn owns_attachment(&self, target_id: &str, session_id: &str) -> bool {
        match self.session_id.as_deref() {
            Some(own) => own == session_id,
            None => self.target_id == target_id,
        }
    }
}

pub type SharedCaptureSession = Arc<Mutex<Option<CaptureSession>>>;

pub struct RecordingState {
    pub active: bool,
    pub output_path: String,
    /// Capture rate for the active (or most recent) recording.
    pub fps: u32,
    /// Frames written to the file, including frames held through gaps.
    pub frame_count: u64,
    /// Distinct frames received from the screencast.
    pub captured_count: u64,
    pub capture_task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    pub shared_frame_count: Option<Arc<AtomicU64>>,
    pub shared_captured_count: Option<Arc<AtomicU64>>,
    pub cancel_tx: Option<oneshot::Sender<()>>,
    /// Shared with the daemon's event handlers.
    pub capture_session: SharedCaptureSession,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active: false,
            output_path: String::new(),
            fps: DEFAULT_FPS,
            frame_count: 0,
            captured_count: 0,
            capture_task: None,
            shared_frame_count: None,
            shared_captured_count: None,
            cancel_tx: None,
            capture_session: Arc::new(Mutex::new(None)),
        }
    }
}

/// [`CaptureSession::owns_attachment`] against the shared slot.
pub fn owns_attachment(shared: &SharedCaptureSession, target_id: &str, session_id: &str) -> bool {
    shared
        .lock()
        .ok()
        .and_then(|guard| {
            guard
                .as_ref()
                .map(|c| c.owns_attachment(target_id, session_id))
        })
        .unwrap_or(false)
}

pub fn recording_start(
    state: &mut RecordingState,
    path: &str,
    fps: Option<u32>,
) -> Result<Value, String> {
    if state.active {
        return Err("Recording already active".to_string());
    }

    validate_output_path(path)?;
    let fps = validate_fps(fps.unwrap_or(DEFAULT_FPS))?;

    state.active = true;
    state.output_path = path.to_string();
    state.fps = fps;
    state.frame_count = 0;
    state.captured_count = 0;

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
        "capturedFrames": state.captured_count,
        "fps": state.fps,
    }))
}

fn build_ffmpeg_command(output_path: &str, fps: u32) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    let high_fps = fps > HIGH_FPS_THRESHOLD;

    // -hide_banner keeps the version and build banner out of stderr, so a
    // failure message is the cause rather than the configure line.
    cmd.args(["-y", "-hide_banner"])
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

    if output_extension(output_path).as_deref() == Some("webm") {
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

/// Attach the recorder's own flattened session to the target behind
/// `page_session_id`, publishing it in `shared` before the command is sent so
/// the resulting `Target.attachedToTarget` is recognised as the recorder's.
pub async fn attach_capture_session(
    client: &CdpClient,
    page_session_id: &str,
    shared: &SharedCaptureSession,
) -> Result<String, String> {
    let info = client
        .send_command_no_params("Target.getTargetInfo", Some(page_session_id))
        .await?;
    let target_id = info
        .get("targetInfo")
        .and_then(|t| t.get("targetId"))
        .and_then(Value::as_str)
        .ok_or("Failed to resolve recording target")?
        .to_string();

    if let Ok(mut guard) = shared.lock() {
        *guard = Some(CaptureSession {
            target_id: target_id.clone(),
            session_id: None,
        });
    }

    let attached: Result<AttachToTargetResult, String> = client
        .send_command_typed(
            "Target.attachToTarget",
            &AttachToTargetParams {
                target_id,
                flatten: true,
            },
            None,
        )
        .await;

    match attached {
        Ok(result) => {
            if let Ok(mut guard) = shared.lock() {
                if let Some(capture) = guard.as_mut() {
                    capture.session_id = Some(result.session_id.clone());
                }
            }
            Ok(result.session_id)
        }
        Err(e) => {
            if let Ok(mut guard) = shared.lock() {
                *guard = None;
            }
            Err(format!("Failed to attach recording session: {}", e))
        }
    }
}

/// Detach a capture session that was attached for recording. This is best
/// effort because the page or browser may already be gone.
pub async fn detach_capture_session(client: &CdpClient, capture_session: &str) {
    let _ = tokio::time::timeout(
        TEARDOWN_TIMEOUT,
        client.send_command(
            "Target.detachFromTarget",
            Some(json!({ "sessionId": capture_session })),
            None,
        ),
    )
    .await;
}

fn ffmpeg_launch_error(error: impl std::fmt::Display) -> String {
    format!(
        "ffmpeg not found or failed to execute: {}. Install ffmpeg to enable recording.",
        error
    )
}

/// Verify that ffmpeg can run without opening or modifying the destination.
/// This keeps missing-binary failures ahead of browser attachment while the
/// real encoder process is deferred until that attachment succeeds.
pub async fn check_ffmpeg_available() -> Result<(), String> {
    let status = tokio::process::Command::new("ffmpeg")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(ffmpeg_launch_error)?;
    if !status.success() {
        return Err(ffmpeg_launch_error(status));
    }
    Ok(())
}

/// Start the encoder process after the browser capture session is ready.
pub fn spawn_ffmpeg(output_path: &str, fps: u32) -> Result<tokio::process::Child, String> {
    spawn_ffmpeg_command(&mut build_ffmpeg_command(output_path, fps))
}

fn spawn_ffmpeg_command(
    command: &mut tokio::process::Command,
) -> Result<tokio::process::Child, String> {
    command.spawn().map_err(ffmpeg_launch_error)
}

/// Spawn a background task that screencasts `capture_session` into the
/// already running `ffmpeg` at `fps`. Chrome pushes a frame on every repaint up to the display rate; a
/// wall-clock ticker writes one frame per slot, holding the last one through
/// gaps, so the file's duration matches the automation it recorded.
#[allow(clippy::too_many_arguments)]
pub fn spawn_recording_task(
    client: Arc<CdpClient>,
    capture_session: String,
    mut ffmpeg: tokio::process::Child,
    fps: u32,
    shared_count: Arc<AtomicU64>,
    shared_captured: Arc<AtomicU64>,
    cancel_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Result<(), String>> {
    tokio::spawn(async move {
        let fps = validate_fps(fps)?;
        let period = frame_period(fps);
        let max_frames_per_tick = MAX_BACKFILL_SECS * fps as u64 + 1;

        // Frames go to a private channel so the daemon's other subscribers
        // neither copy them nor overflow on them. Subscribe before starting
        // the screencast: Chrome sends the first frame immediately.
        let events = client.subscribe_session(&capture_session);

        let stdin = ffmpeg
            .stdin
            .take()
            .ok_or_else(|| "Failed to open ffmpeg stdin".to_string())?;

        let started = client
            .send_command(
                "Page.startScreencast",
                Some(json!({
                    "format": "jpeg",
                    "quality": SCREENCAST_QUALITY,
                    // Always 1: Chrome skips frames by count, and a static
                    // page produces exactly one, which a higher value would
                    // drop, leaving nothing to record.
                    "everyNthFrame": 1,
                })),
                Some(&capture_session),
            )
            .await;

        let capture = match started {
            Ok(_) => {
                capture_frames(
                    &client,
                    &capture_session,
                    events,
                    stdin,
                    period,
                    max_frames_per_tick,
                    &shared_count,
                    &shared_captured,
                    cancel_rx,
                )
                .await
            }
            Err(e) => {
                drop(stdin);
                Err(format!("Failed to start screencast: {}", e))
            }
        };

        client.unsubscribe_session(&capture_session);

        // Best effort: the page may already be closed.
        let _ = tokio::time::timeout(
            TEARDOWN_TIMEOUT,
            client.send_command_no_params("Page.stopScreencast", Some(&capture_session)),
        )
        .await;
        detach_capture_session(&client, &capture_session).await;

        let output = ffmpeg
            .wait_with_output()
            .await
            .map_err(|e| format!("ffmpeg wait failed: {}", e))?;

        capture?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("ffmpeg failed: {}", ffmpeg_error_tail(&stderr)));
        }

        Ok(())
    })
}

/// Pump screencast frames into ffmpeg until cancelled, the page goes away, or
/// the pipe closes. Takes ownership of `stdin` so ffmpeg sees EOF on return.
#[allow(clippy::too_many_arguments)]
async fn capture_frames(
    client: &CdpClient,
    capture_session: &str,
    mut events: mpsc::Receiver<super::cdp::types::CdpEvent>,
    mut stdin: tokio::process::ChildStdin,
    period: Duration,
    max_frames_per_tick: u64,
    shared_count: &AtomicU64,
    shared_captured: &AtomicU64,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let mut cancel_rx = std::pin::pin!(cancel_rx);
    let started = tokio::time::Instant::now();
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Frames waiting to be written, in arrival order, and the last frame
    // written (repeated through gaps). Chrome's frame clock and the ticker
    // are not phase-locked, so a slot sometimes receives two frames and the
    // next none; the queue carries the spare across instead of dropping it
    // and repeating its predecessor.
    let mut pending: VecDeque<Vec<u8>> = VecDeque::new();
    let mut last: Option<Vec<u8>> = None;
    let mut written: u64 = 0;

    loop {
        tokio::select! {
            _ = &mut cancel_rx => break,
            event = events.recv() => {
                let Some(event) = event else { break };
                if event.method == "Page.screencastFrame" {
                    if let Some(sid) = event.params.get("sessionId").and_then(Value::as_i64) {
                        let _ = client
                            .send_command_no_wait(
                                "Page.screencastFrameAck",
                                Some(json!({ "sessionId": sid })),
                                Some(capture_session),
                            )
                            .await;
                    }
                    let decoded = event
                        .params
                        .get("data")
                        .and_then(Value::as_str)
                        .and_then(|data| {
                            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data)
                                .ok()
                        });
                    if let Some(bytes) = decoded {
                        pending.push_back(bytes);
                        // Only a lower recording rate lets the queue grow (a
                        // 60 Hz screencast into a 30 fps file); dropping the
                        // oldest keeps the picture current.
                        if pending.len() > MAX_PENDING_FRAMES {
                            pending.pop_front();
                        }
                        shared_captured.fetch_add(1, Ordering::Relaxed);
                    }
                } else if event.method == "Inspector.detached" {
                    // The recorded page was closed; finish the file.
                    break;
                }
            }
            _ = interval.tick() => {
                if pending.is_empty() && last.is_none() {
                    continue;
                }
                let due = frames_due(started.elapsed(), period, written);
                if due == 0 {
                    continue;
                }
                // A gap longer than MAX_BACKFILL_SECS is held for that long
                // and the rest is dropped, so a hung page does not inflate
                // the file. Advancing `written` by the full amount is what
                // stops the excess being paid off on later ticks.
                let emit = due.min(max_frames_per_tick);
                let mut write_failed = false;
                for _ in 0..emit {
                    if let Some(next) = pending.pop_front() {
                        last = Some(next);
                    }
                    let Some(frame) = last.as_deref() else { break };
                    if stdin.write_all(frame).await.is_err() {
                        write_failed = true;
                        break;
                    }
                }
                if write_failed {
                    break;
                }
                written += due;
                shared_count.fetch_add(emit, Ordering::Relaxed);
            }
        }
    }

    drop(stdin);
    Ok(())
}

pub async fn stop_recording_task(state: &mut RecordingState) -> Result<(), String> {
    if let Some(tx) = state.cancel_tx.take() {
        let _ = tx.send(());
    }

    let counter = state.shared_frame_count.take();
    let captured = state.shared_captured_count.take();
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
    if let Some(c) = captured {
        state.captured_count = c.load(Ordering::Relaxed);
    }
    if let Ok(mut guard) = state.capture_session.lock() {
        *guard = None;
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
    fn test_recording_start_rejects_extensionless_path() {
        let mut state = RecordingState::new();
        for path in [
            "/tmp/take",
            "/tmp/dir.v2/take",
            "/tmp/.hidden",
            "/tmp/take.",
        ] {
            let err = recording_start(&mut state, path, None).unwrap_err();
            assert!(err.contains(path), "error should name the path: {}", err);
            assert!(err.contains("no extension"), "error was: {}", err);
            assert!(err.contains(".webm"), "error should suggest .webm: {}", err);
            assert!(err.contains(".mp4"), "error should suggest .mp4: {}", err);
            assert!(!state.active);
        }
    }

    #[test]
    fn test_validate_output_path_accepts_any_extension() {
        // Only the two documented formats are tuned, but ffmpeg muxes
        // libx264 into .mkv/.mov/.avi fine and those worked before the
        // check existed, so anything with an extension passes.
        for path in [
            "take.webm",
            "take.mp4",
            "./out/TAKE.WEBM",
            "/abs/path/Take.Mp4",
            "dotted.name.webm",
            "take.mkv",
            "take.mov",
            "take.avi",
            "take.webm.part",
            "dir.v2/take.MP4",
        ] {
            assert!(validate_output_path(path).is_ok(), "{} should pass", path);
        }
    }

    #[test]
    fn test_validate_output_path_rejects_missing_extension() {
        for path in ["take", "take.", ".hidden", ".webm", "dir.v2/take", "dir/"] {
            assert!(validate_output_path(path).is_err(), "{} should fail", path);
        }
        assert_eq!(
            validate_output_path("take").unwrap_err(),
            "Invalid output path: 'take' has no extension (use .webm or .mp4)"
        );
    }

    #[test]
    fn test_build_ffmpeg_command_matches_extension_case_insensitively() {
        let cmd = build_ffmpeg_command("/tmp/OUT.WEBM", DEFAULT_FPS);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libvpx"));
        assert!(!args_str.contains(&"libx264"));
    }

    #[test]
    fn test_build_ffmpeg_command_hides_banner() {
        let cmd = build_ffmpeg_command("/tmp/out.webm", DEFAULT_FPS);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        assert!(args.contains(&std::ffi::OsStr::new("-hide_banner")));
    }

    #[test]
    fn test_ffmpeg_error_tail_keeps_short_output_whole() {
        let stderr = "Unable to choose an output format for 'take'\nError opening output files: Invalid argument\n";
        assert_eq!(
            ffmpeg_error_tail(stderr),
            "Unable to choose an output format for 'take'\nError opening output files: Invalid argument"
        );
    }

    #[test]
    fn test_ffmpeg_error_tail_keeps_the_cause_not_the_banner() {
        // A realistic failure: banner and configure line first, the cause
        // last. The old 300-character head never reached the cause.
        let banner = format!(
            "ffmpeg version 8.1 Copyright (c) 2000-2026 the FFmpeg developers\n  built with clang\n  configuration: {}\n",
            "--enable-libx264 ".repeat(40)
        );
        let cause = "[out#0 @ 0x1] Unable to choose an output format for './take'; use a specific format\nError opening output file ./take.\nError opening output files: Invalid argument";
        let stderr = format!("{}{}\n", banner, cause);

        let tail = ffmpeg_error_tail(&stderr);
        assert!(tail.ends_with(cause), "tail was: {}", tail);
        assert!(!tail.contains("ffmpeg version"), "tail was: {}", tail);
        assert!(tail.chars().count() <= FFMPEG_STDERR_TAIL_CHARS);
        // The window opened mid-way through the configure line; that
        // partial line is dropped so the message starts on a whole one.
        assert!(tail.starts_with("[out#0"), "tail was: {}", tail);
    }

    #[test]
    fn test_ffmpeg_error_tail_keeps_a_single_long_line() {
        let line = "x".repeat(FFMPEG_STDERR_TAIL_CHARS + 50);
        let tail = ffmpeg_error_tail(&line);
        assert_eq!(tail.chars().count(), FFMPEG_STDERR_TAIL_CHARS);
    }

    #[test]
    fn test_ffmpeg_error_tail_handles_multibyte_and_progress_lines() {
        // ffmpeg's progress output uses carriage returns; the cut must not
        // split a multi-byte character.
        let stderr = format!(
            "{}\rframe=   12 fps=0.0 q=0.0\rError: ✗ muxer",
            "é".repeat(500)
        );
        let tail = ffmpeg_error_tail(&stderr);
        assert!(tail.ends_with("Error: ✗ muxer"), "tail was: {}", tail);
        assert!(tail.starts_with("frame="), "tail was: {}", tail);
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
    fn test_capture_session_matches_by_target_while_attach_is_in_flight() {
        let shared: SharedCaptureSession = Arc::new(Mutex::new(Some(CaptureSession {
            target_id: "T-REC".into(),
            session_id: None,
        })));
        assert!(owns_attachment(&shared, "T-REC", "S-ANY"));
        assert!(!owns_attachment(&shared, "T-OTHER", "S-ANY"));
    }

    #[test]
    fn test_capture_session_matches_by_session_once_attached() {
        let shared: SharedCaptureSession = Arc::new(Mutex::new(Some(CaptureSession {
            target_id: "T-REC".into(),
            session_id: Some("S-REC".into()),
        })));
        assert!(owns_attachment(&shared, "T-REC", "S-REC"));
        // A later attachment to the same target (e.g. the daemon's own) is
        // not the recorder's.
        assert!(!owns_attachment(&shared, "T-REC", "S-OTHER"));
        assert!(!owns_attachment(
            &Arc::new(Mutex::new(None)),
            "T-REC",
            "S-REC"
        ));
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
            assert_eq!(frames_due(elapsed, period, slot), 1);
        }
    }

    #[test]
    fn test_frames_due_is_zero_when_slot_already_written() {
        let period = frame_period(60);
        // Two screencast frames in one slot: the second waits for the next
        // tick rather than stretching the file.
        assert_eq!(frames_due(period / 2, period, 1), 0);
        assert_eq!(frames_due(Duration::ZERO, period, 10), 0);
    }

    #[test]
    fn test_frames_due_backfills_missed_slots() {
        let period = frame_period(60);
        // The ticker wakes in slot 3 with only slot 0 written, so the two
        // skipped slots are held along with the current one.
        assert_eq!(frames_due(period * 3, period, 1), 3);
    }

    /// Replays the ticker's bookkeeping for a 60s stall followed by on-time
    /// ticks. The cap must bound the file, not just one tick: without
    /// advancing `written` by the full deficit, every later tick would emit
    /// another five seconds of held frames until the stall was paid off.
    #[test]
    fn test_backfill_cap_bounds_a_long_stall() {
        let fps = 30u32;
        let period = frame_period(fps);
        let max_frames = MAX_BACKFILL_SECS * fps as u64 + 1;
        let mut written = 0u64;
        let mut emitted = 0u64;

        let mut elapsed = Duration::from_secs(60);
        let due = frames_due(elapsed, period, written);
        assert!(due > max_frames);
        emitted += due.min(max_frames);
        written += due;

        for _ in 0..20 {
            elapsed += period;
            let due = frames_due(elapsed, period, written);
            assert_eq!(due, 1, "ticks after the stall must emit one frame each");
            emitted += due.min(max_frames);
            written += due;
        }

        assert_eq!(emitted, max_frames + 20);
    }

    #[tokio::test]
    async fn test_spawn_ffmpeg_reports_missing_binary() {
        let mut command = tokio::process::Command::new("agent-browser-no-such-ffmpeg");
        let err = spawn_ffmpeg_command(&mut command).unwrap_err();
        assert!(err.contains("ffmpeg not found"), "{err}");
        assert!(err.contains("Install ffmpeg"), "{err}");
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
