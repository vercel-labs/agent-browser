use serde_json::{json, Value};

use crate::native::actions::{
    interpolated_mouse_bend, interpolated_mouse_duration, interpolated_mouse_point,
    interpolated_mouse_steps,
};
use crate::native::interaction::{char_to_key_info, key_text, named_key_info};

/// Target of a pointer action: a viewport position.
#[derive(Clone, Debug)]
pub(crate) enum Origin {
    Viewport { x: f64, y: f64 },
}

/// Build the BiDi actions for typing `text` as key down/up pairs.
pub(crate) fn key_text_actions(text: &str) -> Value {
    let mut actions = Vec::new();
    for ch in text.chars() {
        let (key_name, _, _) = char_to_key_info(ch);
        let value = key_value(&key_name);
        actions.push(json!({ "type": "keyDown", "value": value }));
        actions.push(json!({ "type": "keyUp", "value": value }));
    }
    key_source(actions)
}

/// Build the BiDi actions for a named key or modifier chord.
pub(crate) fn key_chord_actions(chord: &str) -> Value {
    let (key, modifiers) = split_chord(chord);
    let (key_name, _, _) = named_key_info(&key);
    let value = key_value(&key_name);

    let mut actions = Vec::new();
    for modifier in &modifiers {
        actions.push(json!({ "type": "keyDown", "value": key_value(modifier) }));
    }
    actions.push(json!({ "type": "keyDown", "value": value }));
    actions.push(json!({ "type": "keyUp", "value": value }));
    for modifier in modifiers.iter().rev() {
        actions.push(json!({ "type": "keyUp", "value": key_value(modifier) }));
    }
    key_source(actions)
}

/// Build the BiDi actions for one key event (`keyDown` or `keyUp`) of `key`.
pub(crate) fn key_event_actions(event_type: &str, key: &str) -> Value {
    let (key_name, _, _) = named_key_info(key);
    let action_type = match event_type {
        "keyUp" => "keyUp",
        _ => "keyDown",
    };
    key_source(vec![
        json!({ "type": action_type, "value": key_value(&key_name) }),
    ])
}

/// Build the BiDi actions for a click, repeating the pair for the click count.
pub(crate) fn pointer_click(origin: &Origin, button: &str, click_count: u32) -> Value {
    let button = mouse_button(button);
    let mut actions = vec![pointer_move(origin)];
    for _ in 0..click_count.max(1) {
        actions.push(json!({ "type": "pointerDown", "button": button }));
        actions.push(json!({ "type": "pointerUp", "button": button }));
    }
    pointer_source("mouse", actions)
}

/// Build the BiDi actions for moving the pointer over `origin`.
pub(crate) fn pointer_hover(origin: &Origin) -> Value {
    pointer_source("mouse", vec![pointer_move(origin)])
}

/// Builds timed mouse moves using the same path calculations as Chrome.
pub(crate) fn pointer_mousemove(
    start: (f64, f64),
    target: (f64, f64),
    duration_ms: u64,
    requested_steps: Option<usize>,
    human: bool,
    seed: u64,
) -> Value {
    let dx = target.0 - start.0;
    let dy = target.1 - start.1;
    let distance = dx.hypot(dy);
    let duration_ms = interpolated_mouse_duration(distance, duration_ms, human);
    let steps = interpolated_mouse_steps(distance, duration_ms, requested_steps, human);
    let bend = interpolated_mouse_bend(distance, human, seed);
    let (perp_x, perp_y) = if distance > 0.0 {
        (-dy / distance, dx / distance)
    } else {
        (0.0, 0.0)
    };
    let actions = (1..=steps)
        .map(|step| {
            let (x, y) = interpolated_mouse_point(
                start.0, start.1, target.0, target.1, perp_x, perp_y, bend, step, steps,
            );
            let mut action = pointer_move(&Origin::Viewport { x, y });
            action["duration"] = json!(
                duration_ms / steps as u64 + u64::from(step as u64 <= duration_ms % steps as u64)
            );
            action
        })
        .collect();
    pointer_source("mouse", actions)
}

