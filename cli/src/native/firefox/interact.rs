//! Firefox element pointer commands, raw input, scrolling and dialogs.

use serde_json::{json, Value};

use super::daemon::session;
use super::frame_transform::{self, SETTLE_JS};
use super::input::{
    modifier_actions, modifier_names, mouse_button_mask, mouse_button_name, pointer_click,
    pointer_down, pointer_hover, pointer_mousemove, pointer_move_buttons, pointer_press,
    pointer_release, pointer_up, pointer_wheel, touch_event, touch_swipe_path, touch_tap, Origin,
};
use super::session::FirefoxSession;
use super::target::{resolve_target, Target};
use crate::native::actions::{
    next_mouse_buttons, raw_mouse, DaemonState, MouseState, PendingDialog, PointerMoveOptions,
};
use crate::native::element::{
    intercepted_error, not_actionable_error, ACTIONABILITY_JS, BLOCKER_AT_JS,
};
use crate::native::interaction::PendingRelease;
use crate::native::interaction::{
    dispatch_event_js, CLICK_HREF_JS, FOCUS_JS, HIGHLIGHT_JS, SCROLL_BY_JS, SCROLL_INTO_VIEW_JS,
};
use crate::native::pointer::{ClickPlan, Modifiers, MouseButton};

/// Reports BiDi's in-view center plus the shared actionability and blocker verdicts, after any
/// scroll it needs has settled, together with the viewport the point belongs to.
fn prepare_js(reject_interactive_descendant: bool) -> String {
    let strict = if reject_interactive_descendant {
        "true"
    } else {
        "false"
    };
    format!(
        r#"async function() {{
            {SETTLE_JS}
            const el = this;
            const inView = (r) => r && r.width > 0 && r.height > 0 &&
                r.bottom > 0 && r.right > 0 &&
                r.top < (window.innerHeight || document.documentElement.clientHeight) &&
                r.left < (window.innerWidth || document.documentElement.clientWidth);
            const measure = () => {{
                const connected = el.isConnected && el.ownerDocument === document;
                const rect = el.getClientRects()[0];
                if (!rect || rect.width <= 0 || rect.height <= 0) {{
                    return {{ x: 0, y: 0, width: 0, height: 0, visible: false, inView: false, connected: connected }};
                }}
                const left = Math.max(0, Math.min(rect.x, rect.x + rect.width));
                const right = Math.min(window.innerWidth, Math.max(rect.x, rect.x + rect.width));
                const top = Math.max(0, Math.min(rect.y, rect.y + rect.height));
                const bottom = Math.min(window.innerHeight, Math.max(rect.y, rect.y + rect.height));
                return {{ x: Math.floor((left + right) / 2), y: Math.floor((top + bottom) / 2), stableX: rect.x, stableY: rect.y, width: rect.width, height: rect.height, visible: true, inView: inView(rect), connected: connected }};
            }};
            let point = measure();
            for (let attempt = 0; attempt < 3 && point.visible && !point.inView; attempt++) {{
                el.scrollIntoView({{ block: 'center', inline: 'center', behavior: 'instant' }});
                const settled = await frameSettle(measure);
                if (!settled.stable) {{
                    return {{ error: settled.value && settled.value.connected === false
                        ? 'the target is no longer in the page'
                        : 'the page kept moving while the target was scrolled into view' }};
                }}
                point = settled.value;
            }}
            if (point.connected === false) {{
                return {{ error: 'the target is no longer in the page' }};
            }}
            const viewport = {{ width: window.innerWidth, height: window.innerHeight }};
            if (!point.visible) {{
                return {{ x: 0, y: 0, reason: 'not visible', blocker: null, viewport: viewport }};
            }}
            const blockerAt = {BLOCKER_AT_JS};
            const notActionable = {ACTIONABILITY_JS};
            const blocker = blockerAt(document, el, point.x, point.y, {strict});
            const reachable = point.x >= 0 && point.y >= 0 && point.x < window.innerWidth && point.y < window.innerHeight;
            return {{ x: point.x, y: point.y, reason: notActionable(el, reachable), blocker: blocker, viewport: viewport }};
        }}"#
    )
}

#[derive(Clone, Copy)]
pub(super) enum PointerAction {
    Click,
    DoubleClick,
    Hover,
    Tap,
    Drag,
    Check,
    Raw,
}

impl PointerAction {
    fn requires_actionable(self) -> bool {
        matches!(self, Self::Click | Self::DoubleClick)
    }

    fn reports_dialog(self) -> bool {
        matches!(
            self,
            Self::Click | Self::DoubleClick | Self::Tap | Self::Drag
        )
    }
}

pub(super) struct ActionOutcome {
    pub dialog_opened: bool,
    pub value: Value,
}

struct PointerState<'a> {
    mouse: &'a mut MouseState,
    pending_release: &'a mut Option<PendingRelease>,
}

struct PointerPlan {
    context: String,
    position: (f64, f64),
    action: PointerAction,
    actions: Value,
    value: Value,
}

const PREPARE_ATTEMPTS: usize = 2;

/// The target's point in its own viewport, with the viewport size the page reported for it.
struct TargetPoint {
    position: (f64, f64),
    viewport: Option<(f64, f64)>,
}

/// Scrolls the target into view and applies the shared actionability and blocker checks. The
/// geometry is read in the isolated sandbox, so the page cannot choose the point the pointer
/// state records.
pub(super) async fn prepare(
    ff: &FirefoxSession,
    target: &Target,
    label: &str,
    action: PointerAction,
) -> Result<(f64, f64), String> {
    let mut attempt = 1;
    loop {
        let measured = measure_target(ff, target, action, label).await?;
        if target.context == ff.contexts.active_tab() {
            return Ok(measured.position);
        }
        if !map_through_frames(ff, &target.context, &measured, label).await? {
            return Ok(measured.position);
        }
        let rechecked = measure_target(ff, target, action, label).await?;
        if rechecked.position == measured.position {
            return Ok(rechecked.position);
        }
        if attempt == PREPARE_ATTEMPTS {
            return Err(format!(
                "Element '{label}' kept moving while its frame was scrolled into view, so the input would land on stale coordinates. Wait for the page to settle and try again."
            ));
        }
        attempt += 1;
    }
}

/// Reads the target's point in its own viewport, waiting for the page to settle after a scroll.
async fn measure_target(
    ff: &FirefoxSession,
    target: &Target,
    action: PointerAction,
    label: &str,
) -> Result<TargetPoint, String> {
    let result = ff
        .bidi
        .call_function_on_in_sandbox(
            &target.context,
            super::bidi::SANDBOX,
            &prepare_js(matches!(action, PointerAction::Check)),
            target.node_ref(),
        )
        .await?;
    if let Some(error) = result["error"].as_str() {
        return Err(format!(
            "Element '{label}' could not be prepared: {error}. Wait for the page to settle and try again."
        ));
    }
    if action.requires_actionable() {
        if let Some(reason) = result["reason"].as_str() {
            return Err(not_actionable_error(label, reason));
        }
    }
    if let Some(blocker) = result["blocker"].as_str() {
        return Err(intercepted_error(label, blocker));
    }
    let position = (
        result["x"].as_f64().unwrap_or(0.0),
        result["y"].as_f64().unwrap_or(0.0),
    );
    Ok(TargetPoint {
        position,
        viewport: reported_viewport(&result),
    })
}

/// Maps the target's point out through the frames holding it, checking each frame's own blocker at
/// the mapped point and waiting for the page to settle after a scroll. Reports whether any frame
/// had to scroll, so the caller can revalidate the target afterwards.
async fn map_through_frames(
    ff: &FirefoxSession,
    context: &str,
    measured: &TargetPoint,
    label: &str,
) -> Result<bool, String> {
    let tree = frame_transform::tree(ff).await?;
    let hops =
        frame_transform::path(&tree, context).ok_or("Firefox could not find the click context")?;
    let mut point = measured.position;
    let mut viewport = measured.viewport;
    if !point.0.is_finite() || !point.1.is_finite() {
        return Err("Firefox reported a click point it cannot describe".to_string());
    }
    let mut scrolled = false;
    for (parent, child) in hops.iter().rev() {
        let child_viewport = match viewport {
            Some(viewport) => viewport,
            None => frame_transform::viewport(ff, child).await?,
        };
        let frame = frame_transform::verified_frame(ff, parent, child).await?;
        let js = frame_transform::hop_js(point, child_viewport);
        let mapped = ff
            .bidi
            .call_function_on_in_sandbox(parent, super::bidi::SANDBOX, &js, frame)
            .await?;
        if let Some(error) = mapped["error"].as_str() {
            return Err(frame_transform::hop_error(label, error));
        }
        if let Some(blocker) = mapped["blocker"].as_str() {
            return Err(intercepted_error(label, blocker));
        }
        point = (
            mapped["x"]
                .as_f64()
                .ok_or("Firefox could not map the click point")?,
            mapped["y"]
                .as_f64()
                .ok_or("Firefox could not map the click point")?,
        );
        viewport = reported_viewport(&mapped);
        scrolled |= mapped["scrolled"].as_bool().unwrap_or(false);
    }
    Ok(scrolled)
}

/// The viewport a geometry reply reports, when it is usable for a mapping.
fn reported_viewport(value: &Value) -> Option<(f64, f64)> {
    let width = value["viewport"]["width"].as_f64()?;
    let height = value["viewport"]["height"].as_f64()?;
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some((width, height))
}

