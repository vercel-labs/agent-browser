use serde_json::{json, Value};

use super::actions::session;
use super::forms::require;
use super::input::{
    pointer_click, pointer_down, pointer_hover, pointer_path, pointer_up, pointer_wheel,
    touch_event, touch_tap, Origin,
};
use super::session::FirefoxSession;
use super::target::{resolve_target, Target};
use crate::native::actions::{mouse_button_mask, DaemonState, PendingDialog};
use crate::native::element::{
    intercepted_error, not_actionable_error, ACTIONABILITY_JS, BLOCKER_AT_JS,
};
use crate::native::interaction::{
    dispatch_event_js, CLICK_HREF_JS, FOCUS_JS, HIGHLIGHT_JS, SCROLL_BY_JS, SCROLL_INTO_VIEW_JS,
};

fn node(target: &Target) -> Value {
    json!({ "sharedId": target.shared_id })
}

/// Scrolls `this` into view, then reports its viewport-clipped, rounded center plus Chrome's actionability and blocker verdicts.
fn prepare_js() -> String {
    format!(
        r#"function() {{
            const el = this;
            const inView = (r) => r.width > 0 && r.height > 0 &&
                r.bottom > 0 && r.right > 0 &&
                r.top < (window.innerHeight || document.documentElement.clientHeight) &&
                r.left < (window.innerWidth || document.documentElement.clientWidth);
            let rect = el.getBoundingClientRect();
            if (!inView(rect)) {{
                el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                rect = el.getBoundingClientRect();
            }}
            const vw = window.innerWidth || document.documentElement.clientWidth;
            const vh = window.innerHeight || document.documentElement.clientHeight;
            const x = Math.floor((Math.max(0, rect.left) + Math.min(vw, rect.right)) / 2);
            const y = Math.floor((Math.max(0, rect.top) + Math.min(vh, rect.bottom)) / 2);
            const blockerAt = {BLOCKER_AT_JS};
            const notActionable = {ACTIONABILITY_JS};
            return {{ x: x, y: y, reason: notActionable(document, el, x, y), blocker: blockerAt(document, el, x, y) }};
        }}"#
    )
}

/// Scrolls the target into view and applies the shared actionability and blocker checks.
async fn prepare(
    ff: &FirefoxSession,
    target: &Target,
    label: &str,
    require_actionable: bool,
) -> Result<(f64, f64), String> {
    let result = ff
        .bidi
        .call_function(
            &target.context,
            &prepare_js(),
            Some(node(target)),
            Vec::new(),
        )
        .await?;
    if require_actionable {
        if let Some(reason) = result["reason"].as_str() {
            return Err(not_actionable_error(label, reason));
        }
    }
    if let Some(blocker) = result["blocker"].as_str() {
        return Err(intercepted_error(label, blocker));
    }
    Ok((
        result["x"].as_f64().unwrap_or(0.0),
        result["y"].as_f64().unwrap_or(0.0),
    ))
}

/// Runs the actions and reports whether a prompt opened before they finished.
async fn perform(ff: &FirefoxSession, context: &str, actions: Value) -> Result<bool, String> {
    let reply = ff
        .race_prompt(
            context,
            ff.bidi.send(
                "input.performActions",
                json!({"context": context, "actions": actions}),
            ),
        )
        .await?;
    Ok(reply.is_none())
}

/// Clicks a ref or selector, returning Chrome's `{clicked}` shape.
pub async fn click(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;

    if cmd.get("newTab").and_then(Value::as_bool).unwrap_or(false) {
        return click_new_tab(selector, state).await;
    }

    let target = resolve_target(state, selector).await?;
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let click_count = cmd
        .get("clickCount")
        .and_then(Value::as_i64)
        .unwrap_or(1)
        .max(0) as u32;
    let ff = session(state)?;
    let (x, y) = prepare(ff, &target, selector, true).await?;
    let actions = pointer_click(&Origin::Viewport { x, y }, button, click_count);
    if perform(ff, &target.context, actions).await? {
        return Ok(json!({ "clicked": selector, "dialogOpened": true }));
    }
    Ok(json!({ "clicked": selector }))
}

