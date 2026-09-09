use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::*;
use super::element::{resolve_element_center, resolve_element_object_id, RefMap};

/// Outcome of a click. `dialog_opened` is true if a JavaScript dialog opened
/// mid-sequence (the page is then blocked until `dialog accept`/`dismiss`).
/// `pending_release` is set only when the dialog opened after mousePressed but
/// before mouseReleased: the button is logically held until the caller
/// dispatches the release (done once the dialog is resolved), otherwise the
/// next click would register as a drag or double-click.
#[derive(Default)]
pub struct ClickResult {
    pub dialog_opened: bool,
    pub pending_release: Option<PendingRelease>,
}

pub struct PendingRelease {
    pub session_id: String,
    pub x: f64,
    pub y: f64,
    pub button: String,
}

pub async fn click(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    button: &str,
    click_count: i32,
    iframe_sessions: &HashMap<String, String>,
) -> Result<ClickResult, String> {
    let (x, y, effective_session_id) = resolve_element_center(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    // A click-triggered dialog can fire on the frame's own session (OOPIF) or
    // on the top-level page session; both count as "ours". A dialog on any
    // other session belongs to a background tab and must not abort this click.
    dispatch_click(
        client,
        &effective_session_id,
        &[effective_session_id.as_str(), session_id],
        x,
        y,
        button,
        click_count,
    )
    .await
}

pub async fn dblclick(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<ClickResult, String> {
    click(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        "left",
        2,
        iframe_sessions,
    )
    .await
}

pub async fn hover(
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
    )
    .await?;
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
    Ok(())
}

pub async fn fill(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    value: &str,
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

    // Focus the element
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { this.focus(); }".to_string(),
                object_id: Some(object_id.clone()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    // Select all + delete to clear
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    this.select && this.select();
                    this.value = '';
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    // Insert text (keyboard input dispatched at page level, use parent session_id)
    client
        .send_command_typed::<_, Value>(
            "Input.insertText",
            &InsertTextParams {
                text: value.to_string(),
            },
            Some(session_id),
        )
        .await?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn type_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    text: &str,
    clear: bool,
    delay_ms: Option<u64>,
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

    // Focus
    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { this.focus(); }".to_string(),
                object_id: Some(object_id.clone()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    if clear {
        client
            .send_command_typed::<_, Value>(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: r#"function() {
                        this.select && this.select();
                        this.value = '';
                        this.dispatchEvent(new Event('input', { bubbles: true }));
                    }"#
                    .to_string(),
                    object_id: Some(object_id),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(&effective_session_id),
            )
            .await?;
    }

    type_text_into_active_context(client, session_id, text, delay_ms).await
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
                        native_virtual_key_code: Some(key_code),
                        modifiers: None,
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
                        native_virtual_key_code: Some(key_code),
                        modifiers: None,
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
    let (key_name, code, key_code) = named_key_info(key);

    // Suppress text insertion when Control (2) or Meta (4) modifiers are active,
    // since these are command chords (e.g. Ctrl+A = select-all), not text input.
    let has_command_modifier = modifiers.is_some_and(|m| m & (2 | 4) != 0);
    let text = if has_command_modifier {
        None
    } else {
        key_text(&key_name)
    };

    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &DispatchKeyEventParams {
                event_type: "keyDown".to_string(),
                key: Some(key_name.clone()),
                code: Some(code.clone()),
                text: text.clone(),
                unmodified_text: text.clone(),
                windows_virtual_key_code: Some(key_code),
                native_virtual_key_code: Some(key_code),
                modifiers,
            },
            Some(session_id),
        )
        .await?;

    client
        .send_command_typed::<_, Value>(
            "Input.dispatchKeyEvent",
            &DispatchKeyEventParams {
                event_type: "keyUp".to_string(),
                key: Some(key_name),
                code: Some(code),
                text: None,
                unmodified_text: None,
                windows_virtual_key_code: Some(key_code),
                native_virtual_key_code: Some(key_code),
                modifiers,
            },
            Some(session_id),
        )
        .await?;

    Ok(())
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
        let js = "function(dx, dy) { this.scrollBy(dx, dy); }".to_string();
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

pub async fn select_option(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    values: &[String],
    iframe_sessions: &HashMap<String, String>,
    timeout_ms: u64,
) -> Result<SelectionResult, String> {
    if values.is_empty() {
        return Err(format!(
            "Cannot select from '{}': at least one value is required",
            selector_or_ref
        ));
    }

    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    // Keep this classifier beside the native/custom branch: supported custom
    // controls are explicit ARIA comboboxes and listboxes, values match exact
    // value/label or normalized accessible text, and success requires a
    // standard verified state before the configured action timeout expires.
    // Opaque styled elements, shadow-DOM and cross-document popups remain
    // intentionally unsupported.
    let metadata = call_selection_function(
        client,
        &effective_session_id,
        &object_id,
        "function() { return { tag: (this.tagName || '').toLowerCase(), role: (this.getAttribute('role') || '').toLowerCase() }; }",
        None,
        selector_or_ref,
    )
    .await?;
    let metadata = metadata
        .as_object()
        .ok_or_else(|| selection_error(selector_or_ref, "the target metadata was not returned"))?;
    let kind = classify_selection_control(
        metadata
            .get("tag")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        metadata
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );

    match kind {
        SelectionControlKind::NativeSelect => {
            select_native_option(
                client,
                &effective_session_id,
                &object_id,
                selector_or_ref,
                values,
            )
            .await?;
            Ok(SelectionResult::default())
        }
        SelectionControlKind::AriaCombobox | SelectionControlKind::AriaListbox => {
            select_aria_option(
                client,
                AriaSelectionRequest {
                    session_id,
                    ref_map,
                    target: selector_or_ref,
                    values,
                    iframe_sessions,
                    expected_kind: kind,
                    timeout_ms,
                },
            )
            .await
        }
        SelectionControlKind::Unsupported => Err(selection_error(
            selector_or_ref,
            "the target is not a native select or a role=combobox/listbox control",
        )),
    }
}

/// Selection actions can open a JavaScript dialog during the real input
/// sequence. The daemon stores this release and sends it after the dialog is
/// accepted or dismissed, just as it does for `click`.
#[derive(Default)]
pub struct SelectionResult {
    pub dialog_opened: bool,
    pub pending_release: Option<PendingRelease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionControlKind {
    NativeSelect,
    AriaCombobox,
    AriaListbox,
    Unsupported,
}

fn classify_selection_control(tag: &str, role: &str) -> SelectionControlKind {
    match (tag, role) {
        ("select", _) => SelectionControlKind::NativeSelect,
        (_, "combobox") => SelectionControlKind::AriaCombobox,
        (_, "listbox") => SelectionControlKind::AriaListbox,
        _ => SelectionControlKind::Unsupported,
    }
}

fn normalize_selection_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn selection_option_matches(
    requested: &str,
    value: Option<&str>,
    label: Option<&str>,
    accessible_text: &str,
) -> bool {
    value == Some(requested)
        || label == Some(requested)
        || normalize_selection_whitespace(accessible_text)
            == normalize_selection_whitespace(requested)
}

fn bounded_available_options(options: &[String]) -> String {
    const MAX_OPTIONS: usize = 12;
    const MAX_CHARS: usize = 500;
    let mut output = String::new();
    let mut truncated = options.len() > MAX_OPTIONS;
    for (index, option) in options.iter().take(MAX_OPTIONS).enumerate() {
        let separator = if index == 0 { "" } else { ", " };
        if output.len() + separator.len() + option.len() > MAX_CHARS {
            truncated = true;
            break;
        }
        output.push_str(separator);
        output.push_str(option);
    }
    if truncated {
        if !output.is_empty() {
            output.push_str(", ");
        }
        output.push('…');
    }
    if output.is_empty() {
        "(none)".to_string()
    } else {
        output
    }
}

fn selection_error(target: &str, reason: &str) -> String {
    format!("Selection failed for '{}': {}", target, reason)
}

fn exception_error(target: &str, result: &EvaluateResult) -> Option<String> {
    result.exception_details.as_ref().map(|details| {
        let description = details
            .exception
            .as_ref()
            .and_then(|exception| exception.description.as_deref())
            .unwrap_or(&details.text);
        selection_error(target, &format!("JavaScript exception: {}", description))
    })
}

async fn call_selection_function(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    function_declaration: &str,
    argument: Option<Value>,
    target: &str,
) -> Result<Value, String> {
    let arguments = argument.map(|value| {
        vec![CallArgument {
            value: Some(value),
            object_id: None,
        }]
    });
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: function_declaration.to_string(),
                object_id: Some(object_id.to_string()),
                arguments,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;
    if let Some(error) = exception_error(target, &result) {
        return Err(error);
    }
    result
        .result
        .value
        .ok_or_else(|| selection_error(target, "the browser returned no selection result"))
}

async fn call_selection_object_function(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    function_declaration: &str,
    argument: Option<Value>,
    return_by_value: bool,
    target: &str,
) -> Result<EvaluateResult, String> {
    let arguments = argument.map(|value| {
        vec![CallArgument {
            value: Some(value),
            object_id: None,
        }]
    });
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: function_declaration.to_string(),
                object_id: Some(object_id.to_string()),
                arguments,
                return_by_value: Some(return_by_value),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;
    if let Some(error) = exception_error(target, &result) {
        return Err(error);
    }
    Ok(result)
}

async fn select_native_option(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    target: &str,
    values: &[String],
) -> Result<(), String> {
    let result = call_selection_function(
        client,
        session_id,
        object_id,
        r#"function(vals) {
            const normalize = value => String(value || '').replace(/\s+/g, ' ').trim();
            if ((this.tagName || '').toLowerCase() !== 'select') {
                return { error: 'target is not a native select' };
            }
            if (this.matches(':disabled')) {
                return { error: 'the native select is disabled' };
            }
            if (this.multiple !== true && vals.length > 1) {
                return { error: 'multiple values were requested from a single-select native select' };
            }
            const options = Array.from(this.options || []);
            const describe = option => {
                const label = normalize(option.label);
                const text = normalize(option.textContent);
                const textDetail = label === text ? '' : ` [text: "${text}"]`;
                return `${option.value} ("${label}")${textDetail}${option.matches(':disabled') ? ' [disabled]' : ''}`;
            };
            const available = options.map(describe);
            const matches = (option, value) =>
                option.value === value ||
                normalize(option.label) === normalize(value) ||
                normalize(option.textContent) === normalize(value);
            const enabledMatch = value => options.some(option =>
                !option.matches(':disabled') &&
                matches(option, value)
            );
            const missing = vals.filter(value => !enabledMatch(value));
            if (missing.length) {
                return { error: 'No enabled option matched ' + JSON.stringify(missing), available };
            }
            for (const option of options) {
                option.selected = !option.matches(':disabled') && vals.some(value => matches(option, value));
            }
            this.dispatchEvent(new Event('input', { bubbles: true }));
            this.dispatchEvent(new Event('change', { bubbles: true }));
            return {
                selected: Array.from(this.selectedOptions || []).map(option => ({ value: option.value, label: normalize(option.label), text: normalize(option.textContent) })),
                multiple: this.multiple === true
            };
        }"#,
        Some(serde_json::json!(values)),
        target,
    )
    .await?;

    if let Some(error) = result.get("error").and_then(Value::as_str) {
        let available = result
            .get("available")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(String::from)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return Err(selection_error(
            target,
            &format!(
                "{}. Available options: {}",
                error,
                bounded_available_options(&available)
            ),
        ));
    }

    let verify = call_selection_function(
        client,
        session_id,
        object_id,
        r#"function(vals) {
            const normalize = value => String(value || '').replace(/\s+/g, ' ').trim();
            if ((this.tagName || '').toLowerCase() !== 'select') return { verified: false, selected: [] };
            const selected = Array.from(this.selectedOptions || []).map(option => ({ value: option.value, label: normalize(option.label), text: normalize(option.textContent) }));
            const matches = entry => vals.some(value => entry.value === value || entry.label === normalize(value) || entry.text === normalize(value));
            const verified = this.multiple === true
                ? vals.every(value => selected.some(entry => entry.value === value || entry.label === normalize(value) || entry.text === normalize(value)))
                : selected.some(matches);
            return { verified, selected };
        }"#,
        Some(serde_json::json!(values)),
        target,
    )
    .await?;
    if !verify
        .get("verified")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(selection_error(
            target,
            &format!(
                "the native select did not verify the requested value(s) {}",
                serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
            ),
        ));
    }
    Ok(())
}