pub(super) async fn perform_actions(
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

/// Converts a point from a frame's viewport to the top-level viewport the pointer state is kept in,
/// so it has to run before the input that moves the pointer: a dialog that input opens would block
/// every later browser call. The path comes from the browser's context tree and each hop from a
/// verified frame handle, so nothing the page computes decides where the pointer is recorded.
async fn to_root_position(
    ff: &FirefoxSession,
    context: &str,
    position: (f64, f64),
) -> Result<(f64, f64), String> {
    if context == ff.contexts.active_tab() {
        return Ok(position);
    }
    let tree = frame_transform::tree(ff).await?;
    let hops =
        frame_transform::path(&tree, context).ok_or("Firefox could not find the mouse context")?;
    frame_transform::to_root(ff, &hops, position).await
}

/// The inverse of `to_root_position`: a point in the top-level viewport expressed in `context`.
async fn to_context_position(
    ff: &FirefoxSession,
    context: &str,
    position: (f64, f64),
) -> Result<(f64, f64), String> {
    if context == ff.contexts.active_tab() {
        return Ok(position);
    }
    let tree = frame_transform::tree(ff).await?;
    let hops =
        frame_transform::path(&tree, context).ok_or("Firefox could not find the mouse context")?;
    frame_transform::to_context(ff, &hops, position).await
}

async fn local_mouse_position(state: &DaemonState) -> Result<(f64, f64), String> {
    let ff = state.firefox.as_ref().ok_or("Browser not launched")?;
    to_context_position(
        ff,
        ff.contexts.active(),
        (state.mouse_state.x, state.mouse_state.y),
    )
    .await
}

fn commit_mouse_position(mouse_state: &mut MouseState, position: (f64, f64)) {
    (mouse_state.x, mouse_state.y) = position;
}

fn apply_mouse_action(mouse_state: &mut MouseState, action: &Value) {
    let mask = action["button"]
        .as_u64()
        .map(|button| mouse_button_mask(button as u8))
        .unwrap_or_default();
    match action["type"].as_str() {
        Some("pointerDown") => mouse_state.buttons |= mask,
        Some("pointerUp") => mouse_state.buttons &= !mask,
        _ => {}
    }
}

/// The tab that hosts `context`, cached while the context is still in the tree.
fn host_tab(ff: &FirefoxSession, context: &str) -> String {
    match ff.contexts.tab_of(context) {
        Some(tab) => tab.to_string(),
        None => ff.contexts.active_tab().to_string(),
    }
}

/// The hold a dialog interrupted: the point in the hold's own viewport, which is what a release in
/// that context takes, and the top-level point the pointer state already holds for a release in the
/// owning tab once that context is gone.
fn hold_release(
    context: &str,
    owner_tab: &str,
    position: (f64, f64),
    owner_position: (f64, f64),
    button: MouseButton,
) -> PendingRelease {
    PendingRelease {
        session_id: context.to_string(),
        owner_tab: Some(owner_tab.to_string()),
        blocked: true,
        owner_position,
        x: position.0,
        y: position.1,
        button,
        click_count: 1,
        modifiers: Modifiers::default(),
    }
}

/// Notes a button a dialog left held, so it is released once the dialog closes. Nothing is asked of
/// the browser here: a prompt is open, and the owning tab's point is the already committed one.
fn defer_release(
    mouse: &MouseState,
    pending_release: &mut Option<PendingRelease>,
    context: &str,
    owner_tab: &str,
    position: (f64, f64),
) {
    let held = (0..=4).find(|button| mouse.buttons & mouse_button_mask(*button) != 0);
    if let (None, Some(button)) = (pending_release.as_ref(), held) {
        let button = MouseButton::parse(mouse_button_name(button)).unwrap_or_default();
        *pending_release = Some(hold_release(
            context,
            owner_tab,
            position,
            (mouse.x, mouse.y),
            button,
        ));
    }
}

/// Moves the mouse along the planned path and records where it landed. Points on the way are
/// kept inside the viewport Firefox accepts points in; the destination is sent as asked.
async fn move_mouse(
    ff: &FirefoxSession,
    mouse_state: &mut MouseState,
    context: &str,
    to: (f64, f64),
    options: &PointerMoveOptions,
) -> Result<bool, String> {
    let from = to_context_position(ff, context, (mouse_state.x, mouse_state.y)).await?;
    let root = to_root_position(ff, context, to).await?;
    let mut path = pointer_mousemove(from, to, options);
    let edge = ff
        .bidi
        .call_function_in_sandbox(
            context,
            super::bidi::SANDBOX,
            "() => [innerWidth - 1, innerHeight - 1]",
            vec![],
        )
        .await?;
    if let (Some(right), Some(bottom)) = (edge[0].as_f64(), edge[1].as_f64()) {
        let steps = path[0]["actions"].as_array_mut().into_iter().flatten();
        for step in steps.rev().skip(1) {
            step["x"] = json!(step["x"].as_f64().unwrap_or(0.0).clamp(0.0, right.max(0.0)));
            step["y"] = json!(step["y"]
                .as_f64()
                .unwrap_or(0.0)
                .clamp(0.0, bottom.max(0.0)));
        }
    }
    let dialog_opened = perform_actions(ff, context, path).await?;
    commit_mouse_position(mouse_state, root);
    Ok(dialog_opened)
}

/// Performs a pointer batch. With motion, the shared path moves the pointer first and the
/// batch's own moves are dropped.
async fn perform_pointer(
    ff: &FirefoxSession,
    state: PointerState<'_>,
    plan: PointerPlan,
    motion: Option<&PointerMoveOptions>,
) -> Result<ActionOutcome, String> {
    let PointerPlan {
        context,
        position,
        action,
        actions,
        mut value,
    } = plan;
    let owner_tab = host_tab(ff, &context);
    let mouse_source = actions
        .as_array()
        .and_then(|sources| sources.first())
        .filter(|source| {
            source["type"] == "pointer" && source["parameters"]["pointerType"] == "mouse"
        });
    let mut dialog_opened = match (motion, mouse_source) {
        (Some(options), Some(_)) => {
            move_mouse(ff, state.mouse, &context, position, options).await?
        }
        _ => false,
    };
    if let Some(source) = mouse_source {
        for pointer_action in source["actions"].as_array().cloned().unwrap_or_default() {
            if dialog_opened {
                break;
            }
            if motion.is_some() && pointer_action["type"] == "pointerMove" {
                continue;
            }
            let request = json!([{
                "type": "pointer",
                "id": source["id"],
                "parameters": source["parameters"],
                "actions": [pointer_action],
            }]);
            let point = if pointer_action["type"] == "pointerMove" {
                if pointer_action["origin"] == "viewport" {
                    match (pointer_action["x"].as_f64(), pointer_action["y"].as_f64()) {
                        (Some(x), Some(y)) => Some((x, y)),
                        _ => Some(position),
                    }
                } else {
                    Some(position)
                }
            } else {
                None
            };
            let root = match point {
                Some(point) => Some(to_root_position(ff, &context, point).await?),
                None => None,
            };
            dialog_opened = perform_actions(ff, &context, request).await?;
            if let Some(root) = root {
                commit_mouse_position(state.mouse, root);
            }
            apply_mouse_action(state.mouse, &pointer_action);
            if let Some(release) = state.pending_release.as_ref() {
                state.mouse.buttons |= release.button.mask();
            }
            match pointer_action["type"].as_str() {
                Some("pointerDown") if dialog_opened && state.pending_release.is_none() => {
                    if let Some(button) = pointer_action["button"].as_u64() {
                        let button =
                            MouseButton::parse(mouse_button_name(button as u8)).unwrap_or_default();
                        *state.pending_release = Some(hold_release(
                            &context,
                            &owner_tab,
                            position,
                            (state.mouse.x, state.mouse.y),
                            button,
                        ));
                    }
                }
                _ => {}
            }
        }
    } else {
        let root = if matches!(action, PointerAction::Tap) {
            None
        } else {
            Some(to_root_position(ff, &context, position).await?)
        };
        dialog_opened = perform_actions(ff, &context, actions).await?;
        if let Some(root) = root {
            commit_mouse_position(state.mouse, root);
        }
    }
    if dialog_opened {
        defer_release(
            state.mouse,
            state.pending_release,
            &context,
            &owner_tab,
            position,
        );
    }
    if dialog_opened && action.reports_dialog() {
        value["dialogOpened"] = json!(true);
    }
    Ok(ActionOutcome {
        dialog_opened,
        value,
    })
}

/// Runs one element pointer action: positions the pointer, then presses its buttons.
async fn act_pointer(
    state: &mut DaemonState,
    cmd: &Value,
    target: &Target,
    label: &str,
    action: PointerAction,
    actions: Value,
    value: Value,
) -> Result<ActionOutcome, String> {
    let options = PointerMoveOptions::from_cmd(cmd, state);
    let ff = state.firefox.as_ref().ok_or("Browser not launched")?;
    let position = prepare(ff, target, label, action).await?;
    perform_pointer(
        ff,
        PointerState {
            mouse: &mut state.mouse_state,
            pending_release: &mut state.pending_pointer_release,
        },
        PointerPlan {
            context: target.context.clone(),
            position,
            action,
            actions,
            value,
        },
        Some(&options),
    )
    .await
}

/// Performs one pointer step of a drag, reporting whether it opened a dialog.
async fn drag_step(
    ff: &FirefoxSession,
    mouse: &mut MouseState,
    pending_release: &mut Option<PendingRelease>,
    context: &str,
    position: (f64, f64),
    actions: Value,
) -> Result<bool, String> {
    perform_pointer(
        ff,
        PointerState {
            mouse,
            pending_release,
        },
        PointerPlan {
            context: context.to_string(),
            position,
            action: PointerAction::Drag,
            actions,
            value: json!({}),
        },
        None,
    )
    .await
    .map(|outcome| outcome.dialog_opened)
}

pub(super) async fn act_element(
    state: &mut DaemonState,
    target: &Target,
    label: &str,
    action: PointerAction,
    actions: Value,
    value: Value,
) -> Result<ActionOutcome, String> {
    act_pointer(state, &Value::Null, target, label, action, actions, value).await
}

async fn act_raw(
    state: &mut DaemonState,
    actions: Value,
    position: (f64, f64),
    value: Value,
) -> Result<ActionOutcome, String> {
    let (ff, mouse_state, pending_release) = (
        &state.firefox,
        &mut state.mouse_state,
        &mut state.pending_pointer_release,
    );
    let ff = ff.as_ref().ok_or("Browser not launched")?;
    let context = ff.contexts.active().to_string();
    perform_pointer(
        ff,
        PointerState {
            mouse: mouse_state,
            pending_release,
        },
        PointerPlan {
            context,
            position,
            action: PointerAction::Raw,
            actions,
            value,
        },
        None,
    )
    .await
}

/// Clicks a ref or selector with Chrome's click rules.
pub async fn click(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    click_with(cmd, state, 1, PointerAction::Click).await
}

/// Decodes the click the way Chrome does, then presses the requested button the requested number
/// of times. Modifier keys are refused: nothing here holds them yet.
async fn click_with(
    cmd: &Value,
    state: &mut DaemonState,
    default_count: u8,
    action: PointerAction,
) -> Result<Value, String> {
    let plan = ClickPlan::from_command(cmd, default_count)?;
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let ClickPlan::Pointer(spec) = plan else {
        return click_new_tab(selector, state).await;
    };
    if spec.modifiers != Modifiers::default() {
        return Err("--modifiers is not supported with --engine firefox".to_string());
    }
    let target = resolve_target(state, selector).await?;
    let actions = pointer_click(
        &Origin::Element(target.shared_id.clone()),
        spec.button.name(),
        u32::from(spec.count),
    );
    Ok(act_pointer(
        state,
        cmd,
        &target,
        selector,
        action,
        actions,
        json!({ "clicked": selector }),
    )
    .await?
    .value)
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
    click_with(cmd, state, 2, PointerAction::DoubleClick).await
}

/// Moves the mouse over a ref or selector.
pub async fn hover(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    let actions = pointer_hover(&Origin::Element(target.shared_id.clone()));
    Ok(act_pointer(
        state,
        cmd,
        &target,
        selector,
        PointerAction::Hover,
        actions,
        json!({ "hovered": selector }),
    )
    .await?
    .value)
}

/// Focuses a ref or selector.
pub async fn focus(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let completed = ff
        .race_prompt(
            &target.context,
            ff.bidi.call_function(
                &target.context,
                FOCUS_JS,
                Some(target.node_ref()),
                Vec::new(),
            ),
        )
        .await?;
    let mut value = json!({ "focused": selector });
    if completed.is_none() {
        value["dialogOpened"] = json!(true);
    }
    Ok(value)
}

/// Taps a ref or selector with a touch pointer.
pub async fn tap(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = cmd
        .get("selector")
        .and_then(Value::as_str)
        .ok_or("Missing 'selector' parameter")?;
    let target = resolve_target(state, selector).await?;
    let actions = touch_tap(&Origin::Element(target.shared_id.clone()));
    Ok(act_element(
        state,
        &target,
        selector,
        PointerAction::Tap,
        actions,
        json!({ "tapped": selector }),
    )
    .await?
    .value)
}

/// Swipes by direction or between two viewport points. The fingers a previous `input_touch`
/// pressed stay held, so a later end still releases them instead of stranding them on the page.
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
    perform_actions(ff, ff.contexts.active(), actions).await?;

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

    let options = PointerMoveOptions::from_cmd(cmd, state);
    let (firefox, mouse_state, pending_release) = (
        &state.firefox,
        &mut state.mouse_state,
        &mut state.pending_pointer_release,
    );
    let ff = firefox.as_ref().ok_or("Browser not launched")?;
    let top_level = source_target.context == ff.contexts.active_tab();
    let context = source_target.context.clone();
    let (sx, sy) = prepare(ff, &source_target, source, PointerAction::Drag).await?;
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
        let (tx, ty) = prepare(ff, &target, target_selector, PointerAction::Drag).await?;
        let root = to_root_position(ff, &source_target.context, (tx, ty)).await?;
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
        if completed.is_none() {
            return Ok(json!({
                "dragged": false,
                "source": source,
                "target": target_selector,
                "dialogOpened": true,
            }));
        }
        commit_mouse_position(mouse_state, root);
        if top_level {
            mouse_state.buttons = 0;
        }
        return Ok(json!({ "dragged": true, "source": source, "target": target_selector }));
    }
    let mut release_at = (sx, sy);
    let mut dialog_opened = false;
    let result: Result<(), String> = async {
        dialog_opened = move_mouse(ff, mouse_state, &context, (sx, sy), &options).await?;
        if !dialog_opened {
            dialog_opened = drag_step(
                ff,
                mouse_state,
                pending_release,
                &context,
                (sx, sy),
                pointer_press("left"),
            )
            .await?;
        }
        if !dialog_opened {
            let point = prepare(ff, &target, target_selector, PointerAction::Drag).await?;
            release_at = point;
            dialog_opened = move_mouse(ff, mouse_state, &context, point, &options).await?;
            if !dialog_opened {
                dialog_opened = drag_step(
                    ff,
                    mouse_state,
                    pending_release,
                    &context,
                    point,
                    pointer_release("left"),
                )
                .await?;
            }
        }
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = drag_step(
            ff,
            mouse_state,
            pending_release,
            &context,
            release_at,
            pointer_release("left"),
        )
        .await;
        return Err(error);
    }
    if dialog_opened {
        defer_release(
            mouse_state,
            pending_release,
            &context,
            &host_tab(ff, &context),
            release_at,
        );
        return Ok(json!({
            "dragged": false,
            "source": source,
            "target": target_selector,
            "dialogOpened": true,
        }));
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

fn validate_raw_mouse(cmd: &Value) -> Result<(), String> {
    if cmd.get("clickCount").is_some() {
        return Err("Raw clickCount is not supported on Firefox".to_string());
    }
    Ok(())
}

/// Raw mouse dispatch: move, press, release or wheel at viewport coordinates.
pub async fn mouse(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    validate_raw_mouse(cmd)?;
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

    Ok(
        act_raw(state, actions, (x, y), json!({ "dispatched": event_type }))
            .await?
            .value,
    )
}

/// Moves to viewport coordinates using the requested timing, steps and input mode.
pub async fn mousemove(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let x = cmd.get("x").and_then(Value::as_f64).unwrap_or(0.0);
    let y = cmd.get("y").and_then(Value::as_f64).unwrap_or(0.0);
    let options = PointerMoveOptions::from_cmd(cmd, state);
    let ff = state.firefox.as_ref().ok_or("Browser not launched")?;
    let context = ff.contexts.active().to_string();
    move_mouse(ff, &mut state.mouse_state, &context, (x, y), &options).await?;
    Ok(json!({ "moved": true, "x": x, "y": y }))
}

/// Presses a mouse button at the last known position.
pub async fn mousedown(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let position = local_mouse_position(state).await?;
    let origin = Origin::Viewport {
        x: position.0,
        y: position.1,
    };
    Ok(act_raw(
        state,
        pointer_down(&origin, button),
        position,
        json!({ "pressed": true }),
    )
    .await?
    .value)
}

/// Releases a mouse button at the last known position.
pub async fn mouseup(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let button = cmd.get("button").and_then(Value::as_str).unwrap_or("left");
    let position = local_mouse_position(state).await?;
    let origin = Origin::Viewport {
        x: position.0,
        y: position.1,
    };
    Ok(act_raw(
        state,
        pointer_up(&origin, button),
        position,
        json!({ "released": true }),
    )
    .await?
    .value)
}

async fn raw_position(cmd: &Value, state: &DaemonState) -> Result<(f64, f64), String> {
    let x = cmd.get("x").and_then(Value::as_f64);
    let y = cmd.get("y").and_then(Value::as_f64);
    match (x, y) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => {
            let position = local_mouse_position(state).await?;
            Ok((x.unwrap_or(position.0), y.unwrap_or(position.1)))
        }
    }
}