/// Build the BiDi actions for pressing a mouse button on `origin`.
pub(crate) fn pointer_down(origin: &Origin, button: &str) -> Value {
    pointer_source(
        "mouse",
        vec![
            pointer_move(origin),
            json!({ "type": "pointerDown", "button": mouse_button(button) }),
        ],
    )
}

/// Build the BiDi actions for releasing a mouse button on `origin`.
pub(crate) fn pointer_up(origin: &Origin, button: &str) -> Value {
    pointer_source(
        "mouse",
        vec![
            pointer_move(origin),
            json!({ "type": "pointerUp", "button": mouse_button(button) }),
        ],
    )
}

/// Build the BiDi actions for a wheel scroll at `origin`.
pub(crate) fn pointer_wheel(origin: &Origin, delta_x: f64, delta_y: f64) -> Value {
    let (x, y, origin) = origin_json(origin);
    json!([{
        "type": "wheel",
        "id": "wheel",
        "actions": [{
            "type": "scroll",
            "x": x,
            "y": y,
            "deltaX": delta_x,
            "deltaY": delta_y,
            "origin": origin,
        }],
    }])
}

/// Build the BiDi actions for a `pointer_type` drag or swipe between two points, emitting `steps` intermediate moves.
pub(crate) fn pointer_path(pointer_type: &str, from: &Origin, to: &Origin, steps: u32) -> Value {
    let (fx, fy, _) = origin_json(from);
    let (tx, ty, _) = origin_json(to);
    let steps = steps.max(1);
    let mut actions = vec![
        pointer_move(from),
        json!({ "type": "pointerDown", "button": 0 }),
    ];
    for i in 1..=steps {
        let t = f64::from(i) / f64::from(steps);
        actions.push(json!({
            "type": "pointerMove",
            "x": fx + (tx - fx) * t,
            "y": fy + (ty - fy) * t,
            "origin": "viewport",
        }));
    }
    actions.push(json!({ "type": "pointerUp", "button": 0 }));
    pointer_source(pointer_type, actions)
}

/// Build the BiDi actions for a touch tap.
pub(crate) fn touch_tap(origin: &Origin) -> Value {
    pointer_source(
        "touch",
        vec![
            pointer_move(origin),
            json!({ "type": "pointerDown", "button": 0 }),
            json!({ "type": "pointerUp", "button": 0 }),
        ],
    )
}

/// Build the BiDi actions for one raw touch event over `points`.
pub(crate) fn touch_event(event_type: &str, points: &[(f64, f64)]) -> Value {
    let sources: Vec<Value> = points
        .iter()
        .enumerate()
        .map(|(index, (x, y))| {
            let move_to = json!({ "type": "pointerMove", "x": x, "y": y, "origin": "viewport" });
            let actions = match event_type {
                "touchStart" => vec![move_to, json!({ "type": "pointerDown", "button": 0 })],
                "touchMove" => vec![move_to],
                "touchEnd" => vec![json!({ "type": "pointerUp", "button": 0 })],
                "touchCancel" => vec![json!({ "type": "pointerUp", "button": 0 })],
                _ => Vec::new(),
            };
            json!({
                "type": "pointer",
                "id": format!("finger{index}"),
                "parameters": { "pointerType": "touch" },
                "actions": actions,
            })
        })
        .collect();
    Value::Array(sources)
}

fn key_source(actions: Vec<Value>) -> Value {
    json!([{ "type": "key", "id": "key", "actions": actions }])
}

fn pointer_source(pointer_type: &str, actions: Vec<Value>) -> Value {
    json!([{
        "type": "pointer",
        "id": pointer_type,
        "parameters": { "pointerType": pointer_type },
        "actions": actions,
    }])
}

fn pointer_move(origin: &Origin) -> Value {
    let (x, y, origin) = origin_json(origin);
    json!({ "type": "pointerMove", "x": x, "y": y, "origin": origin })
}

fn origin_json(origin: &Origin) -> (f64, f64, Value) {
    match origin {
        Origin::Viewport { x, y } => (*x, *y, json!("viewport")),
    }
}

