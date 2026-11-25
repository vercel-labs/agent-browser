use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::*;
use super::element::{
    document_gone, resolve_element_center, resolve_element_object_id, session_viewport_offset,
    unblocked_point, RefMap,
};
use super::forms::{CallError, FormControl};
use super::pointer::{ClickSpec, Modifiers, MouseButton};

/// Outcome of a click. `dialog_opened` is true if a JavaScript dialog opened
/// mid-sequence (the page is then blocked until `dialog accept`/`dismiss`).
/// `pending_release` is set only when the dialog opened after mousePressed but
/// before mouseReleased: the button is logically held until the caller
/// dispatches the release (done once the dialog is resolved), otherwise the
/// next click would register as a drag or double-click.
#[derive(Default)]
pub struct ClickResult {
    /// Final pointer position in the top-level page viewport, including dialogs.
    pub position: (f64, f64),
    pub dialog_opened: bool,
    pub pending_release: Option<PendingRelease>,
    pub x: f64,
    pub y: f64,
    pub button_pressed: bool,
}

pub struct PendingRelease {
    pub session_id: String,
    pub owner_tab: Option<String>,
    pub blocked: bool,
    pub owner_position: (f64, f64),
    pub x: f64,
    pub y: f64,
    pub button: Option<MouseButton>,
    pub click_count: u8,
    pub modifiers: Modifiers,
}

impl PendingRelease {
    /// The tab that owns this hold: the cached tab, or the context itself when none was cached.
    pub fn owner_tab(&self) -> &str {
        self.owner_tab.as_deref().unwrap_or(&self.session_id)
    }

    /// True when this hold's owning tab is among the contexts that left the tree.
    pub fn tab_removed(&self, removed: &HashSet<String>) -> bool {
        removed.contains(self.owner_tab())
    }

    /// True when the context this hold was recorded for left the tree while its tab stayed.
    pub fn context_removed(&self, removed: &HashSet<String>) -> bool {
        removed.contains(&self.session_id)
    }

    /// Clears the bit this hold kept pressed.
    pub fn clear_pressed(&self, buttons: &mut i32) {
        if let Some(button) = self.button {
            *buttons &= !button.mask();
        }
    }
}

pub async fn click(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    spec: ClickSpec,
    iframe_sessions: &HashMap<String, String>,
) -> Result<ClickResult, String> {
    click_element(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        spec,
        iframe_sessions,
        true,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn click_element(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    spec: ClickSpec,
    iframe_sessions: &HashMap<String, String>,
    require_actionable: bool,
) -> Result<ClickResult, String> {
    let (x, y, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        require_actionable,
    )
    .await?;
    // A click-triggered dialog can fire on the frame's own session (OOPIF) or
    // on the top-level page session; both count as "ours". A dialog on any
    // other session belongs to a background tab and must not abort this click.
    let offset =
        session_viewport_offset(client, session_id, &effective_session_id, iframe_sessions).await?;
    let mut result = dispatch_click(
        client,
        &effective_session_id,
        &[effective_session_id.as_str(), session_id],
        x,
        y,
        spec,
    )
    .await?;
    // Compute before dispatch: a click may navigate or open a blocking dialog.
    result.position = (x + offset.0, y + offset.1);
    (result.x, result.y) = result.position;
    Ok(result)
}

pub async fn hover(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(f64, f64), String> {
    let (x, y, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        false,
    )
    .await?;
    let offset =
        session_viewport_offset(client, session_id, &effective_session_id, iframe_sessions).await?;
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mouseMoved".to_string(),
                x,
                y,
                button: None,
                buttons: None,
                click_count: None,
                delta_x: None,
                delta_y: None,
                modifiers: None,
            },
            Some(&effective_session_id),
        )
        .await?;
    Ok((x + offset.0, y + offset.1))
}

pub(crate) const FOCUS_JS: &str = "function() { this.focus(); }";

/// Resolves `this`'s href against the document base, or null for a non-link.
pub(crate) const CLICK_HREF_JS: &str = "function() { var h = this.getAttribute('href'); if (!h) return null; try { return new URL(h, document.baseURI).toString(); } catch(e) { return null; } }";

/// The element a form action works on in Chrome, reached over CDP.
pub(crate) struct CdpControl<'a> {
    client: &'a CdpClient,
    page_session: &'a str,
    ref_map: &'a RefMap,
    iframe_sessions: &'a HashMap<String, String>,
    target: &'a str,
    session: String,
    object_id: String,
}

impl<'a> CdpControl<'a> {
    /// Resolves the element `target` names in the page `page_session` shows.
    pub(crate) async fn resolve(
        client: &'a CdpClient,
        page_session: &'a str,
        ref_map: &'a RefMap,
        target: &'a str,
        iframe_sessions: &'a HashMap<String, String>,
    ) -> Result<Self, String> {
        let (object_id, session) =
            resolve_element_object_id(client, page_session, ref_map, target, iframe_sessions)
                .await?;
        Ok(Self {
            client,
            page_session,
            ref_map,
            iframe_sessions,
            target,
            session,
            object_id,
        })
    }

    async fn call_function(
        &self,
        js: &str,
        args: Vec<Value>,
        by_value: bool,
    ) -> Result<EvaluateResult, CallError> {
        let result: EvaluateResult = self
            .client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: js.to_string(),
                    object_id: Some(self.object_id.clone()),
                    arguments: (!args.is_empty()).then(|| {
                        args.into_iter()
                            .map(|value| CallArgument {
                                value: Some(value),
                                object_id: None,
                            })
                            .collect()
                    }),
                    return_by_value: Some(by_value),
                    await_promise: Some(true),
                },
                Some(&self.session),
            )
            .await
            .map_err(cdp_error)?;
        if result.exception_details.is_some() {
            return Err(CallError::Threw);
        }
        Ok(result)
    }
}