const ARIA_STATE_FUNCTION: &str = r#"function(vals) {
    const normalize = value => String(value || '').replace(/\s+/g, ' ').trim();
    const role = (this.getAttribute('role') || '').toLowerCase();
    const roots = [];
    const addRoot = root => { if (root && !roots.includes(root)) roots.push(root); };
    // A listbox owns descendant options directly. A combobox can likewise
    // render its popup below itself, while aria-controls/aria-owns add the
    // standard same-document popup relationship when it is portaled.
    if (role === 'listbox' || role === 'combobox') addRoot(this);
    for (const attribute of ['aria-controls', 'aria-owns']) {
        for (const id of (this.getAttribute(attribute) || '').split(/\s+/).filter(Boolean)) {
            addRoot(this.ownerDocument && this.ownerDocument.getElementById(id));
        }
    }
    if (!roots.length) addRoot(this);
    const options = [];
    const seen = new Set();
    for (const root of roots) {
        if (root.matches && root.matches('[role="option"]') && !seen.has(root)) {
            seen.add(root); options.push(root);
        }
        for (const option of root.querySelectorAll ? root.querySelectorAll('[role="option"]') : []) {
            if (!seen.has(option)) { seen.add(option); options.push(option); }
        }
    }
    const visible = option => {
        if (option.getAttribute('aria-hidden') === 'true') return false;
        const style = option.ownerDocument.defaultView.getComputedStyle(option);
        const rect = option.getBoundingClientRect();
        return style.display !== 'none' && style.visibility !== 'hidden' && parseFloat(style.opacity || '1') > 0 && rect.width > 0 && rect.height > 0;
    };
    const disabled = option => option.matches(':disabled') || option.getAttribute('aria-disabled') === 'true';
    const fields = option => {
        const labelledBy = (option.getAttribute('aria-labelledby') || '').split(/\s+/)
            .map(id => option.ownerDocument.getElementById(id))
            .filter(Boolean)
            .map(element => element.textContent || '')
            .join(' ');
        return {
            value: option.getAttribute('value'),
            label: option.getAttribute('aria-label') || option.getAttribute('label') || (labelledBy ? normalize(labelledBy) : null),
            text: normalize(option.textContent),
        };
    };
    const matches = (field, value) => field.value === value || field.label === value || field.text === normalize(value);
    const state = options.map((option, index) => {
        const field = fields(option);
        const available = visible(option) && !disabled(option);
        const matchValues = vals.filter(value => matches(field, value));
        return {
            index,
            value: field.value,
            label: field.label,
            text: field.text,
            selected: option.getAttribute('aria-selected') === 'true',
            hasSelectedState: option.hasAttribute('aria-selected'),
            available,
            disabled: disabled(option),
            visible: visible(option),
            matchValues,
        };
    });
    const activeId = this.getAttribute('aria-activedescendant');
    const active = activeId && this.ownerDocument ? this.ownerDocument.getElementById(activeId) : null;
    const activeIndex = active ? options.indexOf(active) : -1;
    const committed = [];
    if ('value' in this && typeof this.value === 'string') committed.push(normalize(this.value));
    if (this.getAttribute('aria-valuetext')) committed.push(normalize(this.getAttribute('aria-valuetext')));
    const formControl = this.matches && this.matches('input,textarea,select')
        ? this
        : (this.querySelector && this.querySelector('input,textarea,select'));
    const input = this.matches && this.matches('input,textarea')
        ? this
        : (this.querySelector && this.querySelector('input,textarea'));
    if (formControl && typeof formControl.value === 'string') {
        committed.push(normalize(formControl.value));
    }
    if (role === 'combobox') committed.push(normalize(this.textContent));
    return {
        role,
        disabled: this.matches && (this.matches(':disabled') || this.getAttribute('aria-disabled') === 'true'),
        expanded: this.getAttribute('aria-expanded') === 'true',
        multiselectable: this.getAttribute('aria-multiselectable') === 'true',
        associated: roots.length > 0,
        options: state,
        committed: committed.filter(Boolean),
        inputBacked: !!input,
        inputValue: input && typeof input.value === 'string' ? normalize(input.value) : null,
        // aria-activedescendant is navigation state, not a committed value.
        // Keep it separate so keyboard-operated widgets are committed with
        // Enter before selection can be reported as successful.
        hasCommittedState: committed.length > 0,
        activeIndex: activeIndex >= 0 ? activeIndex : null,
    };
}"#;

