use std::path::PathBuf;
use std::time::Duration;

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