fn mouse_button(button: &str) -> u8 {
    match button.to_ascii_lowercase().as_str() {
        "middle" => 1,
        "right" => 2,
        _ => 0,
    }
}

fn key_value(key_name: &str) -> String {
    match key_name.to_ascii_lowercase().as_str() {
        "shift" => return "\u{E008}".to_string(),
        "control" => return "\u{E009}".to_string(),
        "alt" => return "\u{E00A}".to_string(),
        "meta" => return "\u{E03D}".to_string(),
        _ => {}
    }
    match key_name {
        "Enter" => "\u{E007}".to_string(),
        "Tab" => "\u{E004}".to_string(),
        "Backspace" => "\u{E003}".to_string(),
        "Escape" => "\u{E00C}".to_string(),
        "Delete" => "\u{E017}".to_string(),
        "ArrowUp" => "\u{E013}".to_string(),
        "ArrowDown" => "\u{E015}".to_string(),
        "ArrowLeft" => "\u{E012}".to_string(),
        "ArrowRight" => "\u{E014}".to_string(),
        "Home" => "\u{E011}".to_string(),
        "End" => "\u{E010}".to_string(),
        "PageUp" => "\u{E00E}".to_string(),
        "PageDown" => "\u{E00F}".to_string(),
        "Pause" => "\u{E00B}".to_string(),
        "Insert" => "\u{E016}".to_string(),
        _ => match key_name
            .strip_prefix('F')
            .and_then(|n| n.parse::<u32>().ok())
        {
            Some(n @ 1..=12) => char::from_u32(0xE030 + n).unwrap().to_string(),
            _ => key_text(key_name).unwrap_or_else(|| key_name.to_string()),
        },
    }
}