const ARIA_FOCUS_INPUT_FUNCTION: &str = r#"function() {
    const input = this.matches && this.matches('input,textarea')
        ? this
        : (this.querySelector && this.querySelector('input,textarea'));
    if (!input) return { focused: false, value: null };
    input.focus();
    return {
        focused: input.ownerDocument.activeElement === input,
        value: typeof input.value === 'string' ? input.value : ''
    };
}"#;

const ARIA_GET_OPTION_FUNCTION: &str = r#"function(index) {
    const role = (this.getAttribute('role') || '').toLowerCase();
    const roots = [];
    const addRoot = root => { if (root && !roots.includes(root)) roots.push(root); };
    if (role === 'listbox' || role === 'combobox') addRoot(this);
    for (const attribute of ['aria-controls', 'aria-owns']) {
        for (const id of (this.getAttribute(attribute) || '').split(/\s+/).filter(Boolean)) {
            addRoot(this.ownerDocument && this.ownerDocument.getElementById(id));
        }
    }
    if (!roots.length) addRoot(this);
    const options = [];
    const seen = new Set();
    for (const root of roots) {
        if (root.matches && root.matches('[role="option"]') && !seen.has(root)) { seen.add(root); options.push(root); }
        for (const option of root.querySelectorAll ? root.querySelectorAll('[role="option"]') : []) {
            if (!seen.has(option)) { seen.add(option); options.push(option); }
        }
    }
    return options[index] || null;
}"#;

struct AriaSelectionRequest<'a> {
    session_id: &'a str,
    ref_map: &'a RefMap,
    target: &'a str,
    values: &'a [String],
    iframe_sessions: &'a HashMap<String, String>,
    expected_kind: SelectionControlKind,
    timeout_ms: u64,
}

#[derive(Default)]
struct AriaSelectionVerification<'a> {
    requested_options_seen: bool,
    committed_before_activation: Option<&'a [String]>,
    activation_attempted: bool,
}

