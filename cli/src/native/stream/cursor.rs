use serde_json::json;
use std::time::{Duration, Instant};

use crate::native::cdp::client::CdpClient;
use crate::native::cdp::types::{EvaluateParams, EvaluateResult};

use super::timestamp_ms;

const CURSOR_SAMPLE_INTERVAL: Duration = Duration::from_millis(75);

const ALLOWED_CURSORS: &[&str] = &[
    "auto",
    "default",
    "none",
    "context-menu",
    "help",
    "pointer",
    "progress",
    "wait",
    "cell",
    "crosshair",
    "text",
    "vertical-text",
    "alias",
    "copy",
    "move",
    "no-drop",
    "not-allowed",
    "grab",
    "grabbing",
    "all-scroll",
    "col-resize",
    "row-resize",
    "n-resize",
    "e-resize",
    "s-resize",
    "w-resize",
    "ne-resize",
    "nw-resize",
    "se-resize",
    "sw-resize",
    "ew-resize",
    "ns-resize",
    "nesw-resize",
    "nwse-resize",
    "zoom-in",
    "zoom-out",
];

#[derive(Debug, Default)]
pub(super) struct CursorSampler {
    last_sample_at: Option<Instant>,
    last_cursor: Option<String>,
}

impl CursorSampler {
    pub(super) fn should_sample(&mut self, now: Instant) -> bool {
        if self
            .last_sample_at
            .is_some_and(|last| now.duration_since(last) < CURSOR_SAMPLE_INTERVAL)
        {
            return false;
        }
        self.last_sample_at = Some(now);
        true
    }

    pub(super) fn should_broadcast(&mut self, cursor: &str) -> bool {
        if self.last_cursor.as_deref() == Some(cursor) {
            return false;
        }
        self.last_cursor = Some(cursor.to_string());
        true
    }
}

pub(super) async fn cursor_at_point(
    client: &CdpClient,
    session_id: Option<&str>,
    x: f64,
    y: f64,
) -> Result<String, String> {
    let expression = format!(
        r#"
(() => {{
  const el = document.elementFromPoint({}, {});
  if (!el) return "default";
  return getComputedStyle(el).cursor || "default";
}})()
"#,
        finite_or_zero(x),
        finite_or_zero(y)
    );

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            session_id,
        )
        .await?;

    let raw = result
        .result
        .value
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "default".to_string());

    Ok(sanitize_cursor(&raw))
}

pub(super) fn cursor_message(cursor: &str, x: f64, y: f64) -> String {
    json!({
        "type": "cursor",
        "cursor": sanitize_cursor(cursor),
        "x": finite_or_zero(x),
        "y": finite_or_zero(y),
        "timestamp": timestamp_ms(),
    })
    .to_string()
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn sanitize_cursor(raw: &str) -> String {
    raw.split(',')
        .rev()
        .map(|part| part.trim().to_ascii_lowercase())
        .find(|candidate| ALLOWED_CURSORS.contains(&candidate.as_str()))
        .unwrap_or_else(|| "default".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn sanitize_cursor_accepts_css_keywords() {
        assert_eq!(sanitize_cursor("pointer"), "pointer");
        assert_eq!(sanitize_cursor("TEXT"), "text");
        assert_eq!(sanitize_cursor("not-allowed"), "not-allowed");
    }

    #[test]
    fn sanitize_cursor_uses_fallback_keyword_from_url_cursor() {
        assert_eq!(
            sanitize_cursor("url(\"https://example.test/cursor.cur\"), pointer"),
            "pointer"
        );
    }

    #[test]
    fn sanitize_cursor_rejects_unknown_values() {
        assert_eq!(sanitize_cursor("url(javascript:alert(1))"), "default");
        assert_eq!(sanitize_cursor("inherit"), "default");
        assert_eq!(sanitize_cursor(""), "default");
    }

    #[test]
    fn cursor_sampler_throttles_samples() {
        let mut sampler = CursorSampler::default();
        let now = Instant::now();
        assert!(sampler.should_sample(now));
        assert!(!sampler.should_sample(now + Duration::from_millis(30)));
        assert!(sampler.should_sample(now + Duration::from_millis(80)));
    }

    #[test]
    fn cursor_sampler_suppresses_duplicate_cursor_values() {
        let mut sampler = CursorSampler::default();
        assert!(sampler.should_broadcast("default"));
        assert!(!sampler.should_broadcast("default"));
        assert!(sampler.should_broadcast("pointer"));
    }

    #[test]
    fn cursor_message_is_sanitized() {
        let message: Value = serde_json::from_str(&cursor_message("inherit", f64::NAN, 12.0))
            .expect("cursor message should be valid json");
        assert_eq!(message["type"], "cursor");
        assert_eq!(message["cursor"], "default");
        assert_eq!(message["x"], 0.0);
        assert_eq!(message["y"], 12.0);
        assert!(message["timestamp"].as_u64().is_some());
    }
}