/// Opens the target link in a new tab, returning Chrome's `{clicked, newTab, url}` shape.
async fn click_new_tab(selector: &str, state: &mut DaemonState) -> Result<Value, String> {
    let target = resolve_target(state, selector).await?;
    let href = session(state)?
        .bidi
        .call_function(
            &target.context,
            CLICK_HREF_JS,
            Some(node(&target)),
            Vec::new(),
        )
        .await?;
    let Some(href) = href.as_str().map(str::to_string) else {
        return Err(format!(
            "Element '{selector}' does not have an href attribute. --new-tab only works on links."
        ));
    };
    super::tabs::tab_new(&json!({ "url": href }), state).await?;
    Ok(json!({ "clicked": selector, "newTab": true, "url": href }))
}

/// Double-clicks a ref or selector.
pub async fn dblclick(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let (x, y) = prepare(ff, &target, selector, true).await?;
    let actions = pointer_click(&Origin::Viewport { x, y }, "left", 2);
    if perform(ff, &target.context, actions).await? {
        return Ok(json!({ "clicked": selector, "dialogOpened": true }));
    }
    Ok(json!({ "clicked": selector }))
}

/// Moves the mouse over a ref or selector.
pub async fn hover(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let (x, y) = prepare(ff, &target, selector, true).await?;
    let actions = pointer_hover(&Origin::Viewport { x, y });
    perform(ff, &target.context, actions).await?;
    Ok(json!({ "hovered": selector }))
}

/// Focuses a ref or selector.
pub async fn focus(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    session(state)?
        .bidi
        .call_function(&target.context, FOCUS_JS, Some(node(&target)), Vec::new())
        .await?;
    Ok(json!({ "focused": selector }))
}

/// Taps a ref or selector with a touch pointer.
pub async fn tap(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let (x, y) = prepare(ff, &target, selector, true).await?;
    let actions = touch_tap(&Origin::Viewport { x, y });
    perform(ff, &target.context, actions).await?;
    Ok(json!({ "tapped": selector }))
}

/// Swipes by direction or between two viewport points.
pub async fn swipe(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let start_x = cmd.get("startX").and_then(Value::as_f64).unwrap_or(200.0);
    let start_y = cmd.get("startY").and_then(Value::as_f64).unwrap_or(400.0);

    let (end_x, end_y, direction) = match cmd.get("direction").and_then(Value::as_str) {
        Some(direction) => {
            let distance = cmd.get("distance").and_then(Value::as_f64).unwrap_or(300.0);
            let (dx, dy) = match direction {
                "down" => (0.0, distance),
                "left" => (-distance, 0.0),
                "right" => (distance, 0.0),
                _ => (0.0, -distance),
            };
            (start_x + dx, start_y + dy, Some(direction))
        }
        None => (
            cmd.get("endX").and_then(Value::as_f64).unwrap_or(200.0),
            cmd.get("endY").and_then(Value::as_f64).unwrap_or(100.0),
            None,
        ),
    };

    let actions = pointer_path(
        "touch",
        &Origin::Viewport {
            x: start_x,
            y: start_y,
        },
        &Origin::Viewport { x: end_x, y: end_y },
        10,
    );
    let ff = session(state)?;
    perform(ff, &ff.context.clone(), actions).await?;
    ff.touches.clear();

    Ok(match direction {
        Some(direction) => json!({ "swiped": direction }),
        None => json!({
            "swiped": true,
            "from": [start_x, start_y],
            "to": [end_x, end_y],
        }),
    })
}

/// Drags from one ref or selector to another.
pub async fn drag(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let source = require(cmd, "source")?;
    let target_selector = require(cmd, "target")?;
    let source_target = resolve_target(state, source).await?;
    let target = resolve_target(state, target_selector).await?;
    if source_target.context != target.context {
        return Err("Dragging between different frames is not supported on Firefox".to_string());
    }

    let ff = session(state)?;
    let (sx, sy) = prepare(ff, &source_target, source, true).await?;
    let (tx, ty) = prepare(ff, &target, target_selector, true).await?;
    let actions = pointer_path(
        "mouse",
        &Origin::Viewport { x: sx, y: sy },
        &Origin::Viewport { x: tx, y: ty },
        10,
    );
    perform(ff, &source_target.context, actions).await?;

    Ok(json!({ "dragged": true, "source": source, "target": target_selector }))
}

