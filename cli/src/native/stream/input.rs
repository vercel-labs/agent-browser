use serde_json::{json, Value};

#[derive(Debug, Default)]
pub(super) struct InputState {
    mouse_buttons: i32,
}

impl InputState {
    pub(super) fn mouse_payload(&mut self, parsed: &Value) -> Value {
        let event_type = parsed
            .get("eventType")
            .and_then(|v| v.as_str())
            .unwrap_or("mouseMoved");
        let button = parsed
            .get("button")
            .and_then(|v| v.as_str())
            .unwrap_or("none");

        if let Some(buttons) = parsed.get("buttons").and_then(|v| v.as_i64()) {
            self.mouse_buttons = buttons as i32;
        } else {
            match event_type {
                "mousePressed" => self.mouse_buttons |= mouse_button_mask(button),
                "mouseReleased" => self.mouse_buttons &= !mouse_button_mask(button),
                _ => {}
            }
        }

        json!({
            "type": event_type,
            "x": finite_or_zero(parsed.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0)),
            "y": finite_or_zero(parsed.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0)),
            "button": button_for_event(event_type, button, self.mouse_buttons),
            "buttons": self.mouse_buttons,
            "clickCount": parsed.get("clickCount").and_then(|v| v.as_i64()).unwrap_or(default_click_count(event_type)),
            "deltaX": parsed.get("deltaX").and_then(|v| v.as_f64()).unwrap_or(0.0),
            "deltaY": parsed.get("deltaY").and_then(|v| v.as_f64()).unwrap_or(0.0),
            "modifiers": parsed.get("modifiers").and_then(|v| v.as_i64()).unwrap_or(0),
        })
    }
}

fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

fn default_click_count(event_type: &str) -> i64 {
    if event_type == "mousePressed" {
        1
    } else {
        0
    }
}

fn mouse_button_mask(button: &str) -> i32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        "back" => 8,
        "forward" => 16,
        _ => 0,
    }
}

fn primary_button_from_mask(buttons: i32) -> &'static str {
    if buttons & 1 != 0 {
        "left"
    } else if buttons & 2 != 0 {
        "right"
    } else if buttons & 4 != 0 {
        "middle"
    } else if buttons & 8 != 0 {
        "back"
    } else if buttons & 16 != 0 {
        "forward"
    } else {
        "none"
    }
}

fn button_for_event(event_type: &str, button: &str, buttons: i32) -> &'static str {
    match event_type {
        "mouseMoved" | "mouseWheel" => primary_button_from_mask(buttons),
        _ => match button {
            "left" => "left",
            "right" => "right",
            "middle" => "middle",
            "back" => "back",
            "forward" => "forward",
            _ => primary_button_from_mask(buttons),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_payload_tracks_pressed_button_through_drag() {
        let mut state = InputState::default();

        let down = state.mouse_payload(&json!({
            "eventType": "mousePressed",
            "x": 10,
            "y": 20,
            "button": "left",
        }));
        assert_eq!(down["buttons"], 1);
        assert_eq!(down["button"], "left");
        assert_eq!(down["clickCount"], 1);

        let moved = state.mouse_payload(&json!({
            "eventType": "mouseMoved",
            "x": 30,
            "y": 40,
        }));
        assert_eq!(moved["buttons"], 1);
        assert_eq!(moved["button"], "left");
        assert_eq!(moved["clickCount"], 0);

        let up = state.mouse_payload(&json!({
            "eventType": "mouseReleased",
            "x": 30,
            "y": 40,
            "button": "left",
        }));
        assert_eq!(up["buttons"], 0);
        assert_eq!(up["button"], "left");
    }

    #[test]
    fn mouse_payload_respects_explicit_buttons_bitfield() {
        let mut state = InputState::default();
        let moved = state.mouse_payload(&json!({
            "eventType": "mouseMoved",
            "x": 10,
            "y": 20,
            "buttons": 2,
        }));
        assert_eq!(moved["buttons"], 2);
        assert_eq!(moved["button"], "right");
    }

    #[test]
    fn mouse_payload_preserves_double_click_count() {
        let mut state = InputState::default();
        let down = state.mouse_payload(&json!({
            "eventType": "mousePressed",
            "button": "left",
            "clickCount": 2,
        }));
        assert_eq!(down["clickCount"], 2);
    }
}