/// Scrolls at explicit coordinates or the last accepted pointer position.
pub async fn wheel(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (x, y) = raw_position(cmd, state).await?;
    let delta_x = cmd.get("deltaX").and_then(Value::as_f64).unwrap_or(0.0);
    let delta_y = cmd.get("deltaY").and_then(Value::as_f64).unwrap_or(0.0);
    let actions = pointer_wheel(&Origin::Viewport { x, y }, delta_x, delta_y);

    Ok(act_raw(
        state,
        actions,
        (x, y),
        json!({ "scrolled": true, "deltaX": delta_x, "deltaY": delta_y }),
    )
    .await?
    .value)
}

/// Low-level mouse input, defaulting coordinates to the last known position.
pub async fn input_mouse(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    validate_raw_mouse(cmd)?;
    let raw = raw_mouse(cmd);
    let (x, y) = raw_position(cmd, state).await?;
    let origin = Origin::Viewport { x, y };
    let held = state.mouse_state.buttons;
    let actions = match raw.event_type {
        "mouseWheel" => pointer_wheel(
            &origin,
            raw.delta_x.unwrap_or(0.0),
            raw.delta_y.unwrap_or(0.0),
        ),
        event_type => pointer_move_buttons(
            &origin,
            held,
            next_mouse_buttons(held, event_type, raw.button, raw.buttons),
        ),
    };
    let value = json!({ "dispatched": raw.event_type });
    let modifiers = modifier_names(raw.modifiers.unwrap_or(0));
    if modifiers.is_empty() {
        return Ok(act_raw(state, actions, (x, y), value).await?.value);
    }

    let context = session(state)?.contexts.active().to_string();
    perform_actions(
        session(state)?,
        &context,
        modifier_actions("keyDown", &modifiers),
    )
    .await?;
    let outcome = act_raw(state, actions, (x, y), value).await;
    perform_actions(
        session(state)?,
        &context,
        modifier_actions("keyUp", &modifiers),
    )
    .await?;
    Ok(outcome?.value)
}

/// Low-level touch input over the supplied points, releasing the previous ones on end. Points are
/// held per top-level context, and recorded only once the batch ran without a prompt taking over.
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
    let context = ff.contexts.active().to_string();
    let tab = ff.contexts.active_tab().to_string();
    let held = ff.touches.get(&tab).copied().unwrap_or(0);
    let actions = touch_event(event_type, held, &points);
    if actions
        .as_array()
        .is_some_and(|sources| !sources.is_empty())
        && !perform_actions(ff, &context, actions).await?
    {
        if matches!(event_type, "touchEnd" | "touchCancel") {
            ff.touches.remove(&tab);
        } else {
            ff.touches.insert(tab, points.len());
        }
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
    let known = state
        .pending_dialog
        .as_ref()
        .and_then(|d| d.session_id.clone());
    let context = known
        .clone()
        .or_else(|| {
            state
                .firefox
                .as_ref()
                .map(|ff| ff.contexts.active().to_string())
        })
        .ok_or("Browser not launched")?;

    let mut params = json!({ "context": context.clone(), "accept": accept });
    if let Some(text) = prompt_text {
        params["userText"] = json!(text);
    }
    let result = session(state)?
        .bidi
        .send_raw("browsingContext.handleUserPrompt", params)
        .await;
    if result.is_ok() || result.as_ref().err().is_some_and(prompt_gone) {
        state.pending_dialog = None;
        let (firefox, hold, buttons) = (
            &state.firefox,
            &mut state.pending_pointer_release,
            &mut state.mouse_state.buttons,
        );
        if let Some(ff) = firefox.as_ref() {
            if let Some(release) = hold.as_mut() {
                if closed_prompt_owns_hold(&ff.contexts, release, &context, known.as_deref()) {
                    release.blocked = false;
                }
            }
            release_pending(ff, None, hold, buttons).await;
        }
    }
    result.map_err(|error| error.to_string())?;

    Ok(json!({ "handled": true, "accepted": accept }))
}

/// Releases the button a dialog left held: in the hold's own context while that exists, in its tab
/// otherwise. A release the browser refuses because that context is gone is retried in the tab, a
/// hold whose tab left the tree is dropped, and any other error leaves the hold for a later attempt.
pub(crate) async fn release_pending(
    ff: &FirefoxSession,
    dialog: Option<&PendingDialog>,
    hold: &mut Option<PendingRelease>,
    buttons: &mut i32,
) {
    let Some(release) = hold.as_ref() else {
        return;
    };
    let owning = match ff.contexts.tab_of(release.owner_tab()) {
        Some(tab) => Some(tab.to_string()),
        None => ff.contexts.tab_of(&release.session_id).map(str::to_string),
    };
    let Some(owning) = owning else {
        retire_release(hold, buttons);
        return;
    };
    if release.blocked || dialog_holds_tab(dialog, &owning) {
        return;
    }
    let frame = (release.session_id != owning && ff.contexts.tab_of(&release.session_id).is_some())
        .then(|| release.session_id.clone());
    let context = frame.as_deref().unwrap_or(owning.as_str());
    let position = if frame.is_some() {
        (release.x, release.y)
    } else {
        release.owner_position
    };
    let released = send_release(ff, context, position, release).await;
    if released.is_ok() {
        retire_release(hold, buttons);
        return;
    }
    if !released.as_ref().err().is_some_and(context_gone) {
        return;
    }
    if frame.is_none() {
        retire_release(hold, buttons);
        return;
    }
    let retried = send_release(ff, &owning, release.owner_position, release).await;
    if retried.is_ok() || retried.as_ref().err().is_some_and(context_gone) {
        retire_release(hold, buttons);
    }
}

/// Sends the release for `release` at `position` in `context` and returns the browser's verdict.
async fn send_release(
    ff: &FirefoxSession,
    context: &str,
    position: (f64, f64),
    release: &PendingRelease,
) -> Result<(), super::bidi::BidiError> {
    let origin = Origin::Viewport {
        x: position.0,
        y: position.1,
    };
    ff.bidi
        .send_raw(
            "input.performActions",
            json!({ "context": context, "actions": pointer_up(&origin, release.button.name()) }),
        )
        .await
        .map(|_| ())
}