/// Scrolls an element, or the page when no selector is given.
pub async fn scroll(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (mut dx, mut dy) = (
        cmd.get("x").and_then(Value::as_f64).unwrap_or(0.0),
        cmd.get("y").and_then(Value::as_f64).unwrap_or(0.0),
    );
    if let Some(direction) = cmd.get("direction").and_then(Value::as_str) {
        let amount = cmd.get("amount").and_then(Value::as_f64).unwrap_or(300.0);
        match direction {
            "up" => dy = -amount,
            "down" => dy = amount,
            "left" => dx = -amount,
            "right" => dx = amount,
            _ => {}
        }
    }

    if let Some(selector) = cmd.get("selector").and_then(Value::as_str) {
        let target = resolve_target(state, selector).await?;
        session(state)?
            .bidi
            .call_function(
                &target.context,
                SCROLL_BY_JS,
                Some(node(&target)),
                vec![json!(dx), json!(dy)],
            )
            .await?;
    } else {
        session(state)?
            .eval(&format!("window.scrollBy({dx}, {dy})"))
            .await?;
    }
    Ok(json!({ "scrolled": true }))
}

/// Scrolls a ref or selector into view.
pub async fn scrollintoview(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    session(state)?
        .bidi
        .call_function(
            &target.context,
            SCROLL_INTO_VIEW_JS,
            Some(node(&target)),
            Vec::new(),
        )
        .await?;
    Ok(json!({ "scrolled": selector }))
}

/// Dispatches a DOM event on a ref or selector.
pub async fn dispatch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let event_type = cmd
        .get("event")
        .or_else(|| cmd.get("eventType"))
        .and_then(Value::as_str)
        .ok_or("Missing 'event' parameter")?;
    let js = dispatch_event_js(event_type, cmd.get("eventInit"));
    let target = resolve_target(state, selector).await?;
    session(state)?
        .bidi
        .call_function(&target.context, &js, Some(node(&target)), Vec::new())
        .await?;
    Ok(json!({ "dispatched": event_type, "selector": selector }))
}

/// Highlights a ref or selector with a temporary red outline.
pub async fn highlight(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    session(state)?
        .bidi
        .call_function(
            &target.context,
            HIGHLIGHT_JS,
            Some(node(&target)),
            Vec::new(),
        )
        .await?;
    Ok(json!({ "highlighted": selector }))
}

fn apply_button(state: &mut DaemonState, event_type: &str, button: &str) {
    let mask = mouse_button_mask(button);
    match event_type {
        "mousePressed" => state.mouse_state.buttons |= mask,
        "mouseReleased" => state.mouse_state.buttons &= !mask,
        _ => {}
    }
}

/// Raw mouse dispatch: move, press, release or wheel at viewport coordinates.
pub async fn mouse(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let event_type = cmd
        .get("eventType")
        .and_then(Value::as_str)
        .unwrap_or("mouseMoved");
    let x = cmd.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = cmd.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("none");
    let origin = Origin::Viewport { x, y };
    let actions = match event_type {
        "mousePressed" => pointer_down(&origin, button),
        "mouseReleased" => pointer_up(&origin, button),
        "mouseWheel" => pointer_wheel(&origin, 0.0, 0.0),
        _ => pointer_hover(&origin),
    };

    state.mouse_state.x = x;
    state.mouse_state.y = y;
    apply_button(state, event_type, button);

    let ff = session(state)?;
    perform(ff, &ff.context.clone(), actions).await?;
    Ok(json!({ "dispatched": event_type }))
}

/// Moves the mouse to viewport coordinates.
pub async fn mousemove(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let x = cmd.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = cmd.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    state.mouse_state.x = x;
    state.mouse_state.y = y;

    let ff = session(state)?;
    perform(
        ff,
        &ff.context.clone(),
        pointer_hover(&Origin::Viewport { x, y }),
    )
    .await?;
    Ok(json!({ "moved": true, "x": x, "y": y }))
}

