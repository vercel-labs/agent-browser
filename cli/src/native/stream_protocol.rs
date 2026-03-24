//! Typed message definitions for the stream server WebSocket protocol.
//!
//! These types are the Rust representation of the protocol defined in
//! `schemas/stream-server.asyncapi.yaml`. All server-to-client and
//! client-to-server messages are serialized/deserialized through these
//! structs rather than ad-hoc `json!()` macros, ensuring the wire format
//! stays consistent with the schema.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Server -> Client messages
// ---------------------------------------------------------------------------

/// Screencast frame broadcast to all connected WebSocket clients.
#[derive(Debug, Clone, Serialize)]
pub struct FrameMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub data: &'a str,
    pub metadata: FrameMetadataWire,
}

impl<'a> FrameMessage<'a> {
    pub fn new(data: &'a str, metadata: &super::stream::FrameMetadata) -> Self {
        Self {
            msg_type: "frame",
            data,
            metadata: FrameMetadataWire::from(metadata),
        }
    }
}

/// Wire format for frame metadata (camelCase field names matching the schema).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameMetadataWire {
    pub offset_top: f64,
    pub page_scale_factor: f64,
    pub device_width: u32,
    pub device_height: u32,
    pub scroll_offset_x: f64,
    pub scroll_offset_y: f64,
    pub timestamp: u64,
}

impl From<&super::stream::FrameMetadata> for FrameMetadataWire {
    fn from(m: &super::stream::FrameMetadata) -> Self {
        Self {
            offset_top: m.offset_top,
            page_scale_factor: m.page_scale_factor,
            device_width: m.device_width,
            device_height: m.device_height,
            scroll_offset_x: m.scroll_offset_x,
            scroll_offset_y: m.scroll_offset_y,
            timestamp: m.timestamp,
        }
    }
}

/// Status message sent on connection and when screencast state changes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusMessage {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub connected: bool,
    pub screencasting: bool,
    pub viewport_width: u32,
    pub viewport_height: u32,
    pub engine: String,
    pub recording: bool,
}

impl StatusMessage {
    pub fn new(
        connected: bool,
        screencasting: bool,
        viewport_width: u32,
        viewport_height: u32,
        engine: &str,
        recording: bool,
    ) -> Self {
        Self {
            msg_type: "status",
            connected,
            screencasting,
            viewport_width,
            viewport_height,
            engine: engine.to_string(),
            recording,
        }
    }
}

/// Error message sent to clients.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub message: &'a str,
}

impl<'a> ErrorMessage<'a> {
    pub fn new(message: &'a str) -> Self {
        Self {
            msg_type: "error",
            message,
        }
    }
}

// ---------------------------------------------------------------------------
// Client -> Server messages
// ---------------------------------------------------------------------------

/// Envelope for all client-to-server messages. Dispatched by `msg_type`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    #[serde(rename = "input_mouse")]
    Mouse(InputMouseMessage),
    #[serde(rename = "input_keyboard")]
    Keyboard(InputKeyboardMessage),
    #[serde(rename = "input_touch")]
    Touch(InputTouchMessage),
    #[serde(rename = "status")]
    StatusRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputMouseMessage {
    pub event_type: String,
    pub x: f64,
    pub y: f64,
    #[serde(default = "default_button")]
    pub button: String,
    #[serde(default)]
    pub click_count: i64,
    #[serde(default)]
    pub delta_x: f64,
    #[serde(default)]
    pub delta_y: f64,
    #[serde(default)]
    pub modifiers: i64,
}

fn default_button() -> String {
    "none".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputKeyboardMessage {
    pub event_type: String,
    pub key: Option<serde_json::Value>,
    pub code: Option<serde_json::Value>,
    pub text: Option<serde_json::Value>,
    #[serde(default)]
    pub windows_virtual_key_code: i64,
    #[serde(default)]
    pub modifiers: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputTouchMessage {
    pub event_type: String,
    pub touch_points: serde_json::Value,
    #[serde(default)]
    pub modifiers: i64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_message_serializes_correctly() {
        let meta = super::super::stream::FrameMetadata::default();
        let msg = FrameMessage::new("abc123", &meta);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "frame");
        assert_eq!(json["data"], "abc123");
        assert_eq!(json["metadata"]["deviceWidth"], 1280);
        assert_eq!(json["metadata"]["deviceHeight"], 720);
        assert_eq!(json["metadata"]["pageScaleFactor"], 1.0);
        assert_eq!(json["metadata"]["offsetTop"], 0.0);
        assert_eq!(json["metadata"]["scrollOffsetX"], 0.0);
        assert_eq!(json["metadata"]["scrollOffsetY"], 0.0);
    }

    #[test]
    fn status_message_serializes_correctly() {
        let msg = StatusMessage::new(true, false, 1920, 1080, "chromium", true);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "status");
        assert_eq!(json["connected"], true);
        assert_eq!(json["screencasting"], false);
        assert_eq!(json["viewportWidth"], 1920);
        assert_eq!(json["viewportHeight"], 1080);
        assert_eq!(json["engine"], "chromium");
        assert_eq!(json["recording"], true);
    }

    #[test]
    fn error_message_serializes_correctly() {
        let msg = ErrorMessage::new("something broke");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "error");
        assert_eq!(json["message"], "something broke");
    }

    #[test]
    fn deserialize_mouse_input() {
        let raw = r#"{"type":"input_mouse","eventType":"mousePressed","x":100,"y":200,"button":"left","clickCount":1}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        match msg {
            ClientMessage::Mouse(m) => {
                assert_eq!(m.event_type, "mousePressed");
                assert_eq!(m.x, 100.0);
                assert_eq!(m.y, 200.0);
                assert_eq!(m.button, "left");
                assert_eq!(m.click_count, 1);
            }
            _ => panic!("expected Mouse"),
        }
    }

    #[test]
    fn deserialize_keyboard_input() {
        let raw = r#"{"type":"input_keyboard","eventType":"keyDown","key":"Enter","code":"Enter"}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        match msg {
            ClientMessage::Keyboard(k) => {
                assert_eq!(k.event_type, "keyDown");
                assert_eq!(k.key, Some(serde_json::Value::String("Enter".into())));
            }
            _ => panic!("expected Keyboard"),
        }
    }

    #[test]
    fn deserialize_touch_input() {
        let raw = r#"{"type":"input_touch","eventType":"touchStart","touchPoints":[{"x":50,"y":75}]}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        match msg {
            ClientMessage::Touch(t) => {
                assert_eq!(t.event_type, "touchStart");
                assert!(t.touch_points.is_array());
            }
            _ => panic!("expected Touch"),
        }
    }

    #[test]
    fn deserialize_status_request() {
        let raw = r#"{"type":"status"}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        assert!(matches!(msg, ClientMessage::StatusRequest));
    }

    #[test]
    fn deserialize_mouse_defaults() {
        let raw = r#"{"type":"input_mouse","eventType":"mouseMoved","x":0,"y":0}"#;
        let msg: ClientMessage = serde_json::from_str(raw).unwrap();
        match msg {
            ClientMessage::Mouse(m) => {
                assert_eq!(m.button, "none");
                assert_eq!(m.click_count, 0);
                assert_eq!(m.delta_x, 0.0);
                assert_eq!(m.modifiers, 0);
            }
            _ => panic!("expected Mouse"),
        }
    }
}
