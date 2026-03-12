use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

pub struct RecordingState {
    pub active: bool,
    pub output_path: String,
    pub temp_dir: PathBuf,
    pub frame_count: u64,
}

impl RecordingState {
    pub fn new() -> Self {
        Self {
            active: false,
            output_path: String::new(),
            temp_dir: PathBuf::new(),
            frame_count: 0,
        }
    }
}

pub fn recording_start(state: &mut RecordingState, path: &str) -> Result<Value, String> {
    if state.active {
        return Err("Recording already active".to_string());
    }

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    let temp_dir = std::env::temp_dir().join(format!("agent-browser-recording-{}", timestamp));
    let _ = std::fs::create_dir_all(&temp_dir);

    state.active = true;
    state.output_path = path.to_string();
    state.temp_dir = temp_dir;
    state.frame_count = 0;

    Ok(json!({ "started": true, "path": path }))
}

pub fn recording_add_frame(state: &mut RecordingState, frame_data: &[u8]) {
    if !state.active {
        return;
    }

    let frame_path = state
        .temp_dir
        .join(format!("frame_{:06}.jpg", state.frame_count));
    let _ = std::fs::write(&frame_path, frame_data);
    state.frame_count += 1;
}

pub fn recording_stop(state: &mut RecordingState) -> Result<Value, String> {
    if !state.active {
        return Err("No recording in progress".to_string());
    }

    state.active = false;

    if state.frame_count == 0 {
        let _ = std::fs::remove_dir_all(&state.temp_dir);
        return Err("No frames captured".to_string());
    }

    let frame_pattern = state
        .temp_dir
        .join("frame_%06d.jpg")
        .to_string_lossy()
        .to_string();

    let output = &state.output_path;

    // Encode with ffmpeg
    let result = Command::new("ffmpeg")
        .args([
            "-y",
            "-framerate",
            "30",
            "-i",
            &frame_pattern,
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-preset",
            "fast",
            output,
        ])
        .output();

    let _ = std::fs::remove_dir_all(&state.temp_dir);

    match result {
        Ok(output_result) => {
            if output_result.status.success() {
                Ok(json!({ "path": output, "frames": state.frame_count }))
            } else {
                let stderr = String::from_utf8_lossy(&output_result.stderr);
                Err(format!(
                    "ffmpeg failed: {}",
                    stderr.chars().take(200).collect::<String>()
                ))
            }
        }
        Err(e) => Err(format!(
            "ffmpeg not found or failed to execute: {}. Install ffmpeg to enable recording.",
            e
        )),
    }
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
        // Cleanup
        let _ = std::fs::remove_dir_all(&state.temp_dir);
    }

    #[test]
    fn test_recording_start_while_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test1.mp4").unwrap();
        let temp_dir = state.temp_dir.clone();
        let result = recording_start(&mut state, "/tmp/test2.mp4");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already active"));
        let _ = std::fs::remove_dir_all(&temp_dir);
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
    fn test_recording_add_frame_inactive() {
        let mut state = RecordingState::new();
        recording_add_frame(&mut state, b"fake-frame");
        assert_eq!(state.frame_count, 0);
    }

    #[test]
    fn test_recording_add_frame_active() {
        let mut state = RecordingState::new();
        recording_start(&mut state, "/tmp/test.mp4").unwrap();
        recording_add_frame(&mut state, b"fake-frame-1");
        recording_add_frame(&mut state, b"fake-frame-2");
        assert_eq!(state.frame_count, 2);
        let _ = std::fs::remove_dir_all(&state.temp_dir);
    }
}