/// Presses a mouse button at the last known position.
pub async fn mousedown(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let origin = Origin::Viewport {
        x: state.mouse_state.x,
        y: state.mouse_state.y,
    };
    state.mouse_state.buttons |= mouse_button_mask(button);

    let ff = session(state)?;
    perform(ff, &ff.context.clone(), pointer_down(&origin, button)).await?;
    Ok(json!({ "pressed": true }))
}

/// Releases a mouse button at the last known position.
pub async fn mouseup(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let origin = Origin::Viewport {
        x: state.mouse_state.x,
        y: state.mouse_state.y,
    };
    state.mouse_state.buttons &= !mouse_button_mask(button);

    let ff = session(state)?;
    perform(ff, &ff.context.clone(), pointer_up(&origin, button)).await?;
    Ok(json!({ "released": true }))
}

/// Wheel scroll at viewport coordinates.
pub async fn wheel(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let x = cmd.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = cmd.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let delta_x = cmd.get("deltaX").and_then(Value::as_f64).unwrap_or(0.0);
    let delta_y = cmd.get("deltaY").and_then(Value::as_f64).unwrap_or(0.0);
    let actions = pointer_wheel(&Origin::Viewport { x, y }, delta_x, delta_y);

    let ff = session(state)?;
    perform(ff, &ff.context.clone(), actions).await?;
    Ok(json!({ "scrolled": true, "deltaX": delta_x, "deltaY": delta_y }))
}

/// Low-level mouse input, defaulting coordinates to the last known position.
pub async fn input_mouse(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let event_type = cmd
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("mouseMoved");
    let x = cmd
        .get("x")
        .and_then(Value::as_f64)
        .unwrap_or(state.mouse_state.x);
    let y = cmd
        .get("y")
        .and_then(Value::as_f64)
        .unwrap_or(state.mouse_state.y);
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let delta_x = cmd.get("deltaX").and_then(Value::as_f64).unwrap_or(0.0);
    let delta_y = cmd.get("deltaY").and_then(Value::as_f64).unwrap_or(0.0);

    let origin = Origin::Viewport { x, y };
    let actions = match event_type {
        "mousePressed" => pointer_down(&origin, button),
        "mouseReleased" => pointer_up(&origin, button),
        "mouseWheel" => pointer_wheel(&origin, delta_x, delta_y),
        _ => pointer_hover(&origin),
    };

    state.mouse_state.x = x;
    state.mouse_state.y = y;
    apply_button(state, event_type, button);

    let ff = session(state)?;
    perform(ff, &ff.context.clone(), actions).await?;
    Ok(json!({ "dispatched": event_type }))
}

/// Low-level touch input over the supplied points, releasing the previous ones on end.
pub async fn input_touch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let event_type = cmd
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("touchStart");
    let points: Vec<(f64, f64)> = cmd
        .get("touchPoints")
        .and_then(Value::as_array)
        .map(|points| {
            points
                .iter()
                .filter_map(|p| Some((p.get("x")?.as_f64()?, p.get("y")?.as_f64()?)))
                .collect()
        })
        .unwrap_or_default();

    let ff = session(state)?;
    let effective = if matches!(event_type, "touchEnd" | "touchCancel") && points.is_empty() {
        ff.touches.clone()
    } else {
        points.clone()
    };
    let actions = touch_event(event_type, &effective);
    if actions
        .as_array()
        .is_some_and(|sources| !sources.is_empty())
    {
        perform(ff, &ff.context.clone(), actions).await?;
    }
    if matches!(event_type, "touchEnd" | "touchCancel") {
        ff.touches.clear();
    } else {
        ff.touches = points;
    }
    Ok(json!({ "dispatched": event_type }))
}