/// Drops a hold and clears the single bit it kept pressed.
fn retire_release(hold: &mut Option<PendingRelease>, buttons: &mut i32) {
    if let Some(release) = hold.take() {
        release.clear_pressed(buttons);
    }
}

/// True when a BiDi error means the prompt, context or tab a command targeted is already gone.
fn prompt_gone(error: &super::bidi::BidiError) -> bool {
    error.code.as_deref() == Some("no such alert") || context_gone(error)
}

fn context_gone(error: &super::bidi::BidiError) -> bool {
    matches!(
        error.code.as_deref(),
        Some("no such frame" | "no such context" | "no such window")
    )
}

/// True when the recorded prompt still blocks `tab`, whose page then accepts no input.
fn dialog_holds_tab(dialog: Option<&PendingDialog>, tab: &str) -> bool {
    dialog.is_some_and(|dialog| {
        dialog
            .owner_context
            .as_deref()
            .or(dialog.session_id.as_deref())
            .unwrap_or_default()
            == tab
    })
}

/// True when `prompt` names the dialog this hold waits on: it is in the hold's own tab, and it is
/// the hold's own context, a context the hold's context sits in or around, or - when either of them
/// is the tab itself - a prompt in that tab. A prompt in a sibling frame of the same tab, or in
/// another tab, is a different dialog and never takes this release.
fn dialog_owns_hold(
    contexts: &super::contexts::Contexts,
    hold: &PendingRelease,
    prompt: &str,
) -> bool {
    let Some(tab) = contexts.tab_of(&hold.session_id) else {
        return false;
    };
    if tab != hold.owner_tab() {
        return false;
    }
    if hold.session_id == prompt
        || contexts.is_child_of(prompt, &hold.session_id)
        || contexts.is_child_of(&hold.session_id, prompt)
    {
        return true;
    }
    let tab_context = contexts.tab_of(prompt) == Some(prompt)
        || contexts.tab_of(&hold.session_id) == Some(hold.session_id.as_str());
    tab_context && contexts.tab_of(prompt) == Some(tab)
}