/// Sorts a CDP failure: a document or element that is gone is not a failure
/// of an action that already happened.
fn cdp_error(error: String) -> CallError {
    let lower = error.to_lowercase();
    if document_gone(&error)
        || [
            "element not found",
            "cannot find object",
            "target navigated",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        CallError::Gone
    } else {
        CallError::Failed(error)
    }
}

impl FormControl for CdpControl<'_> {
    fn target(&self) -> &str {
        self.target
    }

    async fn call(&mut self, js: &str, args: Vec<Value>) -> Result<Value, CallError> {
        let result = self.call_function(js, args, true).await?;
        Ok(result.result.value.unwrap_or(Value::Null))
    }

    async fn bind(&mut self, js: &str) -> Result<(), CallError> {
        let result = self.call_function(js, Vec::new(), false).await?;
        if let Some(object_id) = result.result.object_id {
            self.object_id = object_id;
        }
        Ok(())
    }

    async fn rebind(&mut self) -> Result<(), CallError> {
        let (object_id, session) = resolve_element_object_id(
            self.client,
            self.page_session,
            self.ref_map,
            self.target,
            self.iframe_sessions,
        )
        .await
        .map_err(cdp_error)?;
        (self.object_id, self.session) = (object_id, session);
        Ok(())
    }

    async fn insert_text(&mut self, text: &str) -> Result<(), CallError> {
        // Keyboard input is dispatched at page level.
        self.client
            .send_command_typed::<_, Value>(
                "Input.insertText",
                &InsertTextParams {
                    text: text.to_string(),
                },
                Some(self.page_session),
            )
            .await
            .map_err(cdp_error)?;
        Ok(())
    }

    async fn type_text(&mut self, text: &str, delay_ms: Option<u64>) -> Result<(), CallError> {
        type_text_into_active_context(self.client, self.page_session, text, delay_ms)
            .await
            .map_err(cdp_error)
    }

    async fn click(&mut self) -> Result<(f64, f64), CallError> {
        let (x, y) = unblocked_point(self.client, &self.session, &self.object_id, self.target)
            .await
            .map_err(cdp_error)?;
        let offset = session_viewport_offset(
            self.client,
            self.page_session,
            &self.session,
            self.iframe_sessions,
        )
        .await
        .map_err(cdp_error)?;
        dispatch_click(
            self.client,
            &self.session,
            &[self.session.as_str(), self.page_session],
            x,
            y,
            ClickSpec::default(),
        )
        .await
        .map_err(cdp_error)?;
        Ok((x + offset.0, y + offset.1))
    }
}

pub async fn type_text_into_active_context(
    client: &CdpClient,
    session_id: &str,
    text: &str,
    delay_ms: Option<u64>,
) -> Result<(), String> {
    let delay = delay_ms.unwrap_or(0);

    for ch in text.chars() {
        if matches!(ch, '\n' | '\r' | '\t') {
            let (key, code, key_code) = char_to_key_info(ch);
            let text_str = key_text(&key);
            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyDown".to_string(),
                        key: Some(key.clone()),
                        code: Some(code.clone()),
                        text: text_str.clone(),
                        unmodified_text: text_str,
                        windows_virtual_key_code: Some(key_code),
                        modifiers: None,
                        location: None,
                    },
                    Some(session_id),
                )
                .await?;

            client
                .send_command_typed::<_, Value>(
                    "Input.dispatchKeyEvent",
                    &DispatchKeyEventParams {
                        event_type: "keyUp".to_string(),
                        key: Some(key),
                        code: Some(code),
                        text: None,
                        unmodified_text: None,
                        windows_virtual_key_code: Some(key_code),
                        modifiers: None,
                        location: None,
                    },
                    Some(session_id),
                )
                .await?;
        } else {
            // VS Code/Electron webviews reject repeated dispatchKeyEvent calls
            // carrying printable `text`. Insert printable characters directly
            // and reserve key events for controls like Enter and Tab.
            client
                .send_command_typed::<_, Value>(
                    "Input.insertText",
                    &InsertTextParams {
                        text: ch.to_string(),
                    },
                    Some(session_id),
                )
                .await?;
        }

        if delay > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
        }
    }

    Ok(())
}

pub async fn press_key(client: &CdpClient, session_id: &str, key: &str) -> Result<(), String> {
    press_key_with_modifiers(client, session_id, key, None).await
}

/// Dispatch a keyDown+keyUp sequence for `key` with an optional CDP modifier bitmask.
///
/// Modifier values follow the CDP `Input.dispatchKeyEvent` spec:
/// 1 = Alt, 2 = Control, 4 = Meta (Cmd), 8 = Shift.
///
/// Callers that need a platform-appropriate modifier (e.g. Cmd on macOS,
/// Ctrl elsewhere) must choose the value themselves -- see `cfg!(target_os)`.
pub async fn press_key_with_modifiers(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    modifiers: Option<i32>,
) -> Result<(), String> {
    press_key_with_chord_and_held(client, session_id, key, modifiers.unwrap_or(0), 0).await
}

