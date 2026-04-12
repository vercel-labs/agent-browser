use serde::Serialize;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::cdp::client::CdpClient;
use super::cdp::types::{CaptureScreenshotParams, CaptureScreenshotResult};

/// Capture N screenshots at a fixed interval, saving each as a numbered file.
/// Returns a list of saved file paths plus optionally an animated GIF path.
pub async fn burst_capture(
    client: &CdpClient,
    session_id: &str,
    count: u32,
    interval_ms: u64,
    format: &str,
    quality: Option<i32>,
    output_dir: &str,
    gif_path: Option<&str>,
) -> Result<BurstResult, String> {
    let dir = PathBuf::from(output_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create output dir {}: {}", output_dir, e))?;

    let ext = if format == "jpeg" { "jpg" } else { "png" };
    let params = CaptureScreenshotParams {
        format: Some(format.to_string()),
        quality: if format == "jpeg" {
            quality.or(Some(80))
        } else {
            None
        },
        clip: None,
        from_surface: Some(true),
        capture_beyond_viewport: None,
    };

    let mut paths = Vec::with_capacity(count as usize);
    let mut frames_for_gif: Vec<Vec<u8>> = Vec::new();
    let collect_gif = gif_path.is_some();

    let mut interval = tokio::time::interval(Duration::from_millis(interval_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    for i in 0..count {
        interval.tick().await;

        let result: CaptureScreenshotResult = client
            .send_command_typed("Page.captureScreenshot", &params, Some(session_id))
            .await?;

        let bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            &result.data,
        )
        .map_err(|e| format!("Base64 decode error on frame {}: {}", i, e))?;

        let frame_path = dir
            .join(format!("frame-{:04}.{}", i, ext))
            .to_string_lossy()
            .to_string();

        std::fs::write(&frame_path, &bytes)
            .map_err(|e| format!("Failed to write frame {}: {}", i, e))?;

        paths.push(frame_path);

        if collect_gif {
            frames_for_gif.push(bytes);
        }
    }

    let gif_output = if let Some(gif_dest) = gif_path {
        let delay_centisecs = (interval_ms as u16) / 10;
        encode_gif(&frames_for_gif, gif_dest, delay_centisecs)?;
        Some(gif_dest.to_string())
    } else {
        None
    };

    Ok(BurstResult {
        frames: paths,
        gif: gif_output,
    })
}

pub struct BurstResult {
    pub frames: Vec<String>,
    pub gif: Option<String>,
}

/// Use CDP Page.startScreencast for efficient frame streaming.
/// Collects frames for `duration_ms` and saves them, optionally encoding a GIF.
pub async fn screencast_capture(
    client: &CdpClient,
    session_id: &str,
    duration_ms: u64,
    format: &str,
    quality: Option<i32>,
    max_width: Option<u32>,
    max_height: Option<u32>,
    every_nth_frame: Option<u32>,
    output_dir: &str,
    gif_path: Option<&str>,
) -> Result<BurstResult, String> {
    let dir = PathBuf::from(output_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create output dir {}: {}", output_dir, e))?;

    let ext = if format == "jpeg" { "jpg" } else { "png" };

    // Subscribe to CDP events before starting screencast
    let mut event_rx = client.subscribe();

    // Start screencast
    let mut start_params = serde_json::json!({
        "format": format,
    });
    if let Some(q) = quality {
        start_params["quality"] = serde_json::json!(q);
    }
    if let Some(w) = max_width {
        start_params["maxWidth"] = serde_json::json!(w);
    }
    if let Some(h) = max_height {
        start_params["maxHeight"] = serde_json::json!(h);
    }
    if let Some(n) = every_nth_frame {
        start_params["everyNthFrame"] = serde_json::json!(n);
    }

    client
        .send_command(
            "Page.startScreencast",
            Some(start_params),
            Some(session_id),
        )
        .await?;

    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(duration_ms);

    loop {
        let timeout = tokio::time::sleep_until(deadline);
        tokio::pin!(timeout);

        tokio::select! {
            _ = &mut timeout => break,
            event = event_rx.recv() => {
                match event {
                    Ok(cdp_event) => {
                        if cdp_event.method == "Page.screencastFrame" {
                            let params = &cdp_event.params;
                            // Acknowledge the frame so Chrome keeps sending
                            let ack_session = cdp_event
                                .session_id
                                .as_deref()
                                .unwrap_or(session_id);
                            if let Some(frame_number) = params.get("sessionId").and_then(|v| v.as_i64()) {
                                let _ = client.send_command(
                                    "Page.screencastFrameAck",
                                    Some(serde_json::json!({ "sessionId": frame_number })),
                                    Some(ack_session),
                                ).await;
                            }

                            if let Some(data) = params.get("data").and_then(|v| v.as_str()) {
                                if let Ok(bytes) = base64::Engine::decode(
                                    &base64::engine::general_purpose::STANDARD,
                                    data,
                                ) {
                                    let idx = frames.len();
                                    let frame_path = dir
                                        .join(format!("frame-{:04}.{}", idx, ext))
                                        .to_string_lossy()
                                        .to_string();

                                    if std::fs::write(&frame_path, &bytes).is_ok() {
                                        paths.push(frame_path);
                                    }
                                    frames.push(bytes);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    // Stop screencast
    let _ = client
        .send_command_no_params("Page.stopScreencast", Some(session_id))
        .await;

    if frames.is_empty() {
        return Err("No screencast frames captured".to_string());
    }

    let gif_output = if let Some(gif_dest) = gif_path {
        // Estimate delay from duration and frame count
        let delay_centisecs = if frames.len() > 1 {
            ((duration_ms as f64 / frames.len() as f64) / 10.0).max(1.0) as u16
        } else {
            10
        };
        encode_gif(&frames, gif_dest, delay_centisecs)?;
        Some(gif_dest.to_string())
    } else {
        None
    };

    Ok(BurstResult {
        frames: paths,
        gif: gif_output,
    })
}

/// Encode a sequence of image bytes (PNG or JPEG) into an animated GIF.
fn encode_gif(frames: &[Vec<u8>], output_path: &str, delay_centisecs: u16) -> Result<(), String> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Frame, RgbaImage};
    use std::fs::File;

    if frames.is_empty() {
        return Err("No frames to encode".to_string());
    }

    let file =
        File::create(output_path).map_err(|e| format!("Failed to create GIF file: {}", e))?;

    let mut encoder = GifEncoder::new_with_speed(file, 10);
    encoder
        .set_repeat(Repeat::Infinite)
        .map_err(|e| format!("Failed to set GIF repeat: {}", e))?;

    for (i, frame_bytes) in frames.iter().enumerate() {
        let img = image::load_from_memory(frame_bytes)
            .map_err(|e| format!("Failed to decode frame {}: {}", i, e))?;

        let rgba: RgbaImage = img.to_rgba8();
        let delay = image::Delay::from_saturating_duration(Duration::from_millis(
            delay_centisecs as u64 * 10,
        ));
        let frame = Frame::from_parts(rgba, 0, 0, delay);
        encoder
            .encode_frame(frame)
            .map_err(|e| format!("Failed to encode frame {}: {}", i, e))?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Interactive screencast recording with action metadata
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CapturedFrame {
    bytes: Vec<u8>,
    time_ms: u64,
}

#[derive(Clone, Serialize)]
pub struct TimelineEntry {
    #[serde(rename = "timeMs")]
    pub time_ms: u64,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame: Option<usize>,
}

pub struct ScreencastRecording {
    start_time: Instant,
    frames: Arc<Mutex<Vec<CapturedFrame>>>,
    timeline: Vec<TimelineEntry>,
    format: String,
    collector_task: Option<tokio::task::JoinHandle<()>>,
}

impl ScreencastRecording {
    /// Start a new recording with a background task that collects frames
    /// from the CDP event broadcast channel.
    pub fn new(
        format: &str,
        _quality: Option<i32>,
        client: Arc<CdpClient>,
        session_id: &str,
    ) -> Self {
        let frames: Arc<Mutex<Vec<CapturedFrame>>> = Arc::new(Mutex::new(Vec::new()));
        let frames_clone = frames.clone();
        let start_time = Instant::now();
        let client_arc = client.clone();
        let session_owned = session_id.to_string();

        // Background task: collect screencast frames from CDP events,
        // with periodic screenshot polling as fallback for static pages.
        let mut event_rx = client.subscribe();
        let task = tokio::spawn(async move {
            let mut poll_interval =
                tokio::time::interval(Duration::from_millis(250));
            poll_interval.set_missed_tick_behavior(
                tokio::time::MissedTickBehavior::Skip,
            );

            let poll_params = CaptureScreenshotParams {
                format: Some("jpeg".to_string()),
                quality: Some(80),
                clip: None,
                from_surface: Some(true),
                capture_beyond_viewport: None,
            };

            loop {
                tokio::select! {
                    event = event_rx.recv() => {
                        match event {
                            Ok(evt) => {
                                if evt.method == "Page.screencastFrame" {
                                    // ACK the frame so Chrome keeps sending
                                    if let Some(sid) =
                                        evt.params.get("sessionId").and_then(|v| v.as_i64())
                                    {
                                        let _ = client_arc
                                            .send_command(
                                                "Page.screencastFrameAck",
                                                Some(serde_json::json!({ "sessionId": sid })),
                                                Some(&session_owned),
                                            )
                                            .await;
                                    }

                                    if let Some(data) =
                                        evt.params.get("data").and_then(|v| v.as_str())
                                    {
                                        if let Ok(bytes) = base64::Engine::decode(
                                            &base64::engine::general_purpose::STANDARD,
                                            data,
                                        ) {
                                            let time_ms =
                                                start_time.elapsed().as_millis() as u64;
                                            let mut guard = frames_clone.lock().await;
                                            guard.push(CapturedFrame { bytes, time_ms });
                                            // Reset poll interval after receiving a screencast frame
                                            poll_interval.reset();
                                        }
                                    }
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        }
                    }
                    _ = poll_interval.tick() => {
                        // Fallback polling: capture a screenshot when no screencast
                        // frames are arriving (static page or idle periods)
                        if let Ok(result) = client_arc
                            .send_command_typed::<_, CaptureScreenshotResult>(
                                "Page.captureScreenshot",
                                &poll_params,
                                Some(&session_owned),
                            )
                            .await
                        {
                            if let Ok(bytes) = base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                &result.data,
                            ) {
                                let time_ms = start_time.elapsed().as_millis() as u64;
                                let mut guard = frames_clone.lock().await;
                                guard.push(CapturedFrame { bytes, time_ms });
                            }
                        }
                    }
                }
            }
        });

        let mut rec = Self {
            start_time,
            frames,
            timeline: Vec::new(),
            format: format.to_string(),
            collector_task: Some(task),
        };
        rec.timeline.push(TimelineEntry {
            time_ms: 0,
            action: String::new(),
            selector: None,
            value: None,
            event: Some("screencast_start".to_string()),
            frame: Some(0),
        });
        rec
    }

    pub fn log_action(&mut self, action: &str, cmd: &Value) {
        let time_ms = self.start_time.elapsed().as_millis() as u64;
        let selector = cmd
            .get("selector")
            .and_then(|v| v.as_str())
            .map(String::from);
        let value = cmd
            .get("value")
            .or_else(|| cmd.get("text"))
            .and_then(|v| v.as_str())
            .map(String::from);

        self.timeline.push(TimelineEntry {
            time_ms,
            action: action.to_string(),
            selector,
            value,
            event: None,
            frame: None, // filled in during finish()
        });
    }

    pub async fn finish(
        mut self,
        output_dir: &str,
        gif_path: Option<&str>,
    ) -> Result<ScreencastStopResult, String> {
        let stop_time = self.start_time.elapsed().as_millis() as u64;

        // Stop the background collector
        if let Some(task) = self.collector_task.take() {
            task.abort();
        }

        // Give the collector task a moment to process any remaining frames
        tokio::time::sleep(Duration::from_millis(50)).await;

        let frames = match Arc::try_unwrap(self.frames) {
            Ok(mutex) => mutex.into_inner(),
            Err(arc) => {
                let guard = arc.lock().await;
                guard.clone()
            }
        };

        let dir = PathBuf::from(output_dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create output dir: {}", e))?;

        let ext = if self.format == "jpeg" { "jpg" } else { "png" };
        let mut paths = Vec::with_capacity(frames.len());

        for (i, frame) in frames.iter().enumerate() {
            let frame_path = dir
                .join(format!("frame-{:04}.{}", i, ext))
                .to_string_lossy()
                .to_string();
            std::fs::write(&frame_path, &frame.bytes)
                .map_err(|e| format!("Failed to write frame {}: {}", i, e))?;
            paths.push(frame_path);
        }

        // Correlate timeline entries with nearest frame by timestamp
        for entry in &mut self.timeline {
            if entry.frame.is_some() {
                continue; // already set (e.g. screencast_start)
            }
            entry.frame = Some(nearest_frame_index(&frames, entry.time_ms));
        }

        // Add stop event
        self.timeline.push(TimelineEntry {
            time_ms: stop_time,
            action: String::new(),
            selector: None,
            value: None,
            event: Some("screencast_stop".to_string()),
            frame: Some(frames.len().saturating_sub(1)),
        });

        let gif_output = if let Some(gif_dest) = gif_path {
            if frames.is_empty() {
                None
            } else {
                let frame_bytes: Vec<Vec<u8>> =
                    frames.iter().map(|f| f.bytes.clone()).collect();
                let delay = if frames.len() > 1 {
                    let total_ms = frames.last().map(|f| f.time_ms).unwrap_or(stop_time);
                    ((total_ms as f64 / frames.len() as f64) / 10.0).max(1.0) as u16
                } else {
                    10
                };
                encode_gif(&frame_bytes, gif_dest, delay)?;
                Some(gif_dest.to_string())
            }
        } else {
            None
        };

        Ok(ScreencastStopResult {
            frames: paths,
            timeline: self.timeline,
            gif: gif_output,
        })
    }
}

fn nearest_frame_index(frames: &[CapturedFrame], target_ms: u64) -> usize {
    if frames.is_empty() {
        return 0;
    }
    let mut best = 0;
    let mut best_diff = u64::MAX;
    for (i, frame) in frames.iter().enumerate() {
        let diff = if frame.time_ms > target_ms {
            frame.time_ms - target_ms
        } else {
            target_ms - frame.time_ms
        };
        if diff < best_diff {
            best_diff = diff;
            best = i;
        }
    }
    best
}

pub struct ScreencastStopResult {
    pub frames: Vec<String>,
    pub timeline: Vec<TimelineEntry>,
    pub gif: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_gif_rejects_empty_frames() {
        let result = encode_gif(&[], "/tmp/test_empty.gif", 10);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No frames"));
    }

    #[test]
    fn encode_gif_produces_valid_file() {
        // Create a minimal 2x2 red PNG frame
        let mut buf = Vec::new();
        {
            let mut img = image::RgbaImage::new(2, 2);
            for pixel in img.pixels_mut() {
                *pixel = image::Rgba([255, 0, 0, 255]);
            }
            let mut cursor = std::io::Cursor::new(&mut buf);
            img.write_to(&mut cursor, image::ImageFormat::Png).unwrap();
        }

        let frames = vec![buf.clone(), buf];
        let path = "/tmp/agent_browser_test_encode.gif";
        let result = encode_gif(&frames, path, 10);
        assert!(result.is_ok());

        // Verify file exists and has GIF magic bytes
        let data = std::fs::read(path).unwrap();
        assert!(data.starts_with(b"GIF"));
        let _ = std::fs::remove_file(path);
    }
}
