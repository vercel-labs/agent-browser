use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::Path;
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

/// Changed-pixel ratio that selects a contact-sheet frame when the caller
/// does not provide one. Five percent filters minor animation while retaining
/// meaningful UI transitions.
pub const DEFAULT_CONTACT_SHEET_THRESHOLD: f64 = 0.05;

/// Contact sheets stay reviewable and memory-bounded during long recordings.
pub const MAX_CONTACT_SHEET_FRAMES: usize = 100;

const CONTACT_SHEET_COLUMNS: u32 = 4;
const CONTACT_SHEET_CELL_WIDTH: u32 = 640;
const CONTACT_SHEET_LABEL_HEIGHT: u32 = 24;
const CONTACT_SHEET_GAP: u32 = 8;
const CONTACT_SHEET_DIFF_WIDTH: u32 = 320;
const CONTACT_SHEET_TILE_SIZE: u32 = 8;
const CONTACT_SHEET_MIN_TILE_PIXELS: u32 = 4;
const CONTACT_SHEET_MIN_REGION_PIXELS: u64 = 8;
const CONTACT_SHEET_REGION_PADDING_TILES: u32 = 1;
const CONTACT_SHEET_REGION_MERGE_GAP_TILES: u32 = 4;
const CONTACT_SHEET_BURST_WINDOW_MS: u64 = 1_000;
const CONTACT_SHEET_BURST_RATE: usize = 25;
const CONTACT_SHEET_BURST_QUIET_MS: u64 = 250;
const CONTACT_SHEET_BURST_FRAMES: usize = 7;

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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RecordingCursorState {
    pub x: f64,
    pub y: f64,
    pub buttons: i32,
    pub visible: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RecordingCursorHistory {
    enabled: bool,
    samples: VecDeque<(f64, RecordingCursorState)>,
}

impl RecordingCursorHistory {
    /// Record input after Chrome acknowledges it.
    pub fn record(&mut self, x: f64, y: f64, buttons: i32) {
        if self.enabled {
            self.record_at(cursor_timestamp(), x, y, buttons);
        }
    }

    pub fn record_at(&mut self, timestamp: f64, x: f64, y: f64, buttons: i32) {
        self.samples.push_back((
            timestamp,
            RecordingCursorState {
                x,
                y,
                buttons,
                visible: true,
            },
        ));
    }

    pub fn at(&self, timestamp: f64) -> RecordingCursorState {
        self.samples
            .iter()
            .rev()
            .find(|(time, _)| *time <= timestamp)
            .map(|(_, state)| *state)
            .unwrap_or_default()
    }

    fn interpolated_at(&self, timestamp: f64) -> RecordingCursorState {
        let Some(next_index) = self.samples.iter().position(|(time, _)| *time > timestamp) else {
            return self.at(timestamp);
        };
        if next_index == 0 {
            return RecordingCursorState::default();
        }
        let (before_time, before) = self.samples[next_index - 1];
        let (after_time, after) = self.samples[next_index];
        if before.buttons != 0 || after.buttons != 0 || after_time <= before_time {
            return before;
        }
        let progress = ((timestamp - before_time) / (after_time - before_time)).clamp(0.0, 1.0);
        RecordingCursorState {
            x: before.x + (after.x - before.x) * progress,
            y: before.y + (after.y - before.y) * progress,
            ..before
        }
    }

    fn click_starts(&self) -> Vec<(f64, RecordingCursorState)> {
        let mut previous_buttons = 0;
        self.samples
            .iter()
            .filter_map(|&(timestamp, state)| {
                let started = previous_buttons == 0 && state.buttons != 0;
                previous_buttons = state.buttons;
                started.then_some((timestamp, state))
            })
            .collect()
    }
}

pub fn cursor_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

pub type SharedRecordingCursor = Arc<Mutex<RecordingCursorHistory>>;

const CURSOR_PATH: [(f64, f64); 4] = [(0.0, 0.0), (14.0, 8.5), (7.5, 10.0), (4.0, 16.0)];
const CURSOR_BASE_SCALE: f64 = 28.0 / 24.0;
const CURSOR_PRESS_SCALE: f64 = 0.8;
const CURSOR_STROKE_WIDTH: f64 = 1.5;
const CURSOR_SUPERSAMPLE: usize = 4;
const RIPPLE_DURATION_SECS: f64 = 0.4;
const RIPPLE_FILL_RADIUS: f64 = 24.0;
const RIPPLE_RING_RADIUS: f64 = 32.0;
const RIPPLE_COLOR: [u8; 3] = [96, 165, 250];

fn point_in_polygon(points: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut inside = false;
    let mut previous = points.len() - 1;
    for (current, &(cx, cy)) in points.iter().enumerate() {
        let (px, py) = points[previous];
        if (cy > y) != (py > y) && x < (px - cx) * (y - cy) / (py - cy) + cx {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn distance_to_segment(x: f64, y: f64, start: (f64, f64), end: (f64, f64)) -> f64 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length_squared = dx * dx + dy * dy;
    if length_squared == 0.0 {
        return (x - start.0).hypot(y - start.1);
    }
    let t = (((x - start.0) * dx + (y - start.1) * dy) / length_squared).clamp(0.0, 1.0);
    (x - (start.0 + t * dx)).hypot(y - (start.1 + t * dy))
}

fn distance_to_polygon(points: &[(f64, f64)], x: f64, y: f64) -> f64 {
    points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(&start, &end)| distance_to_segment(x, y, start, end))
        .fold(f64::INFINITY, f64::min)
}

fn blend_pixel(pixel: &mut image::Rgb<u8>, color: [u8; 3], alpha: f64) {
    let alpha = alpha.clamp(0.0, 1.0);
    for (channel, source) in pixel.0.iter_mut().zip(color) {
        *channel = ((*channel as f64 * (1.0 - alpha)) + (source as f64 * alpha)).round() as u8;
    }
}

fn composite_ripple(frame: &mut image::RgbImage, x: f64, y: f64, progress: f64) {
    if !(0.0..1.0).contains(&progress) {
        return;
    }
    let fill_radius = RIPPLE_FILL_RADIUS * progress;
    let ring_radius = RIPPLE_RING_RADIUS * progress;
    let fill_opacity = 0.8 * (1.0 - progress);
    let ring_opacity = 0.9 * (1.0 - progress);
    let bound = RIPPLE_RING_RADIUS.ceil() as i32 + 2;
    let origin_x = x.round() as i32;
    let origin_y = y.round() as i32;
    let samples = (CURSOR_SUPERSAMPLE * CURSOR_SUPERSAMPLE) as f64;

    for py in origin_y - bound..=origin_y + bound {
        for px in origin_x - bound..=origin_x + bound {
            if px < 0 || py < 0 || px >= frame.width() as i32 || py >= frame.height() as i32 {
                continue;
            }
            let mut fill_coverage = 0.0;
            let mut ring_coverage = 0.0;
            for sy in 0..CURSOR_SUPERSAMPLE {
                for sx in 0..CURSOR_SUPERSAMPLE {
                    let sample_x = px as f64 + (sx as f64 + 0.5) / CURSOR_SUPERSAMPLE as f64;
                    let sample_y = py as f64 + (sy as f64 + 0.5) / CURSOR_SUPERSAMPLE as f64;
                    let distance = (sample_x - x).hypot(sample_y - y);
                    fill_coverage += if distance <= fill_radius { 1.0 } else { 0.0 };
                    ring_coverage += if (distance - ring_radius).abs() <= 1.0 {
                        1.0
                    } else {
                        0.0
                    };
                }
            }
            let pixel = frame.get_pixel_mut(px as u32, py as u32);
            blend_pixel(pixel, RIPPLE_COLOR, fill_opacity * fill_coverage / samples);
            blend_pixel(pixel, RIPPLE_COLOR, ring_opacity * ring_coverage / samples);
        }
    }
}

/// Draw the recording pointer after the clean frame has been analyzed for the
/// contact sheet, keeping presentation pixels out of change detection.
fn composite_cursor(frame: &mut image::RgbImage, cursor: RecordingCursorState) {
    if !cursor.visible {
        return;
    }
    let pressed_scale = if cursor.buttons == 0 {
        1.0
    } else {
        CURSOR_PRESS_SCALE
    };
    let scale = CURSOR_BASE_SCALE * pressed_scale;
    let points: Vec<(f64, f64)> = CURSOR_PATH
        .iter()
        .map(|&(x, y)| (cursor.x + x * scale, cursor.y + y * scale))
        .collect();
    let max_x = points.iter().map(|point| point.0).fold(cursor.x, f64::max);
    let max_y = points.iter().map(|point| point.1).fold(cursor.y, f64::max);
    let samples = (CURSOR_SUPERSAMPLE * CURSOR_SUPERSAMPLE) as f64;

    for py in (cursor.y.floor() as i32 - 4)..=(max_y.ceil() as i32 + 5) {
        for px in (cursor.x.floor() as i32 - 4)..=(max_x.ceil() as i32 + 5) {
            if px < 0 || py < 0 || px >= frame.width() as i32 || py >= frame.height() as i32 {
                continue;
            }
            let mut fill_coverage = 0.0;
            let mut stroke_coverage = 0.0;
            let mut shadow_alpha = 0.0;
            for sy in 0..CURSOR_SUPERSAMPLE {
                for sx in 0..CURSOR_SUPERSAMPLE {
                    let sample_x = px as f64 + (sx as f64 + 0.5) / CURSOR_SUPERSAMPLE as f64;
                    let sample_y = py as f64 + (sy as f64 + 0.5) / CURSOR_SUPERSAMPLE as f64;
                    let inside = point_in_polygon(&points, sample_x, sample_y);
                    let edge_distance = distance_to_polygon(&points, sample_x, sample_y);
                    fill_coverage += if inside { 1.0 } else { 0.0 };
                    stroke_coverage += if edge_distance <= CURSOR_STROKE_WIDTH * scale / 2.0 {
                        1.0
                    } else {
                        0.0
                    };

                    let shadow_x = sample_x;
                    let shadow_y = sample_y - 1.0;
                    let shadow_inside = point_in_polygon(&points, shadow_x, shadow_y);
                    let shadow_distance = distance_to_polygon(&points, shadow_x, shadow_y);
                    let shadow_sample = if shadow_inside {
                        0.5
                    } else if shadow_distance < 3.0 {
                        0.5 * (1.0 - shadow_distance / 3.0).powi(2)
                    } else {
                        0.0
                    };
                    shadow_alpha += shadow_sample;
                }
            }
            let pixel = frame.get_pixel_mut(px as u32, py as u32);
            blend_pixel(pixel, [0, 0, 0], shadow_alpha / samples);
            blend_pixel(pixel, [255, 255, 255], fill_coverage / samples);
            blend_pixel(pixel, [0, 0, 0], stroke_coverage / samples);
        }
    }
}

/// PPM carries exact RGB pixels to FFmpeg and supports frame-size changes.
/// Only the video encoder compresses the composited frame.
fn cursor_video_frame(source: &[u8], cursor: RecordingCursorState) -> Result<Vec<u8>, String> {
    let mut frame = image::load_from_memory(source)
        .map_err(|e| format!("Failed to decode recording frame: {}", e))?
        .to_rgb8();
    composite_cursor(&mut frame, cursor);
    let mut output = format!("P6\n{} {}\n255\n", frame.width(), frame.height()).into_bytes();
    output.extend_from_slice(frame.as_raw());
    Ok(output)
}

pub struct RecordingState {
    pub active: bool,
    pub output_path: String,
    /// Capture rate for the active (or most recent) recording.
    pub fps: u32,
    /// Frames written to the file.
    pub frame_count: u64,
    /// Frames received from the screencast.
    pub captured_count: u64,
    pub contact_sheet_frame_count: u64,
    pub capture_task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    pub shared_frame_count: Option<Arc<AtomicU64>>,
    pub shared_captured_count: Option<Arc<AtomicU64>>,
    pub shared_contact_sheet_count: Option<Arc<AtomicU64>>,
    pub cancel_tx: Option<oneshot::Sender<()>>,
    /// Shared with the daemon's event handlers.
    pub capture_session: SharedCaptureSession,
    /// Whether the encoded video includes a synthetic pointer.
    pub cursor: bool,
    /// Pointer state composited onto encoded frames after contact-sheet analysis.
    pub shared_cursor: SharedRecordingCursor,
    /// Whether the capture task exports selected frames as a PNG sheet.
    pub contact_sheet: bool,
    pub contact_sheet_threshold: f64,
    pub contact_sheet_path: Option<String>,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active: false,
            output_path: String::new(),
            fps: DEFAULT_FPS,
            frame_count: 0,
            captured_count: 0,
            contact_sheet_frame_count: 0,
            capture_task: None,
            shared_frame_count: None,
            shared_captured_count: None,
            shared_contact_sheet_count: None,
            cancel_tx: None,
            capture_session: Arc::new(Mutex::new(None)),
            cursor: false,
            shared_cursor: Arc::new(Mutex::new(RecordingCursorHistory::default())),
            contact_sheet: false,
            contact_sheet_threshold: DEFAULT_CONTACT_SHEET_THRESHOLD,
            contact_sheet_path: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RecordingOptions {
    pub fps: Option<u32>,
    pub cursor: bool,
    pub contact_sheet: bool,
    pub contact_sheet_threshold: f64,
}

impl Default for RecordingOptions {
    fn default() -> Self {
        Self {
            fps: None,
            cursor: false,
            contact_sheet: false,
            contact_sheet_threshold: DEFAULT_CONTACT_SHEET_THRESHOLD,
        }
    }
}

pub fn validate_contact_sheet_threshold(threshold: f64) -> Result<f64, String> {
    if threshold.is_finite() && (0.0..=1.0).contains(&threshold) {
        Ok(threshold)
    } else {
        Err(format!(
            "Invalid contact sheet threshold: {} is out of range (valid range: 0-1)",
            threshold
        ))
    }
}

pub fn contact_sheet_path(output_path: &str) -> String {
    let path = Path::new(output_path);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("recording");
    let filename = format!("{}.contact-sheet.png", stem);
    path.parent()
        .unwrap_or_else(|| Path::new(""))
        .join(filename)
        .to_string_lossy()
        .to_string()
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
    options: RecordingOptions,
) -> Result<Value, String> {
    if state.active {
        return Err("Recording already active".to_string());
    }

    validate_output_path(path)?;
    let fps = validate_fps(options.fps.unwrap_or(DEFAULT_FPS))?;
    let threshold = validate_contact_sheet_threshold(options.contact_sheet_threshold)?;

    state.active = true;
    state.output_path = path.to_string();
    state.fps = fps;
    state.frame_count = 0;
    state.captured_count = 0;
    state.contact_sheet_frame_count = 0;
    state.cursor = options.cursor;
    if let Ok(mut cursor) = state.shared_cursor.lock() {
        *cursor = RecordingCursorHistory {
            enabled: options.cursor,
            ..Default::default()
        };
    }
    state.contact_sheet = options.contact_sheet;
    state.contact_sheet_threshold = threshold;
    state.contact_sheet_path = options.contact_sheet.then(|| contact_sheet_path(path));

    let mut result = json!({
        "started": true,
        "path": path,
        "fps": fps,
        "cursor": options.cursor,
        "contactSheet": options.contact_sheet
    });
    if let Some(ref contact_path) = state.contact_sheet_path {
        result["contactSheetPath"] = json!(contact_path);
        result["contactSheetThreshold"] = json!(threshold);
    }
    Ok(result)
}

pub fn recording_stop(state: &mut RecordingState) -> Result<Value, String> {
    if !state.active {
        return Err("No recording in progress".to_string());
    }

    state.active = false;

    if state.frame_count == 0 {
        return Err("No frames captured".to_string());
    }

    let mut result = json!({
        "path": &state.output_path,
        "frames": state.frame_count,
        "capturedFrames": state.captured_count,
        "fps": state.fps,
    });
    if let Some(ref path) = state.contact_sheet_path {
        result["contactSheetPath"] = json!(path);
        result["contactSheetFrames"] = json!(state.contact_sheet_frame_count);
        result["contactSheetThreshold"] = json!(state.contact_sheet_threshold);
    }
    Ok(result)
}

fn build_ffmpeg_command(output_path: &str, fps: u32, cursor: bool) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("ffmpeg");
    let high_fps = fps > HIGH_FPS_THRESHOLD;

    // -hide_banner keeps the version and build banner out of stderr, so a
    // failure message is the cause rather than the configure line.
    cmd.args(["-y", "-hide_banner", "-loglevel", "error"])
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
            if cursor { "ppm" } else { "png" },
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

#[derive(Clone, Debug)]
struct ContactSheetFrame {
    source: Arc<image::RgbImage>,
    image_data: Vec<u8>,
    elapsed_ms: u64,
    change_ratio: f64,
    cursor: RecordingCursorState,
    device_width: f64,
    device_height: f64,
}

struct ContactSheetCell {
    rendered: image::RgbaImage,
    elapsed_ms: u64,
}

struct ContactSheetBaseline {
    source: Arc<image::RgbImage>,
    preview: image::RgbImage,
}

struct ContactSheetCollector {
    threshold: f64,
    selected: Vec<ContactSheetCell>,
    pending: Vec<ContactSheetFrame>,
    burst: bool,
    previous_candidate: Option<ContactSheetBaseline>,
    previous_rendered: Option<Arc<image::RgbImage>>,
    latest: Option<ContactSheetFrame>,
    cell_height: Option<u32>,
}

impl ContactSheetCollector {
    fn new(threshold: f64) -> Self {
        Self {
            threshold,
            selected: Vec::new(),
            pending: Vec::new(),
            burst: false,
            previous_candidate: None,
            previous_rendered: None,
            latest: None,
            cell_height: None,
        }
    }

    fn commit_frame(&mut self, frame: ContactSheetFrame) {
        if self.selected.len() >= MAX_CONTACT_SHEET_FRAMES - 1 {
            return;
        }
        let cell = render_contact_cell(
            &frame,
            self.previous_rendered.as_deref(),
            self.cell_height.unwrap_or(1),
        );
        self.previous_rendered = Some(frame.source.clone());
        self.selected.push(cell);
    }

    fn flush_pending(&mut self) {
        let remaining = (MAX_CONTACT_SHEET_FRAMES - 1).saturating_sub(self.selected.len());
        let pending = std::mem::take(&mut self.pending);
        for frame in pending.into_iter().take(remaining) {
            self.commit_frame(frame);
        }
        self.burst = false;
    }

    fn flush_if_quiet(&mut self, elapsed_ms: u64) {
        if self.pending.last().is_some_and(|last| {
            elapsed_ms.saturating_sub(last.elapsed_ms) >= CONTACT_SHEET_BURST_QUIET_MS
        }) {
            self.flush_pending();
        }
    }

    fn compact_pending_burst(&mut self) {
        while self.pending.len() > CONTACT_SHEET_BURST_FRAMES {
            let remove = (1..self.pending.len() - 1)
                .min_by(|&a, &b| {
                    let score = |index: usize| {
                        let span = self.pending[index + 1]
                            .elapsed_ms
                            .saturating_sub(self.pending[index - 1].elapsed_ms)
                            as f64;
                        span * (1.0 + self.pending[index].change_ratio)
                    };
                    score(a)
                        .partial_cmp(&score(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .expect("a burst with more than two frames has an interior frame");
            self.pending.remove(remove);
        }
    }

    fn stage_candidate(&mut self, frame: ContactSheetFrame) {
        self.flush_if_quiet(frame.elapsed_ms);
        self.pending.push(frame);

        if self.burst {
            self.compact_pending_burst();
            return;
        }

        let latest_ms = self.pending.last().map_or(0, |latest| latest.elapsed_ms);
        while self.pending.first().is_some_and(|first| {
            latest_ms.saturating_sub(first.elapsed_ms) > CONTACT_SHEET_BURST_WINDOW_MS
        }) {
            let frame = self.pending.remove(0);
            self.commit_frame(frame);
        }

        if self.pending.len() > CONTACT_SHEET_BURST_RATE {
            self.burst = true;
            self.compact_pending_burst();
        }
    }

    fn consider(
        &mut self,
        image_data: &[u8],
        elapsed: Duration,
        cursor: RecordingCursorState,
        device_width: f64,
        device_height: f64,
    ) {
        let elapsed_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
        self.flush_if_quiet(elapsed_ms);
        // Encoded equality is only a shortcut; selection compares decoded pixels.
        if let Some(latest) = self
            .latest
            .as_mut()
            .filter(|frame| frame.image_data == image_data)
        {
            latest.elapsed_ms = elapsed_ms;
            latest.cursor = cursor;
            latest.device_width = device_width;
            latest.device_height = device_height;
            return;
        }
        let Ok(source) = image::load_from_memory(image_data) else {
            return;
        };
        let preview_width = source.width().clamp(1, CONTACT_SHEET_DIFF_WIDTH);
        let preview_height = ((source.height() as f64 * preview_width as f64
            / source.width().max(1) as f64)
            .round() as u32)
            .max(1);
        // Integer averaging is sufficient for selection; preserve full pixels
        // for region detection and the high-quality cell rendering below.
        let preview = source
            .thumbnail_exact(preview_width, preview_height)
            .into_rgb8();
        let source = Arc::new(source.into_rgb8());
        self.cell_height.get_or_insert_with(|| {
            ((CONTACT_SHEET_CELL_WIDTH as f64 * source.height() as f64
                / source.width().max(1) as f64)
                .round() as u32)
                .max(1)
        });
        let frame = ContactSheetFrame {
            source,
            image_data: image_data.to_vec(),
            elapsed_ms,
            change_ratio: 0.0,
            cursor,
            device_width,
            device_height,
        };
        // Reserve one slot for the final frame. Compare with the last candidate
        // so small changes accumulate instead of disappearing.
        let change_ratio = self.previous_candidate.as_ref().map_or(1.0, |previous| {
            if previous.source.dimensions() != frame.source.dimensions() {
                1.0
            } else {
                changed_pixel_ratio(&previous.preview, &preview)
            }
        });
        if change_ratio > 0.0 && change_ratio >= self.threshold {
            let mut candidate = frame.clone();
            candidate.change_ratio = change_ratio;
            self.stage_candidate(candidate);
            self.previous_candidate = Some(ContactSheetBaseline {
                source: frame.source.clone(),
                preview,
            });
        }
        self.latest = Some(frame);
    }

    fn finish(mut self) -> Vec<ContactSheetCell> {
        self.flush_pending();
        if let Some(latest) = self.latest {
            let already_selected = self
                .selected
                .last()
                .is_some_and(|frame| frame.elapsed_ms == latest.elapsed_ms);
            if !already_selected {
                let cell = render_contact_cell(
                    &latest,
                    self.previous_rendered.as_deref(),
                    self.cell_height.unwrap_or(1),
                );
                self.selected.push(cell);
            }
        }
        self.selected
    }
}

fn changed_pixel_ratio(before: &image::RgbImage, after: &image::RgbImage) -> f64 {
    if before.dimensions() != after.dimensions() {
        return 1.0;
    }
    let (width, height) = after.dimensions();
    if width == 0 || height == 0 {
        return 0.0;
    }
    let changed = before
        .as_raw()
        .as_chunks::<3>()
        .0
        .iter()
        .zip(after.as_raw().as_chunks::<3>().0)
        .filter(|(a, b)| a != b)
        .count();
    changed as f64 / (width as u64 * height as u64) as f64
}

/// Normalized bounds of visually changed tile clusters. This runs only for
/// frames already selected as contact-sheet cells.
fn changed_pixel_regions(before: &image::RgbImage, after: &image::RgbImage) -> Vec<[f32; 4]> {
    if before.dimensions() != after.dimensions() {
        return vec![[0.0, 0.0, 1.0, 1.0]];
    }
    let (width, height) = after.dimensions();
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let tiles_wide = width.div_ceil(CONTACT_SHEET_TILE_SIZE);
    let tiles_high = height.div_ceil(CONTACT_SHEET_TILE_SIZE);
    let mut tile_counts = vec![0u32; (tiles_wide * tiles_high) as usize];
    for (index, (a, b)) in before
        .as_raw()
        .as_chunks::<3>()
        .0
        .iter()
        .zip(after.as_raw().as_chunks::<3>().0)
        .enumerate()
    {
        if a != b {
            let tile_x = (index % width as usize) as u32 / CONTACT_SHEET_TILE_SIZE;
            let tile_y = (index / width as usize) as u32 / CONTACT_SHEET_TILE_SIZE;
            tile_counts[(tile_y * tiles_wide + tile_x) as usize] += 1;
        }
    }
    let mut components: Vec<(u64, u32, u32, u32, u32)> = Vec::new();
    for start_y in 0..tiles_high {
        for start_x in 0..tiles_wide {
            let start = (start_y * tiles_wide + start_x) as usize;
            if tile_counts[start] < CONTACT_SHEET_MIN_TILE_PIXELS {
                continue;
            }
            let initial_pixels = tile_counts[start] as u64;
            tile_counts[start] = 0;
            let mut pending = std::collections::VecDeque::from([(start_x, start_y)]);
            let (mut min_x, mut min_y, mut max_x, mut max_y) = (start_x, start_y, start_x, start_y);
            let mut pixels = initial_pixels;
            while let Some((x, y)) = pending.pop_front() {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
                for neighbor_y in y.saturating_sub(1)..=(y + 1).min(tiles_high - 1) {
                    for neighbor_x in x.saturating_sub(1)..=(x + 1).min(tiles_wide - 1) {
                        let neighbor = (neighbor_y * tiles_wide + neighbor_x) as usize;
                        if tile_counts[neighbor] >= CONTACT_SHEET_MIN_TILE_PIXELS {
                            pixels += tile_counts[neighbor] as u64;
                            tile_counts[neighbor] = 0;
                            pending.push_back((neighbor_x, neighbor_y));
                        }
                    }
                }
            }
            if pixels >= CONTACT_SHEET_MIN_REGION_PIXELS {
                components.push((pixels, min_x, min_y, max_x, max_y));
            }
        }
    }
    let gap = CONTACT_SHEET_REGION_MERGE_GAP_TILES;
    // Revisit all pairs after a union: the enlarged region may now reach a
    // component inspected earlier. Never discard regions to meet a box limit.
    let mut index = 0;
    while index < components.len() {
        let mut other = index + 1;
        while other < components.len() {
            let (_, ax1, ay1, ax2, ay2) = components[index];
            let (_, bx1, by1, bx2, by2) = components[other];
            let close = ax1 <= bx2.saturating_add(gap)
                && bx1 <= ax2.saturating_add(gap)
                && ay1 <= by2.saturating_add(gap)
                && by1 <= ay2.saturating_add(gap);
            if close {
                let merged = components.swap_remove(other);
                components[index].0 += merged.0;
                components[index].1 = components[index].1.min(merged.1);
                components[index].2 = components[index].2.min(merged.2);
                components[index].3 = components[index].3.max(merged.3);
                components[index].4 = components[index].4.max(merged.4);
                index = 0;
                other = 1;
            } else {
                other += 1;
            }
        }
        index += 1;
    }
    components.sort_by_key(|&(_, min_x, min_y, _, _)| (min_y, min_x));
    components
        .into_iter()
        .map(|(_, min_tile_x, min_tile_y, max_tile_x, max_tile_y)| {
            let min_x = min_tile_x.saturating_sub(CONTACT_SHEET_REGION_PADDING_TILES)
                * CONTACT_SHEET_TILE_SIZE;
            let min_y = min_tile_y.saturating_sub(CONTACT_SHEET_REGION_PADDING_TILES)
                * CONTACT_SHEET_TILE_SIZE;
            let max_x = ((max_tile_x + CONTACT_SHEET_REGION_PADDING_TILES + 1)
                * CONTACT_SHEET_TILE_SIZE)
                .min(width);
            let max_y = ((max_tile_y + CONTACT_SHEET_REGION_PADDING_TILES + 1)
                * CONTACT_SHEET_TILE_SIZE)
                .min(height);
            [
                min_x as f32 / width as f32,
                min_y as f32 / height as f32,
                (max_x - min_x) as f32 / width as f32,
                (max_y - min_y) as f32 / height as f32,
            ]
        })
        .collect()
}

fn format_contact_timestamp(milliseconds: u64) -> String {
    let hours = milliseconds / 3_600_000;
    let minutes = (milliseconds / 60_000) % 60;
    let seconds = (milliseconds / 1_000) % 60;
    let millis = milliseconds % 1_000;
    format!("{:02}:{:02}:{:02}.{:03}", hours, minutes, seconds, millis)
}

fn glyph_rows(character: char) -> [u8; 7] {
    match character {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        ':' => [0, 0b00100, 0b00100, 0, 0b00100, 0b00100, 0],
        '.' => [0, 0, 0, 0, 0, 0b00100, 0b00100],
        _ => [0; 7],
    }
}

fn draw_timestamp(image: &mut image::RgbaImage, x: u32, y: u32, value: &str) {
    const SCALE: u32 = 2;
    for (index, character) in value.chars().enumerate() {
        for (row, bits) in glyph_rows(character).iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) == 0 {
                    continue;
                }
                for dy in 0..SCALE {
                    for dx in 0..SCALE {
                        let px = x + index as u32 * 6 * SCALE + column * SCALE + dx;
                        let py = y + row as u32 * SCALE + dy;
                        if px < image.width() && py < image.height() {
                            image.put_pixel(px, py, image::Rgba([255, 255, 255, 255]));
                        }
                    }
                }
            }
        }
    }
}

fn draw_changed_region(image: &mut image::RgbaImage, x: u32, y: u32, width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    let right = (x + width - 1).min(image.width().saturating_sub(1));
    let bottom = (y + height - 1).min(image.height().saturating_sub(1));
    // A light tint keeps the content legible; the solid edge defines its bounds.
    for py in y..=bottom {
        for px in x..=right {
            let pixel = image.get_pixel_mut(px, py);
            for (channel, tint) in pixel.0[..3].iter_mut().zip([239u16, 68, 68]) {
                *channel = ((*channel as u16 * 7 + tint) / 8) as u8;
            }
        }
    }
    for thickness in 0..2 {
        let left = x.saturating_sub(thickness);
        let top = y.saturating_sub(thickness);
        let r = (right + thickness).min(image.width().saturating_sub(1));
        let b = (bottom + thickness).min(image.height().saturating_sub(1));
        for px in left..=r {
            image.put_pixel(px, top, image::Rgba([239, 68, 68, 255]));
            image.put_pixel(px, b, image::Rgba([239, 68, 68, 255]));
        }
        for py in top..=b {
            image.put_pixel(left, py, image::Rgba([239, 68, 68, 255]));
            image.put_pixel(r, py, image::Rgba([239, 68, 68, 255]));
        }
    }
}

fn render_contact_cell(
    frame: &ContactSheetFrame,
    previous: Option<&image::RgbImage>,
    cell_height: u32,
) -> ContactSheetCell {
    let source = frame.source.as_ref();
    let regions = previous
        .map(|before| changed_pixel_regions(before, source))
        .unwrap_or_default();
    let mut display = source.clone();
    composite_cursor(
        &mut display,
        scale_cursor_dimensions(
            frame.cursor,
            frame.device_width,
            frame.device_height,
            source.width(),
            source.height(),
        ),
    );
    let rendered = image::DynamicImage::ImageRgb8(display)
        .resize(
            CONTACT_SHEET_CELL_WIDTH,
            cell_height,
            image::imageops::FilterType::Triangle,
        )
        .to_rgba8();
    let mut cell = image::RgbaImage::from_pixel(
        CONTACT_SHEET_CELL_WIDTH,
        cell_height + CONTACT_SHEET_LABEL_HEIGHT,
        image::Rgba([17, 24, 39, 255]),
    );
    let x = (CONTACT_SHEET_CELL_WIDTH - rendered.width()) / 2;
    copy_contact_cell(&mut cell, &rendered, x, CONTACT_SHEET_LABEL_HEIGHT);
    draw_timestamp(&mut cell, 4, 4, &format_contact_timestamp(frame.elapsed_ms));
    for [rx, ry, rw, rh] in regions {
        draw_changed_region(
            &mut cell,
            x + (rx * rendered.width() as f32).round() as u32,
            CONTACT_SHEET_LABEL_HEIGHT + (ry * rendered.height() as f32).round() as u32,
            (rw * rendered.width() as f32).round().max(1.0) as u32,
            (rh * rendered.height() as f32).round().max(1.0) as u32,
        );
    }
    ContactSheetCell {
        rendered: cell,
        elapsed_ms: frame.elapsed_ms,
    }
}

/// Assemble cells whose image analysis and rendering are already complete.
fn write_contact_sheet(path: &Path, frames: &[ContactSheetCell]) -> Result<(), String> {
    let first = frames
        .first()
        .map(|frame| &frame.rendered)
        .ok_or("No frames rendered for contact sheet")?;
    let columns = CONTACT_SHEET_COLUMNS.min(frames.len() as u32).max(1);
    let rows = (frames.len() as u32).div_ceil(columns);
    let width = CONTACT_SHEET_GAP + columns * (CONTACT_SHEET_CELL_WIDTH + CONTACT_SHEET_GAP);
    let height = CONTACT_SHEET_GAP + rows * (first.height() + CONTACT_SHEET_GAP);
    let mut canvas = image::RgbaImage::from_raw(
        width,
        height,
        [17, 24, 39, 255].repeat(width as usize * height as usize),
    )
    .expect("sheet dimensions");
    for (index, frame) in frames.iter().enumerate() {
        let cell = &frame.rendered;
        let x = CONTACT_SHEET_GAP
            + index as u32 % columns * (CONTACT_SHEET_CELL_WIDTH + CONTACT_SHEET_GAP);
        let y = CONTACT_SHEET_GAP + index as u32 / columns * (first.height() + CONTACT_SHEET_GAP);
        copy_contact_cell(&mut canvas, cell, x, y);
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create contact sheet directory: {}", e))?;
    }
    canvas
        .save(path)
        .map_err(|e| format!("Failed to save contact sheet: {}", e))
}

/// Contact-sheet images are opaque and fully inside the canvas; no blending is needed.
fn copy_contact_cell(canvas: &mut image::RgbaImage, cell: &image::RgbaImage, x: u32, y: u32) {
    let stride = canvas.width() as usize * 4;
    let row_bytes = cell.width() as usize * 4;
    for (row, pixels) in cell.as_raw().chunks_exact(row_bytes).enumerate() {
        let offset = (y as usize + row) * stride + x as usize * 4;
        canvas.as_mut()[offset..offset + row_bytes].copy_from_slice(pixels);
    }
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
    sequence: u64,
    image_data: Arc<Vec<u8>>,
    elapsed: Duration,
    captured_at: tokio::time::Instant,
    timestamp: f64,
    device_width: f64,
    device_height: f64,
}

pub struct InitialRecordingFrame {
    pub image_data: Vec<u8>,
    pub device_width: f64,
    pub device_height: f64,
}

/// Drain Chrome independently from the encoder so FFmpeg cannot stall frame ACKs.
#[allow(clippy::too_many_arguments)]
pub fn spawn_recording_task(
    client: Arc<CdpClient>,
    capture_session: String,
    initial_frame: InitialRecordingFrame,
    output_path: String,
    fps: u32,
    shared_count: Arc<AtomicU64>,
    shared_captured: Arc<AtomicU64>,
    cursor: bool,
    shared_cursor: SharedRecordingCursor,
    contact_sheet_path: Option<String>,
    contact_sheet_threshold: f64,
    shared_contact_sheet_count: Arc<AtomicU64>,
    cancel_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<Result<(), String>> {
    tokio::spawn(async move {
        let fps = validate_fps(fps)?;
        let events = client.subscribe_session(&capture_session);
        let (frame_tx, frame_rx) = mpsc::channel(ENCODER_FRAME_BUFFER);
        let (contact_tx, mut contact_worker) = if let Some(path) = contact_sheet_path.as_ref() {
            let (tx, rx) = std::sync::mpsc::sync_channel(ENCODER_FRAME_BUFFER);
            let contact_cursor = shared_cursor.clone();
            let path = path.clone();
            let worker = tokio::task::spawn_blocking(move || {
                let frames =
                    collect_contact_frames(rx, contact_sheet_threshold, cursor, &contact_cursor)?;
                write_contact_sheet(Path::new(&path), &frames)?;
                Ok::<u64, String>(frames.len() as u64)
            });
            (Some(tx), Some(worker))
        } else {
            if std::env::var_os("AGENT_BROWSER_DEBUG").is_some() {
                eprintln!("[contact-sheet] disabled; no worker or queue");
            }
            (None, None)
        };
        let encoder = tokio::spawn(encode_stream(
            output_path,
            fps,
            cursor,
            shared_cursor.clone(),
            frame_rx,
        ));

        // Chrome does not reliably emit an initial PNG screencast frame for a
        // static page. Seed both outputs explicitly before listening for
        // later repaints.
        let frame = CapturedVideoFrame {
            sequence: 0,
            image_data: Arc::new(initial_frame.image_data),
            elapsed: Duration::ZERO,
            captured_at: tokio::time::Instant::now(),
            timestamp: cursor_timestamp(),
            device_width: initial_frame.device_width,
            device_height: initial_frame.device_height,
        };
        let seeded = frame_tx
            .send(frame.clone())
            .await
            .map_err(|_| "Recording encoder stopped unexpectedly".to_string());
        let seeded = if let (Ok(()), Some(contact_tx)) = (&seeded, contact_tx.as_ref()) {
            contact_tx
                .send(frame)
                .map_err(|_| "Contact sheet analyzer stopped unexpectedly".to_string())
        } else {
            seeded
        };
        shared_captured.fetch_add(1, Ordering::Relaxed);

        let started = match seeded {
            Ok(()) => client
                .send_command(
                    "Page.startScreencast",
                    Some(json!({
                        "format": "png",
                        "everyNthFrame": 1,
                    })),
                    Some(&capture_session),
                )
                .await
                .map_err(|error| format!("Failed to start screencast: {error}")),
            Err(error) => Err(error),
        };

        let captured = match started {
            Ok(_) => {
                collect_frames(
                    &client,
                    &capture_session,
                    events,
                    frame_tx,
                    contact_tx,
                    &shared_captured,
                    cancel_rx,
                )
                .await
            }
            Err(error) => Err(error),
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
            if error == "Contact sheet analyzer stopped unexpectedly" {
                return match contact_worker.take() {
                    Some(worker) => match worker.await {
                        Ok(Err(contact_error)) => Err(contact_error),
                        Ok(Ok(_)) => Err(error),
                        Err(join_error) => {
                            Err(format!("Contact sheet task failed: {}", join_error))
                        }
                    },
                    None => Err(error),
                };
            }
            return Err(error);
        }
        let streamed = encoder
            .await
            .map_err(|e| format!("Recording encoder task failed: {}", e))??;
        shared_count.store(streamed, Ordering::Relaxed);
        let contact_sheet_count = match contact_worker {
            Some(worker) => worker
                .await
                .map_err(|e| format!("Contact sheet task failed: {}", e))??,
            None => 0,
        };
        shared_contact_sheet_count.store(contact_sheet_count, Ordering::Relaxed);

        Ok(())
    })
}

fn decode_frame_data(value: &Value) -> Option<Vec<u8>> {
    value.get("data").and_then(Value::as_str).and_then(|data| {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).ok()
    })
}

pub async fn capture_initial_image(
    client: &CdpClient,
    session_id: &str,
) -> Result<InitialRecordingFrame, String> {
    let viewport = client
        .send_command(
            "Runtime.evaluate",
            Some(json!({
                "expression": "[window.innerWidth, window.innerHeight]",
                "returnByValue": true,
            })),
            Some(session_id),
        )
        .await
        .map_err(|error| format!("Failed to read initial recording viewport: {error}"))?;
    let dimensions = viewport
        .pointer("/result/value")
        .and_then(Value::as_array)
        .filter(|dimensions| dimensions.len() == 2)
        .and_then(|dimensions| Some((dimensions[0].as_f64()?, dimensions[1].as_f64()?)))
        .filter(|(width, height)| {
            width.is_finite() && *width > 0.0 && height.is_finite() && *height > 0.0
        })
        .ok_or_else(|| "Initial recording viewport returned invalid dimensions".to_string())?;
    let result = client
        .send_command(
            "Page.captureScreenshot",
            Some(json!({"format": "png", "fromSurface": true})),
            Some(session_id),
        )
        .await
        .map_err(|error| format!("Failed to capture initial recording frame: {error}"))?;
    let image_data = decode_frame_data(&result)
        .ok_or_else(|| "Initial recording screenshot returned no image data".to_string())?;
    Ok(InitialRecordingFrame {
        image_data,
        device_width: dimensions.0,
        device_height: dimensions.1,
    })
}

async fn collect_frames(
    client: &CdpClient,
    capture_session: &str,
    mut events: mpsc::Receiver<super::cdp::types::CdpEvent>,
    frame_tx: mpsc::Sender<CapturedVideoFrame>,
    contact_tx: Option<std::sync::mpsc::SyncSender<CapturedVideoFrame>>,
    shared_captured: &AtomicU64,
    cancel_rx: oneshot::Receiver<()>,
) -> Result<(), String> {
    let mut cancel_rx = std::pin::pin!(cancel_rx);
    let started = tokio::time::Instant::now();
    let mut sequence = 1u64;

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
                    let decoded = decode_frame_data(&event.params);
                    if let Some(bytes) = decoded {
                        let metadata = &event.params["metadata"];
                        let elapsed = started.elapsed();
                        let timestamp = metadata["timestamp"]
                            .as_f64()
                            .unwrap_or_else(cursor_timestamp);
                        let frame = CapturedVideoFrame {
                            sequence,
                            image_data: Arc::new(bytes),
                            elapsed,
                            captured_at: tokio::time::Instant::now(),
                            timestamp,
                            device_width: metadata["deviceWidth"].as_f64().unwrap_or(0.0),
                            device_height: metadata["deviceHeight"].as_f64().unwrap_or(0.0),
                        };
                        sequence += 1;
                        shared_captured.fetch_add(1, Ordering::Relaxed);
                        frame_tx.try_send(frame.clone()).map_err(|error| match error {
                            mpsc::error::TrySendError::Full(_) => format!(
                                "Recording encoder fell behind by more than {} buffered frames",
                                ENCODER_FRAME_BUFFER
                            ),
                            mpsc::error::TrySendError::Closed(_) => {
                                "Recording encoder stopped unexpectedly".to_string()
                            }
                        })?;
                        if let Some(contact_tx) = contact_tx.as_ref() {
                            contact_tx.try_send(frame).map_err(|error| match error {
                                std::sync::mpsc::TrySendError::Full(_) => format!(
                                    "Contact sheet analyzer fell behind by more than {} buffered frames",
                                    ENCODER_FRAME_BUFFER
                                ),
                                std::sync::mpsc::TrySendError::Disconnected(_) => {
                                    "Contact sheet analyzer stopped unexpectedly".to_string()
                                }
                            })?;
                        }
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

/// Analyze frames and render finalized cells on the blocking worker.
fn collect_contact_frames(
    frames: std::sync::mpsc::Receiver<CapturedVideoFrame>,
    threshold: f64,
    cursor: bool,
    shared_cursor: &SharedRecordingCursor,
) -> Result<Vec<ContactSheetCell>, String> {
    let mut collector = ContactSheetCollector::new(threshold);
    let mut max_lag = Duration::ZERO;
    let mut processed = 0u64;
    loop {
        let frame = match frames.recv_timeout(Duration::from_millis(CONTACT_SHEET_BURST_QUIET_MS)) {
            Ok(frame) => frame,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                collector.flush_pending();
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let cursor_state = if cursor {
            shared_cursor
                .lock()
                .map(|history| history.at(frame.timestamp))
                .unwrap_or_default()
        } else {
            RecordingCursorState::default()
        };
        collector.consider(
            &frame.image_data,
            frame.elapsed,
            cursor_state,
            frame.device_width,
            frame.device_height,
        );
        // Measure through completed selection/rendering, not just dequeue.
        let lag = frame.captured_at.elapsed();
        max_lag = max_lag.max(lag);
        processed += 1;
        if lag > MAX_ENCODER_LAG {
            return Err("Contact sheet analysis fell more than 500 ms behind capture".to_string());
        }
    }
    let frames = collector.finish();
    if std::env::var_os("AGENT_BROWSER_DEBUG").is_some() {
        eprintln!(
            "[contact-sheet] processed={} max_analysis_lag_ms={:.3} rendered_cells={}",
            processed,
            max_lag.as_secs_f64() * 1000.0,
            frames.len()
        );
    }
    Ok(frames)
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

fn render_cursor_frame(
    frame: &CapturedVideoFrame,
    history: &RecordingCursorHistory,
    output_timestamp: f64,
    decoded: &mut Option<(u64, image::RgbImage)>,
) -> Result<Vec<u8>, String> {
    if decoded.as_ref().is_none_or(|(id, _)| *id != frame.sequence) {
        let image = image::load_from_memory(&frame.image_data)
            .map_err(|e| format!("Failed to decode recording frame: {}", e))?
            .to_rgb8();
        *decoded = Some((frame.sequence, image));
    }
    let source = &decoded.as_ref().expect("decoded frame exists").1;
    let state = scaled_cursor(
        cursor_for_video_frame(history, frame, output_timestamp),
        frame,
        source.width(),
        source.height(),
    );
    let mut rendered = source.clone();
    for (started, click) in history.click_starts().into_iter().filter(|(started, _)| {
        output_timestamp >= *started && output_timestamp - *started < RIPPLE_DURATION_SECS
    }) {
        let click = scaled_cursor(click, frame, source.width(), source.height());
        composite_ripple(
            &mut rendered,
            click.x,
            click.y,
            (output_timestamp - started) / RIPPLE_DURATION_SECS,
        );
    }
    composite_cursor(&mut rendered, state);
    let mut bytes = format!("P6\n{} {}\n255\n", rendered.width(), rendered.height()).into_bytes();
    bytes.extend_from_slice(rendered.as_raw());
    Ok(bytes)
}

async fn encode_stream(
    output_path: String,
    fps: u32,
    cursor: bool,
    shared_cursor: SharedRecordingCursor,
    mut frames: mpsc::Receiver<CapturedVideoFrame>,
) -> Result<u64, String> {
    let mut command = build_ffmpeg_command(&output_path, fps, cursor);
    let mut ffmpeg = spawn_ffmpeg_command(&mut command)?;
    let mut stdin = ffmpeg
        .stdin
        .take()
        .ok_or_else(|| "Failed to open ffmpeg stdin".to_string())?;
    let mut interval = tokio::time::interval(frame_period(fps));
    // Catch up after short pipe stalls instead of shortening the video.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
    let mut latest: Option<CapturedVideoFrame> = None;
    let mut decoded = None;
    let mut written = 0u64;

    loop {
        tokio::select! {
            frame = frames.recv() => {
                let Some(frame) = frame else { break };
                if frame.captured_at.elapsed() > MAX_ENCODER_LAG {
                    return Err("Recording encoder fell more than 500 ms behind capture".to_string());
                }
                if latest.is_none() {
                    interval.reset_at(frame.captured_at);
                }
                latest = Some(frame);
            }
            tick = interval.tick() => {
                if tick.elapsed() > MAX_ENCODER_LAG && latest.is_some() {
                    return Err("Recording encoder fell more than 500 ms behind capture".to_string());
                }
                let Some(frame) = latest.as_ref() else { continue };
                let output_timestamp = cursor_timestamp();
                let history = shared_cursor
                    .lock()
                    .map(|history| history.clone())
                    .unwrap_or_default();
                if cursor {
                    let bytes =
                        render_cursor_frame(frame, &history, output_timestamp, &mut decoded)?;
                    write_encoder_bytes(&mut stdin, &bytes).await?;
                } else {
                    write_encoder_bytes(&mut stdin, &frame.image_data).await?;
                }
                written += 1;
            }
        }
    }

    let Some(frame) = latest.as_ref() else {
        return Err("No frames captured".to_string());
    };
    let output_timestamp = cursor_timestamp();
    let final_image = if cursor {
        let history = shared_cursor
            .lock()
            .map(|history| history.clone())
            .unwrap_or_default();
        render_cursor_frame(frame, &history, output_timestamp, &mut decoded)?
    } else {
        frame.image_data.as_ref().clone()
    };
    // Include the final image even when the take stops before its first tick.
    write_encoder_bytes(&mut stdin, &final_image).await?;
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

fn scale_cursor_dimensions(
    mut cursor: RecordingCursorState,
    device_width: f64,
    device_height: f64,
    width: u32,
    height: u32,
) -> RecordingCursorState {
    if device_width > 0.0 {
        cursor.x *= width as f64 / device_width;
    }
    if device_height > 0.0 {
        cursor.y *= height as f64 / device_height;
    }
    cursor
}

fn scaled_cursor(
    cursor: RecordingCursorState,
    frame: &CapturedVideoFrame,
    width: u32,
    height: u32,
) -> RecordingCursorState {
    scale_cursor_dimensions(
        cursor,
        frame.device_width,
        frame.device_height,
        width,
        height,
    )
}

fn cursor_for_video_frame(
    history: &RecordingCursorHistory,
    page_frame: &CapturedVideoFrame,
    timestamp: f64,
) -> RecordingCursorState {
    let animated = history.interpolated_at(timestamp);
    let anchored = history.at(page_frame.timestamp);
    if animated.buttons != 0 || anchored.buttons != 0 {
        anchored
    } else {
        animated
    }
}

pub async fn stop_recording_task(state: &mut RecordingState) -> Result<(), String> {
    if let Some(tx) = state.cancel_tx.take() {
        let _ = tx.send(());
    }

    if let Ok(mut cursor) = state.shared_cursor.lock() {
        cursor.enabled = false;
    }

    let counter = state.shared_frame_count.take();
    let captured = state.shared_captured_count.take();
    let contact_sheet = state.shared_contact_sheet_count.take();
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
    if let Some(c) = contact_sheet {
        state.contact_sheet_frame_count = c.load(Ordering::Relaxed);
    }
    if let Ok(mut guard) = state.capture_session.lock() {
        *guard = None;
    }

    if let Ok(mut cursor) = state.shared_cursor.lock() {
        *cursor = RecordingCursorHistory::default();
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_frame_data_decodes_capture_screenshot_and_screencast_results() {
        assert_eq!(
            decode_frame_data(&json!({"data": "AQID"})),
            Some(vec![1, 2, 3])
        );
        assert_eq!(decode_frame_data(&json!({"data": "not-base64"})), None);
        assert_eq!(decode_frame_data(&json!({})), None);
    }

    #[test]
    fn cursor_history_only_collects_during_cursor_recordings() {
        let mut state = RecordingState::new();
        state.shared_cursor.lock().unwrap().record(1.0, 2.0, 0);
        assert!(state.shared_cursor.lock().unwrap().samples.is_empty());
        recording_start(
            &mut state,
            "unused.webm",
            RecordingOptions {
                cursor: true,
                ..Default::default()
            },
        )
        .unwrap();
        state.shared_cursor.lock().unwrap().record(3.0, 4.0, 0);
        assert_eq!(state.shared_cursor.lock().unwrap().samples.len(), 1);
    }

    #[test]
    fn test_cursor_history_matches_capture_not_arrival() {
        let mut history = RecordingCursorHistory::default();
        for (time, x, buttons) in [(10.0, 100.0, 0), (11.0, 200.0, 1), (12.0, 300.0, 0)] {
            history.samples.push_back((
                time,
                RecordingCursorState {
                    x,
                    y: 50.0,
                    buttons,
                    visible: true,
                },
            ));
        }
        assert!(!history.at(9.0).visible);
        assert_eq!(history.at(11.5).x, 200.0);
        assert_eq!(history.at(11.5).buttons, 1);
        assert_eq!(history.at(12.0).x, 300.0);
        assert!(!history.at(f64::NAN).visible);
        history.samples.push_back((
            14.0,
            RecordingCursorState {
                x: 500.0,
                y: 150.0,
                buttons: 0,
                visible: true,
            },
        ));
        let interpolated = history.interpolated_at(13.0);
        assert_eq!((interpolated.x, interpolated.y), (400.0, 100.0));
    }

    #[test]
    fn test_cursor_interpolation_stops_while_button_is_down() {
        let mut history = RecordingCursorHistory::default();
        history.record_at(10.0, 10.0, 20.0, 0);
        history.record_at(11.0, 110.0, 120.0, 0);
        history.record_at(12.0, 210.0, 220.0, 1);

        let moving = history.interpolated_at(10.5);
        assert_eq!((moving.x, moving.y), (60.0, 70.0));

        let pressed = history.interpolated_at(11.5);
        assert_eq!((pressed.x, pressed.y, pressed.buttons), (110.0, 120.0, 0));
    }

    #[test]
    fn test_click_starts_only_on_button_transitions() {
        let mut history = RecordingCursorHistory::default();
        history.record_at(1.0, 10.0, 20.0, 0);
        history.record_at(2.0, 10.0, 20.0, 1);
        history.record_at(3.0, 20.0, 30.0, 1);
        history.record_at(4.0, 20.0, 30.0, 0);
        history.record_at(5.0, 30.0, 40.0, 1);

        let clicks = history.click_starts();
        assert_eq!(clicks.len(), 2);
        assert_eq!((clicks[0].0, clicks[0].1.x), (2.0, 10.0));
        assert_eq!((clicks[1].0, clicks[1].1.x), (5.0, 30.0));
    }

    #[test]
    fn test_pressed_cursor_uses_position_for_page_frame() {
        let mut history = RecordingCursorHistory::default();
        history.record_at(10.0, 10.0, 20.0, 0);
        history.record_at(11.0, 110.0, 120.0, 1);
        history.record_at(12.0, 210.0, 220.0, 1);
        history.record_at(13.0, 310.0, 320.0, 0);
        history.record_at(14.0, 410.0, 420.0, 0);
        let frame = CapturedVideoFrame {
            sequence: 0,
            image_data: Arc::new(Vec::new()),
            elapsed: Duration::ZERO,
            captured_at: tokio::time::Instant::now(),
            timestamp: 11.0,
            device_width: 1000.0,
            device_height: 500.0,
        };

        let pressed = cursor_for_video_frame(&history, &frame, 12.5);
        assert_eq!((pressed.x, pressed.y, pressed.buttons), (110.0, 120.0, 1));

        let released_frame = CapturedVideoFrame {
            timestamp: 13.0,
            ..frame
        };
        let moving = cursor_for_video_frame(&history, &released_frame, 13.5);
        assert_eq!((moving.x, moving.y, moving.buttons), (360.0, 370.0, 0));
    }

    #[test]
    fn test_cursor_scales_from_page_to_screencast_pixels() {
        let frame = CapturedVideoFrame {
            sequence: 0,
            image_data: Arc::new(Vec::new()),
            elapsed: Duration::ZERO,
            captured_at: tokio::time::Instant::now(),
            timestamp: 0.0,
            device_width: 640.0,
            device_height: 360.0,
        };
        let scaled = scaled_cursor(
            RecordingCursorState {
                x: 320.0,
                y: 180.0,
                buttons: 0,
                visible: true,
            },
            &frame,
            1280,
            720,
        );
        assert_eq!((scaled.x, scaled.y), (640.0, 360.0));
    }

    #[test]
    fn test_contact_sheet_selection_keeps_clean_source_pixels() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clean.png");
        let sheet_path = directory.path().join("sheet.png");
        let clean = image::RgbImage::from_pixel(80, 80, image::Rgb([240, 240, 240]));
        clean.save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut history = RecordingCursorHistory::default();
        history.record_at(0.5, 10.0, 20.0, 0);
        history.record_at(1.5, 20.0, 30.0, 0);
        history.record_at(2.5, 30.0, 40.0, 0);

        let mut collector = ContactSheetCollector::new(0.05);
        for index in 0..3 {
            let timestamp = index as f64 + 1.0;
            collector.consider(
                &original,
                Duration::from_secs(index),
                history.at(timestamp),
                80.0,
                80.0,
            );
        }
        assert_eq!(
            collector
                .previous_candidate
                .as_ref()
                .map(|previous| previous.source.as_ref()),
            Some(&clean)
        );
        assert_eq!(collector.latest.as_ref().unwrap().image_data, original);
        assert_eq!(collector.latest.as_ref().unwrap().cursor.x, 30.0);
        let selected = collector.finish();
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].elapsed_ms, 0);
        assert_eq!(selected[1].elapsed_ms, 2000);

        write_contact_sheet(&sheet_path, &selected).unwrap();
        let sheet = image::open(sheet_path).unwrap().to_rgb8();
        assert_ne!(sheet.get_pixel(88, 192), &image::Rgb([240, 240, 240]));
    }

    #[test]
    fn test_cursor_hotspot_stays_at_input_when_pressed() {
        let frame = image::RgbImage::from_pixel(100, 100, image::Rgb([255, 255, 255]));
        for buttons in [0, 1] {
            let mut output = frame.clone();
            composite_cursor(
                &mut output,
                RecordingCursorState {
                    x: 40.0,
                    y: 30.0,
                    buttons,
                    visible: true,
                },
            );
            assert!(output.get_pixel(40, 31)[0] < 180);
            assert_eq!(output.get_pixel(10, 10), frame.get_pixel(10, 10));
        }
    }

    #[test]
    fn test_cursor_and_ripple_edges_are_antialiased() {
        let mut frame = image::RgbImage::from_pixel(100, 100, image::Rgb([255, 255, 255]));
        composite_ripple(&mut frame, 40.0, 30.0, 0.5);
        composite_cursor(
            &mut frame,
            RecordingCursorState {
                x: 40.0,
                y: 30.0,
                buttons: 0,
                visible: true,
            },
        );
        assert!(frame
            .pixels()
            .any(|pixel| { pixel.0.iter().any(|&channel| channel > 0 && channel < 255) }));
        assert_ne!(frame.get_pixel(40, 42), &image::Rgb([255, 255, 255]));
        assert_eq!(frame.get_pixel(5, 5), &image::Rgb([255, 255, 255]));
    }

    fn options(fps: Option<u32>) -> RecordingOptions {
        RecordingOptions {
            fps,
            ..RecordingOptions::default()
        }
    }

    /// Requires FFmpeg and ffprobe, but no browser. Check the actual file timeline.
    #[tokio::test]
    #[ignore]
    async fn recording_sparse_frames_preserve_timestamps() {
        for (extension, cursor) in [
            ("webm", false),
            ("mp4", false),
            ("webm", true),
            ("mp4", true),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("sparse.{extension}"));
            let (tx, rx) = mpsc::channel(4);
            let encoder = tokio::spawn(encode_stream(
                path.to_string_lossy().into_owned(),
                30,
                cursor,
                Arc::new(Mutex::new(RecordingCursorHistory::default())),
                rx,
            ));
            for (sequence, color) in [[255, 0, 0], [0, 0, 255]].into_iter().enumerate() {
                let image = image::RgbImage::from_pixel(64, 64, image::Rgb(color));
                let mut png = std::io::Cursor::new(Vec::new());
                image.write_to(&mut png, image::ImageFormat::Png).unwrap();
                tx.send(CapturedVideoFrame {
                    sequence: sequence as u64,
                    image_data: Arc::new(png.into_inner()),
                    elapsed: Duration::ZERO,
                    captured_at: tokio::time::Instant::now(),
                    timestamp: cursor_timestamp(),
                    device_width: 64.0,
                    device_height: 64.0,
                })
                .await
                .unwrap();
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            drop(tx);
            let count = encoder.await.unwrap().unwrap();
            assert!(count >= 24, "expected repeated frames, got {count}");
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
            assert_eq!(times.len() as u64, count);
            assert!(
                times
                    .windows(2)
                    .all(|pair| { ((pair[1] - pair[0]) - 1.0 / 30.0).abs() < 0.002 }),
                "{extension}: {times:?}"
            );
            let decoded = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&path)
                .args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
                .output()
                .unwrap();
            assert!(
                decoded.status.success(),
                "{}",
                String::from_utf8_lossy(&decoded.stderr)
            );
            for (at, channel) in [(0.2, 0), (0.6, 2)] {
                let index = times.iter().position(|time| *time >= at).unwrap();
                let pixel = &decoded.stdout[index * 64 * 64 * 3..][..3];
                assert!(
                    pixel[channel] > 200 && pixel[2 - channel] < 50,
                    "{extension}: wrong color at {at}s: {pixel:?}"
                );
            }
            assert!(
                (0.7..1.1).contains(times.last().unwrap()),
                "{extension}: {times:?}"
            );
        }
    }

    #[test]
    fn test_cursor_video_transport_preserves_decoded_pixels() {
        let source =
            image::RgbImage::from_fn(64, 64, |x, y| image::Rgb([x as u8 * 3, y as u8 * 3, 77]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(source.clone())
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let decoded = image::load_from_memory(&png).unwrap().to_rgb8();
        let header = b"P6\n64 64\n255\n";
        let plain = cursor_video_frame(&png, RecordingCursorState::default()).unwrap();
        assert_eq!(&plain[..header.len()], header);
        assert_eq!(&plain[header.len()..], decoded.as_raw());
        let composited = cursor_video_frame(
            &png,
            RecordingCursorState {
                x: 30.0,
                y: 20.0,
                buttons: 1,
                visible: true,
            },
        )
        .unwrap();
        for y in 0..64 {
            for x in 0..64 {
                if !(26..54).contains(&x) || !(16..46).contains(&y) {
                    let offset = header.len() + (y * 64 + x) * 3;
                    assert_eq!(&composited[offset..offset + 3], &plain[offset..offset + 3]);
                }
            }
        }
        let cmd = build_ffmpeg_command("/tmp/out.webm", 30, true);
        assert!(cmd.as_std().get_args().any(|arg| arg == "ppm"));
    }

    #[test]
    fn test_recording_state_new() {
        let state = RecordingState::new();
        assert!(!state.active);
        assert!(!state.contact_sheet);
        assert!(!RecordingOptions::default().contact_sheet);
        assert!(state.output_path.is_empty());
        assert_eq!(state.frame_count, 0);
        assert_eq!(state.fps, DEFAULT_FPS);
    }

    #[test]
    fn test_recording_start_sets_active() {
        let mut state = RecordingState::new();
        let result = recording_start(&mut state, "/tmp/test.mp4", options(None));
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
        let result = recording_start(&mut state, "/tmp/test.webm", options(Some(60))).unwrap();
        assert_eq!(state.fps, 60);
        assert_eq!(result["fps"], 60);
    }

    #[test]
    fn test_recording_start_sets_cursor_and_contact_sheet_options() {
        let mut state = RecordingState::new();
        let result = recording_start(
            &mut state,
            "/tmp/demo.webm",
            RecordingOptions {
                cursor: true,
                contact_sheet: true,
                contact_sheet_threshold: 0.12,
                ..RecordingOptions::default()
            },
        )
        .unwrap();
        assert!(state.cursor);
        assert!(state.contact_sheet);
        assert_eq!(state.contact_sheet_threshold, 0.12);
        assert_eq!(
            state.contact_sheet_path.as_deref(),
            Some("/tmp/demo.contact-sheet.png")
        );
        assert_eq!(result["contactSheetPath"], "/tmp/demo.contact-sheet.png");
    }

    #[test]
    fn test_contact_sheet_path_replaces_extension() {
        assert_eq!(contact_sheet_path("demo.webm"), "demo.contact-sheet.png");
        assert_eq!(
            contact_sheet_path("artifacts/demo.capture.webm"),
            "artifacts/demo.capture.contact-sheet.png"
        );
    }

    #[test]
    fn test_validate_contact_sheet_threshold_range() {
        assert_eq!(validate_contact_sheet_threshold(0.0).unwrap(), 0.0);
        assert_eq!(validate_contact_sheet_threshold(1.0).unwrap(), 1.0);
        assert!(validate_contact_sheet_threshold(-0.01).is_err());
        assert!(validate_contact_sheet_threshold(1.01).is_err());
        assert!(validate_contact_sheet_threshold(f64::NAN).is_err());
    }

    #[test]
    fn test_changed_pixel_regions_reports_separate_bounds() {
        let before = image::RgbImage::from_pixel(64, 64, image::Rgb([0, 0, 0]));
        let mut after = before.clone();
        for y in 8..16 {
            for x in 8..16 {
                after.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
        for y in 40..48 {
            for x in 48..56 {
                after.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
        let ratio = changed_pixel_ratio(&before, &after);
        let regions = changed_pixel_regions(&before, &after);
        assert!((ratio - 0.03125).abs() < f64::EPSILON);
        assert_eq!(
            regions,
            vec![[0.0, 0.0, 0.375, 0.375], [0.625, 0.5, 0.375, 0.375]]
        );
    }

    #[test]
    fn test_contact_sheet_covers_every_flyout_control_and_distant_region() {
        let before = image::RgbImage::from_pixel(1024, 512, image::Rgb([255, 255, 255]));
        let mut after = before.clone();
        let mut changed_points = Vec::new();
        // Rows in a newly opened flyout, plus more than eight remote changes.
        for (x, y) in (0..6)
            .map(|row| (16, 16 + row * 32))
            .chain((0..12).map(|i| (256 + (i % 6) * 112, 32 + (i / 6) * 200)))
        {
            for py in y..y + 8 {
                for px in x..x + 16 {
                    after.put_pixel(px, py, image::Rgb([0, 0, 0]));
                    changed_points.push((px, py));
                }
            }
        }
        let regions = changed_pixel_regions(&before, &after);
        assert_eq!(regions.len(), 13, "flyout rows should form one region");
        for (x, y) in changed_points {
            assert!(
                regions.iter().any(|[rx, ry, rw, rh]| {
                    let x = x as f32 / 1024.0;
                    let y = y as f32 / 512.0;
                    x >= *rx && x < rx + rw && y >= *ry && y < ry + rh
                }),
                "uncovered change at {x},{y}"
            );
        }
    }

    #[test]
    fn test_contact_sheet_selects_subtle_loading_panel_and_its_return() {
        let mut collector = ContactSheetCollector::new(DEFAULT_CONTACT_SHEET_THRESHOLD);
        for (elapsed, color) in [
            (0, [226, 232, 240]),
            (120, [219, 234, 254]),
            (240, [226, 232, 240]),
        ] {
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
                640,
                360,
                image::Rgb(color),
            ))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
            collector.consider(
                &png,
                Duration::from_millis(elapsed),
                RecordingCursorState::default(),
                640.0,
                360.0,
            );
        }
        assert_eq!(
            collector.finish().len(),
            3,
            "subtle loading transitions must be selected"
        );
    }

    #[test]
    fn test_contact_sheet_row_copy_matches_opaque_overlay() {
        let mut actual = image::RgbaImage::from_pixel(23, 19, image::Rgba([17, 24, 39, 255]));
        let mut expected = actual.clone();
        let cell = image::RgbaImage::from_fn(13, 7, |x, y| {
            image::Rgba([x as u8 * 13, y as u8 * 19, 80, 255])
        });
        image::imageops::overlay(&mut expected, &cell, 5, 9);
        copy_contact_cell(&mut actual, &cell, 5, 9);
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_contact_sheet_preview_preserves_thin_changes_between_sample_points() {
        let mut collector = ContactSheetCollector::new(0.002);
        let mut source = image::RgbImage::new(1280, 720);
        for elapsed in [0, 10] {
            if elapsed != 0 {
                // Nearest-neighbor sampling at every fourth x would miss this.
                for y in 0..720 {
                    source.put_pixel(1, y, image::Rgb([255; 3]));
                }
            }
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(source.clone())
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            collector.consider(
                &png,
                Duration::from_millis(elapsed),
                RecordingCursorState::default(),
                1280.0,
                720.0,
            );
        }
        assert_eq!(collector.finish().len(), 2);
    }

    #[test]
    fn test_contact_sheet_streams_cells_and_preserves_accumulated_changes_and_flashes() {
        let mut collector = ContactSheetCollector::new(0.10);
        let base = image::RgbImage::new(100, 100);
        let mut send = |source: image::RgbImage, elapsed| {
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(source)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            collector.consider(
                &png,
                Duration::from_millis(elapsed),
                RecordingCursorState::default(),
                100.0,
                100.0,
            );
        };
        send(base.clone(), 0);
        let mut changed = base.clone();
        for y in 0..5 {
            for x in 0..100 {
                changed.put_pixel(x, y, image::Rgb([255; 3]));
            }
        }
        send(changed.clone(), 10);
        for y in 5..12 {
            for x in 0..100 {
                changed.put_pixel(x, y, image::Rgb([255; 3]));
            }
        }
        send(changed, 20);
        // A one-frame flash must be selected, including its return to baseline.
        send(
            image::RgbImage::from_pixel(100, 100, image::Rgb([255; 3])),
            30,
        );
        send(base, 40);
        let frames = collector.finish();
        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.elapsed_ms)
                .collect::<Vec<_>>(),
            vec![0, 20, 30, 40]
        );
        assert!(
            frames
                .iter()
                .all(|frame| frame.rendered.width() == CONTACT_SHEET_CELL_WIDTH),
            "selected cells must be rendered"
        );
    }

    #[test]
    fn test_contact_sheet_compacts_high_rate_scroll_burst() {
        let mut collector = ContactSheetCollector::new(0.01);
        let mut last_png = Vec::new();
        for index in 0..=60u32 {
            let source = image::RgbImage::from_fn(100, 60, |x, y| {
                let document_y = y + index * 8;
                let band = ((document_y / 12) % 6) as u8;
                image::Rgb([
                    band.saturating_mul(35),
                    (x as u8).wrapping_add(band.saturating_mul(11)),
                    255u8.saturating_sub(band.saturating_mul(25)),
                ])
            });
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(source)
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            collector.consider(
                &png,
                Duration::from_millis(index as u64 * 16),
                RecordingCursorState::default(),
                100.0,
                60.0,
            );
            last_png = png;
        }
        // A held frame ends the burst and must still become the final cell.
        collector.consider(
            &last_png,
            Duration::from_millis(1_300),
            RecordingCursorState::default(),
            100.0,
            60.0,
        );

        let frames = collector.finish();
        let timestamps = frames
            .iter()
            .map(|frame| frame.elapsed_ms)
            .collect::<Vec<_>>();
        assert_eq!(timestamps.first(), Some(&0));
        assert_eq!(timestamps.last(), Some(&1_300));
        assert!(timestamps.len() <= CONTACT_SHEET_BURST_FRAMES + 1);
        assert!(
            timestamps
                .iter()
                .filter(|&&time| time > 0 && time < 960)
                .count()
                >= 3,
            "the compacted scroll should retain representative middle frames"
        );
    }

    #[test]
    fn test_contact_sheet_renders_finalized_cells_during_capture() {
        let mut collector = ContactSheetCollector::new(0.01);
        for (elapsed, color) in [(0, [0, 0, 0]), (300, [255, 255, 255])] {
            let mut png = Vec::new();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(100, 60, image::Rgb(color)))
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            collector.consider(
                &png,
                Duration::from_millis(elapsed),
                RecordingCursorState::default(),
                100.0,
                60.0,
            );
        }

        assert_eq!(collector.selected.len(), 1);
        assert_eq!(collector.selected[0].elapsed_ms, 0);
        assert_eq!(
            collector.selected[0].rendered.width(),
            CONTACT_SHEET_CELL_WIDTH
        );
        assert_eq!(collector.pending.len(), 1);
    }

    #[test]
    fn test_contact_sheet_exact_duplicate_advances_final_frame_without_reselection() {
        let mut collector = ContactSheetCollector::new(0.05);
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(100, 50))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        collector.consider(
            &png,
            Duration::from_millis(10),
            RecordingCursorState::default(),
            100.0,
            50.0,
        );
        collector.consider(
            &png,
            Duration::from_millis(900),
            RecordingCursorState {
                x: 40.0,
                y: 20.0,
                buttons: 0,
                visible: true,
            },
            100.0,
            50.0,
        );

        assert_eq!(collector.latest.as_ref().unwrap().cursor.x, 40.0);
        assert_eq!(collector.latest.as_ref().unwrap().image_data, png);
        let frames = collector.finish();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].elapsed_ms, 900);
        assert!(frames
            .iter()
            .all(|frame| frame.rendered.width() == CONTACT_SHEET_CELL_WIDTH));
    }

    #[test]
    fn test_recording_start_rejects_out_of_range_fps() {
        let mut state = RecordingState::new();
        let too_high = recording_start(&mut state, "/tmp/test.webm", options(Some(61)));
        assert!(too_high.unwrap_err().contains("valid range: 1-60"));
        assert!(!state.active);

        let zero = recording_start(&mut state, "/tmp/test.webm", options(Some(0)));
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
            let err = recording_start(&mut state, path, RecordingOptions::default()).unwrap_err();
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
        let cmd = build_ffmpeg_command("/tmp/OUT.WEBM", DEFAULT_FPS, false);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libvpx"));
        assert!(!args_str.contains(&"libx264"));
    }

    #[test]
    fn test_build_ffmpeg_command_hides_banner() {
        let cmd = build_ffmpeg_command("/tmp/out.webm", DEFAULT_FPS, false);
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
        recording_start(&mut state, "/tmp/test1.mp4", options(None)).unwrap();
        let result = recording_start(&mut state, "/tmp/test2.mp4", options(None));
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
        recording_start(&mut state, "/tmp/test.mp4", options(None)).unwrap();
        let result = recording_stop(&mut state);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No frames"));
        assert!(!state.active);
    }

    #[test]
    fn test_recording_stop_reports_fps() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test.webm", options(Some(60))).unwrap();
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
        let cmd = build_ffmpeg_command("/tmp/out.webm", DEFAULT_FPS, false);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libvpx"));
        assert!(args_str.contains(&"/tmp/out.webm"));
        assert!(args_str.contains(&"8000k"));
        assert!(args_str.contains(&"18"));
        assert!(args_str.contains(&"image2pipe"));
        assert!(args_str.contains(&"png"));
        assert!(!args_str.contains(&"-use_wallclock_as_timestamps"));
        assert!(args_str.contains(&"vfr"));
    }

    #[test]
    fn test_build_ffmpeg_command_mp4() {
        let cmd = build_ffmpeg_command("/tmp/out.mp4", DEFAULT_FPS, false);
        let args: Vec<&std::ffi::OsStr> = cmd.as_std().get_args().collect();
        let args_str: Vec<&str> = args.iter().filter_map(|a| a.to_str()).collect();
        assert!(args_str.contains(&"libx264"));
        assert!(args_str.contains(&"/tmp/out.mp4"));
    }

    #[test]
    fn test_build_ffmpeg_command_passes_framerate() {
        let cmd = build_ffmpeg_command("/tmp/out.webm", 60, false);
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .filter_map(|a| a.to_str().map(String::from))
            .collect();
        let framerate = args.windows(2).find(|pair| pair[0] == "-framerate");
        assert_eq!(framerate.map(|pair| pair[1].as_str()), Some("60"));
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
        let cmd = build_ffmpeg_command("/tmp/out.mp4", DEFAULT_FPS, false);
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