/// Dispatch keyDown/keyUp for `key` with chord modifiers (emitting keyDown/keyUp
/// for chord modifiers not already held) combined with `held_modifiers`.
pub async fn press_key_with_chord_and_held(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    chord_modifiers: i32,
    held_modifiers: i32,
) -> Result<(), String> {
    let chord_only = chord_modifiers & !held_modifiers;
    let modifier_keys = modifier_key_names(chord_only);
    let mut pressed_modifiers: Vec<&'static str> = Vec::with_capacity(modifier_keys.len());
    let mut active_modifiers = held_modifiers;

    for modifier in &modifier_keys {
        let bit = key_modifier_bit(modifier).expect("modifier names have a CDP bit");
        let next_modifiers = active_modifiers | bit;
        if let Err(error) = dispatch_single_key_event(
            client,
            session_id,
            modifier,
            "keyDown",
            nonzero_modifiers(next_modifiers),
        )
        .await
        {
            for pressed in pressed_modifiers.iter().rev() {
                active_modifiers &=
                    !key_modifier_bit(pressed).expect("modifier names have a CDP bit");
                let _ = dispatch_single_key_event(
                    client,
                    session_id,
                    pressed,
                    "keyUp",
                    nonzero_modifiers(active_modifiers),
                )
                .await;
            }
            return Err(error);
        }
        pressed_modifiers.push(*modifier);
        active_modifiers = next_modifiers;
    }

    let own_bit = key_modifier_bit(key).unwrap_or(0);
    let down_modifiers = nonzero_modifiers(active_modifiers | own_bit);
    let up_modifiers = nonzero_modifiers(active_modifiers & !(own_bit & !held_modifiers));

    let key_result = async {
        dispatch_single_key_event(client, session_id, key, "keyDown", down_modifiers).await?;
        dispatch_single_key_event(client, session_id, key, "keyUp", up_modifiers).await
    }
    .await;

    let mut release_error = None;
    for modifier in pressed_modifiers.iter().rev() {
        active_modifiers &= !key_modifier_bit(modifier).expect("modifier names have a CDP bit");
        if let Err(error) = dispatch_single_key_event(
            client,
            session_id,
            modifier,
            "keyUp",
            nonzero_modifiers(active_modifiers),
        )
        .await
        {
            release_error.get_or_insert(error);
        }
    }

    key_result?;
    release_error.map_or(Ok(()), Err)
}

/// Press `key` without releasing it and return the updated held modifier mask.
pub async fn key_down(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    held_modifiers: i32,
) -> Result<i32, String> {
    let next_modifiers = held_modifiers | key_modifier_bit(key).unwrap_or(0);
    dispatch_single_key_event(
        client,
        session_id,
        key,
        "keyDown",
        nonzero_modifiers(next_modifiers),
    )
    .await?;
    Ok(next_modifiers)
}

/// Release `key` and return the updated held modifier mask.
pub async fn key_up(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    held_modifiers: i32,
) -> Result<i32, String> {
    let next_modifiers = held_modifiers & !key_modifier_bit(key).unwrap_or(0);
    dispatch_single_key_event(
        client,
        session_id,
        key,
        "keyUp",
        nonzero_modifiers(next_modifiers),
    )
    .await?;
    Ok(next_modifiers)
}

async fn dispatch_single_key_event(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    event_type: &str,
    modifiers: Option<i32>,
) -> Result<(), String> {
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &single_key_event_params(key, event_type, modifiers),
            Some(session_id),
        )
        .await?;
    Ok(())
}

pub(crate) const SCROLL_BY_JS: &str = "function(dx, dy) { this.scrollBy(dx, dy); }";

fn single_key_event_params(
    key: &str,
    event_type: &str,
    modifiers: Option<i32>,
) -> DispatchKeyEventParams {
    let (key_name, code, key_code, text, unmodified_text) = modified_key_info(key, modifiers);
    let is_down = event_type == "keyDown";
    let modifiers = if is_down {
        with_own_modifier_bit(&key_name, modifiers)
    } else {
        modifiers
    };
    DispatchKeyEventParams {
        event_type: event_type.to_string(),
        location: key_location(&code),
        key: Some(key_name),
        code: Some(code),
        text: if is_down { text } else { None },
        unmodified_text: if is_down { unmodified_text } else { None },
        windows_virtual_key_code: Some(key_code),
        modifiers,
    }
}

fn nonzero_modifiers(modifiers: i32) -> Option<i32> {
    (modifiers != 0).then_some(modifiers)
}

fn modifier_key_names(modifiers: i32) -> Vec<&'static str> {
    [(2, "Control"), (1, "Alt"), (8, "Shift"), (4, "Meta")]
        .into_iter()
        .filter_map(|(bit, key)| (modifiers & bit != 0).then_some(key))
        .collect()
}

