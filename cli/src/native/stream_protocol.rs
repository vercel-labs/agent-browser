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

/// Notification that a command has begun executing.
#[derive(Debug, Clone, Serialize)]
pub struct CommandMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub action: &'a str,
    pub id: &'a str,
    pub params: &'a serde_json::Value,
    pub timestamp: u64,
}

impl<'a> CommandMessage<'a> {
    pub fn new(action: &'a str, id: &'a str, params: &'a serde_json::Value) -> Self {
        Self {
            msg_type: "command",
            action,
            id,
            params,
            timestamp: super::stream::timestamp_ms(),
        }
    }
}

/// Result of a completed command execution.
#[derive(Debug, Clone, Serialize)]
pub struct ResultMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub id: &'a str,
    pub action: &'a str,
    pub success: bool,
    pub data: &'a serde_json::Value,
    pub duration_ms: u64,
    pub timestamp: u64,
}

impl<'a> ResultMessage<'a> {
    pub fn new(
        id: &'a str,
        action: &'a str,
        success: bool,
        data: &'a serde_json::Value,
        duration_ms: u64,
    ) -> Self {
        Self {
            msg_type: "result",
            id,
            action,
            success,
            data,
            duration_ms,
            timestamp: super::stream::timestamp_ms(),
        }
    }
}

/// Console log message from the browser page.
#[derive(Debug, Clone, Serialize)]
pub struct ConsoleMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub level: &'a str,
    pub text: &'a str,
    pub timestamp: u64,
}

impl<'a> ConsoleMessage<'a> {
    pub fn new(level: &'a str, text: &'a str) -> Self {
        Self {
            msg_type: "console",
            level,
            text,
            timestamp: super::stream::timestamp_ms(),
        }
    }
}

/// Uncaught exception from the browser page.
#[derive(Debug, Clone, Serialize)]
pub struct PageErrorMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub text: &'a str,
    pub line: Option<i64>,
    pub column: Option<i64>,
    pub timestamp: u64,
}

impl<'a> PageErrorMessage<'a> {
    pub fn new(text: &'a str, line: Option<i64>, column: Option<i64>) -> Self {
        Self {
            msg_type: "page_error",
            text,
            line,
            column,
            timestamp: super::stream::timestamp_ms(),
        }
    }
}

/// Current browser tab list.
#[derive(Debug, Clone, Serialize)]
pub struct TabsMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub tabs: &'a [serde_json::Value],
    pub timestamp: u64,
}

impl<'a> TabsMessage<'a> {
    pub fn new(tabs: &'a [serde_json::Value]) -> Self {
        Self {
            msg_type: "tabs",
            tabs,
            timestamp: super::stream::timestamp_ms(),
        }
    }
}

/// URL navigation event for the active tab.
#[derive(Debug, Clone, Serialize)]
pub struct UrlMessage<'a> {
    #[serde(rename = "type")]
    pub msg_type: &'static str,
    pub url: &'a str,
    pub timestamp: u64,
}

impl<'a> UrlMessage<'a> {
    pub fn new(url: &'a str) -> Self {
        Self {
            msg_type: "url",
            url,
            timestamp: super::stream::timestamp_ms(),
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
    fn command_message_serializes_correctly() {
        let params = serde_json::json!({"url": "https://example.com"});
        let msg = CommandMessage::new("navigate", "cmd-1", &params);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "command");
        assert_eq!(json["action"], "navigate");
        assert_eq!(json["id"], "cmd-1");
        assert_eq!(json["params"]["url"], "https://example.com");
        assert!(json["timestamp"].as_u64().is_some());
    }

    #[test]
    fn result_message_serializes_correctly() {
        let data = serde_json::json!({"ok": true});
        let msg = ResultMessage::new("cmd-1", "navigate", true, &data, 42);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "result");
        assert_eq!(json["id"], "cmd-1");
        assert_eq!(json["action"], "navigate");
        assert_eq!(json["success"], true);
        assert_eq!(json["data"]["ok"], true);
        assert_eq!(json["duration_ms"], 42);
        assert!(json["timestamp"].as_u64().is_some());
    }

    #[test]
    fn console_message_serializes_correctly() {
        let msg = ConsoleMessage::new("warn", "something happened");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "console");
        assert_eq!(json["level"], "warn");
        assert_eq!(json["text"], "something happened");
        assert!(json["timestamp"].as_u64().is_some());
    }

    #[test]
    fn page_error_message_serializes_correctly() {
        let msg = PageErrorMessage::new("ReferenceError: x is not defined", Some(10), Some(5));
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "page_error");
        assert_eq!(json["text"], "ReferenceError: x is not defined");
        assert_eq!(json["line"], 10);
        assert_eq!(json["column"], 5);
        assert!(json["timestamp"].as_u64().is_some());
    }

    #[test]
    fn page_error_message_with_none_fields() {
        let msg = PageErrorMessage::new("Unknown error", None, None);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "page_error");
        assert!(json["line"].is_null());
        assert!(json["column"].is_null());
    }

    #[test]
    fn tabs_message_serializes_correctly() {
        let tabs = vec![serde_json::json!({"url": "https://example.com", "active": true})];
        let msg = TabsMessage::new(&tabs);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "tabs");
        assert!(json["tabs"].is_array());
        assert_eq!(json["tabs"][0]["url"], "https://example.com");
        assert!(json["timestamp"].as_u64().is_some());
    }

    #[test]
    fn url_message_serializes_correctly() {
        let msg = UrlMessage::new("https://example.com/page");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["type"], "url");
        assert_eq!(json["url"], "https://example.com/page");
        assert!(json["timestamp"].as_u64().is_some());
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
