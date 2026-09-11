use serde_json::{json, Value};
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

/// Rate above which the live encoder uses additional threads.
const HIGH_FPS_THRESHOLD: u32 = 30;
const HIGH_FPS_ENCODER_THREADS: &str = "4";

/// VP8 budget chosen for readable UI text and thin drawing strokes.
const WEBM_BITRATE_KBPS: u32 = 8000;

/// Captured frames may wait briefly for compositing, but overload must fail
/// the recording instead of silently degrading it into held frames.
const ENCODER_FRAME_BUFFER: usize = 16;
const MAX_ENCODER_LAG: Duration = Duration::from_millis(500);
const ENCODER_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

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

fn frame_period(fps: u32) -> Duration {
    Duration::from_micros(1_000_000 / fps.clamp(1, MAX_FPS) as u64)
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
    /// Frames written to the file.
    pub frame_count: u64,
    /// Frames received from the screencast.
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
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(["-avioflags", "direct"])
        .args(["-use_wallclock_as_timestamps", "1"])
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
            "png",
            "-framerate",
            &fps.to_string(),
            "-i",
            "pipe:0",
        ])
        .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"])
        .args(["-fps_mode", "vfr"]);

    if output_extension(output_path).as_deref() == Some("webm") {
        cmd.args(["-c:v", "libvpx", "-crf", "18"])
            .args(["-b:v", &format!("{}k", WEBM_BITRATE_KBPS)])
            .args(["-deadline", "realtime", "-cpu-used", "4"]);
    } else {
        cmd.args(["-c:v", "libx264", "-preset", "ultrafast"]);
    }

    // One encoder thread keeps CPU away from the browser at ordinary rates;
    // above 30 fps the encoder needs more workers to drain the pipe in time.
    cmd.args(["-pix_fmt", "yuv420p"])
        .args([
            "-threads",
            if high_fps {
                HIGH_FPS_ENCODER_THREADS
            } else {
                "1"
            },
        ])
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

fn spawn_ffmpeg_command(
    command: &mut tokio::process::Command,
) -> Result<tokio::process::Child, String> {
    command.spawn().map_err(ffmpeg_launch_error)
}

#[derive(Clone, Debug)]
struct CapturedVideoFrame {
    image_data: Arc<Vec<u8>>,
    captured_at: tokio::time::Instant,
}

/// Drain Chrome independently from the encoder so FFmpeg cannot stall frame ACKs.
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
        let events = client.subscribe_session(&capture_session);
        let (frame_tx, frame_rx) = mpsc::channel(ENCODER_FRAME_BUFFER);
        let encoder = tokio::spawn(encode_stream(output_path, fps, frame_rx));

        let started = client
            .send_command(
                "Page.startScreencast",
                Some(json!({
                    "format": "png",
                    // Always 1: Chrome skips frames by count, and a static
                    // page produces exactly one, which a higher value would
                    // drop, leaving nothing to record.
                    "everyNthFrame": 1,
                })),
                Some(&capture_session),
            )
            .await;

        let captured = match started {
            Ok(_) => {
                collect_frames(
                    &client,
                    &capture_session,
                    events,
                    frame_tx,
                    &shared_captured,
                    cancel_rx,
                )
                .await
            }
            Err(e) => Err(format!("Failed to start screencast: {}", e)),
        };

        client.unsubscribe_session(&capture_session);

        // Best effort: the page may already be closed.
        let _ = tokio::time::timeout(
            TEARDOWN_TIMEOUT,
            client.send_command_no_params("Page.stopScreencast", Some(&capture_session)),
        )
        .await;
        detach_capture_session(&client, &capture_session).await;

        if let Err(error) = captured {
            if error == "Recording encoder stopped unexpectedly" {
                return match encoder.await {
                    Ok(Err(encoder_error)) => Err(encoder_error),
                    Ok(Ok(_)) => Err(error),
                    Err(join_error) => {
                        Err(format!("Recording encoder task failed: {}", join_error))
                    }
                };
            }
            encoder.abort();
            let _ = encoder.await;
            return Err(error);
        }
        let streamed = encoder
            .await
            .map_err(|e| format!("Recording encoder task failed: {}", e))??;
        shared_count.store(streamed, Ordering::Relaxed);
        Ok(())
    })
}