pub async fn scroll(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: Option<&str>,
    delta_x: f64,
    delta_y: f64,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    if let Some(sel) = selector_or_ref {
        let (object_id, effective_session_id) =
            resolve_element_object_id(client, session_id, ref_map, sel, iframe_sessions).await?;
        let js = SCROLL_BY_JS.to_string();
        client
            .send_command_typed::<_, Value>(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: js,
                    object_id: Some(object_id),
                    arguments: Some(vec![
                        CallArgument {
                            value: Some(serde_json::json!(delta_x)),
                            object_id: None,
                        },
                        CallArgument {
                            value: Some(serde_json::json!(delta_y)),
                            object_id: None,
                        },
                    ]),
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(&effective_session_id),
            )
            .await?;
    } else {
        let js = format!("window.scrollBy({}, {})", delta_x, delta_y);
        client
            .send_command_typed::<_, Value>(
                "Runtime.evaluate",
                &EvaluateParams {
                    expression: js,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await?;
    }
    Ok(())
}

pub async fn focus(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: FOCUS_JS.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub(crate) const CLEAR_VALUE_JS: &str = r#"function() {
                    this.focus();
                    this.value = '';
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                    this.dispatchEvent(new Event('change', { bubbles: true }));
                }"#;

pub async fn clear(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: CLEAR_VALUE_JS.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub(crate) const SELECT_ALL_JS: &str = r#"function() {
                    this.focus();
                    if (typeof this.select === 'function') {
                        this.select();
                    } else {
                        const range = document.createRange();
                        range.selectNodeContents(this);
                        const sel = window.getSelection();
                        sel.removeAllRanges();
                        sel.addRange(range);
                    }
                }"#;

pub async fn select_all(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: SELECT_ALL_JS.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub(crate) const SCROLL_INTO_VIEW_JS: &str =
    "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); }";

pub async fn scroll_into_view(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: SCROLL_INTO_VIEW_JS.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

/// The page JS that dispatches `event_type` with `event_init` on `this`.
pub(crate) fn dispatch_event_js(event_type: &str, event_init: Option<&Value>) -> String {
    let init_json = event_init
        .map(|v| serde_json::to_string(v).unwrap_or("{}".to_string()))
        .unwrap_or_else(|| "{ bubbles: true }".to_string());
    format!(
        "function() {{ this.dispatchEvent(new Event({}, {})); }}",
        serde_json::to_string(event_type).unwrap_or_default(),
        init_json
    )
}

pub async fn dispatch_event(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    event_type: &str,
    event_init: Option<&Value>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let js = dispatch_event_js(event_type, event_init);

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub(crate) const HIGHLIGHT_JS: &str = r#"function() {
                    this.style.outline = '2px solid red';
                    this.style.outlineOffset = '2px';
                    const el = this;
                    setTimeout(() => {
                        el.style.outline = '';
                        el.style.outlineOffset = '';
                    }, 3000);
                }"#;

pub async fn highlight(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: HIGHLIGHT_JS.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn tap_touch(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (x, y, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
        false,
    )
    .await?;

    client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(serde_json::json!({
                "type": "touchStart",
                "touchPoints": [{ "x": x, "y": y }],
            })),
            Some(&effective_session_id),
        )
        .await?;

    client
        .send_command(
            "Input.dispatchTouchEvent",
            Some(serde_json::json!({
                "type": "touchEnd",
                "touchPoints": [],
            })),
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

/// Dispatches one mouse event and waits for the browser to ack it, but
/// returns Ok(true) if a JavaScript dialog opens first. A synchronous dialog
/// (confirm/prompt/alert in the event handler) blocks the renderer's main
/// thread, so the input ack cannot arrive until the dialog is resolved;
/// without this the command hangs until the client read timeout and the agent
/// never sees the pending-dialog warning.
async fn dispatch_mouse_or_dialog(
    client: &CdpClient,
    session_id: &str,
    accept_sessions: &[&str],
    params: &DispatchMouseEventParams,
) -> Result<bool, String> {
    use tokio::sync::broadcast::error::RecvError;

    // Subscribe before sending so the dialog event cannot slip past us.
    let mut events = client.subscribe();
    let send =
        client.send_command_typed::<_, Value>("Input.dispatchMouseEvent", params, Some(session_id));
    tokio::pin!(send);
    loop {
        tokio::select! {
            res = &mut send => {
                res?;
                return Ok(false);
            }
            event = events.recv() => {
                match event {
                    Ok(e) if e.method == "Page.javascriptDialogOpening" => {
                        // Only a dialog on this click's frame/page session
                        // aborts it; a background-tab dialog must not. A
                        // session-less event has no flat session and is
                        // treated as the top-level page (i.e. ours).
                        let ours = match e.session_id.as_deref() {
                            Some(sid) => accept_sessions.contains(&sid),
                            None => true,
                        };
                        if ours {
                            return Ok(true);
                        }
                        continue;
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => {
                        (&mut send).await?;
                        return Ok(false);
                    }
                }
            }
        }
    }
}

/// One step of a click: the pointer moves to the target, then each click presses and releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickPhase {
    Move,
    Press(u8),
    Release(u8),
}

impl ClickPhase {
    /// The phase that follows this one, or None when the last click is released.
    fn next(self, spec: &ClickSpec) -> Option<Self> {
        match self {
            Self::Move => Some(Self::Press(1)),
            Self::Press(n) => Some(Self::Release(n)),
            Self::Release(n) if n < spec.count => Some(Self::Press(n + 1)),
            Self::Release(_) => None,
        }
    }

    /// The CDP mouse event this phase dispatches.
    fn event(self, spec: &ClickSpec, x: f64, y: f64) -> DispatchMouseEventParams {
        let modifiers = spec.modifiers.cdp();
        match self {
            Self::Move => DispatchMouseEventParams {
                event_type: "mouseMoved".to_string(),
                x,
                y,
                button: None,
                buttons: None,
                click_count: None,
                delta_x: None,
                delta_y: None,
                modifiers,
            },
            Self::Press(n) => DispatchMouseEventParams {
                event_type: "mousePressed".to_string(),
                x,
                y,
                button: Some(spec.button.name().to_string()),
                buttons: Some(spec.button.mask()),
                click_count: Some(i32::from(n)),
                delta_x: None,
                delta_y: None,
                modifiers,
            },
            Self::Release(n) => DispatchMouseEventParams {
                event_type: "mouseReleased".to_string(),
                x,
                y,
                button: Some(spec.button.name().to_string()),
                buttons: Some(0),
                click_count: Some(i32::from(n)),
                delta_x: None,
                delta_y: None,
                modifiers,
            },
        }
    }
}

async fn dispatch_click(
    client: &CdpClient,
    session_id: &str,
    accept_sessions: &[&str],
    x: f64,
    y: f64,
    spec: ClickSpec,
) -> Result<ClickResult, String> {
    let mut phase = Some(ClickPhase::Move);
    let mut button_pressed = false;
    while let Some(current) = phase {
        if matches!(current, ClickPhase::Press(_)) {
            button_pressed = true;
        }
        if dispatch_mouse_or_dialog(
            client,
            session_id,
            accept_sessions,
            &current.event(&spec, x, y),
        )
        .await?
        {
            return Ok(ClickResult {
                position: (x, y),
                dialog_opened: true,
                pending_release: match current {
                    ClickPhase::Press(n) => Some(PendingRelease {
                        session_id: session_id.to_string(),
                        owner_tab: None,
                        blocked: true,
                        owner_position: (x, y),
                        x,
                        y,
                        button: Some(spec.button),
                        click_count: n,
                        modifiers: spec.modifiers,
                    }),
                    _ => None,
                },
                x,
                y,
                button_pressed,
            });
        }
        phase = current.next(&spec);
    }
    Ok(ClickResult {
        position: (x, y),
        dialog_opened: false,
        pending_release: None,
        x,
        y,
        button_pressed: true,
    })
}

/// Best-effort mouseReleased to clear a button left logically down when a
/// dialog opened mid-click. Called after the dialog is resolved.
pub async fn dispatch_pending_release(
    client: &CdpClient,
    release: &PendingRelease,
) -> Result<(), String> {
    let Some(button) = release.button else {
        return Ok(());
    };
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mouseReleased".to_string(),
                x: release.x,
                y: release.y,
                button: Some(button.name().to_string()),
                buttons: Some(0),
                click_count: Some(i32::from(release.click_count)),
                delta_x: None,
                delta_y: None,
                modifiers: release.modifiers.cdp(),
            },
            Some(&release.session_id),
        )
        .await?;
    Ok(())
}

pub(crate) fn char_to_key_info(ch: char) -> (String, String, i32) {
    match ch {
        '\n' | '\r' => ("Enter".to_string(), "Enter".to_string(), 13),
        '\t' => ("Tab".to_string(), "Tab".to_string(), 9),
        ' ' => (" ".to_string(), "Space".to_string(), 32),
        _ => {
            let key = ch.to_string();
            if ch.is_ascii_alphabetic() {
                // For letters the Windows VK code equals the uppercase ASCII value.
                let upper = ch.to_ascii_uppercase();
                let code = format!("Key{}", upper);
                let key_code = upper as i32;
                (key, code, key_code)
            } else if ch.is_ascii_digit() {
                let code = format!("Digit{}", ch);
                let key_code = ch as i32;
                (key, code, key_code)
            } else {
                let (code, key_code) = punctuation_key_info(ch);
                (key, code.to_string(), key_code)
            }
        }
    }
}

/// Return the DOM `KeyboardEvent.code` value and Windows virtual-key code for
/// a punctuation / symbol character assuming a US keyboard layout.
///
/// The Windows virtual-key codes (VK_OEM_*) differ from ASCII values for
/// punctuation.  Using the raw ASCII code would misidentify characters – e.g.
/// '.' (ASCII 46) collides with VK_DELETE (0x2E = 46), causing the period to
/// be swallowed.
fn punctuation_key_info(ch: char) -> (&'static str, i32) {
    match ch {
        // VK_OEM_1 (0xBA = 186) — ";:" key on US layout
        ';' | ':' => ("Semicolon", 186),
        // VK_OEM_PLUS (0xBB = 187) — "=+" key
        '=' | '+' => ("Equal", 187),
        // VK_OEM_COMMA (0xBC = 188) — ",<" key
        ',' | '<' => ("Comma", 188),
        // VK_OEM_MINUS (0xBD = 189) — "-_" key
        '-' | '_' => ("Minus", 189),
        // VK_OEM_PERIOD (0xBE = 190) — ".>" key
        '.' | '>' => ("Period", 190),
        // VK_OEM_2 (0xBF = 191) — "/?" key
        '/' | '?' => ("Slash", 191),
        // VK_OEM_3 (0xC0 = 192) — "`~" key
        '`' | '~' => ("Backquote", 192),
        // VK_OEM_4 (0xDB = 219) — "[{" key
        '[' | '{' => ("BracketLeft", 219),
        // VK_OEM_5 (0xDC = 220) — "\\|" key
        '\\' | '|' => ("Backslash", 220),
        // VK_OEM_6 (0xDD = 221) — "]}" key
        ']' | '}' => ("BracketRight", 221),
        // VK_OEM_7 (0xDE = 222) — "'\""" key
        '\'' | '"' => ("Quote", 222),
        _ => ("", 0),
    }
}

/// Return the `text` value that CDP `Input.dispatchKeyEvent` needs on the
/// `keyDown` event so that Chrome performs the default action for the key.
/// For example Enter needs `"\r"` to actually submit a form, and Tab needs
/// `"\t"` to move focus.  Non-printable / navigation keys return `None`.
pub(crate) fn key_text(key_name: &str) -> Option<String> {
    match key_name {
        "Enter" => Some("\r".to_string()),
        "Tab" => Some("\t".to_string()),
        " " => Some(" ".to_string()),
        _ => {
            // Single printable characters carry themselves as text.
            if key_name.len() == 1 {
                Some(key_name.to_string())
            } else {
                None
            }
        }
    }
}

pub(crate) fn modified_key_info(
    key: &str,
    modifiers: Option<i32>,
) -> (String, String, i32, Option<String>, Option<String>) {
    let (unmodified_key, code, key_code) = named_key_info(key);
    let mask = modifiers.unwrap_or(0);
    let key_name = if mask & 8 != 0 {
        shifted_char(&unmodified_key)
            .map(|ch| ch.to_string())
            .unwrap_or_else(|| unmodified_key.clone())
    } else {
        unmodified_key.clone()
    };
    let (text, unmodified_text) = if mask & (1 | 2 | 4) != 0 {
        (None, None)
    } else {
        (key_text(&key_name), key_text(&unmodified_key))
    };
    (key_name, code, key_code, text, unmodified_text)
}

pub(crate) fn parse_key_chord(input: &str) -> (String, Option<i32>) {
    let parts: Vec<&str> = input.split('+').collect();
    if parts.len() < 2 {
        return (input.to_string(), None);
    }

    let mut modifiers = 0i32;
    let mut key_parts: Vec<&str> = Vec::new();

    for part in &parts {
        if let Some(bit) = key_modifier_bit(part) {
            modifiers |= bit;
        } else {
            key_parts.push(part);
        }
    }

    if modifiers == 0 {
        return (input.to_string(), None);
    }

    let actual_key = if key_parts.is_empty() {
        input.to_string()
    } else {
        key_parts.join("+")
    };

    (actual_key, Some(modifiers))
}

pub(crate) fn is_non_printable_control_key(key_name: &str) -> bool {
    matches!(
        key_name,
        "Enter"
            | "Tab"
            | "Escape"
            | "Backspace"
            | "Delete"
            | "ArrowUp"
            | "ArrowDown"
            | "ArrowLeft"
            | "ArrowRight"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
    )
}

pub(crate) fn shifted_char(key_name: &str) -> Option<char> {
    let mut chars = key_name.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    if ch.is_ascii_lowercase() {
        return Some(ch.to_ascii_uppercase());
    }
    const PAIRS: &[(char, char)] = &[
        ('1', '!'),
        ('2', '@'),
        ('3', '#'),
        ('4', '$'),
        ('5', '%'),
        ('6', '^'),
        ('7', '&'),
        ('8', '*'),
        ('9', '('),
        ('0', ')'),
        ('`', '~'),
        ('-', '_'),
        ('=', '+'),
        ('[', '{'),
        (']', '}'),
        ('\\', '|'),
        (';', ':'),
        ('\'', '"'),
        (',', '<'),
        ('.', '>'),
        ('/', '?'),
    ];
    PAIRS
        .iter()
        .find(|(plain, _)| *plain == ch)
        .map(|(_, shifted)| *shifted)
}

pub(crate) fn named_key_info(key: &str) -> (String, String, i32) {
    match key.to_lowercase().as_str() {
        "enter" | "return" => ("Enter".to_string(), "Enter".to_string(), 13),
        "tab" => ("Tab".to_string(), "Tab".to_string(), 9),
        "escape" | "esc" => ("Escape".to_string(), "Escape".to_string(), 27),
        "backspace" => ("Backspace".to_string(), "Backspace".to_string(), 8),
        "delete" => ("Delete".to_string(), "Delete".to_string(), 46),
        "arrowup" | "up" => ("ArrowUp".to_string(), "ArrowUp".to_string(), 38),
        "arrowdown" | "down" => ("ArrowDown".to_string(), "ArrowDown".to_string(), 40),
        "arrowleft" | "left" => ("ArrowLeft".to_string(), "ArrowLeft".to_string(), 37),
        "arrowright" | "right" => ("ArrowRight".to_string(), "ArrowRight".to_string(), 39),
        "home" => ("Home".to_string(), "Home".to_string(), 36),
        "end" => ("End".to_string(), "End".to_string(), 35),
        "pageup" => ("PageUp".to_string(), "PageUp".to_string(), 33),
        "pagedown" => ("PageDown".to_string(), "PageDown".to_string(), 34),
        "space" | " " => (" ".to_string(), "Space".to_string(), 32),
        "shift" | "shiftleft" => ("Shift".to_string(), "ShiftLeft".to_string(), 16),
        "shiftright" => ("Shift".to_string(), "ShiftRight".to_string(), 16),
        "control" | "ctrl" | "controlleft" => {
            ("Control".to_string(), "ControlLeft".to_string(), 17)
        }
        "controlright" => ("Control".to_string(), "ControlRight".to_string(), 17),
        "alt" | "altleft" => ("Alt".to_string(), "AltLeft".to_string(), 18),
        "altright" => ("Alt".to_string(), "AltRight".to_string(), 18),
        "meta" | "cmd" | "command" | "metaleft" => ("Meta".to_string(), "MetaLeft".to_string(), 91),
        "metaright" => ("Meta".to_string(), "MetaRight".to_string(), 92),
        lower => {
            let code_char = lower
                .strip_prefix("key")
                .filter(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_lowercase())
                .or_else(|| {
                    lower
                        .strip_prefix("digit")
                        .filter(|rest| rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit())
                });
            if let Some(rest) = code_char {
                char_to_key_info(rest.chars().next().unwrap())
            } else if key.len() == 1 {
                let ch = key.chars().next().unwrap();
                char_to_key_info(ch)
            } else {
                (key.to_string(), key.to_string(), 0)
            }
        }
    }
}

fn key_location(code: &str) -> Option<i32> {
    match code {
        "ShiftLeft" | "ControlLeft" | "AltLeft" | "MetaLeft" => Some(1),
        "ShiftRight" | "ControlRight" | "AltRight" | "MetaRight" => Some(2),
        _ => None,
    }
}

pub(crate) fn key_modifier_bit(key: &str) -> Option<i32> {
    match key.to_ascii_lowercase().as_str() {
        "alt" | "altleft" | "altright" => Some(1),
        "control" | "ctrl" | "controlleft" | "controlright" => Some(2),
        "meta" | "cmd" | "command" | "metaleft" | "metaright" => Some(4),
        "shift" | "shiftleft" | "shiftright" => Some(8),
        _ => None,
    }
}

fn with_own_modifier_bit(key_name: &str, modifiers: Option<i32>) -> Option<i32> {
    let Some(bit) = key_modifier_bit(key_name) else {
        return modifiers;
    };
    Some(modifiers.unwrap_or(0) | bit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that `char_to_key_info` returns the correct (key, code,
    /// windowsVirtualKeyCode) triple for every character in Playwright's
    /// USKeyboardLayout.  The expected values below are taken verbatim from
    /// playwright-core/lib/server/usKeyboardLayout.js so that any drift from
    /// Playwright's behaviour is caught immediately.
    #[test]
    fn test_char_to_key_info_matches_playwright_layout() {
        // (character, expected_code, expected_vk_code)
        let cases: &[(char, &str, i32)] = &[
            // Letters – VK code must equal the uppercase ASCII value.
            ('a', "KeyA", 65),
            ('z', "KeyZ", 90),
            ('A', "KeyA", 65),
            // Digits
            ('0', "Digit0", 48),
            ('9', "Digit9", 57),
            // Punctuation – these are the values from Playwright's layout.
            // The bug that prompted this test sent '.' as VK 46 (= VK_DELETE).
            ('.', "Period", 190),
            (',', "Comma", 188),
            ('/', "Slash", 191),
            (';', "Semicolon", 186),
            ('\'', "Quote", 222),
            ('[', "BracketLeft", 219),
            (']', "BracketRight", 221),
            ('\\', "Backslash", 220),
            ('`', "Backquote", 192),
            ('-', "Minus", 189),
            ('=', "Equal", 187),
            // Shifted variants produced by the same physical keys.
            ('>', "Period", 190),
            ('<', "Comma", 188),
            ('?', "Slash", 191),
            (':', "Semicolon", 186),
            ('"', "Quote", 222),
            ('{', "BracketLeft", 219),
            ('}', "BracketRight", 221),
            ('|', "Backslash", 220),
            ('~', "Backquote", 192),
            ('_', "Minus", 189),
            ('+', "Equal", 187),
            // Whitespace / control
            (' ', "Space", 32),
            ('\n', "Enter", 13),
            ('\t', "Tab", 9),
        ];

        for &(ch, expected_code, expected_vk) in cases {
            let (key, code, vk) = char_to_key_info(ch);
            assert_eq!(
                code, expected_code,
                "char {:?}: expected code {:?}, got {:?}",
                ch, expected_code, code
            );
            assert_eq!(
                vk, expected_vk,
                "char {:?}: expected VK {}, got {} (ASCII would be {})",
                ch, expected_vk, vk, ch as i32
            );
            // key should be the character itself (except control chars).
            if !ch.is_control() {
                assert_eq!(key, ch.to_string(), "char {:?}: key mismatch", ch);
            }
        }
    }

    /// Regression test: period must NEVER map to VK 46 (VK_DELETE).
    #[test]
    fn test_period_is_not_vk_delete() {
        let (_, _, vk) = char_to_key_info('.');
        assert_ne!(
            vk, 46,
            "Period must not use VK code 46 (VK_DELETE); expected 190 (VK_OEM_PERIOD)"
        );
        assert_eq!(vk, 190);
    }

    /// Characters outside the US keyboard layout should return (key, "", 0)
    /// so that `type_text` falls back to `Input.insertText`.
    #[test]
    fn test_unmapped_chars_return_zero_keycode() {
        for ch in ['@', '#', '$', '%', '^', '&', '*', '(', ')', '€', '£', '你'] {
            let (key, code, vk) = char_to_key_info(ch);
            assert_eq!(
                code, "",
                "char {:?}: unmapped char should have empty code, got {:?}",
                ch, code
            );
            assert_eq!(
                vk, 0,
                "char {:?}: unmapped char should have VK 0, got {}",
                ch, vk
            );
            assert_eq!(key, ch.to_string());
        }
    }

    #[test]
    fn click_phase_walks_move_press_release_for_each_click() {
        let spec = ClickSpec {
            button: MouseButton::Right,
            count: 2,
            modifiers: Modifiers::parse_list("ctrl+shift").unwrap(),
        };
        let mut phases = Vec::new();
        let mut phase = Some(ClickPhase::Move);
        while let Some(current) = phase {
            phases.push(current);
            phase = current.next(&spec);
        }
        assert_eq!(
            phases,
            [
                ClickPhase::Move,
                ClickPhase::Press(1),
                ClickPhase::Release(1),
                ClickPhase::Press(2),
                ClickPhase::Release(2),
            ]
        );

        let press = ClickPhase::Press(2).event(&spec, 10.0, 20.0);
        assert_eq!(press.event_type, "mousePressed");
        assert_eq!(press.button.as_deref(), Some("right"));
        assert_eq!(press.buttons, Some(2));
        assert_eq!(press.click_count, Some(2));
        assert_eq!(press.modifiers, Some(10));
    }

    #[test]
    fn test_key_text_returns_correct_text_for_special_keys() {
        assert_eq!(key_text("Enter"), Some("\r".to_string()));
        assert_eq!(key_text("Tab"), Some("\t".to_string()));
        assert_eq!(key_text(" "), Some(" ".to_string()));
        // Single printable characters carry themselves.
        assert_eq!(key_text("a"), Some("a".to_string()));
        assert_eq!(key_text("Z"), Some("Z".to_string()));
        // Non-printable named keys return None.
        assert_eq!(key_text("Escape"), None);
        assert_eq!(key_text("ArrowUp"), None);
        assert_eq!(key_text("Backspace"), None);
        assert_eq!(key_text("Delete"), None);
    }

    #[test]
    fn cdp_failures_of_a_gone_document_or_element_end_verification() {
        for error in [
            "CDP error (Runtime.callFunctionOn): Cannot find context with specified id",
            "Element not found: #cb",
            "Ref @e1 is from a document that is no longer loaded (x)",
        ] {
            assert!(
                matches!(cdp_error(error.to_string()), CallError::Gone),
                "{error}"
            );
        }
        assert!(matches!(
            cdp_error("Could not inspect the element".to_string()),
            CallError::Failed(_)
        ));
    }

    #[test]
    fn test_named_key_info_modifiers_and_code_names() {
        let cases: &[(&str, &str, &str, i32, Option<i32>)] = &[
            ("Shift", "Shift", "ShiftLeft", 16, Some(1)),
            ("ShiftRight", "Shift", "ShiftRight", 16, Some(2)),
            ("Control", "Control", "ControlLeft", 17, Some(1)),
            ("ctrl", "Control", "ControlLeft", 17, Some(1)),
            ("ControlRight", "Control", "ControlRight", 17, Some(2)),
            ("Alt", "Alt", "AltLeft", 18, Some(1)),
            ("AltRight", "Alt", "AltRight", 18, Some(2)),
            ("Meta", "Meta", "MetaLeft", 91, Some(1)),
            ("Cmd", "Meta", "MetaLeft", 91, Some(1)),
            ("MetaRight", "Meta", "MetaRight", 92, Some(2)),
            ("KeyW", "w", "KeyW", 87, None),
            ("keya", "a", "KeyA", 65, None),
            ("Digit7", "7", "Digit7", 55, None),
            ("Key", "Key", "Key", 0, None),
            ("KeyAB", "KeyAB", "KeyAB", 0, None),
            ("Digit10", "Digit10", "Digit10", 0, None),
            ("w", "w", "KeyW", 87, None),
            ("ArrowUp", "ArrowUp", "ArrowUp", 38, None),
        ];
        for &(input, key, code, vk, location) in cases {
            assert_eq!(
                (named_key_info(input), key_location(code)),
                ((key.to_string(), code.to_string(), vk), location),
                "input {input:?}"
            );
        }
    }

    #[test]
    fn test_modified_key_info_applies_shift_and_suppresses_command_text() {
        let check = |key: &str,
                     modifiers: Option<i32>,
                     want: (&str, &str, i32, Option<&str>, Option<&str>)| {
            let (want_key, want_code, want_vk, want_text, want_unmod) = want;
            assert_eq!(
                modified_key_info(key, modifiers),
                (
                    want_key.to_string(),
                    want_code.to_string(),
                    want_vk,
                    want_text.map(String::from),
                    want_unmod.map(String::from),
                ),
                "press {key} with modifiers {modifiers:?}"
            );
        };
        check("l", None, ("l", "KeyL", 76, Some("l"), Some("l")));
        check("l", Some(8), ("L", "KeyL", 76, Some("L"), Some("l")));
        check("1", Some(8), ("!", "Digit1", 49, Some("!"), Some("1")));
        check("/", Some(8), ("?", "Slash", 191, Some("?"), Some("/")));
        check("L", Some(8), ("L", "KeyL", 76, Some("L"), Some("L")));
        check(
            "Enter",
            Some(8),
            ("Enter", "Enter", 13, Some("\r"), Some("\r")),
        );
        check("l", Some(1), ("l", "KeyL", 76, None, None));
        check("l", Some(1 | 8), ("L", "KeyL", 76, None, None));
        check("a", Some(2), ("a", "KeyA", 65, None, None));
        check("a", Some(4), ("a", "KeyA", 65, None, None));
    }

    #[test]
    fn test_single_key_event_params() {
        let params = |key, event_type, modifiers| {
            serde_json::to_value(single_key_event_params(key, event_type, modifiers)).unwrap()
        };
        assert_eq!(
            params("Shift", "keyDown", Some(8)),
            serde_json::json!({
                "type": "keyDown", "key": "Shift", "code": "ShiftLeft",
                "windowsVirtualKeyCode": 16, "modifiers": 8, "location": 1
            })
        );
        assert_eq!(
            params("Shift", "keyUp", None),
            serde_json::json!({
                "type": "keyUp", "key": "Shift", "code": "ShiftLeft",
                "windowsVirtualKeyCode": 16, "location": 1
            })
        );
        assert_eq!(
            params("w", "keyDown", None),
            serde_json::json!({
                "type": "keyDown", "key": "w", "code": "KeyW", "text": "w",
                "unmodifiedText": "w", "windowsVirtualKeyCode": 87
            })
        );
        assert_eq!(
            params("w", "keyDown", Some(8)),
            serde_json::json!({
                "type": "keyDown", "key": "W", "code": "KeyW", "text": "W",
                "unmodifiedText": "w", "windowsVirtualKeyCode": 87, "modifiers": 8
            })
        );
        assert_eq!(
            params("w", "keyUp", None),
            serde_json::json!({
                "type": "keyUp", "key": "w", "code": "KeyW", "windowsVirtualKeyCode": 87
            })
        );
    }

    #[test]
    fn test_modifier_helpers() {
        assert_eq!(key_modifier_bit("Alt"), Some(1));
        assert_eq!(key_modifier_bit("AltRight"), Some(1));
        assert_eq!(key_modifier_bit("Control"), Some(2));
        assert_eq!(key_modifier_bit("ControlLeft"), Some(2));
        assert_eq!(key_modifier_bit("Meta"), Some(4));
        assert_eq!(key_modifier_bit("MetaRight"), Some(4));
        assert_eq!(key_modifier_bit("Shift"), Some(8));
        assert_eq!(key_modifier_bit("ShiftRight"), Some(8));
        assert_eq!(key_modifier_bit("KeyW"), None);
        assert_eq!(with_own_modifier_bit("Shift", None), Some(8));
        assert_eq!(with_own_modifier_bit("Control", Some(8)), Some(10));
        assert_eq!(with_own_modifier_bit("w", Some(2)), Some(2));
        assert_eq!(modifier_key_names(2 | 8), vec!["Control", "Shift"]);
        assert_eq!(
            modifier_key_names(1 | 2 | 4 | 8),
            vec!["Control", "Alt", "Shift", "Meta"]
        );
        assert_eq!(
            parse_key_chord("ShiftRight+KeyA"),
            ("KeyA".to_string(), Some(8))
        );
        assert_eq!(parse_key_chord("ControlLeft+l"), ("l".to_string(), Some(2)));
    }
}