/// Reports, accepts or dismisses the pending JavaScript dialog.
pub async fn dialog(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let response = cmd.get("response").and_then(Value::as_str);

    if response == Some("status") {
        return Ok(dialog_status(state.pending_dialog.as_ref()));
    }

    let accept = response
        .map(|r| r == "accept")
        .or_else(|| cmd.get("accept").and_then(Value::as_bool))
        .unwrap_or(true);
    let prompt_text = cmd.get("promptText").and_then(Value::as_str);
    let context = state
        .pending_dialog
        .as_ref()
        .and_then(|d| d.session_id.clone())
        .or_else(|| state.firefox.as_ref().map(|ff| ff.context.clone()))
        .ok_or("Browser not launched")?;

    let mut params = json!({ "context": context, "accept": accept });
    if let Some(text) = prompt_text {
        params["userText"] = json!(text);
    }
    let result = session(state)?
        .bidi
        .send("browsingContext.handleUserPrompt", params)
        .await;
    state.pending_dialog = None;
    result?;

    Ok(json!({ "handled": true, "accepted": accept }))
}

fn dialog_status(dialog: Option<&PendingDialog>) -> Value {
    match dialog {
        Some(dialog) => {
            let mut obj = json!({
                "hasDialog": true,
                "type": dialog.dialog_type,
                "message": dialog.message,
            });
            if let Some(ref prompt) = dialog.default_prompt {
                obj["defaultPrompt"] = json!(prompt);
            }
            obj
        }
        None => json!({ "hasDialog": false }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::session::tests::{fake_session_with_script, Calls};

    fn connected(params: &Value) -> Value {
        if params["functionDeclaration"]
            .as_str()
            .is_some_and(|js| js.contains("isConnected"))
        {
            return json!({ "type": "success", "result": { "type": "boolean", "value": true } });
        }
        located(None)
    }

    fn located(blocker: Option<&str>) -> Value {
        let mut entries = vec![
            json!(["x", { "type": "number", "value": 40 }]),
            json!(["y", { "type": "number", "value": 50 }]),
            json!(["reason", { "type": "null" }]),
            json!(["blocker", { "type": "null" }]),
        ];
        if let Some(blocker) = blocker {
            entries[3] = json!(["blocker", { "type": "string", "value": blocker }]);
        }
        json!({ "type": "success", "result": { "type": "object", "value": entries } })
    }

    fn blocked(params: &Value) -> Value {
        if params["functionDeclaration"]
            .as_str()
            .is_some_and(|js| js.contains("isConnected"))
        {
            return json!({ "type": "success", "result": { "type": "boolean", "value": true } });
        }
        located(Some("div#overlay"))
    }

    async fn state_with_refs(reply: fn(&Value) -> Value) -> (DaemonState, Calls) {
        let mut state = DaemonState::new();
        let (mut ff, _tx, calls) = fake_session_with_script(reply).await;
        ff.refs.insert(("ctx-active".into(), "sid-1".into()), 7);
        ff.refs.insert(("ctx-active".into(), "sid-2".into()), 8);
        state.firefox = Some(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(7), "button", "OK", None, None);
        state
            .ref_map
            .add_with_frame("e2".into(), Some(8), "button", "Cancel", None, None);
        (state, calls)
    }

    #[tokio::test]
    async fn a_covered_target_fails_before_any_pointer_action() {
        let (mut state, calls) = state_with_refs(blocked).await;

        let error = click(&json!({ "selector": "@e1" }), &mut state)
            .await
            .unwrap_err();

        assert!(error.contains("covered by <div#overlay>"), "{error}");
        assert_eq!(
            *calls.lock().unwrap(),
            ["script.callFunction", "script.callFunction"]
        );
    }

    #[tokio::test]
    async fn input_touch_releases_the_points_a_previous_start_recorded() {
        let (mut state, calls) = state_with_refs(connected).await;

        assert_eq!(
            input_touch(
                &json!({ "type": "touchStart", "touchPoints": [{ "x": 1, "y": 2 }] }),
                &mut state
            )
            .await
            .unwrap(),
            json!({ "dispatched": "touchStart" })
        );
        assert_eq!(
            input_touch(&json!({ "type": "touchEnd" }), &mut state)
                .await
                .unwrap(),
            json!({ "dispatched": "touchEnd" })
        );

        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|method| *method == "input.performActions")
                .count(),
            2
        );
        assert!(state.firefox.as_ref().unwrap().touches.is_empty());
    }
}
