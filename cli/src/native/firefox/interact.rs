//! Firefox element pointer commands, raw input, scrolling and dialogs.

use serde_json::{json, Value};

use super::actions::session;
use super::input::{
    pointer_click, pointer_down, pointer_hover, pointer_mousemove, pointer_up, pointer_wheel,
    touch_event, touch_swipe_path, touch_tap, Origin,
};
use super::session::FirefoxSession;
use super::target::{resolve_target, Target};
use crate::native::actions::{DaemonState, PendingDialog};
use crate::native::element::{
    intercepted_error, not_actionable_error, ACTIONABILITY_JS, BLOCKER_AT_JS,
};
use crate::native::interaction::{
    dispatch_event_js, CLICK_HREF_JS, FOCUS_JS, HIGHLIGHT_JS, SCROLL_BY_JS, SCROLL_INTO_VIEW_JS,
};

/// Reports BiDi's in-view center plus the shared actionability and blocker verdicts.
fn prepare_js() -> String {
    format!(
        r#"function() {{
            const el = this;
            const inView = (r) => r && r.width > 0 && r.height > 0 &&
                r.bottom > 0 && r.right > 0 &&
                r.top < (window.innerHeight || document.documentElement.clientHeight) &&
                r.left < (window.innerWidth || document.documentElement.clientWidth);
            let rect = el.getClientRects()[0];
            if (!inView(rect)) {{
                el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                rect = el.getClientRects()[0];
            }}
            if (!rect || rect.width <= 0 || rect.height <= 0) {{
                return {{ x: 0, y: 0, reason: 'not visible', blocker: null }};
            }}
            const left = Math.max(0, Math.min(rect.x, rect.x + rect.width));
            const right = Math.min(window.innerWidth, Math.max(rect.x, rect.x + rect.width));
            const top = Math.max(0, Math.min(rect.y, rect.y + rect.height));
            const bottom = Math.min(window.innerHeight, Math.max(rect.y, rect.y + rect.height));
            const x = Math.floor((left + right) / 2);
            const y = Math.floor((top + bottom) / 2);
            const blockerAt = {BLOCKER_AT_JS};
            const notActionable = {ACTIONABILITY_JS};
            return {{ x: x, y: y, reason: notActionable(document, el, x, y), blocker: blockerAt(document, el, x, y) }};
        }}"#
    )
}