fn split_chord(chord: &str) -> (String, Vec<&'static str>) {
    let parts: Vec<&str> = chord.split('+').collect();
    if parts.len() < 2 {
        return (chord.to_string(), Vec::new());
    }

    let mut modifiers = Vec::new();
    let mut keys = Vec::new();
    for part in parts {
        match part.to_ascii_lowercase().as_str() {
            "alt" => modifiers.push("Alt"),
            "control" | "ctrl" => modifiers.push("Control"),
            "meta" | "cmd" | "command" => modifiers.push("Meta"),
            "shift" => modifiers.push("Shift"),
            _ => keys.push(part),
        }
    }

    if modifiers.is_empty() {
        return (chord.to_string(), modifiers);
    }

    let key = if keys.is_empty() {
        chord.to_string()
    } else {
        keys.join("+")
    };
    (key, modifiers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn steps(value: &Value) -> &[Value] {
        value[0]["actions"].as_array().unwrap()
    }

    #[test]
    fn mouse_path_preserves_timing_seed_and_endpoint() {
        let path = pointer_mousemove((10.0, 20.0), (130.0, 20.0), 101, Some(4), true, 42);
        let moves = steps(&path);
        assert_eq!(moves.len(), 4);
        assert_eq!(
            moves
                .iter()
                .map(|action| action["duration"].as_u64().unwrap())
                .sum::<u64>(),
            101
        );
        assert_ne!(moves[1]["y"], json!(20.0));
        assert_eq!(moves.last().unwrap()["x"], json!(130.0));
        assert_eq!(moves.last().unwrap()["y"], json!(20.0));
        assert_eq!(
            path,
            pointer_mousemove((10.0, 20.0), (130.0, 20.0), 101, Some(4), true, 42)
        );
        assert_ne!(
            path,
            pointer_mousemove((10.0, 20.0), (130.0, 20.0), 101, Some(4), true, 43)
        );
        let straight = pointer_mousemove((10.0, 20.0), (130.0, 20.0), 0, Some(2), false, 42);
        assert_eq!(steps(&straight)[0]["x"], json!(70.0));
        assert_eq!(steps(&straight)[0]["y"], json!(20.0));
        assert_eq!(steps(&straight)[0]["duration"], 0);
    }

    #[test]
    fn mouse_path_clamps_steps_and_handles_stationary_and_long_moves() {
        let stationary = pointer_mousemove((10.0, 20.0), (10.0, 20.0), 0, Some(0), true, 0);
        assert_eq!(steps(&stationary).len(), 1);
        assert_eq!(steps(&stationary)[0]["duration"], 100);
        assert_eq!(steps(&stationary)[0]["x"], json!(10.0));
        assert_eq!(steps(&stationary)[0]["y"], json!(20.0));
        let long = pointer_mousemove(
            (0.0, 0.0),
            (100.0, 100.0),
            u64::MAX,
            Some(usize::MAX),
            false,
            0,
        );
        assert_eq!(steps(&long).len(), 240);
        assert_eq!(
            steps(&long)
                .iter()
                .map(|action| u128::from(action["duration"].as_u64().unwrap()))
                .sum::<u128>(),
            u128::from(u64::MAX)
        );
    }

    #[test]
    fn text_actions_pair_key_down_and_up_per_character() {
        let actions = key_text_actions("ab");

        assert_eq!(
            steps(&actions),
            &[
                json!({"type": "keyDown", "value": "a"}),
                json!({"type": "keyUp", "value": "a"}),
                json!({"type": "keyDown", "value": "b"}),
                json!({"type": "keyUp", "value": "b"}),
            ]
        );
    }

    #[test]
    fn named_keys_use_webdriver_code_points() {
        for (name, code) in [
            ("Enter", "\u{E007}"),
            ("Tab", "\u{E004}"),
            ("ArrowLeft", "\u{E012}"),
            ("Escape", "\u{E00C}"),
            ("Backspace", "\u{E003}"),
            ("Insert", "\u{E016}"),
            ("F1", "\u{E031}"),
            ("F12", "\u{E03C}"),
        ] {
            let actions = key_chord_actions(name);
            let steps = steps(&actions);

            assert_eq!(steps[0]["value"], code, "{name} keyDown value");
            assert_eq!(steps[1]["value"], code, "{name} keyUp value");
        }
    }

    #[test]
    fn control_chord_presses_and_releases_modifier_around_key() {
        let actions = key_chord_actions("Control+a");

        assert_eq!(
            steps(&actions),
            &[
                json!({"type": "keyDown", "value": "\u{E009}"}),
                json!({"type": "keyDown", "value": "a"}),
                json!({"type": "keyUp", "value": "a"}),
                json!({"type": "keyUp", "value": "\u{E009}"}),
            ]
        );
    }

    #[test]
    fn click_with_click_count_two_emits_two_down_up_pairs() {
        let origin = Origin::Viewport { x: 10.0, y: 20.0 };
        let actions = pointer_click(&origin, "left", 2);
        let steps = steps(&actions);
        let downs = steps.iter().filter(|s| s["type"] == "pointerDown").count();
        let ups = steps.iter().filter(|s| s["type"] == "pointerUp").count();

        assert_eq!((downs, ups), (2, 2));
    }

    #[test]
    fn single_key_event_uses_webdriver_code_points_and_the_requested_direction() {
        let down = key_event_actions("keyDown", "Enter");
        assert_eq!(
            steps(&down),
            &[json!({"type": "keyDown", "value": "\u{E007}"})]
        );

        let up = key_event_actions("keyUp", "Tab");
        assert_eq!(steps(&up), &[json!({"type": "keyUp", "value": "\u{E004}"})]);

        let printable = key_event_actions("keyDown", "a");
        assert_eq!(
            steps(&printable),
            &[json!({"type": "keyDown", "value": "a"})]
        );

        let modifier = key_event_actions("keyDown", "Shift");
        assert_eq!(
            steps(&modifier),
            &[json!({"type": "keyDown", "value": "\u{E008}"})]
        );
    }

    #[test]
    fn touch_tap_uses_touch_pointer_type() {
        let origin = Origin::Viewport { x: 10.0, y: 20.0 };
        let actions = touch_tap(&origin);

        assert_eq!(actions[0]["parameters"]["pointerType"], "touch");
        assert_eq!(actions[0]["actions"][1]["type"], "pointerDown");
    }
}