async fn collect_frames(
    client: &CdpClient,
    capture_session: &str,
    mut events: mpsc::Receiver<super::cdp::types::CdpEvent>,
    frame_tx: mpsc::Sender<CapturedVideoFrame>,
    shared_captured: &AtomicU64,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let mut cancel_rx = std::pin::pin!(cancel_rx);

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
                        let frame = CapturedVideoFrame {
                            image_data: Arc::new(bytes),
                            captured_at: tokio::time::Instant::now(),
                        };
                        shared_captured.fetch_add(1, Ordering::Relaxed);
                        frame_tx.try_send(frame).map_err(|error| match error {
                            mpsc::error::TrySendError::Full(_) => format!(
                                "Recording encoder fell behind by more than {} buffered frames",
                                ENCODER_FRAME_BUFFER
                            ),
                            mpsc::error::TrySendError::Closed(_) => {
                                "Recording encoder stopped unexpectedly".to_string()
                            }
                        })?;
                    }
                } else if event.method == "Inspector.detached" {
                    // The recorded page was closed; finish the file.
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn write_encoder_bytes(
    stdin: &mut tokio::process::ChildStdin,
    bytes: &[u8],
) -> Result<(), String> {
    tokio::time::timeout(ENCODER_WRITE_TIMEOUT, stdin.write_all(bytes))
        .await
        .map_err(|_| "Recording encoder pipe was blocked for more than 2 seconds".to_string())?
        .map_err(|e| format!("ffmpeg write failed: {}", e))
}

async fn encode_stream(
    output_path: String,
    fps: u32,
    mut frames: mpsc::Receiver<CapturedVideoFrame>,
) -> Result<u64, String> {
    let mut command = build_ffmpeg_command(&output_path, fps);
    let mut ffmpeg = spawn_ffmpeg_command(&mut command)?;
    let mut stdin = ffmpeg
        .stdin
        .take()
        .ok_or_else(|| "Failed to open ffmpeg stdin".to_string())?;
    let mut interval = tokio::time::interval(frame_period(fps));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut latest: Option<CapturedVideoFrame> = None;
    let mut last_page: Option<Arc<Vec<u8>>> = None;
    let mut written = 0u64;

    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { break };
                if frame.captured_at.elapsed() > MAX_ENCODER_LAG {
                    return Err("Recording encoder fell more than 500 ms behind capture".to_string());
                }
                latest = Some(frame);
            }
            _ = interval.tick() => {
                let Some(frame) = latest.as_ref() else { continue };
                let page_changed = last_page
                    .as_deref()
                    .is_none_or(|previous| previous != frame.image_data.as_slice());
                if !page_changed {
                    continue;
                }
                write_encoder_bytes(&mut stdin, &frame.image_data).await?;
                last_page = Some(frame.image_data.clone());
                written += 1;
            }
        }
    }

    let Some(frame) = latest.as_ref() else {
        return Err("No frames captured".to_string());
    };
    write_encoder_bytes(&mut stdin, &frame.image_data).await?;
    written += 1;
    drop(stdin);

    let output = ffmpeg
        .wait_with_output()
        .await
        .map_err(|e| format!("ffmpeg wait failed: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffmpeg failed: {}", ffmpeg_error_tail(&stderr)));
    }
    Ok(written)
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

    /// Requires FFmpeg and ffprobe, but no browser. Check the actual file timeline.
    #[tokio::test]
    #[ignore]
    async fn recording_sparse_frames_preserve_timestamps() {
        for extension in ["webm", "mp4"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("sparse.{extension}"));
            let (tx, rx) = mpsc::channel(4);
            let encoder = tokio::spawn(encode_stream(path.to_string_lossy().into_owned(), 30, rx));
            for color in [[255, 0, 0], [0, 0, 255]] {
                let image = image::RgbImage::from_pixel(64, 64, image::Rgb(color));
                let mut png = std::io::Cursor::new(Vec::new());
                image.write_to(&mut png, image::ImageFormat::Png).unwrap();
                tx.send(CapturedVideoFrame {
                    image_data: Arc::new(png.into_inner()),
                    captured_at: tokio::time::Instant::now(),
                })
                .await
                .unwrap();
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            drop(tx);
            let count = encoder.await.unwrap().unwrap();
            assert!(
                (2..=4).contains(&count),
                "expected sparse frames, got {count}"
            );
            let probe = std::process::Command::new("ffprobe")
                .args([
                    "-v",
                    "error",
                    "-show_entries",
                    "packet=pts_time",
                    "-of",
                    "json",
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                probe.status.success(),
                "{}",
                String::from_utf8_lossy(&probe.stderr)
            );
            let data: Value = serde_json::from_slice(&probe.stdout).unwrap();
            let times: Vec<f64> = data["packets"]
                .as_array()
                .unwrap()
                .iter()
                .map(|packet| packet["pts_time"].as_str().unwrap().parse().unwrap())
                .collect();
            assert!(times.len() >= 3, "{extension}: {times:?}");
            assert!((0.3..0.6).contains(&times[1]), "{extension}: {times:?}");
            assert!(
                (0.7..1.1).contains(times.last().unwrap()),
                "{extension}: {times:?}"
            );
        }
    }

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
    fn test_frame_period_matches_requested_rate() {
        assert_eq!(frame_period(1), Duration::from_secs(1));
        assert_eq!(frame_period(30), Duration::from_micros(33_333));
        assert_eq!(frame_period(60), Duration::from_micros(16_666));
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
        assert!(args_str.contains(&"8000k"));
        assert!(args_str.contains(&"18"));
        assert!(args_str.contains(&"png"));
        assert!(args_str.contains(&"-use_wallclock_as_timestamps"));
        assert!(args_str.contains(&"vfr"));
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
        assert!(args.iter().any(|a| a == "8000k"));
        let threads = args
            .iter()
            .position(|a| a == "-threads")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str);
        assert_eq!(threads, Some(HIGH_FPS_ENCODER_THREADS));
        assert!(args.iter().any(|a| a == "realtime"));
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