/// Scrolls the target into view and applies the shared actionability and blocker checks.
pub(super) async fn prepare(
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
            Some(target.node_ref()),
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
pub(super) async fn perform(
    ff: &FirefoxSession,
    context: &str,
    actions: Value,
) -> Result<bool, String> {
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
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;

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
    let actions = pointer_click(
        &Origin::Element(target.shared_id.clone()),
        button,
        click_count,
    );

    let ff = session(state)?;
    let position = prepare(ff, &target, selector, true).await?;
    let same_context = target.context == ff.top_context;
    let dialog_opened = perform(ff, &target.context, actions).await?;
    if same_context && !dialog_opened {
        (state.mouse_state.x, state.mouse_state.y) = position;
    }
    if dialog_opened {
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
            Some(target.node_ref()),
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
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    let actions = pointer_click(&Origin::Element(target.shared_id.clone()), "left", 2);

    let ff = session(state)?;
    let position = prepare(ff, &target, selector, true).await?;
    let same_context = target.context == ff.top_context;
    let dialog_opened = perform(ff, &target.context, actions).await?;
    if same_context && !dialog_opened {
        (state.mouse_state.x, state.mouse_state.y) = position;
    }
    if dialog_opened {
        return Ok(json!({ "clicked": selector, "dialogOpened": true }));
    }
    Ok(json!({ "clicked": selector }))
}

/// Moves the mouse over a ref or selector.
pub async fn hover(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    let actions = pointer_hover(&Origin::Element(target.shared_id.clone()));

    let ff = session(state)?;
    let position = prepare(ff, &target, selector, false).await?;
    let same_context = target.context == ff.top_context;
    let dialog_opened = perform(ff, &target.context, actions).await?;
    if same_context && !dialog_opened {
        (state.mouse_state.x, state.mouse_state.y) = position;
    }
    Ok(json!({ "hovered": selector }))
}

/// Focuses a ref or selector.
pub async fn focus(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    session(state)?
        .bidi
        .call_function(
            &target.context,
            FOCUS_JS,
            Some(target.node_ref()),
            Vec::new(),
        )
        .await?;
    Ok(json!({ "focused": selector }))
}

/// Taps a ref or selector with a touch pointer.
pub async fn tap(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    let actions = touch_tap(&Origin::Element(target.shared_id.clone()));

    let ff = session(state)?;
    prepare(ff, &target, selector, false).await?;
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

    let actions = touch_swipe_path(
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

/// Drags between selectors or refs using DOM events for HTML5 sources and pointer input otherwise.
pub async fn drag(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let source = cmd
        .get("source")
        .and_then(Value::as_str)
        .ok_or("Missing 'source' parameter")?;
    let target_selector = cmd
        .get("target")
        .and_then(Value::as_str)
        .ok_or("Missing 'target' parameter")?;
    let source_target = resolve_target(state, source).await?;
    let target = resolve_target(state, target_selector).await?;
    if source_target.context != target.context {
        return Err("Dragging between different frames is not supported on Firefox".to_string());
    }

    let ff = state.firefox.as_ref().ok_or("Browser not launched")?;
    let top_level = source_target.context == ff.top_context;
    let (sx, sy) = prepare(ff, &source_target, source, false).await?;
    let html5 = ff
        .bidi
        .call_function(
            &source_target.context,
            "function() { return this.draggable === true; }",
            Some(source_target.node_ref()),
            vec![],
        )
        .await?;
    if html5 == true {
        let (tx, ty) = prepare(ff, &target, target_selector, false).await?;
        let completed = ff.race_prompt(
            &source_target.context,
            ff.bidi.call_function(
                &source_target.context,
                r#"function(target, points) {
                    const dataTransfer = new DataTransfer();
                    dataTransfer.effectAllowed = 'all';
                    const uri = this.localName === 'a' ? this.href
                        : this.localName === 'img' ? this.currentSrc || this.src : null;
                    if (uri) {
                        dataTransfer.setData('text/uri-list', uri);
                        dataTransfer.setData('text/plain', uri);
                        dataTransfer.setData('text/html', this.outerHTML);
                    }
                    const fire = (node, type, x, y) => node.dispatchEvent(new DragEvent(type, {
                        bubbles: true, cancelable: true, composed: true,
                        clientX: x, clientY: y, dataTransfer,
                    }));
                    const rect = this.getBoundingClientRect();
                    const sx = rect.x + rect.width / 2;
                    const sy = rect.y + rect.height / 2;
                    const [tx, ty] = points;
                    if (!fire(this, 'dragstart', sx, sy)) return;
                    const allowed = {
                        none: [], copy: ['copy'], link: ['link'], move: ['move'],
                        copyLink: ['copy', 'link'], copyMove: ['copy', 'move'],
                        linkMove: ['link', 'move'], all: ['copy', 'link', 'move'],
                        uninitialized: ['copy', 'link', 'move'],
                    }[dataTransfer.effectAllowed];
                    dataTransfer.dropEffect = allowed[0] || 'none';
                    fire(target, 'dragenter', tx, ty);
                    if (!fire(target, 'dragover', tx, ty) && allowed.includes(dataTransfer.dropEffect)) {
                        fire(target, 'drop', tx, ty);
                    } else {
                        dataTransfer.dropEffect = 'none';
                    }
                    fire(this, 'dragend', tx, ty);
                }"#,
                Some(source_target.node_ref()),
                vec![target.node_ref(), json!([tx, ty])],
            ),
        )
        .await?;
        if top_level && completed.is_some() {
            (state.mouse_state.x, state.mouse_state.y) = (tx, ty);
            state.mouse_state.buttons = 0;
        }
        return Ok(json!({ "dragged": true, "source": source, "target": target_selector }));
    }
    let source_origin = Origin::Viewport { x: sx, y: sy };
    let mut release_at = source_origin.clone();
    let result: Result<(), String> = async {
        if perform(
            ff,
            &source_target.context,
            pointer_down(&source_origin, "left"),
        )
        .await?
        {
            return Ok(());
        }
        if top_level {
            (state.mouse_state.x, state.mouse_state.y) = (sx, sy);
            state.mouse_state.buttons = 1;
        }
        let (tx, ty) = prepare(ff, &target, target_selector, false).await?;
        let target_origin = Origin::Viewport { x: tx, y: ty };
        release_at = target_origin.clone();
        if perform(ff, &source_target.context, pointer_hover(&target_origin)).await? {
            return Ok(());
        }
        if top_level {
            (state.mouse_state.x, state.mouse_state.y) = (tx, ty);
        }
        if !perform(
            ff,
            &source_target.context,
            pointer_up(&target_origin, "left"),
        )
        .await?
            && top_level
        {
            state.mouse_state.buttons = 0;
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        if matches!(
            perform(ff, &source_target.context, pointer_up(&release_at, "left")).await,
            Ok(false)
        ) && top_level
        {
            if let Origin::Viewport { x, y } = release_at {
                (state.mouse_state.x, state.mouse_state.y) = (x, y);
            }
            state.mouse_state.buttons = 0;
        }
        return Err(error);
    }

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
                Some(target.node_ref()),
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
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    session(state)?
        .bidi
        .call_function(
            &target.context,
            SCROLL_INTO_VIEW_JS,
            Some(target.node_ref()),
            Vec::new(),
        )
        .await?;
    Ok(json!({ "scrolled": selector }))
}

/// Dispatches a DOM event on a ref or selector.
pub async fn dispatch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let event_type = cmd
        .get("event")
        .or_else(|| cmd.get("eventType"))
        .and_then(Value::as_str)
        .ok_or("Missing 'event' parameter")?;
    let js = dispatch_event_js(event_type, cmd.get("eventInit"));
    let target = resolve_target(state, selector).await?;
    super::forms::run(session(state)?, &target, &js, vec![]).await?;
    Ok(json!({ "dispatched": event_type, "selector": selector }))
}

/// Highlights a ref or selector with a temporary red outline.
pub async fn highlight(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    session(state)?
        .bidi
        .call_function(
            &target.context,
            HIGHLIGHT_JS,
            Some(target.node_ref()),
            Vec::new(),
        )
        .await?;
    Ok(json!({ "highlighted": selector }))
}

fn button_mask(button: &str) -> i32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        "back" => 8,
        "forward" => 16,
        _ => 0,
    }
}

fn apply_button(state: &mut DaemonState, event_type: &str, button: &str) {
    let mask = button_mask(button);
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

    let ff = session(state)?;
    if !perform(ff, &ff.context.clone(), actions).await? {
        state.mouse_state.x = x;
        state.mouse_state.y = y;
        apply_button(state, event_type, button);
    }
    Ok(json!({ "dispatched": event_type }))
}

/// Moves to viewport coordinates using the requested timing, steps and input mode.
pub async fn mousemove(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let x = cmd.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = cmd.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let duration = cmd.get("duration").and_then(Value::as_u64).unwrap_or(0);
    let mut steps = cmd
        .get("steps")
        .and_then(Value::as_u64)
        .map(|value| value as usize);
    let mode = cmd
        .get("inputMode")
        .and_then(Value::as_str)
        .unwrap_or(&state.input_mode);
    if mode == "instant" && duration == 0 && steps.is_none() {
        steps = Some(1);
    }
    let actions = pointer_mousemove(
        (state.mouse_state.x, state.mouse_state.y),
        (x, y),
        duration,
        steps,
        mode == "human",
        cmd.get("seed").and_then(Value::as_u64).unwrap_or(0),
    );

    let ff = session(state)?;
    if !perform(ff, &ff.context.clone(), actions).await? {
        state.mouse_state.x = x;
        state.mouse_state.y = y;
    }
    Ok(json!({ "moved": true, "x": x, "y": y }))
}

/// Presses a mouse button at the last known position.
pub async fn mousedown(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let origin = Origin::Viewport {
        x: state.mouse_state.x,
        y: state.mouse_state.y,
    };
    let ff = session(state)?;
    if !perform(ff, &ff.context.clone(), pointer_down(&origin, button)).await? {
        state.mouse_state.buttons |= button_mask(button);
    }
    Ok(json!({ "pressed": true }))
}

/// Releases a mouse button at the last known position.
pub async fn mouseup(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let origin = Origin::Viewport {
        x: state.mouse_state.x,
        y: state.mouse_state.y,
    };
    let ff = session(state)?;
    if !perform(ff, &ff.context.clone(), pointer_up(&origin, button)).await? {
        state.mouse_state.buttons &= !button_mask(button);
    }
    Ok(json!({ "released": true }))
}

/// Scrolls at explicit coordinates or the last accepted pointer position.
pub async fn wheel(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let x = cmd
        .get("x")
        .and_then(Value::as_f64)
        .unwrap_or(state.mouse_state.x);
    let y = cmd
        .get("y")
        .and_then(Value::as_f64)
        .unwrap_or(state.mouse_state.y);
    let delta_x = cmd.get("deltaX").and_then(Value::as_f64).unwrap_or(0.0);
    let delta_y = cmd.get("deltaY").and_then(Value::as_f64).unwrap_or(0.0);
    let actions = pointer_wheel(&Origin::Viewport { x, y }, delta_x, delta_y);

    let ff = session(state)?;
    if !perform(ff, &ff.context.clone(), actions).await? {
        state.mouse_state.x = x;
        state.mouse_state.y = y;
    }
    Ok(json!({ "scrolled": true, "deltaX": delta_x, "deltaY": delta_y }))
}

/// Low-level mouse input, defaulting coordinates to the last known position.
pub async fn input_mouse(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if cmd.get("clickCount").is_some() {
        return Err("Raw clickCount is not supported on Firefox".to_string());
    }
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

    let ff = session(state)?;
    if !perform(ff, &ff.context.clone(), actions).await? {
        state.mouse_state.x = x;
        state.mouse_state.y = y;
        apply_button(state, event_type, button);
    }
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
    use crate::native::firefox::session::tests::{
        fake_session_with, fake_session_with_script, Calls,
    };
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn mousemove_honors_session_modes_overrides_and_explicit_timing() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        for (mode, options, count, duration) in [
            ("instant", json!({}), 1, 0),
            ("smooth", json!({}), 10, 0),
            ("human", json!({}), 10, 122),
            ("human", json!({"inputMode": "instant"}), 1, 0),
            (
                "instant",
                json!({"inputMode": "human", "seed": 42}),
                10,
                122,
            ),
            ("instant", json!({"steps": 3}), 3, 0),
            ("instant", json!({"duration": 101}), 10, 101),
            ("instant", json!({"duration": 101, "steps": 4}), 4, 101),
        ] {
            state.input_mode = mode.into();
            state.mouse_state.x = 10.0;
            state.mouse_state.y = 20.0;
            state.mouse_state.buttons = 1;
            let mut command = json!({"x": 130, "y": 20});
            command
                .as_object_mut()
                .unwrap()
                .extend(options.as_object().unwrap().clone());
            mousemove(&command, &mut state).await.unwrap();
            let request = requests.lock().unwrap().pop().unwrap();
            let moves = request["params"]["actions"][0]["actions"]
                .as_array()
                .unwrap();
            assert_eq!(moves.len(), count, "{mode}: {command}");
            assert_eq!(
                moves
                    .iter()
                    .map(|action| action["duration"].as_u64().unwrap())
                    .sum::<u64>(),
                duration
            );
            assert_eq!(moves.last().unwrap()["x"], json!(130.0));
            assert_eq!(moves.last().unwrap()["y"], json!(20.0));
            assert!(moves.iter().all(|action| action["type"] == "pointerMove"));
            assert_eq!(
                (
                    state.mouse_state.x,
                    state.mouse_state.y,
                    state.mouse_state.buttons
                ),
                (130.0, 20.0, 1)
            );
            assert_eq!(state.input_mode, mode);
        }
    }

    #[tokio::test]
    async fn extra_buttons_dispatch_and_track_the_same_button() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        for (button, code, mask) in [("back", 3, 8), ("forward", 4, 16)] {
            mousedown(&json!({"button": button}), &mut state)
                .await
                .unwrap();
            assert_eq!(state.mouse_state.buttons, mask);
            mouseup(&json!({"button": button}), &mut state)
                .await
                .unwrap();
            assert_eq!(state.mouse_state.buttons, 0);
            let requests = requests.lock().unwrap();
            let down = &requests[requests.len() - 2]["params"]["actions"][0]["actions"][1];
            let up = &requests[requests.len() - 1]["params"]["actions"][0]["actions"][1];
            assert_eq!(down, &json!({"type": "pointerDown", "button": code}));
            assert_eq!(up, &json!({"type": "pointerUp", "button": code}));
        }
    }

    #[tokio::test]
    async fn wheel_uses_and_commits_each_coordinate_independently() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.mouse_state.x = 17.0;
        state.mouse_state.y = 23.0;
        state.mouse_state.buttons = 1;
        for (command, expected) in [
            (json!({"deltaY": 12}), (17.0, 23.0)),
            (json!({"x": 80, "deltaY": 12}), (80.0, 23.0)),
            (json!({"y": 90, "deltaY": 12}), (80.0, 90.0)),
            (json!({"deltaY": 12}), (80.0, 90.0)),
        ] {
            wheel(&command, &mut state).await.unwrap();
            let request = requests.lock().unwrap().pop().unwrap();
            let scroll = &request["params"]["actions"][0]["actions"][0];
            assert_eq!(scroll["x"], json!(expected.0));
            assert_eq!(scroll["y"], json!(expected.1));
            assert_eq!(scroll["deltaY"], json!(12.0));
            assert_eq!((state.mouse_state.x, state.mouse_state.y), expected);
            assert_eq!(state.mouse_state.buttons, 1);
        }
    }

    #[tokio::test]
    async fn rejected_pointer_dispatches_preserve_coordinates_and_buttons() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let (mut ff, _tx, _) = fake_session_with(|_| json!({})).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for _ in 0..6 {
                let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
                let request: Value = serde_json::from_str(&message).unwrap();
                socket
                    .send(Message::Text(
                        json!({
                            "type": "error", "id": request["id"],
                            "error": "invalid argument", "message": "rejected pointer action"
                        })
                        .to_string(),
                    ))
                    .await
                    .unwrap();
            }
        });
        ff.bidi = super::super::bidi::BidiClient::connect(&address)
            .await
            .unwrap()
            .into();
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.mouse_state.x = 17.0;
        state.mouse_state.y = 23.0;
        state.mouse_state.buttons = 1;
        let command = json!({"x": 80, "y": 90, "deltaY": 12, "button": "right", "eventType": "mousePressed", "type": "mousePressed"});
        for action in [
            "mouse",
            "mousemove",
            "wheel",
            "input_mouse",
            "mousedown",
            "mouseup",
        ] {
            let result = match action {
                "mouse" => mouse(&command, &mut state).await,
                "mousemove" => mousemove(&command, &mut state).await,
                "wheel" => wheel(&command, &mut state).await,
                "input_mouse" => input_mouse(&command, &mut state).await,
                "mousedown" => mousedown(&command, &mut state).await,
                _ => mouseup(&json!({"button": "left"}), &mut state).await,
            };
            assert!(result.unwrap_err().contains("rejected pointer action"));
            assert_eq!(
                (
                    state.mouse_state.x,
                    state.mouse_state.y,
                    state.mouse_state.buttons
                ),
                (17.0, 23.0, 1),
                "{action}"
            );
        }
        server.await.unwrap();
    }

    #[tokio::test]
    async fn element_pointer_tracking_uses_only_top_level_coordinates() {
        for action in ["click", "dblclick", "hover"] {
            for (selected_context, target_context) in [
                ("ctx-active", "ctx-active"),
                ("ctx-active", "ctx-frame"),
                ("ctx-frame", "ctx-frame"),
                ("ctx-frame", "ctx-active"),
            ] {
                let (mut state, _) = state_with_refs(connected).await;
                let ff = state.firefox.as_mut().unwrap();
                ff.context = selected_context.into();
                ff.refs.retain(|(_, shared_id), _| shared_id != "sid-1");
                ff.refs.insert((target_context.into(), "sid-1".into()), 7);
                state.mouse_state.x = 17.0;
                state.mouse_state.y = 23.0;
                let command = json!({"selector": "@e1"});
                match action {
                    "click" => click(&command, &mut state).await.unwrap(),
                    "dblclick" => dblclick(&command, &mut state).await.unwrap(),
                    _ => hover(&command, &mut state).await.unwrap(),
                };
                let expected = if target_context == "ctx-active" {
                    (40.0, 50.0)
                } else {
                    (17.0, 23.0)
                };
                assert_eq!(
                    (state.mouse_state.x, state.mouse_state.y),
                    expected,
                    "{action}: selected={selected_context}, target={target_context}"
                );
            }
        }
    }

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
    async fn click_scrolls_checks_and_performs_the_pointer_action() {
        let (mut state, calls) = state_with_refs(connected).await;

        let result = click(&json!({ "selector": "@e1" }), &mut state)
            .await
            .unwrap();

        assert_eq!(result, json!({ "clicked": "@e1" }));
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "script.callFunction",
                "script.callFunction",
                "input.performActions"
            ]
        );
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
    async fn element_commands_return_chrome_shapes() {
        let (mut state, _calls) = state_with_refs(connected).await;

        assert_eq!(
            focus(&json!({ "selector": "@e1" }), &mut state)
                .await
                .unwrap(),
            json!({ "focused": "@e1" })
        );
        assert_eq!(
            scrollintoview(&json!({ "selector": "@e1" }), &mut state)
                .await
                .unwrap(),
            json!({ "scrolled": "@e1" })
        );
        assert_eq!(
            highlight(&json!({ "selector": "@e1" }), &mut state)
                .await
                .unwrap(),
            json!({ "highlighted": "@e1" })
        );
        assert_eq!(
            dispatch(
                &json!({ "selector": "@e1", "event": "click", "eventInit": { "bubbles": true } }),
                &mut state
            )
            .await
            .unwrap(),
            json!({ "dispatched": "click", "selector": "@e1" })
        );
        assert_eq!(
            scroll(
                &json!({ "selector": "@e1", "direction": "down", "amount": 120 }),
                &mut state
            )
            .await
            .unwrap(),
            json!({ "scrolled": true })
        );
        assert_eq!(
            scroll(&json!({ "y": 80 }), &mut state).await.unwrap(),
            json!({ "scrolled": true })
        );
    }

    #[tokio::test]
    async fn drag_updates_position_for_following_wheel() {
        let (mut state, _) = state_with_refs(connected).await;
        state.mouse_state.x = 17.0;
        state.mouse_state.y = 23.0;
        drag(&json!({"source": "@e1", "target": "@e2"}), &mut state)
            .await
            .unwrap();
        assert_eq!(
            (
                state.mouse_state.x,
                state.mouse_state.y,
                state.mouse_state.buttons
            ),
            (40.0, 50.0, 0)
        );
        wheel(&json!({"deltaY": 12}), &mut state).await.unwrap();
        assert_eq!((state.mouse_state.x, state.mouse_state.y), (40.0, 50.0));
    }

    #[tokio::test]
    async fn raw_pointer_commands_return_chrome_shapes() {
        let (mut state, _calls) = state_with_refs(connected).await;

        assert_eq!(
            wheel(&json!({ "x": 5, "y": 6, "deltaY": 100 }), &mut state)
                .await
                .unwrap(),
            json!({ "scrolled": true, "deltaX": 0.0, "deltaY": 100.0 })
        );
        assert_eq!(
            swipe(&json!({ "direction": "up" }), &mut state)
                .await
                .unwrap(),
            json!({ "swiped": "up" })
        );
        assert_eq!(
            swipe(&json!({}), &mut state).await.unwrap(),
            json!({ "swiped": true, "from": [200.0, 400.0], "to": [200.0, 100.0] })
        );
        assert_eq!(
            drag(&json!({ "source": "@e1", "target": "@e2" }), &mut state)
                .await
                .unwrap(),
            json!({ "dragged": true, "source": "@e1", "target": "@e2" })
        );
    }

    #[tokio::test]
    async fn raw_click_count_is_rejected_without_action_or_state_change() {
        let (mut state, calls) = state_with_refs(connected).await;
        state.mouse_state.x = 17.0;
        state.mouse_state.y = 23.0;
        state.mouse_state.buttons = 1;

        let error = input_mouse(
            &json!({ "type": "mousePressed", "x": 80, "y": 90, "clickCount": 2 }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("clickCount"), "{error}");
        assert_eq!(state.mouse_state.x, 17.0);
        assert_eq!(state.mouse_state.y, 23.0);
        assert_eq!(state.mouse_state.buttons, 1);
        assert!(calls.lock().unwrap().is_empty());
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

    #[tokio::test]
    async fn dialog_reports_and_dismisses_the_pending_prompt() {
        let (mut state, calls) = state_with_refs(connected).await;
        state.pending_dialog = Some(PendingDialog {
            dialog_type: "prompt".into(),
            message: "name?".into(),
            default_prompt: Some("Ada".into()),
            session_id: Some("ctx-active".into()),
            ..Default::default()
        });

        assert_eq!(
            dialog(&json!({ "response": "status" }), &mut state)
                .await
                .unwrap(),
            json!({
                "hasDialog": true,
                "type": "prompt",
                "message": "name?",
                "defaultPrompt": "Ada",
            })
        );
        assert_eq!(
            dialog(
                &json!({ "response": "dismiss", "promptText": "Bob" }),
                &mut state
            )
            .await
            .unwrap(),
            json!({ "handled": true, "accepted": false })
        );

        assert!(state.pending_dialog.is_none());
        assert_eq!(*calls.lock().unwrap(), ["browsingContext.handleUserPrompt"]);
    }
}