async fn select_aria_option(
    client: &CdpClient,
    request: AriaSelectionRequest<'_>,
) -> Result<SelectionResult, String> {
    let AriaSelectionRequest {
        session_id,
        ref_map,
        target,
        values,
        iframe_sessions,
        expected_kind,
        timeout_ms,
    } = request;
    if expected_kind == SelectionControlKind::AriaCombobox && values.len() > 1 {
        return Err(selection_error(
            target,
            "multiple values are only supported for an explicitly multiselectable ARIA listbox",
        ));
    }

    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut opened = false;
    let mut requested_options_seen = false;
    let mut filter_input_sent = false;
    let mut activation_attempted = false;
    // A combobox's input/display can contain the requested text while the
    // popup is only filtering options. Keep the pre-activation committed state
    // so that fallback verification requires an actual commit transition.
    let mut committed_before_activation: Option<Vec<String>> = None;

    loop {
        let (object_id, effective_session_id) =
            match resolve_element_object_id(client, session_id, ref_map, target, iframe_sessions)
                .await
            {
                Ok(resolved) => resolved,
                Err(error) if is_stale_selection_error(&error) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(selection_error(
                            target,
                            "the control rerendered before inspection",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
        let state = match call_selection_function(
            client,
            &effective_session_id,
            &object_id,
            ARIA_STATE_FUNCTION,
            Some(serde_json::json!(values)),
            target,
        )
        .await
        {
            Ok(state) => state,
            Err(error) if is_stale_selection_error(&error) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(selection_error(
                        target,
                        "the control rerendered during inspection",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        let state_role = state
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if classify_selection_control("", state_role) != expected_kind {
            return Err(selection_error(
                target,
                "the control changed to an unsupported ARIA role",
            ));
        }

        let multiselectable = state
            .get("multiselectable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if state
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(selection_error(target, "the ARIA control is disabled"));
        }
        if expected_kind == SelectionControlKind::AriaListbox
            && values.len() > 1
            && !multiselectable
        {
            return Err(selection_error(
                target,
                "multiple values were requested from a single-select ARIA listbox",
            ));
        }

        if expected_kind == SelectionControlKind::AriaCombobox
            && !opened
            && !state
                .get("expanded")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            let click_result = click(
                client,
                session_id,
                ref_map,
                target,
                "left",
                1,
                iframe_sessions,
            )
            .await;
            let click_result = match click_result {
                Ok(result) => result,
                Err(error) if is_stale_selection_error(&error) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(selection_error(
                            target,
                            "the combobox rerendered while opening",
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            opened = true;
            if click_result.dialog_opened {
                return Ok(SelectionResult {
                    dialog_opened: true,
                    pending_release: click_result.pending_release,
                });
            }
            continue;
        }

        let options = state
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if values.iter().all(|value| {
            options.iter().any(|option| {
                option
                    .get("available")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && option_matches_requested_value(option, value)
            })
        }) {
            requested_options_seen = true;
        }
        // Verify before requiring a visible option. A normal combobox closes
        // its popup immediately after a successful choice, so its committed
        // value or aria-selected state can be the only observable signal on
        // the next poll.
        if aria_selection_confirmed(
            &state,
            expected_kind,
            values,
            multiselectable,
            &AriaSelectionVerification {
                requested_options_seen,
                committed_before_activation: committed_before_activation.as_deref(),
                activation_attempted,
            },
        ) {
            return Ok(SelectionResult::default());
        }

        // Input-backed comboboxes commonly render no options until the user
        // types a filter. Use the same trusted keyboard input as `type`, then
        // continue polling and re-resolving the option nodes. This remains
        // limited to standard ARIA comboboxes; opaque custom controls are not
        // inferred from arbitrary page attributes.
        if expected_kind == SelectionControlKind::AriaCombobox
            && !filter_input_sent
            && state
                .get("inputBacked")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            && !values.iter().all(|value| {
                options.iter().any(|option| {
                    option
                        .get("available")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                        && option_matches_requested_value(option, value)
                })
            })
        {
            let focus_result = call_selection_function(
                client,
                &effective_session_id,
                &object_id,
                ARIA_FOCUS_INPUT_FUNCTION,
                None,
                target,
            )
            .await?;
            if !focus_result
                .get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(selection_error(
                    target,
                    "the combobox input could not receive keyboard input",
                ));
            }
            let current_value = focus_result
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !current_value.is_empty() {
                let modifier = if cfg!(target_os = "macos") { 4 } else { 2 };
                press_key_with_modifiers(client, &effective_session_id, "a", Some(modifier))
                    .await?;
                press_key(client, &effective_session_id, "Backspace").await?;
            }
            type_text_into_active_context(client, &effective_session_id, &values[0], None).await?;
            filter_input_sent = true;
            continue;
        }

        let available = options
            .iter()
            .filter(|option| {
                option
                    .get("available")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        if available.is_empty() {
            if tokio::time::Instant::now() >= deadline {
                return Err(selection_error(
                    target,
                    &format!(
                        "no visible, enabled ARIA options rendered before the timeout (available: {})",
                        bounded_available_options(&aria_option_descriptions(&options))
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }

        let missing = values
            .iter()
            .filter(|value| {
                !available.iter().any(|option| {
                    option
                        .get("matchValues")
                        .and_then(Value::as_array)
                        .is_some_and(|matches| {
                            matches
                                .iter()
                                .any(|matched| matched.as_str() == Some(value))
                        })
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            if tokio::time::Instant::now() >= deadline {
                return Err(selection_error(
                    target,
                    &format!(
                        "no visible, enabled option matched {} before the timeout. Available options: {}",
                        serde_json::to_string(&missing).unwrap_or_else(|_| "[]".to_string()),
                        bounded_available_options(&aria_option_descriptions(&options))
                    ),
                ));
            }
            // A visible option does not prove that an asynchronously rendered
            // or virtualized option set is complete. Keep polling so a later
            // option can become matchable within the configured timeout.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        }

        let click_index = aria_next_click_index(&state, expected_kind, values, multiselectable);
        let Some(click_index) = click_index else {
            if tokio::time::Instant::now() >= deadline {
                return Err(selection_error(
                    target,
                    &format!(
                        "selection verification failed: the control did not expose a standard verified selection state for {}",
                        serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
                    ),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };

        // Keyboard-operated comboboxes may expose the requested option as the
        // active descendant without making the option itself clickable. Enter
        // is the normal commit action for that standard widget contract.
        if aria_active_descendant_matches(&state, expected_kind, click_index) {
            // aria-activedescendant is navigation state on a combobox, so
            // Enter must be delivered to the focused control rather than to
            // the option node. Listboxes still receive the normal option
            // click path because an active descendant does not define their
            // selection commitment semantics.
            let focused = call_selection_function(
                client,
                &effective_session_id,
                &object_id,
                "function() { this.focus(); return document.activeElement === this; }",
                None,
                target,
            )
            .await?;
            if !focused.as_bool().unwrap_or(false) {
                return Err(selection_error(
                    target,
                    "the control could not receive keyboard input",
                ));
            }
            if committed_before_activation.is_none() {
                committed_before_activation = selection_committed_values(&state);
            }
            activation_attempted = true;
            if dispatch_key_for_selection(
                client,
                &effective_session_id,
                &[effective_session_id.as_str(), session_id],
                "Enter",
            )
            .await?
            {
                // Enter can synchronously open a confirm or prompt from the
                // widget's key handler. The renderer then cannot acknowledge
                // the key event until the dialog is resolved, so surface the
                // dialog immediately instead of waiting for the CDP timeout.
                return Ok(SelectionResult {
                    dialog_opened: true,
                    pending_release: None,
                });
            }
            continue;
        }

        let fresh_object = match call_selection_object_function(
            client,
            &effective_session_id,
            &object_id,
            ARIA_GET_OPTION_FUNCTION,
            Some(serde_json::json!(click_index)),
            false,
            target,
        )
        .await
        {
            Ok(result) => result,
            Err(error) if is_stale_selection_error(&error) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(selection_error(
                        target,
                        "the control rerendered before input",
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some(option_object_id) = fresh_object.result.object_id else {
            if tokio::time::Instant::now() >= deadline {
                return Err(selection_error(
                    target,
                    "the matched option became stale before input",
                ));
            }
            continue;
        };
        let geometry = match call_selection_object_function(
            client,
            &effective_session_id,
            &option_object_id,
            r#"function(add_frame_offsets) {
                if (!this.isConnected) return { error: 'option became detached' };
                this.scrollIntoView({ block: 'nearest', inline: 'nearest' });
                const rect = this.getBoundingClientRect();
                const style = this.ownerDocument.defaultView.getComputedStyle(this);
                if (style.display === 'none' || style.visibility === 'hidden' || rect.width <= 0 || rect.height <= 0) {
                    return { error: 'option is not visible' };
                }
                let x = rect.left + rect.width / 2;
                let y = rect.top + rect.height / 2;
                const hit = this.ownerDocument.elementFromPoint(x, y);
                if (hit && hit !== this && !this.contains(hit)) {
                    const name = hit.tagName.toLowerCase() + (hit.id ? '#' + hit.id : '');
                    return { error: 'option is covered by <' + name + '>' };
                }
                if (add_frame_offsets) {
                    let win = this.ownerDocument.defaultView;
                    while (win && win.frameElement) {
                        const frameRect = win.frameElement.getBoundingClientRect();
                        x += frameRect.x + win.frameElement.clientLeft;
                        y += frameRect.y + win.frameElement.clientTop;
                        win = win.parent;
                    }
                }
                return { x, y };
            }"#,
            Some(serde_json::json!(effective_session_id == session_id)),
            true,
            target,
        )
        .await
        {
            Ok(geometry) => geometry,
            Err(error) if is_stale_selection_error(&error) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(selection_error(target, "the matched option became stale before input"));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        let geometry = geometry.result.value.unwrap_or(Value::Null);
        if let Some(error) = geometry.get("error").and_then(Value::as_str) {
            if matches!(error, "option became detached" | "option is not visible") {
                if tokio::time::Instant::now() >= deadline {
                    return Err(selection_error(target, error));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            }
            return Err(selection_error(target, error));
        }
        let Some(x) = geometry.get("x").and_then(Value::as_f64) else {
            if tokio::time::Instant::now() >= deadline {
                return Err(selection_error(
                    target,
                    "the matched option had no usable click coordinates",
                ));
            }
            continue;
        };
        let Some(y) = geometry.get("y").and_then(Value::as_f64) else {
            if tokio::time::Instant::now() >= deadline {
                return Err(selection_error(
                    target,
                    "the matched option had no usable click coordinates",
                ));
            }
            continue;
        };
        if committed_before_activation.is_none() {
            committed_before_activation = selection_committed_values(&state);
        }
        activation_attempted = true;
        let click_result = dispatch_click(
            client,
            &effective_session_id,
            &[effective_session_id.as_str(), session_id],
            x,
            y,
            "left",
            1,
        )
        .await?;
        if click_result.dialog_opened {
            return Ok(SelectionResult {
                dialog_opened: true,
                pending_release: click_result.pending_release,
            });
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(selection_error(target, "selection verification timed out"));
        }
    }
}

fn is_stale_selection_error(error: &str) -> bool {
    [
        "Could not find object with given id",
        "Cannot find context with specified id",
        "Execution context was destroyed",
        "Object reference chain is too long",
    ]
    .iter()
    .any(|marker| error.contains(marker))
}

fn aria_active_descendant_matches(
    state: &Value,
    kind: SelectionControlKind,
    click_index: usize,
) -> bool {
    kind == SelectionControlKind::AriaCombobox
        && state
            .get("activeIndex")
            .and_then(Value::as_u64)
            .map(|index| index as usize)
            == Some(click_index)
}

fn aria_option_descriptions(options: &[Value]) -> Vec<String> {
    options
        .iter()
        .map(|option| {
            let value = option
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let label = option
                .get("label")
                .and_then(Value::as_str)
                .or_else(|| option.get("text").and_then(Value::as_str))
                .unwrap_or_default();
            let status = if option
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                " [disabled]"
            } else if !option
                .get("visible")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                " [hidden]"
            } else {
                ""
            };
            format!("{} (\"{}\"){}", value, label, status)
        })
        .collect()
}

fn option_matches_values(option: &Value, values: &[String]) -> bool {
    values.iter().any(|requested| {
        selection_option_matches(
            requested,
            option.get("value").and_then(Value::as_str),
            option.get("label").and_then(Value::as_str),
            option
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
    })
}

fn option_matches_requested_value(option: &Value, value: &str) -> bool {
    option
        .get("matchValues")
        .and_then(Value::as_array)
        .is_some_and(|matches| {
            matches
                .iter()
                .any(|matched| matched.as_str() == Some(value))
        })
        || selection_option_matches(
            value,
            option.get("value").and_then(Value::as_str),
            option.get("label").and_then(Value::as_str),
            option
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        )
}

fn selection_committed_values(state: &Value) -> Option<Vec<String>> {
    state
        .get("committed")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
}

fn committed_state_changed(state: &Value, before: Option<&[String]>) -> bool {
    let Some(before) = before else {
        return false;
    };
    selection_committed_values(state).is_some_and(|after| after != before)
}

fn aria_selection_confirmed(
    state: &Value,
    kind: SelectionControlKind,
    values: &[String],
    multiselectable: bool,
    verification: &AriaSelectionVerification<'_>,
) -> bool {
    let AriaSelectionVerification {
        requested_options_seen,
        committed_before_activation,
        activation_attempted,
    } = verification;
    let requested_options_seen = *requested_options_seen;
    let committed_before_activation = *committed_before_activation;
    let activation_attempted = *activation_attempted;
    let options = state
        .get("options")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let selected = options
        .iter()
        .filter(|option| {
            option
                .get("selected")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    let selected_matches = values.iter().all(|value| {
        selected.iter().any(|option| {
            (!option
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && (option
                    .get("available")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || (kind == SelectionControlKind::AriaCombobox && requested_options_seen)))
                && option_matches_requested_value(option, value)
        })
    });
    let known_requested_options = values.iter().all(|value| {
        options.iter().any(|option| {
            (kind != SelectionControlKind::AriaListbox
                || option
                    .get("available")
                    .and_then(Value::as_bool)
                    .unwrap_or(false))
                && option_matches_requested_value(option, value)
        })
    });
    if kind == SelectionControlKind::AriaListbox {
        let has_state = options.iter().any(|option| {
            option
                .get("hasSelectedState")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        });
        let final_set = if multiselectable {
            selected.iter().all(|option| {
                option
                    .get("available")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && option_matches_values(option, values)
            }) && selected_matches
        } else {
            selected.len() == 1
                && selected[0]
                    .get("available")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                && selected_matches
        };
        return known_requested_options && has_state && final_set;
    }
    if known_requested_options
        && selected_matches
        && selected.iter().any(|option| {
            option
                .get("hasSelectedState")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
    {
        return true;
    }
    // A combobox commonly removes its popup options after committing a
    // choice. In that case the exact input/display or associated form value
    // is the only remaining standard postcondition, so it must not depend on
    // the option node still being mounted. The value must have changed after
    // activation; a filter-only input value and popup closure are not a
    // selection commit.
    let committed_after_activation = committed_state_changed(state, committed_before_activation);
    requested_options_seen
        && activation_attempted
        && committed_after_activation
        && state
            .get("committed")
            .and_then(Value::as_array)
            .is_some_and(|committed| {
                state
                    .get("hasCommittedState")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    && values.iter().all(|value| {
                        committed.iter().any(|entry| {
                            entry.as_str().is_some_and(|entry| {
                                entry == value
                                    || normalize_selection_whitespace(entry)
                                        == normalize_selection_whitespace(value)
                            })
                        })
                    })
            })
}

fn aria_next_click_index(
    state: &Value,
    kind: SelectionControlKind,
    values: &[String],
    multiselectable: bool,
) -> Option<usize> {
    let options = state.get("options")?.as_array()?;
    if kind == SelectionControlKind::AriaListbox && multiselectable {
        if let Some(option) = options.iter().find(|option| {
            option
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && option
                    .get("selected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                && !option_matches_values(option, values)
        }) {
            return option
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as usize);
        }
        if let Some(option) = options.iter().find(|option| {
            option
                .get("available")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && !option
                    .get("selected")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                && option_matches_values(option, values)
        }) {
            return option
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as usize);
        }
        return None;
    }
    options.iter().find_map(|option| {
        (option
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && option_matches_values(option, values)
            && !option
                .get("selected")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            && option.get("index").and_then(Value::as_u64).is_some())
        .then(|| {
            option
                .get("index")
                .and_then(Value::as_u64)
                .map(|index| index as usize)
        })
        .flatten()
    })
}

pub async fn check(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let is_checked = super::element::is_element_checked(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    if !is_checked {
        click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            "left",
            1,
            iframe_sessions,
        )
        .await?;

        // Verify the click changed the state (Playwright parity: _setChecked re-checks).
        // If the coordinate-based click missed (e.g. hidden input, overlay), retry
        // with a JS .click() on the element and its associated input.
        if !super::element::is_element_checked(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await?
        {
            js_click_checkbox(
                client,
                session_id,
                ref_map,
                selector_or_ref,
                iframe_sessions,
            )
            .await?;
        }
    }
    Ok(())
}

pub async fn uncheck(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let is_checked = super::element::is_element_checked(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;
    if is_checked {
        click(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            "left",
            1,
            iframe_sessions,
        )
        .await?;

        // Same verify-and-retry as check().
        if super::element::is_element_checked(
            client,
            session_id,
            ref_map,
            selector_or_ref,
            iframe_sessions,
        )
        .await?
        {
            js_click_checkbox(
                client,
                session_id,
                ref_map,
                selector_or_ref,
                iframe_sessions,
            )
            .await?;
        }
    }
    Ok(())
}

/// Fallback for when the coordinate-based CDP click did not toggle the
/// checkbox/radio state. This mirrors how Playwright dispatches clicks
/// through the DOM rather than via raw Input.dispatchMouseEvent coordinates.
///
/// Uses the same follow-label resolution as `is_element_checked`:
/// 1. If the element is a native input → `.click()` it directly.
/// 2. If the element is inside a `<label>` → `.click()` the label's `.control`.
/// 3. If the element has a nested `<input>` → `.click()` that input.
/// 4. Otherwise → `.click()` the element itself (handles ARIA role controls).
async fn js_click_checkbox(
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

    let js = r#"function() {
            var el = this;
            var tag = el.tagName && el.tagName.toUpperCase();
            // 1. Native input — click it directly
            if (tag === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')) {
                el.click();
                return;
            }
            // 2. Follow label → control association
            var label = tag === 'LABEL' ? el : (el.closest && el.closest('label'));
            if (label && label.tagName && label.tagName.toUpperCase() === 'LABEL' && label.control) {
                label.control.click();
                return;
            }
            // 3. Nested native input
            var input = el.querySelector && el.querySelector('input[type="checkbox"], input[type="radio"]');
            if (input) {
                input.click();
                return;
            }
            // 4. ARIA role control — click the element itself
            el.click();
        }"#;

    client
        .send_command_typed::<_, Value>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js.to_string(),
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
                function_declaration: "function() { this.focus(); }".to_string(),
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
                function_declaration: r#"function() {
                    this.focus();
                    this.value = '';
                    this.dispatchEvent(new Event('input', { bubbles: true }));
                    this.dispatchEvent(new Event('change', { bubbles: true }));
                }"#
                .to_string(),
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
                function_declaration: r#"function() {
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
                }"#
                .to_string(),
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
                function_declaration:
                    "function() { this.scrollIntoView({ block: 'center', inline: 'center' }); }"
                        .to_string(),
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

    let init_json = event_init
        .map(|v| serde_json::to_string(v).unwrap_or("{}".to_string()))
        .unwrap_or_else(|| "{ bubbles: true }".to_string());

    let js = format!(
        "function() {{ this.dispatchEvent(new Event({}, {})); }}",
        serde_json::to_string(event_type).unwrap_or_default(),
        init_json
    );

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
                function_declaration: r#"function() {
                    this.style.outline = '2px solid red';
                    this.style.outlineOffset = '2px';
                    const el = this;
                    setTimeout(() => {
                        el.style.outline = '';
                        el.style.outlineOffset = '';
                    }, 3000);
                }"#
                .to_string(),
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

/// Dispatch one key event and return whether a JavaScript dialog opened before
/// the browser acknowledged it. Synchronous dialogs block the renderer's
/// acknowledgement just as they do for mouse events, so keyboard activation
/// must observe the dialog event concurrently with the CDP request.
async fn dispatch_key_or_dialog(
    client: &CdpClient,
    session_id: &str,
    accept_sessions: &[&str],
    params: &DispatchKeyEventParams,
) -> Result<bool, String> {
    use tokio::sync::broadcast::error::RecvError;

    let mut events = client.subscribe();
    let send =
        client.send_command_typed::<_, Value>("Input.dispatchKeyEvent", params, Some(session_id));
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
                        let ours = match e.session_id.as_deref() {
                            Some(sid) => accept_sessions.contains(&sid),
                            None => true,
                        };
                        if ours {
                            return Ok(true);
                        }
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => {
                        (&mut send).await?;
                        return Ok(false);
                    }
                }
            }
        }
    }
}

/// Send a trusted keyDown+keyUp sequence for selection, stopping as soon as a
/// dialog opens. No keyUp is sent after a dialog interrupts keyDown because the
/// blocked renderer cannot process it; unlike a mouse button, a keyboard key
/// does not need a pending release tracked in daemon state.
async fn dispatch_key_for_selection(
    client: &CdpClient,
    session_id: &str,
    accept_sessions: &[&str],
    key: &str,
) -> Result<bool, String> {
    let (key_name, code, key_code) = named_key_info(key);
    let text = key_text(&key_name);
    let key_down = DispatchKeyEventParams {
        event_type: "keyDown".to_string(),
        key: Some(key_name.clone()),
        code: Some(code.clone()),
        text: text.clone(),
        unmodified_text: text,
        windows_virtual_key_code: Some(key_code),
        native_virtual_key_code: Some(key_code),
        modifiers: None,
    };
    if dispatch_key_or_dialog(client, session_id, accept_sessions, &key_down).await? {
        return Ok(true);
    }

    let key_up = DispatchKeyEventParams {
        event_type: "keyUp".to_string(),
        key: Some(key_name),
        code: Some(code),
        text: None,
        unmodified_text: None,
        windows_virtual_key_code: Some(key_code),
        native_virtual_key_code: Some(key_code),
        modifiers: None,
    };
    dispatch_key_or_dialog(client, session_id, accept_sessions, &key_up).await
}

async fn dispatch_click(
    client: &CdpClient,
    session_id: &str,
    accept_sessions: &[&str],
    x: f64,
    y: f64,
    button: &str,
    click_count: i32,
) -> Result<ClickResult, String> {
    // Move
    if dispatch_mouse_or_dialog(
        client,
        session_id,
        accept_sessions,
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
    )
    .await?
    {
        // No button was pressed yet, nothing to release.
        return Ok(ClickResult {
            dialog_opened: true,
            pending_release: None,
        });
    }

    let button_value = match button {
        "right" => 2,
        "middle" => 4,
        _ => 1,
    };

    // Press
    if dispatch_mouse_or_dialog(
        client,
        session_id,
        accept_sessions,
        &DispatchMouseEventParams {
            event_type: "mousePressed".to_string(),
            x,
            y,
            button: Some(button.to_string()),
            buttons: Some(button_value),
            click_count: Some(click_count),
            delta_x: None,
            delta_y: None,
            modifiers: None,
        },
    )
    .await?
    {
        // Dialog opened from the mousedown handler: the button is held and the
        // release will never arrive on its own. Hand the caller what it needs
        // to release once the dialog is resolved.
        return Ok(ClickResult {
            dialog_opened: true,
            pending_release: Some(PendingRelease {
                session_id: session_id.to_string(),
                x,
                y,
                button: button.to_string(),
            }),
        });
    }

    // Release. A dialog here fired from the click/mouseup handler, which runs
    // after the button is already up, so there is nothing left to release.
    let dialog_opened = dispatch_mouse_or_dialog(
        client,
        session_id,
        accept_sessions,
        &DispatchMouseEventParams {
            event_type: "mouseReleased".to_string(),
            x,
            y,
            button: Some(button.to_string()),
            buttons: Some(0),
            click_count: Some(click_count),
            delta_x: None,
            delta_y: None,
            modifiers: None,
        },
    )
    .await?;
    Ok(ClickResult {
        dialog_opened,
        pending_release: None,
    })
}

/// Best-effort mouseReleased to clear a button left logically down when a
/// dialog opened mid-click. Called after the dialog is resolved.
pub async fn dispatch_pending_release(
    client: &CdpClient,
    release: &PendingRelease,
) -> Result<(), String> {
    client
        .send_command_typed::<_, Value>(
            "Input.dispatchMouseEvent",
            &DispatchMouseEventParams {
                event_type: "mouseReleased".to_string(),
                x: release.x,
                y: release.y,
                button: Some(release.button.clone()),
                buttons: Some(0),
                click_count: Some(1),
                delta_x: None,
                delta_y: None,
                modifiers: None,
            },
            Some(&release.session_id),
        )
        .await?;
    Ok(())
}

fn char_to_key_info(ch: char) -> (String, String, i32) {
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
fn key_text(key_name: &str) -> Option<String> {
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

fn named_key_info(key: &str) -> (String, String, i32) {
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
        _ => {
            if key.len() == 1 {
                let ch = key.chars().next().unwrap();
                char_to_key_info(ch)
            } else {
                (key.to_string(), key.to_string(), 0)
            }
        }
    }
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
    fn selection_classifier_accepts_native_and_standard_aria_controls_only() {
        assert_eq!(
            classify_selection_control("select", ""),
            SelectionControlKind::NativeSelect
        );
        assert_eq!(
            classify_selection_control("div", "combobox"),
            SelectionControlKind::AriaCombobox
        );
        assert_eq!(
            classify_selection_control("ul", "listbox"),
            SelectionControlKind::AriaListbox
        );
        assert_eq!(
            classify_selection_control("div", "button"),
            SelectionControlKind::Unsupported
        );
    }

    #[test]
    fn selection_whitespace_and_diagnostics_are_bounded() {
        assert_eq!(
            normalize_selection_whitespace("  New\n York\tCity "),
            "New York City"
        );
        assert_eq!(bounded_available_options(&[]), "(none)");
        let options = (0..20)
            .map(|index| format!("option-{index}"))
            .collect::<Vec<_>>();
        let diagnostic = bounded_available_options(&options);
        assert!(diagnostic.len() <= 503);
        assert!(diagnostic.ends_with("…"));
        assert!(!diagnostic.contains("option-19"));
    }

    #[test]
    fn selection_option_matching_is_exact_with_normalized_text() {
        assert!(selection_option_matches(
            "us",
            Some("us"),
            Some("United States"),
            "United States"
        ));
        assert!(selection_option_matches(
            "United States",
            Some("us"),
            None,
            " United\n States "
        ));
        assert!(!selection_option_matches(
            "u",
            Some("us"),
            Some("United States"),
            "United States"
        ));
        assert!(selection_option_matches(
            "United States",
            Some("us"),
            None,
            "United\nStates"
        ));
    }

    #[test]
    fn aria_selection_verification_requires_standard_state_and_exact_values() {
        let state = serde_json::json!({
            "options": [
                { "index": 0, "matchValues": ["red", "Red"], "selected": false, "available": true, "hasSelectedState": true },
                { "index": 1, "matchValues": ["blue", "Blue"], "selected": true, "available": true, "hasSelectedState": true }
            ],
            "committed": [],
            "hasCommittedState": false
        });
        assert!(aria_selection_confirmed(
            &state,
            SelectionControlKind::AriaListbox,
            &["blue".to_string()],
            false,
            &AriaSelectionVerification {
                requested_options_seen: true,
                ..Default::default()
            }
        ));
        assert!(!aria_selection_confirmed(
            &state,
            SelectionControlKind::AriaListbox,
            &["blu".to_string()],
            false,
            &AriaSelectionVerification {
                requested_options_seen: true,
                ..Default::default()
            }
        ));

        let no_state = serde_json::json!({
            "options": [{ "index": 0, "matchValues": ["blue"], "selected": true, "hasSelectedState": false }],
            "committed": [],
            "hasCommittedState": false
        });
        assert!(!aria_selection_confirmed(
            &no_state,
            SelectionControlKind::AriaListbox,
            &["blue".to_string()],
            false,
            &AriaSelectionVerification::default()
        ));

        let disabled_selected = serde_json::json!({
            "options": [{
                "index": 0,
                "matchValues": ["blue"],
                "selected": true,
                "available": false,
                "disabled": true,
                "hasSelectedState": true
            }]
        });
        assert!(!aria_selection_confirmed(
            &disabled_selected,
            SelectionControlKind::AriaCombobox,
            &["blue".to_string()],
            false,
            &AriaSelectionVerification {
                requested_options_seen: true,
                ..Default::default()
            }
        ));

        let hidden_selected_after_observation = serde_json::json!({
            "options": [{
                "index": 0,
                "matchValues": ["blue"],
                "selected": true,
                "available": false,
                "visible": false,
                "hasSelectedState": true
            }]
        });
        assert!(aria_selection_confirmed(
            &hidden_selected_after_observation,
            SelectionControlKind::AriaCombobox,
            &["blue".to_string()],
            false,
            &AriaSelectionVerification {
                requested_options_seen: true,
                ..Default::default()
            }
        ));

        let committed_without_popup = serde_json::json!({
            "options": [],
            "committed": ["Blue Label"],
            "hasCommittedState": true
        });
        let previous_committed = vec!["Choose".to_string()];
        assert!(aria_selection_confirmed(
            &committed_without_popup,
            SelectionControlKind::AriaCombobox,
            &["Blue Label".to_string()],
            false,
            &AriaSelectionVerification {
                requested_options_seen: true,
                committed_before_activation: Some(&previous_committed),
                activation_attempted: true,
            }
        ));

        let transient_input = serde_json::json!({
            "options": [{
                "index": 0,
                "matchValues": ["Blue"],
                "selected": false,
                "available": true,
                "hasSelectedState": true
            }],
            "committed": ["Blue"],
            "hasCommittedState": true
        });
        let same_committed = vec!["Blue".to_string()];
        assert!(!aria_selection_confirmed(
            &transient_input,
            SelectionControlKind::AriaCombobox,
            &["Blue".to_string()],
            false,
            &AriaSelectionVerification {
                requested_options_seen: true,
                committed_before_activation: Some(&same_committed),
                ..Default::default()
            }
        ));

        let filter_only_closed_popup = serde_json::json!({
            "options": [{
                "index": 0,
                "matchValues": ["Blue"],
                "selected": false,
                "available": false,
                "visible": false,
                "hasSelectedState": true
            }],
            "committed": ["Blue"],
            "hasCommittedState": true,
            "expanded": false
        });
        assert!(!aria_selection_confirmed(
            &filter_only_closed_popup,
            SelectionControlKind::AriaCombobox,
            &["Blue".to_string()],
            false,
            &AriaSelectionVerification {
                requested_options_seen: true,
                committed_before_activation: Some(&same_committed),
                activation_attempted: true,
            }
        ));
    }

    #[test]
    fn aria_multiselect_next_click_drives_the_final_set() {
        let state = serde_json::json!({
            "options": [
                { "index": 0, "matchValues": ["red"], "selected": true, "available": true },
                { "index": 1, "matchValues": ["blue"], "selected": false, "available": true }
            ]
        });
        assert_eq!(
            aria_next_click_index(
                &state,
                SelectionControlKind::AriaListbox,
                &["blue".to_string()],
                true,
            ),
            Some(0)
        );
    }

    #[test]
    fn active_descendant_commit_is_reserved_for_comboboxes() {
        let state = serde_json::json!({ "activeIndex": 1 });
        assert!(aria_active_descendant_matches(
            &state,
            SelectionControlKind::AriaCombobox,
            1
        ));
        assert!(!aria_active_descendant_matches(
            &state,
            SelectionControlKind::AriaListbox,
            1
        ));
    }
}
