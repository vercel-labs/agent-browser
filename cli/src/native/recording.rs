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
        let _ = std::fs::remove_file(&state.output_path);
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

fn check_ffmpeg_exit(frame_count: u64, succeeded: bool, stderr: &[u8]) -> Result<(), String> {
    if succeeded || frame_count == 0 {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(stderr);
    Err(format!(
        "ffmpeg failed: {}",
        stderr.chars().take(300).collect::<String>()
    ))
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

/// Spawn a background task that screencasts `capture_session` into ffmpeg at
/// `fps`. Chrome pushes a frame on every repaint up to the display rate; a
/// wall-clock ticker writes one frame per slot, holding the last one through
/// gaps, so the file's duration matches the automation it recorded.
#[allow(clippy::too_many_arguments)]
pub fn spawn_recording_task(
    client: Arc<CdpClient>,
    capture_session: String,
    output_path: String,
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

        let mut command = build_ffmpeg_command(&output_path, fps);
        let mut ffmpeg = command.spawn().map_err(|e| {
            format!(
                "ffmpeg not found or failed to execute: {}. Install ffmpeg to enable recording.",
                e
            )
        })?;

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
        let _ = tokio::time::timeout(
            TEARDOWN_TIMEOUT,
            client.send_command(
                "Target.detachFromTarget",
                Some(json!({ "sessionId": capture_session })),
                None,
            ),
        )
        .await;

        let output = ffmpeg
            .wait_with_output()
            .await
            .map_err(|e| format!("ffmpeg wait failed: {}", e))?;

        capture?;

        check_ffmpeg_exit(
            shared_count.load(Ordering::Relaxed),
            output.status.success(),
            &output.stderr,
        )?;

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
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.mp4");
        std::fs::write(&path, []).unwrap();
        let mut state = RecordingState::new();
        recording_start(&mut state, path.to_str().unwrap(), None).unwrap();
        let result = recording_stop(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No frames"));
        assert!(!state.active);
        assert!(!path.exists());
    }

    #[test]
    fn test_ffmpeg_failure_is_deferred_when_no_frames_were_written() {
        assert!(check_ffmpeg_exit(0, false, b"empty input").is_ok());
        assert_eq!(
            check_ffmpeg_exit(1, false, b"encoder failed").unwrap_err(),
            "ffmpeg failed: encoder failed"
        );
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