/// True when the prompt that closed in `context` is the one this hold waits on: the dialog the
/// daemon tracks, and only when that dialog is this hold's own; with nothing tracked, this hold's
/// own context closing.
pub(crate) fn closed_prompt_owns_hold(
    contexts: &super::contexts::Contexts,
    hold: &PendingRelease,
    context: &str,
    tracked: Option<&str>,
) -> bool {
    match tracked {
        Some(tracked) => context == tracked && dialog_owns_hold(contexts, hold, tracked),
        None => context == hold.session_id,
    }
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
        fake_session_with, fake_session_with_replies, fake_session_with_script, Calls,
    };
    use crate::native::firefox::target::IN_DOCUMENT_JS;
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
            let down = &requests[requests.len() - 3]["params"]["actions"][0]["actions"][0];
            let up = &requests[requests.len() - 1]["params"]["actions"][0]["actions"][0];
            assert_eq!(down, &json!({"type": "pointerDown", "button": code}));
            assert_eq!(up, &json!({"type": "pointerUp", "button": code}));
        }
    }

    #[tokio::test]
    async fn input_mouse_holds_modifiers_around_the_pointer_action() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.mouse_state.x = 10.0;
        state.mouse_state.y = 20.0;

        assert_eq!(
            input_mouse(
                &json!({"type": "mousePressed", "x": 40, "y": 50, "modifiers": 8}),
                &mut state
            )
            .await
            .unwrap(),
            json!({"dispatched": "mousePressed"})
        );

        let requests = requests.lock().unwrap();
        let actions: Vec<Value> = requests
            .iter()
            .map(|request| request["params"]["actions"][0]["actions"][0].clone())
            .collect();
        assert_eq!(
            actions.first(),
            Some(&json!({"type": "keyDown", "value": "\u{E008}"}))
        );
        assert_eq!(
            actions.last(),
            Some(&json!({"type": "keyUp", "value": "\u{E008}"}))
        );
        assert!(actions.contains(&json!({"type": "pointerDown", "button": 0})));
        assert_eq!(state.mouse_state.buttons, 1);
    }

    #[tokio::test]
    async fn input_mouse_presses_the_buttons_a_mask_holds_around_a_move() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        assert_eq!(
            input_mouse(
                &json!({"type": "mouseMoved", "x": 40, "y": 50, "buttons": 1}),
                &mut state
            )
            .await
            .unwrap(),
            json!({"dispatched": "mouseMoved"})
        );

        let actions: Vec<Value> = requests
            .lock()
            .unwrap()
            .iter()
            .map(|request| request["params"]["actions"][0]["actions"][0].clone())
            .collect();
        assert_eq!(
            actions,
            vec![
                json!({"type": "pointerMove", "x": 40.0, "y": 50.0, "origin": "viewport"}),
                json!({"type": "pointerDown", "button": 0}),
            ]
        );
        assert_eq!(state.mouse_state.buttons, 1);
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
        let (done, finished) = tokio::sync::oneshot::channel::<()>();
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
            let _ = finished.await;
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
            let error = result.unwrap_err();
            assert!(
                error.contains("rejected pointer action"),
                "{action}: {error}"
            );
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
        let _ = done.send(());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn element_pointer_tracking_commits_the_pointer_position() {
        for action in ["click", "dblclick", "hover"] {
            for (selected_context, target_context) in [
                ("ctx-active", "ctx-active"),
                ("ctx-active", "ctx-frame"),
                ("ctx-frame", "ctx-frame"),
                ("ctx-frame", "ctx-active"),
            ] {
                let (mut state, _) = state_with_refs(connected).await;
                let ff = state.firefox.as_mut().unwrap();
                ff.contexts.activate(selected_context);
                ff.refs.retain(|(_, shared_id, _), _| shared_id != "sid-1");
                ff.refs
                    .insert((target_context.into(), "sid-1".into(), "doc".into()), 7);
                state.mouse_state.x = 17.0;
                state.mouse_state.y = 23.0;
                let command = json!({"selector": "@e1"});
                match action {
                    "click" => click(&command, &mut state).await.unwrap(),
                    "dblclick" => dblclick(&command, &mut state).await.unwrap(),
                    _ => hover(&command, &mut state).await.unwrap(),
                };
                let expected = if target_context == "ctx-frame" {
                    (40.0 + FRAME_ORIGIN.0, 50.0 + FRAME_ORIGIN.1)
                } else {
                    (40.0, 50.0)
                };
                assert_eq!(
                    (state.mouse_state.x, state.mouse_state.y),
                    expected,
                    "{action}: selected={selected_context}, target={target_context}"
                );
            }
        }
    }

    /// A hostile page answers whatever runs in its own realm with coordinates aimed at a control
    /// in its parent; only the sandbox, the frame handle and the context tree are honest.
    fn hostile_page(params: &Value) -> Value {
        let function = params["functionDeclaration"].as_str().unwrap_or_default();
        if function == IN_DOCUMENT_JS || params["target"]["sandbox"].is_string() {
            return connected(params);
        }
        if function.contains("getClientRects") {
            return located_at((440.0, 305.0), None);
        }
        json!({ "type": "success", "result": { "type": "array", "value": [
            { "type": "number", "value": 460 },
            { "type": "number", "value": 325 },
            { "type": "boolean", "value": true },
        ] } })
    }

    #[tokio::test]
    async fn a_page_cannot_choose_the_position_the_pointer_state_records() {
        for action in ["click", "hover"] {
            let (mut state, _) = state_with_refs(hostile_page).await;
            let ff = state.firefox.as_mut().unwrap();
            ff.refs.retain(|(_, shared_id, _), _| shared_id != "sid-1");
            ff.refs
                .insert(("ctx-frame".into(), "sid-1".into(), "doc".into()), 7);
            let command = json!({"selector": "@e1"});
            match action {
                "click" => click(&command, &mut state).await.unwrap(),
                _ => hover(&command, &mut state).await.unwrap(),
            };
            assert_eq!(
                (state.mouse_state.x, state.mouse_state.y),
                (40.0 + FRAME_ORIGIN.0, 50.0 + FRAME_ORIGIN.1),
                "{action}"
            );
        }
    }

    fn connected(params: &Value) -> Value {
        if params["functionDeclaration"].as_str() == Some(IN_DOCUMENT_JS) {
            return json!({ "type": "success", "result": { "type": "boolean", "value": true } });
        }
        located(None)
    }

    const FRAME_ORIGIN: (f64, f64) = (5.0, 7.0);
    const FRAME_SCALE: (f64, f64) = (1.0, 1.0);
    const FRAME_VIEWPORT: (f64, f64) = (300.0, 250.0);

    /// Where a frame's viewport starts in its parent, the scale one child pixel covers, and the
    /// point that frame's own mapping produces in the parent's viewport.
    #[derive(Clone, Copy)]
    struct FrameGeometry {
        origin: (f64, f64),
        scale: (f64, f64),
        mapped: (f64, f64),
    }

    /// What a frame element answers: a mapping, or the reason it has none.
    #[derive(Clone, Copy)]
    enum HopReply {
        Maps(FrameGeometry),
        Blocked(&'static str),
        Refused(&'static str),
    }

    fn maps(origin: (f64, f64), scale: (f64, f64), mapped: (f64, f64)) -> HopReply {
        HopReply::Maps(FrameGeometry {
            origin,
            scale,
            mapped,
        })
    }

    fn plain_hop() -> HopReply {
        maps(FRAME_ORIGIN, FRAME_SCALE, FRAME_ORIGIN)
    }

    fn frame_tree() -> Value {
        json!({ "contexts": [{ "context": "ctx-active", "children": [
            { "context": "ctx-frame" }
        ] }] })
    }

    fn nested_frame_tree() -> Value {
        json!({ "contexts": [{ "context": "ctx-active", "children": [
            { "context": "ctx-mid", "children": [{ "context": "ctx-frame" }] }
        ] }] })
    }

    fn viewport_reply() -> Value {
        json!({ "type": "success", "result": { "type": "object", "value": [
            ["width", { "type": "number", "value": FRAME_VIEWPORT.0 }],
            ["height", { "type": "number", "value": FRAME_VIEWPORT.1 }],
        ] } })
    }

    fn frame_reply(geometry: FrameGeometry, scrolled: bool, blocker: Option<&str>) -> Value {
        let mut entries = vec![
            json!(["x", { "type": "number", "value": geometry.mapped.0 }]),
            json!(["y", { "type": "number", "value": geometry.mapped.1 }]),
            json!(["originX", { "type": "number", "value": geometry.origin.0 }]),
            json!(["originY", { "type": "number", "value": geometry.origin.1 }]),
            json!(["scaleX", { "type": "number", "value": geometry.scale.0 }]),
            json!(["scaleY", { "type": "number", "value": geometry.scale.1 }]),
            json!(["scrolled", { "type": "boolean", "value": scrolled }]),
            json!(["blocker", { "type": "null" }]),
            json!(["viewport", { "type": "object", "value": [
                ["width", { "type": "number", "value": FRAME_VIEWPORT.0 }],
                ["height", { "type": "number", "value": FRAME_VIEWPORT.1 }],
            ] }]),
        ];
        if let Some(blocker) = blocker {
            entries[7] = json!(["blocker", { "type": "string", "value": blocker }]);
        }
        json!({ "type": "success", "result": { "type": "object", "value": entries } })
    }

    fn hop_reply(hop: HopReply, scrolled: bool) -> Value {
        match hop {
            HopReply::Maps(geometry) => frame_reply(geometry, scrolled, None),
            HopReply::Blocked(blocker) => frame_reply(
                FrameGeometry {
                    origin: FRAME_ORIGIN,
                    scale: FRAME_SCALE,
                    mapped: FRAME_ORIGIN,
                },
                scrolled,
                Some(blocker),
            ),
            HopReply::Refused(error) => {
                json!({ "type": "success", "result": { "type": "object", "value": [
                ["error", { "type": "string", "value": error }],
            ] } })
            }
        }
    }

    /// The point of the last step in `request` that carries coordinates.
    fn dispatch_point(request: &Value) -> Option<(f64, f64)> {
        request["params"]["actions"]
            .as_array()?
            .iter()
            .filter_map(|source| source["actions"].as_array())
            .flatten()
            .rfind(|step| step["x"].is_number() && step["y"].is_number())
            .map(|step| {
                (
                    step["x"].as_f64().unwrap_or_default(),
                    step["y"].as_f64().unwrap_or_default(),
                )
            })
    }

    /// The last dispatched pointer batch that carries coordinates.
    fn last_pointed_request(requests: &Arc<Mutex<Vec<Value>>>) -> Option<Value> {
        requests
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|request| {
                request["method"] == "input.performActions" && dispatch_point(request).is_some()
            })
            .cloned()
    }

    /// The context and the point the pointer was last released at.
    fn released_action(requests: &Arc<Mutex<Vec<Value>>>) -> (Value, (f64, f64)) {
        let request = last_pointed_request(requests)
            .expect("a pointer action with coordinates was dispatched");
        (
            request["params"]["context"].clone(),
            dispatch_point(&request).unwrap(),
        )
    }

    fn located(blocker: Option<&str>) -> Value {
        located_at((40.0, 50.0), blocker)
    }

    fn located_at((x, y): (f64, f64), blocker: Option<&str>) -> Value {
        let mut entries = vec![
            json!(["x", { "type": "number", "value": x }]),
            json!(["y", { "type": "number", "value": y }]),
            json!(["reason", { "type": "null" }]),
            json!(["blocker", { "type": "null" }]),
            json!(["viewport", { "type": "object", "value": [
                ["width", { "type": "number", "value": FRAME_VIEWPORT.0 }],
                ["height", { "type": "number", "value": FRAME_VIEWPORT.1 }],
            ] }]),
        ];
        if let Some(blocker) = blocker {
            entries[3] = json!(["blocker", { "type": "string", "value": blocker }]);
        }
        json!({ "type": "success", "result": { "type": "object", "value": entries } })
    }

    fn blocked(params: &Value) -> Value {
        if params["functionDeclaration"].as_str() == Some(IN_DOCUMENT_JS) {
            return json!({ "type": "success", "result": { "type": "boolean", "value": true } });
        }
        located(Some("div#overlay"))
    }

    fn disabled(params: &Value) -> Value {
        let mut result = located(None);
        if params["functionDeclaration"].as_str() != Some(IN_DOCUMENT_JS) {
            result["result"]["value"][2] =
                json!(["reason", {"type": "string", "value": "disabled"}]);
        }
        result
    }

    async fn state_with_refs(reply: fn(&Value) -> Value) -> (DaemonState, Calls) {
        let (state, calls, _) =
            frame_session(frame_tree(), |_| plain_hop(), false, reply, Vec::new()).await;
        (state, calls)
    }

    /// A session that answers the browser's context tree with `tree`, each frame element with what
    /// `hop` reports for its parent context, and the target with the points in `measures` in order
    /// (the last one repeating, falling back to `reply` when there are none). Every request is
    /// recorded, so a test can see what a command did to the browser.
    async fn frame_session(
        tree: Value,
        hop: fn(&str) -> HopReply,
        scrolled: bool,
        reply: fn(&Value) -> Value,
        measures: Vec<(f64, f64)>,
    ) -> (DaemonState, Calls, Arc<Mutex<Vec<Value>>>) {
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(measures)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let mut state = DaemonState::new();
        let (mut ff, _tx, calls) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            let params = &request["params"];
            let function = params["functionDeclaration"].as_str().unwrap_or_default();
            match request["method"].as_str() {
                Some("browsingContext.getTree") => tree.clone(),
                Some("browsingContext.locateNodes") => {
                    json!({ "nodes": [{ "sharedId": "frame" }] })
                }
                Some("script.callFunction") if function == IN_DOCUMENT_JS => {
                    json!({ "type": "success", "result": { "type": "boolean", "value": true } })
                }
                Some("script.callFunction") if function == "() => document" => {
                    json!({ "type": "success", "result": { "type": "node", "sharedId": "doc" } })
                }
                Some("script.callFunction") if params["this"]["sharedId"] == "frame" => hop_reply(
                    hop(params["target"]["context"].as_str().unwrap_or_default()),
                    scrolled,
                ),
                Some("script.callFunction") if params["this"].is_null() => viewport_reply(),
                Some("script.callFunction") => {
                    let next = {
                        let mut queue = queue.lock().unwrap();
                        if queue.len() > 1 {
                            queue.pop_front()
                        } else {
                            queue.front().copied()
                        }
                    };
                    match next {
                        Some((x, y)) => located_at((x, y), None),
                        None => reply(params),
                    }
                }
                _ => json!({}),
            }
        })
        .await;
        ff.contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        ff.refs
            .insert(("ctx-active".into(), "sid-1".into(), "doc".into()), 7);
        ff.refs
            .insert(("ctx-active".into(), "sid-2".into(), "doc".into()), 8);
        state.firefox = Some(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(7), "button", "OK", None, None);
        state
            .ref_map
            .add_with_frame("e2".into(), Some(8), "button", "Cancel", None, None);
        (state, calls, requests)
    }

    /// Puts the target ref in `context`, so a command has to map out of the frame holding it.
    fn target_in(state: &mut DaemonState, context: &str) {
        let ff = state.firefox.as_mut().unwrap();
        ff.refs.retain(|(_, shared_id, _), _| shared_id != "sid-1");
        ff.refs
            .insert((context.into(), "sid-1".into(), "doc".into()), 7);
    }

    /// Makes `context` active while a tab still owns it, so the pointer state stays in the
    /// top-level viewport and a coordinate-less command has to be mapped back down.
    fn active_in_frame(state: &mut DaemonState, context: &str) {
        let ff = state.firefox.as_mut().unwrap();
        ff.contexts.record(&json!({ "context": "ctx-active" }));
        ff.contexts.activate(context);
    }

    fn dispatched_any(requests: &Arc<Mutex<Vec<Value>>>) -> bool {
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["method"] == "input.performActions")
    }

    #[tokio::test]
    async fn a_scaled_frame_maps_the_pointer_state_through_its_content_box() {
        let (mut state, _, _) = frame_session(
            frame_tree(),
            |_| maps((5.0, 7.0), (2.0, 3.0), (85.0, 157.0)),
            false,
            connected,
            Vec::new(),
        )
        .await;
        target_in(&mut state, "ctx-frame");

        click(&json!({"selector": "@e1"}), &mut state)
            .await
            .unwrap();

        assert_eq!(
            (state.mouse_state.x, state.mouse_state.y),
            (85.0, 157.0),
            "the point is 40,50 in the frame, and one frame pixel is 2 by 3 pixels of the page"
        );
    }

    #[tokio::test]
    async fn a_scaled_frame_maps_the_pointer_back_into_the_frame() {
        let (mut state, _, requests) = frame_session(
            frame_tree(),
            |_| maps((5.0, 7.0), (2.0, 3.0), (85.0, 157.0)),
            false,
            connected,
            Vec::new(),
        )
        .await;
        target_in(&mut state, "ctx-frame");
        active_in_frame(&mut state, "ctx-frame");
        state.mouse_state.x = 85.0;
        state.mouse_state.y = 157.0;

        assert_eq!(local_mouse_position(&state).await.unwrap(), (40.0, 50.0));
        wheel(&json!({"x": 40, "y": 50, "deltaY": 10}), &mut state)
            .await
            .unwrap();

        assert_eq!(
            released_action(&requests),
            (json!("ctx-frame"), (40.0, 50.0))
        );
    }

    #[tokio::test]
    async fn nested_frames_compose_their_scales() {
        let (mut state, _, _) = frame_session(
            nested_frame_tree(),
            |parent| match parent {
                "ctx-mid" => maps((10.0, 20.0), (2.0, 2.0), (90.0, 120.0)),
                _ => maps((5.0, 7.0), (0.5, 0.5), (50.0, 67.0)),
            },
            false,
            connected,
            Vec::new(),
        )
        .await;
        target_in(&mut state, "ctx-frame");

        click(&json!({"selector": "@e1"}), &mut state)
            .await
            .unwrap();

        assert_eq!(
            (state.mouse_state.x, state.mouse_state.y),
            (50.0, 67.0),
            "the inner frame doubles and the outer frame halves the same point"
        );
    }

    #[tokio::test]
    async fn nested_frames_map_the_pointer_back_into_the_inner_frame() {
        let (mut state, _, requests) = frame_session(
            nested_frame_tree(),
            |parent| match parent {
                "ctx-mid" => maps((10.0, 20.0), (2.0, 2.0), (90.0, 120.0)),
                _ => maps((5.0, 7.0), (0.5, 0.5), (50.0, 67.0)),
            },
            false,
            connected,
            Vec::new(),
        )
        .await;
        target_in(&mut state, "ctx-mid");
        active_in_frame(&mut state, "ctx-frame");
        state.mouse_state.x = 50.0;
        state.mouse_state.y = 67.0;

        assert_eq!(local_mouse_position(&state).await.unwrap(), (40.0, 50.0));
        wheel(&json!({"x": 40, "y": 50, "deltaY": 10}), &mut state)
            .await
            .unwrap();

        assert_eq!(
            released_action(&requests),
            (json!("ctx-frame"), (40.0, 50.0))
        );
    }

    #[tokio::test]
    async fn a_rotated_frame_is_refused_before_any_input() {
        let (mut state, _, requests) = frame_session(
            frame_tree(),
            |_| HopReply::Refused("the frame is rotated, skewed or mirrored"),
            false,
            connected,
            Vec::new(),
        )
        .await;
        target_in(&mut state, "ctx-frame");

        let error = click(&json!({"selector": "@e1"}), &mut state)
            .await
            .unwrap_err();

        assert!(error.contains("rotated"), "{error}");
        assert!(!dispatched_any(&requests));
    }

    #[tokio::test]
    async fn a_cover_over_the_mapped_point_refuses_the_click() {
        let (mut state, _, requests) = frame_session(
            frame_tree(),
            |_| HopReply::Blocked("div#cover"),
            false,
            connected,
            Vec::new(),
        )
        .await;
        target_in(&mut state, "ctx-frame");

        let error = click(&json!({"selector": "@e1"}), &mut state)
            .await
            .unwrap_err();

        assert!(error.contains("div#cover"), "{error}");
        assert!(!dispatched_any(&requests));
    }

    #[tokio::test]
    async fn a_target_that_moves_while_its_frame_scrolls_is_prepared_again() {
        let (mut state, _, _) = frame_session(
            frame_tree(),
            |_| maps((5.0, 7.0), (1.0, 1.0), (200.0, 90.0)),
            true,
            connected,
            vec![(40.0, 50.0), (200.0, 90.0), (200.0, 90.0), (200.0, 90.0)],
        )
        .await;
        target_in(&mut state, "ctx-frame");

        click(&json!({"selector": "@e1"}), &mut state)
            .await
            .unwrap();

        assert_eq!((state.mouse_state.x, state.mouse_state.y), (205.0, 97.0));
    }

    #[tokio::test]
    async fn a_target_that_keeps_moving_is_refused() {
        let (mut state, _, requests) = frame_session(
            frame_tree(),
            |_| maps((5.0, 7.0), (1.0, 1.0), (200.0, 90.0)),
            true,
            connected,
            vec![(40.0, 50.0), (200.0, 90.0), (300.0, 90.0), (400.0, 90.0)],
        )
        .await;
        target_in(&mut state, "ctx-frame");

        let error = click(&json!({"selector": "@e1"}), &mut state)
            .await
            .unwrap_err();

        assert!(error.contains("kept moving"), "{error}");
        assert!(!dispatched_any(&requests));
    }

    #[tokio::test]
    async fn a_target_that_scrolls_but_stays_put_is_prepared_once_more() {
        let (mut state, _, requests) = frame_session(
            frame_tree(),
            |_| maps((5.0, 7.0), (1.0, 1.0), (40.0, 50.0)),
            true,
            connected,
            vec![(40.0, 50.0), (40.0, 50.0)],
        )
        .await;
        target_in(&mut state, "ctx-frame");

        click(&json!({"selector": "@e1"}), &mut state)
            .await
            .unwrap();

        assert_eq!(
            released_action(&requests),
            (json!("ctx-frame"), (40.0, 50.0))
        );
        assert_eq!((state.mouse_state.x, state.mouse_state.y), (45.0, 57.0));
    }

    #[tokio::test]
    async fn action_kind_owns_the_actionability_policy() {
        let (ff, _, _) = fake_session_with_script(disabled).await;
        let target = Target {
            context: "ctx-active".into(),
            shared_id: "sid-1".into(),
        };

        assert_eq!(
            prepare(&ff, &target, "@e1", PointerAction::Hover)
                .await
                .unwrap(),
            (40.0, 50.0)
        );
        assert!(prepare(&ff, &target, "@e1", PointerAction::Click)
            .await
            .unwrap_err()
            .contains("disabled"));
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
                "script.callFunction",
                "script.callFunction",
                "input.performActions",
                "input.performActions",
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
            [
                "script.callFunction",
                "script.callFunction",
                "script.callFunction"
            ]
        );
    }

    #[tokio::test]
    async fn element_and_raw_pointer_commands_return_chrome_shapes() {
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

        assert_eq!(error, "Raw clickCount is not supported on Firefox");
        let error = mouse(
            &json!({ "eventType": "mousePressed", "x": 80, "y": 90, "clickCount": 2 }),
            &mut state,
        )
        .await
        .unwrap_err();
        assert_eq!(error, "Raw clickCount is not supported on Firefox");
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

    /// A start that presses two fingers, the shape a pinch or a two-finger scroll uses.
    fn two_finger_start() -> Value {
        json!({ "type": "touchStart", "touchPoints": [{ "x": 1, "y": 2 }, { "x": 3, "y": 4 }] })
    }

    /// The `input.performActions` parameters Firefox received, so a test can assert the pointers a
    /// command really dispatched instead of only the state it kept.
    fn touch_batches(requests: &[Value]) -> Vec<Value> {
        requests
            .iter()
            .filter(|request| request["method"] == "input.performActions")
            .map(|request| request["params"].clone())
            .collect()
    }

    fn finger_ids(params: &Value) -> Vec<String> {
        params["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|source| source["id"].as_str().map(String::from))
            .collect()
    }

    fn finger_steps(params: &Value, finger: usize) -> Vec<&str> {
        params["actions"][finger]["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|step| step["type"].as_str())
            .collect()
    }

    /// A session whose BiDi socket accepts every command and records what it received.
    async fn touch_session() -> (DaemonState, Arc<Mutex<Vec<Value>>>) {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _calls) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        (state, requests)
    }

    /// A swipe drives its own short-lived finger, so the two fingers a start left down have to
    /// survive it and still be released by the end. Clearing the record on the swipe stranded them:
    /// the end then counted zero fingers and sent no pointer-up at all.
    #[tokio::test]
    async fn a_swipe_keeps_the_fingers_a_previous_start_holds_down() {
        let (mut state, requests) = touch_session().await;

        input_touch(&two_finger_start(), &mut state).await.unwrap();
        swipe(&json!({ "direction": "up", "distance": 120 }), &mut state)
            .await
            .unwrap();
        input_touch(&json!({ "type": "touchEnd" }), &mut state)
            .await
            .unwrap();

        let batches = touch_batches(&requests.lock().unwrap());
        assert_eq!(batches.len(), 3, "{batches:?}");
        assert_eq!(finger_ids(&batches[0]), ["finger0", "finger1"]);
        for finger in 0..2 {
            assert_eq!(
                finger_steps(&batches[0], finger),
                ["pointerMove", "pointerDown"]
            );
        }

        assert_eq!(finger_ids(&batches[1]), ["touch"]);
        let swipe_steps = finger_steps(&batches[1], 0);
        assert_eq!(
            swipe_steps
                .iter()
                .filter(|step| **step == "pointerDown")
                .count(),
            1
        );
        assert_eq!(swipe_steps.last().copied(), Some("pointerUp"));

        assert_eq!(finger_ids(&batches[2]), ["finger0", "finger1"]);
        for finger in 0..2 {
            assert_eq!(
                finger_steps(&batches[2], finger),
                ["pointerUp"],
                "{batches:?}"
            );
        }
        assert!(state.firefox.as_ref().unwrap().touches.is_empty());
    }

    /// Fingers belong to the tab that pressed them: an end in another tab must not release them,
    /// and coming back has to release them in the tab that still holds them.
    #[tokio::test]
    async fn a_tab_switch_keeps_each_tabs_held_fingers_separate() {
        let (mut state, requests) = touch_session().await;
        state
            .firefox
            .as_mut()
            .unwrap()
            .contexts
            .record(&json!({ "context": "ctx-other" }));

        input_touch(
            &json!({ "type": "touchStart", "touchPoints": [{ "x": 1, "y": 2 }] }),
            &mut state,
        )
        .await
        .unwrap();
        state
            .firefox
            .as_mut()
            .unwrap()
            .contexts
            .activate("ctx-other");
        input_touch(&json!({ "type": "touchEnd" }), &mut state)
            .await
            .unwrap();
        assert_eq!(
            touch_batches(&requests.lock().unwrap()).len(),
            1,
            "the other tab holds no fingers, so it has nothing to release"
        );

        state
            .firefox
            .as_mut()
            .unwrap()
            .contexts
            .activate("ctx-active");
        input_touch(&json!({ "type": "touchEnd" }), &mut state)
            .await
            .unwrap();
        let batches = touch_batches(&requests.lock().unwrap());
        assert_eq!(batches.len(), 2, "{batches:?}");
        assert_eq!(batches[1]["context"], "ctx-active");
        assert_eq!(finger_steps(&batches[1], 0), ["pointerUp"]);
        assert!(state.firefox.as_ref().unwrap().touches.is_empty());
    }

    /// A destroyed tab takes its held fingers with it, so a context id Firefox hands to a new tab
    /// cannot release pointers that died with the old one.
    #[tokio::test]
    async fn destroying_a_tab_drops_the_fingers_it_was_holding() {
        let (mut state, requests) = touch_session().await;
        {
            let ff = state.firefox.as_mut().unwrap();
            ff.contexts.record(&json!({ "context": "ctx-other" }));
            ff.contexts.activate("ctx-other");
        }
        input_touch(&two_finger_start(), &mut state).await.unwrap();
        assert_eq!(
            finger_ids(&touch_batches(&requests.lock().unwrap())[0]).len(),
            2
        );

        let mut dialog = None;
        state
            .firefox
            .as_mut()
            .unwrap()
            .forget_context("ctx-other", &mut dialog);
        assert!(
            state.firefox.as_ref().unwrap().touches.is_empty(),
            "the destroyed tab keeps no pointers"
        );

        {
            let ff = state.firefox.as_mut().unwrap();
            ff.contexts.record(&json!({ "context": "ctx-other" }));
            ff.contexts.activate("ctx-other");
        }
        input_touch(&json!({ "type": "touchEnd" }), &mut state)
            .await
            .unwrap();
        assert_eq!(
            touch_batches(&requests.lock().unwrap()).len(),
            1,
            "a fresh tab with the dead tab's context id has no fingers to release"
        );
    }

    /// The active context can be one the registry never recorded, which happens when its
    /// `contextCreated` event was lost. The batch still goes to that context, so the fingers it
    /// pressed have to be released from it rather than stranded.
    #[tokio::test]
    async fn a_context_the_registry_never_recorded_still_releases_its_fingers() {
        let (mut state, requests) = touch_session().await;
        state
            .firefox
            .as_mut()
            .unwrap()
            .contexts
            .activate("ctx-unknown");

        input_touch(
            &json!({ "type": "touchStart", "touchPoints": [{ "x": 5, "y": 6 }] }),
            &mut state,
        )
        .await
        .unwrap();
        input_touch(&json!({ "type": "touchEnd" }), &mut state)
            .await
            .unwrap();

        let batches = touch_batches(&requests.lock().unwrap());
        assert_eq!(batches.len(), 2, "{batches:?}");
        assert_eq!(batches[0]["context"], "ctx-unknown");
        assert_eq!(finger_steps(&batches[0], 0), ["pointerMove", "pointerDown"]);
        assert_eq!(batches[1]["context"], "ctx-unknown");
        assert_eq!(finger_steps(&batches[1], 0), ["pointerUp"]);
        assert!(state.firefox.as_ref().unwrap().touches.is_empty());
    }

    /// The batch goes out and the prompt wins the race, so the reply never says whether the fingers
    /// went down. Recording them would claim a release the browser never confirmed.
    #[tokio::test]
    async fn a_prompt_that_takes_over_a_start_leaves_no_fingers_behind() {
        let prompt = json!({
            "type": "event",
            "method": "browsingContext.userPromptOpened",
            "params": { "context": "ctx-active", "type": "alert", "message": "stop" },
        });
        let events: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>> =
            Arc::new(Mutex::new(None));
        let sender = events.clone();
        let (ff, tx, calls) = fake_session_with_replies(move |request| {
            if request["method"] == "input.performActions" {
                if let Some(tx) = sender.lock().unwrap().as_ref() {
                    tx.send(prompt.clone()).unwrap();
                }
                return None;
            }
            Some(json!({}))
        })
        .await;
        *events.lock().unwrap() = Some(tx);
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        assert_eq!(
            input_touch(&two_finger_start(), &mut state).await.unwrap(),
            json!({ "dispatched": "touchStart" })
        );
        assert!(
            state.firefox.as_ref().unwrap().touches.is_empty(),
            "the batch a prompt took over was never confirmed"
        );

        input_touch(&json!({ "type": "touchEnd" }), &mut state)
            .await
            .unwrap();
        assert_eq!(
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|method| *method == "input.performActions")
                .count(),
            1,
            "an unconfirmed start leaves nothing for the end to release"
        );
    }

    /// A batch that fails moved nothing in the browser, so it must record nothing and report the
    /// failure instead of pretending the fingers are down.
    #[tokio::test(start_paused = true)]
    async fn a_touch_batch_that_fails_records_nothing_and_reports_the_error() {
        let (ff, _tx, _calls) = fake_session_with_replies(|request| {
            (request["method"] != "input.performActions").then(|| json!({}))
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        let error = input_touch(
            &json!({ "type": "touchStart", "touchPoints": [{ "x": 1, "y": 2 }] }),
            &mut state,
        )
        .await
        .unwrap_err();

        assert!(error.contains("timed out"), "{error}");
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

    #[tokio::test]
    async fn clicks_refuse_modifier_keys() {
        let mut state = DaemonState::new();
        let held = json!({"selector": "#a", "modifiers": 2});
        let refused = "--modifiers is not supported with --engine firefox";
        assert_eq!(click(&held, &mut state).await.unwrap_err(), refused);
        assert_eq!(dblclick(&held, &mut state).await.unwrap_err(), refused);
    }

    const LOCAL_POINT: (f64, f64) = (40.0, 50.0);
    const OWNER_POINT: (f64, f64) = (140.0, 250.0);

    fn held_release(tab: &str, context: &str, button: MouseButton) -> PendingRelease {
        PendingRelease {
            session_id: context.to_string(),
            owner_tab: Some(tab.to_string()),
            blocked: true,
            owner_position: OWNER_POINT,
            x: LOCAL_POINT.0,
            y: LOCAL_POINT.1,
            button,
            click_count: 1,
            modifiers: Modifiers::default(),
        }
    }

    fn unblocked(tab: &str, context: &str, button: MouseButton) -> Option<PendingRelease> {
        let mut release = held_release(tab, context, button);
        release.blocked = false;
        Some(release)
    }

    /// A BiDi socket that answers `input.performActions` aimed at `release_error_context` (every
    /// one when it is `None`) with `release_error` and every other command with `prompt_error`,
    /// recording each request's method, context and pointer point.
    async fn session_answering(
        prompt_error: Option<&'static str>,
        release_error: Option<&'static str>,
        release_error_context: Option<&'static str>,
    ) -> (
        FirefoxSession,
        Arc<Mutex<Vec<(String, String, (f64, f64))>>>,
    ) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = format!("ws://{}", listener.local_addr().unwrap());
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let context = request["params"]["context"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let action = &request["params"]["actions"][0]["actions"][0];
                log.lock().unwrap().push((
                    method.clone(),
                    context.clone(),
                    (
                        action["x"].as_f64().unwrap_or_default(),
                        action["y"].as_f64().unwrap_or_default(),
                    ),
                ));
                let failure = match (method.as_str(), release_error_context) {
                    ("input.performActions", Some(only)) if only != context => None,
                    ("input.performActions", _) => release_error,
                    _ => prompt_error,
                };
                let reply = match failure {
                    Some(code) => json!({
                        "type": "error", "id": request["id"], "error": code, "message": "test"
                    }),
                    None => json!({"type": "success", "id": request["id"], "result": {}}),
                };
                socket.send(Message::Text(reply.to_string())).await.unwrap();
            }
        });
        let (mut ff, _tx, _) = fake_session_with(|_| json!({})).await;
        ff.bidi = super::super::bidi::BidiClient::connect(&address)
            .await
            .unwrap()
            .into();
        (ff, sent)
    }

    fn pending_prompt(context: &str) -> PendingDialog {
        PendingDialog {
            dialog_type: "confirm".into(),
            message: "sure?".into(),
            session_id: Some(context.into()),
            owner_context: Some(context.into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_gone_prompt_still_releases_the_button_it_left_held() {
        let (ff, sent) = session_answering(Some("no such alert"), None, None).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.pending_dialog = Some(pending_prompt("ctx-active"));
        state.pending_pointer_release =
            Some(held_release("ctx-active", "ctx-active", MouseButton::Right));
        state.mouse_state.buttons = 3;

        let error = dialog(&json!({ "response": "accept" }), &mut state)
            .await
            .unwrap_err();

        assert!(error.contains("no such alert"), "{error}");
        assert!(state.pending_dialog.is_none());
        assert!(state.pending_pointer_release.is_none());
        assert_eq!(state.mouse_state.buttons, 1);
        assert!(
            sent.lock().unwrap().iter().any(|(method, context, point)| {
                method == "input.performActions" && context == "ctx-active" && *point == OWNER_POINT
            }),
            "{sent:?}"
        );
    }

    #[tokio::test]
    async fn an_unrelated_release_error_keeps_the_held_button() {
        for code in ["internal error", "no such alert"] {
            let (ff, sent) = session_answering(None, Some(code), None).await;
            let mut state = DaemonState::new();
            state.firefox = Some(ff);
            state.pending_dialog = Some(pending_prompt("ctx-active"));
            state.pending_pointer_release =
                Some(held_release("ctx-active", "ctx-active", MouseButton::Left));
            state.mouse_state.buttons = 1;

            dialog(&json!({ "response": "accept" }), &mut state)
                .await
                .unwrap();

            assert!(state.pending_dialog.is_none());
            assert!(state.pending_pointer_release.is_some(), "{code}");
            assert_eq!(state.mouse_state.buttons, 1);
            assert!(sent
                .lock()
                .unwrap()
                .iter()
                .any(|(method, _, _)| method == "input.performActions"));
        }
    }

    #[tokio::test]
    async fn an_unrelated_prompt_error_keeps_the_dialog_and_the_held_button() {
        let (ff, _sent) = session_answering(Some("internal error"), None, None).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.pending_dialog = Some(pending_prompt("ctx-active"));
        state.pending_pointer_release =
            Some(held_release("ctx-active", "ctx-active", MouseButton::Left));
        state.mouse_state.buttons = 1;

        let error = dialog(&json!({ "response": "accept" }), &mut state)
            .await
            .unwrap_err();

        assert!(error.contains("internal error"), "{error}");
        assert!(state.pending_dialog.is_some());
        assert!(state.pending_pointer_release.is_some());
        assert_eq!(state.mouse_state.buttons, 1);
    }

    #[tokio::test]
    async fn a_release_whose_frame_is_gone_goes_to_its_tab() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.pending_pointer_release = unblocked("ctx-active", "ctx-frame", MouseButton::Right);
        state.mouse_state.buttons = 2;

        let (firefox, hold, buttons) = (
            &state.firefox,
            &mut state.pending_pointer_release,
            &mut state.mouse_state.buttons,
        );
        release_pending(firefox.as_ref().unwrap(), None, hold, buttons).await;

        assert!(state.pending_pointer_release.is_none());
        assert_eq!(state.mouse_state.buttons, 0);
        let released: Vec<(Value, Value, Value)> = requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request["method"] == "input.performActions")
            .map(|request| {
                let action = &request["params"]["actions"][0]["actions"][0];
                (
                    request["params"]["context"].clone(),
                    action["x"].clone(),
                    action["y"].clone(),
                )
            })
            .collect();
        assert_eq!(
            released,
            [(
                json!("ctx-active"),
                json!(OWNER_POINT.0),
                json!(OWNER_POINT.1)
            )]
        );
    }

    #[tokio::test]
    async fn a_held_button_dies_with_its_tab() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.pending_pointer_release = unblocked("ctx-closed", "ctx-closed", MouseButton::Left);
        state.mouse_state.buttons = 3;

        let (firefox, hold, buttons) = (
            &state.firefox,
            &mut state.pending_pointer_release,
            &mut state.mouse_state.buttons,
        );
        release_pending(firefox.as_ref().unwrap(), None, hold, buttons).await;

        assert!(state.pending_pointer_release.is_none());
        assert_eq!(state.mouse_state.buttons, 2);
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_open_prompt_keeps_the_button_even_without_its_blocked_flag() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.pending_dialog = Some(pending_prompt("ctx-active"));
        state.pending_pointer_release = unblocked("ctx-active", "ctx-active", MouseButton::Left);
        state.mouse_state.buttons = 1;

        let (firefox, dialog, hold, buttons) = (
            &state.firefox,
            state.pending_dialog.as_ref(),
            &mut state.pending_pointer_release,
            &mut state.mouse_state.buttons,
        );
        release_pending(firefox.as_ref().unwrap(), dialog, hold, buttons).await;

        assert!(state.pending_pointer_release.is_some());
        assert_eq!(state.mouse_state.buttons, 1);
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_a_close_event_for_its_tab_unblocks_the_held_button() {
        let (mut ff, tx, _) = fake_session_with(|_| json!({})).await;
        let mut dialog = None;
        let mut release = Some(held_release("ctx-active", "ctx-active", MouseButton::Left));
        let close = |context: &str| {
            json!({"type": "event", "method": "browsingContext.userPromptClosed",
                "params": {"context": context}})
        };

        tx.send(close("ctx-other")).unwrap();
        for _ in 0..200 {
            ff.drain_events_with(&mut dialog, &mut release);
            tokio::task::yield_now().await;
        }
        assert!(release.as_ref().unwrap().blocked);

        tx.send(close("ctx-active")).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while release.as_ref().is_some_and(|hold| hold.blocked) {
                ff.drain_events_with(&mut dialog, &mut release);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!release.as_ref().unwrap().blocked);
    }

    #[tokio::test]
    async fn a_frame_that_dies_before_the_send_is_released_at_its_tab() {
        let (ff, sent) = session_answering(None, Some("no such frame"), Some("ctx-frame")).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .firefox
            .as_mut()
            .unwrap()
            .contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        state.pending_pointer_release = unblocked("ctx-active", "ctx-frame", MouseButton::Right);
        state.mouse_state.buttons = 2;

        let (firefox, hold, buttons) = (
            &state.firefox,
            &mut state.pending_pointer_release,
            &mut state.mouse_state.buttons,
        );
        release_pending(firefox.as_ref().unwrap(), None, hold, buttons).await;

        assert!(state.pending_pointer_release.is_none());
        assert_eq!(state.mouse_state.buttons, 0);
        let released: Vec<(String, (f64, f64))> = sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, _, _)| method == "input.performActions")
            .map(|(_, context, point)| (context.clone(), *point))
            .collect();
        assert_eq!(
            released,
            [
                ("ctx-frame".to_string(), LOCAL_POINT),
                ("ctx-active".to_string(), OWNER_POINT)
            ]
        );
    }

    /// A press inside a scaled frame that a prompt takes over records the frame's own point for a
    /// release in the frame, and the top-level point the pointer state already holds for a release
    /// in the tab.
    #[tokio::test]
    async fn a_scaled_frames_prompted_press_records_both_points() {
        let prompt = json!({
            "type": "event", "method": "browsingContext.userPromptOpened",
            "params": { "context": "ctx-frame", "type": "confirm", "message": "sure?" },
        });
        let events: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>> =
            Arc::new(Mutex::new(None));
        let sender = events.clone();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (mut ff, tx, _) = fake_session_with_replies(move |request| {
            captured.lock().unwrap().push(request.clone());
            let params = &request["params"];
            let function = params["functionDeclaration"].as_str().unwrap_or_default();
            match request["method"].as_str() {
                Some("input.performActions") => {
                    let presses = params["actions"][0]["actions"]
                        .as_array()
                        .is_some_and(|steps| {
                            steps.iter().any(|step| step["type"] == "pointerDown")
                        });
                    if presses {
                        if let Some(tx) = sender.lock().unwrap().as_ref() {
                            tx.send(prompt.clone()).unwrap();
                        }
                        return None;
                    }
                    Some(json!({}))
                }
                Some("browsingContext.getTree") => Some(frame_tree()),
                Some("browsingContext.locateNodes") => {
                    Some(json!({ "nodes": [{ "sharedId": "frame" }] }))
                }
                Some("script.callFunction") if function == IN_DOCUMENT_JS => {
                    Some(connected(params))
                }
                Some("script.callFunction") if function == "() => document" => Some(
                    json!({ "type": "success", "result": { "type": "node", "sharedId": "doc" } }),
                ),
                Some("script.callFunction") if params["this"]["sharedId"] == "frame" => Some(
                    hop_reply(maps((5.0, 7.0), (2.0, 3.0), (85.0, 157.0)), false),
                ),
                Some("script.callFunction") if params["this"].is_null() => Some(viewport_reply()),
                Some("script.callFunction") => Some(located(None)),
                _ => Some(json!({})),
            }
        })
        .await;
        *events.lock().unwrap() = Some(tx);
        ff.contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        ff.refs
            .insert(("ctx-active".into(), "sid-1".into(), "doc".into()), 7);
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(7), "button", "OK", None, None);
        target_in(&mut state, "ctx-frame");

        click(&json!({ "selector": "@e1" }), &mut state)
            .await
            .unwrap();

        let hold = state
            .pending_pointer_release
            .as_ref()
            .expect("the interrupted press leaves a hold");
        assert_eq!(hold.session_id, "ctx-frame");
        assert_eq!(hold.owner_tab.as_deref(), Some("ctx-active"));
        assert!(hold.blocked);
        assert_eq!(
            (hold.x, hold.y),
            (40.0, 50.0),
            "the frame's own point is the one a release in the frame takes"
        );
        assert_eq!(
            hold.owner_position,
            (85.0, 157.0),
            "the committed top-level point is the one a release in the tab takes"
        );

        state.pending_pointer_release.as_mut().unwrap().blocked = false;
        let (firefox, hold, buttons) = (
            &state.firefox,
            &mut state.pending_pointer_release,
            &mut state.mouse_state.buttons,
        );
        release_pending(firefox.as_ref().unwrap(), None, hold, buttons).await;

        assert!(state.pending_pointer_release.is_none());
        assert_eq!(state.mouse_state.buttons, 0);
        let released = released_action(&requests);
        assert_eq!(released.0, json!("ctx-frame"));
        assert_eq!(released.1, (40.0, 50.0));

        click(&json!({ "selector": "@e1" }), &mut state)
            .await
            .unwrap();
        state.firefox.as_mut().unwrap().contexts.remove("ctx-frame");
        state.pending_pointer_release.as_mut().unwrap().blocked = false;
        let (firefox, hold, buttons) = (
            &state.firefox,
            &mut state.pending_pointer_release,
            &mut state.mouse_state.buttons,
        );
        release_pending(firefox.as_ref().unwrap(), None, hold, buttons).await;

        assert!(state.pending_pointer_release.is_none());
        let released = released_action(&requests);
        assert_eq!(released.0, json!("ctx-active"));
        assert_eq!(released.1, (85.0, 157.0));
    }

    #[tokio::test]
    async fn mouseup_in_another_tab_keeps_the_pending_release() {
        let (ff, _tx, _) = fake_session_with(|_| json!({})).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        let ff = state.firefox.as_mut().unwrap();
        ff.contexts.record(&json!({"context": "ctx-second"}));
        ff.contexts.activate("ctx-second");
        state.pending_pointer_release =
            Some(held_release("ctx-active", "ctx-active", MouseButton::Left));
        state.mouse_state.buttons = 1;

        mouseup(&json!({}), &mut state).await.unwrap();

        assert_eq!(state.mouse_state.buttons, 1);
        assert_eq!(
            state.pending_pointer_release.as_ref().unwrap().owner_tab(),
            "ctx-active"
        );
    }

    #[tokio::test]
    async fn a_prompt_in_another_tab_keeps_this_tabs_held_button() {
        let (ff, _tx, _) = fake_session_with(|_| json!({})).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .firefox
            .as_mut()
            .unwrap()
            .contexts
            .record(&json!({ "context": "ctx-second" }));
        state.pending_dialog = Some(pending_prompt("ctx-active"));
        state.pending_pointer_release =
            Some(held_release("ctx-second", "ctx-second", MouseButton::Left));
        state.mouse_state.buttons = 1;

        dialog(&json!({ "response": "accept" }), &mut state)
            .await
            .unwrap();

        assert!(state.pending_dialog.is_none());
        assert!(state.pending_pointer_release.is_some());
        assert_eq!(state.mouse_state.buttons, 1);
    }

    #[tokio::test]
    async fn a_prompt_in_the_parent_context_releases_the_holds_button() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .firefox
            .as_mut()
            .unwrap()
            .contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        state.pending_dialog = Some(pending_prompt("ctx-active"));
        state.pending_pointer_release =
            Some(held_release("ctx-active", "ctx-frame", MouseButton::Left));
        state.mouse_state.buttons = 1;

        dialog(&json!({ "response": "accept" }), &mut state)
            .await
            .unwrap();

        assert!(state.pending_dialog.is_none());
        assert!(state.pending_pointer_release.is_none());
        assert_eq!(state.mouse_state.buttons, 0);
        let released = released_action(&requests);
        assert_eq!(released.0, json!("ctx-frame"));
        assert_eq!(released.1, LOCAL_POINT);
    }

    #[tokio::test]
    async fn a_prompt_in_a_sibling_frame_keeps_the_holds_button() {
        let (ff, _tx, _) = fake_session_with(|_| json!({})).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        {
            let ff = state.firefox.as_mut().unwrap();
            ff.contexts
                .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
            ff.contexts
                .record(&json!({"context": "ctx-other", "parent": "ctx-active"}));
        }
        state.pending_dialog = Some(pending_prompt("ctx-other"));
        state.pending_pointer_release =
            Some(held_release("ctx-active", "ctx-frame", MouseButton::Left));
        state.mouse_state.buttons = 1;

        dialog(&json!({ "response": "accept" }), &mut state)
            .await
            .unwrap();

        assert!(state.pending_dialog.is_none());
        assert!(state.pending_pointer_release.is_some());
        assert_eq!(state.mouse_state.buttons, 1);
    }

    #[tokio::test]
    async fn an_unrelated_prompt_close_never_takes_the_held_button() {
        let (mut ff, tx, _) = fake_session_with(|_| json!({})).await;
        ff.contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        ff.contexts
            .record(&json!({"context": "ctx-other", "parent": "ctx-active"}));
        ff.contexts.record(&json!({"context": "ctx-two"}));
        let close = |context: &str| {
            json!({"type": "event", "method": "browsingContext.userPromptClosed",
                "params": {"context": context}})
        };

        for tracked in [Some("ctx-other"), Some("ctx-two"), None] {
            let mut dialog = tracked.map(pending_prompt);
            let mut release = Some(held_release("ctx-active", "ctx-frame", MouseButton::Left));
            tx.send(close(tracked.unwrap_or("ctx-other"))).unwrap();
            for _ in 0..200 {
                ff.drain_events_with(&mut dialog, &mut release);
                tokio::task::yield_now().await;
            }
            assert!(
                release.as_ref().unwrap().blocked,
                "closing {tracked:?} must not take this hold"
            );
        }
    }

    #[tokio::test]
    async fn a_close_for_the_prompt_the_hold_waits_on_unblocks_it() {
        let (mut ff, tx, _) = fake_session_with(|_| json!({})).await;
        ff.contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        let close = |context: &str| {
            json!({"type": "event", "method": "browsingContext.userPromptClosed",
                "params": {"context": context}})
        };

        for tracked in [Some("ctx-frame"), Some("ctx-active"), None] {
            let mut dialog = tracked.map(pending_prompt);
            let mut release = Some(held_release("ctx-active", "ctx-frame", MouseButton::Left));
            tx.send(close(tracked.unwrap_or("ctx-frame"))).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while release.as_ref().is_some_and(|hold| hold.blocked) {
                    ff.drain_events_with(&mut dialog, &mut release);
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!release.as_ref().unwrap().blocked, "{tracked:?}");
        }
    }

    #[tokio::test]
    async fn focus_returns_when_onfocus_opens_a_prompt() {
        let prompt = json!({
            "type": "event",
            "method": "browsingContext.userPromptOpened",
            "params": { "context": "ctx-active", "type": "alert", "message": "focus" },
        });
        let events: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Value>>>> =
            Arc::new(Mutex::new(None));
        let sender = events.clone();
        let (mut ff, tx, _) = fake_session_with_replies(move |request| {
            if request["method"] == "script.callFunction"
                && request["params"]["functionDeclaration"] == FOCUS_JS
            {
                if let Some(tx) = sender.lock().unwrap().as_ref() {
                    tx.send(prompt.clone()).unwrap();
                }
                return None;
            }
            if request["method"] == "script.callFunction"
                && request["params"]["functionDeclaration"] == "() => document"
            {
                return Some(
                    json!({ "type": "success", "result": { "type": "node", "sharedId": "doc" } }),
                );
            }
            Some(json!({ "type": "success", "result": { "type": "boolean", "value": true } }))
        })
        .await;
        *events.lock().unwrap() = Some(tx);
        ff.auto_dialog
            .store(false, std::sync::atomic::Ordering::Release);
        ff.refs
            .insert(("ctx-active".into(), "sid-1".into(), "doc".into()), 7);
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(7), "textbox", "Name", None, None);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            focus(&json!({ "selector": "@e1" }), &mut state),
        )
        .await
        .expect("focus must not stall when onfocus opens a prompt")
        .unwrap();
        assert_eq!(result["focused"], "@e1");
        assert_eq!(result["dialogOpened"], true);
    }
}
