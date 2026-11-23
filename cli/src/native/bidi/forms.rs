//! Firefox keyboard and form commands.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::client::SANDBOX;
use super::daemon::session;
use super::input::{
    key_chord_actions, key_event_actions, key_from_raw, key_text_actions, pointer_click, Origin,
};
use super::interact::{act_element, perform_actions, PointerAction};
use super::session::FirefoxSession;
use super::target::{resolve_target, Target};
use crate::native::actions::{
    check_upload_files, raw_key, upload_browser_is_remote, upload_files, DaemonState,
};
use crate::native::browser::ensure_file_input_shape;
use crate::native::element::{
    click_checkbox_js, decide_check_plan, element_check_probe_js, not_checkable_error, CheckPlan,
    CheckProbe, ELEMENT_CHECKED_JS, ELEMENT_CHECK_CONTROL_JS,
};
use crate::native::interaction::{
    CLEAR_INPUT_JS, CLEAR_VALUE_JS, FOCUS_JS, SELECT_ALL_JS, SELECT_OPTION_JS,
};

const LITERAL_EDIT_JS: &str = r#"function(text, replace, useActive) {
    let el = useActive ? ((this && this.ownerDocument) || document).activeElement : this;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    if (!el || !el.isConnected || el.matches(':disabled') || el.readOnly) return 'unsupported';
    if (useActive) try { el.focus(); } catch {}
    const doc = el.ownerDocument;
    const editable = el.isContentEditable || el.contentEditable === 'true' || el.contentEditable === 'plaintext-only';
    const nativeEdit = () => {
        let canceled = false;
        const watch = event => canceled = canceled || event.defaultPrevented;
        doc.addEventListener('beforeinput', watch);
        let inserted;
        try {
            inserted = doc.execCommand && doc.execCommand(replace && text === '' ? 'delete' : 'insertText', false, text);
        } catch {
            inserted = false;
        } finally {
            doc.removeEventListener('beforeinput', watch);
        }
        return canceled ? 'canceled' : inserted ? 'ok' : null;
    };
    if (!replace && text === '') return 'ok';
    if (editable) {
        if (replace) {
            const range = doc.createRange();
            range.selectNodeContents(el);
            const selection = doc.getSelection();
            selection.removeAllRanges();
            selection.addRange(range);
        }
        return nativeEdit() || 'unsupported';
    }
    if (!(el instanceof HTMLTextAreaElement) && !(el instanceof HTMLInputElement && ['text', 'search', 'url', 'tel', 'email', 'password', 'number'].includes(el.type))) return 'unsupported';
    let start;
    let end;
    try {
        start = el.selectionStart;
        end = el.selectionEnd;
    } catch {}
    const hasSelection = Number.isInteger(start) && Number.isInteger(end);
    if (replace) try { el.select(); } catch {}
    const result = replace || !hasSelection ? nativeEdit() : null;
    if (result) return result;
    const deleting = replace && text === '';
    const inputType = deleting ? 'deleteContentBackward' : replace ? 'insertReplacementText' : 'insertText';
    if (!el.dispatchEvent(new InputEvent('beforeinput', { bubbles: true, cancelable: true, inputType, data: text }))) return 'canceled';
    if (hasSelection) {
        el.setRangeText(text, replace ? 0 : start, replace ? el.value.length : end, 'end');
    } else {
        const prototype = el instanceof HTMLInputElement ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
        const value = replace ? text : el.value + text;
        Object.getOwnPropertyDescriptor(prototype, 'value').set.call(el, value);
    }
    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType, data: text }));
    return 'ok';
}"#;

const SAME_DOCUMENT_JS: &str = "function() { return this.ownerDocument === document; }";

const FOCUSED_FRAME_CONTEXT_JS: &str = r#"() => {
    let el = document.activeElement;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    return el && el.matches('iframe,frame') ? el.contentWindow : null;
}"#;

/// Reads a node's tag name and raw `type` attribute for the file-input check.
const FILE_INPUT_PROBE_JS: &str = "function() { return { name: this.tagName || '', type: this.getAttribute ? this.getAttribute('type') : null }; }";

fn require<'a>(cmd: &'a Value, key: &str) -> Result<&'a str, String> {
    cmd.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing '{key}' parameter"))
}

pub(super) async fn run(
    ff: &FirefoxSession,
    target: &Target,
    js: &str,
    args: Vec<Value>,
) -> Result<Option<Value>, String> {
    ff.race_prompt(
        &target.context,
        ff.bidi
            .call_function(&target.context, js, Some(target.node_ref()), args),
    )
    .await
}

/// The context an edit runs in: the target's own, or without a target the tab or the frame
/// focused inside it.
async fn edit_context(ff: &FirefoxSession, target: Option<&Target>) -> Result<String, String> {
    if let Some(target) = target {
        return Ok(target.context.clone());
    }
    let mut focused = ff.contexts.active_tab().to_string();
    let mut visited = HashSet::new();
    while visited.insert(focused.clone()) {
        let child = ff
            .bidi
            .call_function_in_sandbox(&focused, "agent-browser", FOCUSED_FRAME_CONTEXT_JS, vec![])
            .await?;
        let Some(child) = child.as_str() else {
            break;
        };
        focused = child.to_string();
    }
    Ok(focused)
}

/// Replaces the target's value, or types `text` at the focused element. Typing at a target stays
/// in the target's document, so text meant for it never reaches another frame that took focus.
async fn literal_edit_in_context(
    ff: &FirefoxSession,
    context: &str,
    target: Option<&Target>,
    text: &str,
    replace: bool,
) -> Result<bool, String> {
    let result = ff
        .race_prompt(
            context,
            ff.bidi.call_function(
                context,
                LITERAL_EDIT_JS,
                target.map(Target::node_ref),
                vec![json!(text), json!(replace), json!(!replace)],
            ),
        )
        .await?;
    let Some(result) = result else {
        return Ok(true);
    };
    match result.as_str() {
        Some("ok" | "canceled") => Ok(false),
        Some("unsupported") if !replace => Ok(false),
        _ => Err("Text input is not supported for the focused element".to_string()),
    }
}

async fn literal_edit(
    ff: &FirefoxSession,
    target: Option<&Target>,
    text: &str,
    replace: bool,
) -> Result<(), String> {
    let context = edit_context(ff, target).await?;
    literal_edit_in_context(ff, &context, target, text, replace)
        .await
        .map(|_| ())
}

/// Whether the target's document is still the one its context shows, asked in the sandbox.
async fn in_current_document(ff: &FirefoxSession, target: &Target) -> bool {
    ff.bidi
        .call_function_on_in_sandbox(
            &target.context,
            "agent-browser",
            SAME_DOCUMENT_JS,
            target.node_ref(),
        )
        .await
        .is_ok_and(|same| same == true)
}

async fn focus_target(ff: &FirefoxSession, target: &Target) -> Result<bool, String> {
    Ok(run(ff, target, FOCUS_JS, vec![]).await?.is_some())
}

/// Types `text` one character at a time. Typing ends early, still as a success, when a dialog opens
/// or a navigation replaces the target's document.
async fn send_text(
    ff: &FirefoxSession,
    target: Option<&Target>,
    text: &str,
    delay_ms: Option<u64>,
) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let context = edit_context(ff, target).await?;
    for ch in text.chars() {
        let text = ch.to_string();
        let ended = if matches!(ch, '\n' | '\r' | '\t') {
            match target {
                Some(target) if !in_current_document(ff, target).await => true,
                _ => perform_actions(ff, &context, key_text_actions(&text)).await?,
            }
        } else {
            match literal_edit_in_context(ff, &context, target, &text, false).await {
                Err(e) if target.is_some() && e.starts_with("no such node") => true,
                result => result?,
            }
        };
        if ended {
            break;
        }
        if let Some(delay) = delay_ms.filter(|delay| *delay > 0) {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }
    Ok(())
}

fn select_values(cmd: &Value) -> Vec<String> {
    match cmd.get("values") {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        Some(Value::String(value)) => vec![value.clone()],
        _ => cmd
            .get("value")
            .and_then(Value::as_str)
            .map(|value| vec![value.to_string()])
            .unwrap_or_default(),
    }
}

fn absolute_files(files: &[String]) -> Vec<String> {
    let cwd = std::env::current_dir().ok();
    files
        .iter()
        .map(|file| {
            let path = Path::new(file);
            if path.is_absolute() {
                file.clone()
            } else {
                cwd.as_ref()
                    .map(|dir| dir.join(path))
                    .unwrap_or_else(|| PathBuf::from(file))
                    .to_string_lossy()
                    .into_owned()
            }
        })
        .collect()
}

/// Focuses the element and replaces its value with literal text.
pub async fn fill(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let value = require(cmd, "value")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    if !focus_target(ff, &target).await? {
        return Ok(json!({ "filled": selector, "dialogOpened": true }));
    }
    literal_edit(ff, Some(&target), value, true).await?;
    Ok(json!({ "filled": selector }))
}

/// Focuses the element and types `text`, optionally clearing first.
pub async fn type_text(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let text = require(cmd, "text")?;
    let clear = cmd.get("clear").and_then(Value::as_bool).unwrap_or(false);
    let delay = cmd.get("delay").and_then(Value::as_u64);
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    if !focus_target(ff, &target).await? {
        return Ok(json!({ "typed": text, "dialogOpened": true }));
    }
    if !clear || run(ff, &target, CLEAR_INPUT_JS, vec![]).await?.is_some() {
        send_text(ff, Some(&target), text, delay).await?;
    }
    Ok(json!({ "typed": text }))
}

/// Presses a key or chord such as `Control+a` on the active tab.
pub async fn press(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let key = require(cmd, "key")?;
    let ff = session(state)?;
    perform_actions(ff, ff.contexts.active(), key_chord_actions(key)).await?;
    Ok(json!({ "pressed": key }))
}

/// Sends one keyDown event for `key`.
pub async fn keydown(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let key = require(cmd, "key")?;
    let ff = session(state)?;
    perform_actions(ff, ff.contexts.active(), key_event_actions("keyDown", key)).await?;
    Ok(json!({ "keydown": key }))
}

/// Sends one keyUp event for `key`.
pub async fn keyup(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let key = require(cmd, "key")?;
    let ff = session(state)?;
    perform_actions(ff, ff.contexts.active(), key_event_actions("keyUp", key)).await?;
    Ok(json!({ "keyup": key }))
}

/// Handles the `keyboard` options: typing text, inserting text or one key event.
pub async fn keyboard(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    match cmd.get("subaction").and_then(Value::as_str) {
        Some("type") => {
            let text = require(cmd, "text")?;
            let ff = session(state)?;
            send_text(ff, None, text, None).await?;
            Ok(json!({ "typed": text }))
        }
        Some("insertText") => inserttext(cmd, state).await,
        _ => {
            let mut event = cmd.clone();
            event["type"] = cmd["eventType"].clone();
            input_keyboard(&event, state).await
        }
    }
}

/// Sends one raw key event from the `input_keyboard` options.
pub async fn input_keyboard(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let event_type = cmd.get("type").and_then(Value::as_str).unwrap_or("keyDown");
    if event_type == "char" {
        let text = require(cmd, "text")?;
        let ff = session(state)?;
        literal_edit(ff, None, text, false).await?;
        return Ok(json!({ "dispatched": event_type }));
    }
    let key = key_from_raw(&raw_key(cmd))?;
    let ff = session(state)?;
    perform_actions(
        ff,
        ff.contexts.active(),
        key_event_actions(event_type, &key),
    )
    .await?;
    Ok(json!({ "dispatched": event_type }))
}

/// Types `text` into the active tab without targeting an element.
pub async fn inserttext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let text = require(cmd, "text")?;
    let ff = session(state)?;
    if !text.is_empty() {
        literal_edit(ff, None, text, false).await?;
    }
    Ok(json!({ "inserted": true }))
}

/// Clears an input through the shared page JS.
pub async fn clear(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    run(ff, &target, CLEAR_VALUE_JS, vec![]).await?;
    Ok(json!({ "cleared": selector }))
}

/// Selects all text in an input or the contents of an element.
pub async fn selectall(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    run(ff, &target, SELECT_ALL_JS, vec![]).await?;
    Ok(json!({ "selected": selector }))
}

/// Selects one or more option values through the shared page JS.
pub async fn select(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let values = select_values(cmd);
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let result = run(ff, &target, SELECT_OPTION_JS, vec![json!(values)])
        .await?
        .unwrap_or_default();
    if let Some(error) = result.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(json!({ "selected": values }))
}

/// Resolves a target to its canonical checkable control node inside the isolated sandbox.
async fn resolve_check_control(
    ff: &FirefoxSession,
    target: &Target,
) -> Result<Option<Target>, String> {
    let resolved = ff
        .bidi
        .call_function_on_in_sandbox(
            &target.context,
            SANDBOX,
            ELEMENT_CHECK_CONTROL_JS,
            target.node_ref(),
        )
        .await?;
    Ok(resolved["sharedId"].as_str().map(|shared_id| Target {
        context: target.context.clone(),
        shared_id: shared_id.to_string(),
    }))
}

/// Reads checked state, native-input status, and clickability in the isolated sandbox.
async fn probe_check_control(
    ff: &FirefoxSession,
    control: &Target,
) -> Result<Option<CheckProbe>, String> {
    let probed = ff
        .bidi
        .call_function_on_in_sandbox(
            &control.context,
            SANDBOX,
            &element_check_probe_js(),
            control.node_ref(),
        )
        .await?;
    if probed.is_null() {
        return Ok(None);
    }
    Ok(Some(CheckProbe {
        checked: probed
            .get("checked")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        native: probed
            .get("native")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        clickable: probed
            .get("clickable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        blocker: None,
    }))
}

async fn set_checked(
    state: &mut DaemonState,
    target: &Target,
    selector: &str,
    checked: bool,
) -> Result<(), String> {
    let Some(control) = resolve_check_control(session(state)?, target).await? else {
        return Err(not_checkable_error(selector));
    };
    let probe = probe_check_control(session(state)?, &control).await?;
    match decide_check_plan(selector, checked, probe, control) {
        CheckPlan::Reject(err) => Err(err),
        CheckPlan::AlreadySet => Ok(()),
        CheckPlan::DomClick(control) => {
            run(session(state)?, &control, &click_checkbox_js(), vec![]).await?;
            Ok(())
        }
        CheckPlan::Pointer(control) => {
            let outcome = act_element(
                state,
                &control,
                selector,
                PointerAction::Check,
                pointer_click(&Origin::Element(control.shared_id.clone()), "left", 1),
                json!({}),
            )
            .await?;
            if outcome.dialog_opened {
                return Ok(());
            }
            let after = session(state)?
                .bidi
                .call_function_on_in_sandbox(
                    &control.context,
                    SANDBOX,
                    ELEMENT_CHECKED_JS,
                    control.node_ref(),
                )
                .await?;
            if after.as_bool() == Some(!checked) {
                run(session(state)?, &control, &click_checkbox_js(), vec![]).await?;
            }
            Ok(())
        }
    }
}

/// Checks a checkbox or radio unless it is already checked.
pub async fn check(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    set_checked(state, &target, selector, true).await?;
    Ok(json!({ "checked": selector }))
}

/// Unchecks a checkbox or radio only when it is checked.
pub async fn uncheck(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    set_checked(state, &target, selector, false).await?;
    Ok(json!({ "unchecked": selector }))
}

/// Sets an input's value and dispatches `input` and `change`, as Chrome does.
pub async fn setvalue(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let value = require(cmd, "value")?;
    let js = format!(
        "function() {{ this.value = {}; this.dispatchEvent(new Event('input', {{bubbles: true}})); this.dispatchEvent(new Event('change', {{bubbles: true}})); }}",
        serde_json::to_string(value).unwrap_or_default()
    );
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    run(ff, &target, &js, vec![]).await?;
    Ok(json!({ "set": selector, "value": value }))
}

/// Uploads files to a file input with `input.setFiles`.
pub async fn upload(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let _ = session(state)?;
    let selector = require(cmd, "selector")?;
    let files = upload_files(cmd);
    let paths = absolute_files(&files);
    let warning = check_upload_files(&files, upload_browser_is_remote(state))?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let shape = ff
        .bidi
        .call_function_on_in_sandbox(
            &target.context,
            SANDBOX,
            FILE_INPUT_PROBE_JS,
            target.node_ref(),
        )
        .await?;
    ensure_file_input_shape(
        selector,
        shape["name"].as_str().unwrap_or(""),
        shape["type"].as_str(),
    )?;
    let params = json!({
        "context": target.context,
        "element": target.node_ref(),
        "files": paths,
    });
    let _ = ff
        .race_prompt(&target.context, ff.bidi.send("input.setFiles", params))
        .await?;
    if let Some(warning) = warning {
        state.push_action_warning(&warning);
    }
    Ok(json!({ "uploaded": files.len(), "selector": selector }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::session::tests::{fake_session, fake_session_with, Calls};
    use crate::native::browser::precondition_code;

    fn state_with(ff: FirefoxSession) -> DaemonState {
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
    }

    fn script(remote: Value) -> Value {
        json!({ "type": "success", "result": remote })
    }

    fn file_input_shape(name: &str, input_type: Option<&str>) -> Value {
        let input_type = match input_type {
            Some(value) => json!({ "type": "string", "value": value }),
            None => json!({ "type": "null" }),
        };
        json!({
            "type": "object",
            "value": [
                ["name", { "type": "string", "value": name }],
                ["type", input_type],
            ],
        })
    }

    fn upload_responder(shape: Value) -> impl Fn(&Value) -> Value + Send + 'static {
        move |req: &Value| match req["method"].as_str() {
            Some("script.callFunction") => {
                let js = req["params"]["functionDeclaration"].as_str().unwrap_or("");
                if js == FILE_INPUT_PROBE_JS {
                    script(shape.clone())
                } else if js.starts_with("() =>") {
                    script(json!({ "type": "node", "sharedId": "node-1" }))
                } else {
                    script(json!({ "type": "undefined" }))
                }
            }
            _ => json!({}),
        }
    }

    fn node_responder(req: &Value) -> Value {
        match req["method"].as_str() {
            Some("script.callFunction") => {
                let js = req["params"]["functionDeclaration"].as_str().unwrap_or("");
                if js == FOCUSED_FRAME_CONTEXT_JS {
                    script(json!({"type": "null"}))
                } else if js.starts_with("() =>") {
                    script(json!({"type": "node", "sharedId": "node-1"}))
                } else if js.contains("function(text, replace, useActive)") {
                    script(json!({"type": "string", "value": "ok"}))
                } else {
                    script(json!({"type": "undefined"}))
                }
            }
            _ => json!({}),
        }
    }

    fn connected_node_responder(req: &Value) -> Value {
        if req["method"] == "script.callFunction"
            && req["params"]["functionDeclaration"]
                .as_str()
                .is_some_and(|js| js == super::super::target::IN_DOCUMENT_JS)
        {
            script(json!({"type": "boolean", "value": true}))
        } else {
            node_responder(req)
        }
    }

    fn checkbox_responder(checked: bool) -> impl Fn(&Value) -> Value + Send + 'static {
        move |req: &Value| match req["method"].as_str() {
            Some("script.callFunction") => {
                let js = req["params"]["functionDeclaration"].as_str().unwrap_or("");
                if js.starts_with("() =>") {
                    script(json!({"type": "node", "sharedId": "node-1"}))
                } else if js == ELEMENT_CHECK_CONTROL_JS {
                    script(json!({"type": "node", "sharedId": "checkbox-1"}))
                } else if js == element_check_probe_js() {
                    script(json!({
                        "type": "object",
                        "value": [
                            ["checked", {"type": "boolean", "value": checked}],
                            ["native", {"type": "boolean", "value": true}],
                            ["clickable", {"type": "boolean", "value": false}],
                        ]
                    }))
                } else if js == ELEMENT_CHECKED_JS {
                    script(json!({"type": "boolean", "value": checked}))
                } else {
                    script(json!({"type": "undefined"}))
                }
            }
            _ => json!({}),
        }
    }

    fn assert_all(calls: &Calls, expected: &[&str]) {
        assert_eq!(*calls.lock().unwrap(), expected);
    }

    #[test]
    fn select_values_accept_arrays_strings_and_a_single_value() {
        assert_eq!(select_values(&json!({"values": ["s", "m"]})), ["s", "m"]);
        assert_eq!(select_values(&json!({"values": "s"})), ["s"]);
        assert_eq!(select_values(&json!({"value": "m"})), ["m"]);
        assert!(select_values(&json!({})).is_empty());
    }

    #[test]
    fn relative_upload_paths_become_absolute() {
        let absolute = std::env::temp_dir()
            .join("already")
            .join("absolute.txt")
            .to_string_lossy()
            .into_owned();
        let files = vec!["fixture.txt".to_string(), absolute.clone()];
        let paths = absolute_files(&files);
        assert!(Path::new(&paths[0]).is_absolute(), "{}", paths[0]);
        assert!(paths[0].ends_with("fixture.txt"));
        assert!(Path::new(&files[1]).is_absolute(), "{}", files[1]);
        assert_eq!(paths[1], absolute);
    }

    #[tokio::test]
    async fn insert_text_scripts_the_literal_text_without_key_events() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        let (ff, _tx, calls) = fake_session_with(move |req| {
            if req["method"] == "script.callFunction" {
                log.lock().unwrap().push(req["params"].clone());
                if req["params"]["functionDeclaration"] == FOCUSED_FRAME_CONTEXT_JS {
                    return json!({"type": "success", "result": {"type": "null"}});
                }
            }
            json!({"type": "success", "result": {"type": "string", "value": "ok"}})
        })
        .await;
        let mut state = state_with(ff);

        inserttext(&json!({"text": "a\n\tb"}), &mut state)
            .await
            .unwrap();
        keyboard(
            &json!({"subaction": "insertText", "text": "c\nd"}),
            &mut state,
        )
        .await
        .unwrap();

        assert_eq!(*calls.lock().unwrap(), ["script.callFunction"; 4]);
        let params: Vec<_> = seen
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p["functionDeclaration"] != FOCUSED_FRAME_CONTEXT_JS)
            .cloned()
            .collect();
        assert_eq!(params.len(), 2);
        assert_eq!(
            params[0]["arguments"][0],
            json!({"type": "string", "value": "a\n\tb"})
        );
        assert_eq!(
            params[1]["arguments"][0],
            json!({"type": "string", "value": "c\nd"})
        );
    }

    #[tokio::test]
    async fn key_commands_send_bidi_key_actions_and_return_chrome_shapes() {
        let (ff, _tx, calls) = fake_session_with(node_responder).await;
        let mut state = state_with(ff);

        assert_eq!(
            press(&json!({"key": "Control+a"}), &mut state)
                .await
                .unwrap(),
            json!({"pressed": "Control+a"})
        );
        assert_eq!(
            keydown(&json!({"key": "Tab"}), &mut state).await.unwrap(),
            json!({"keydown": "Tab"})
        );
        assert_eq!(
            keyup(&json!({"key": "Shift"}), &mut state).await.unwrap(),
            json!({"keyup": "Shift"})
        );
        assert_eq!(
            keyboard(&json!({"subaction": "type", "text": "hi"}), &mut state)
                .await
                .unwrap(),
            json!({"typed": "hi"})
        );
        assert_eq!(
            keyboard(
                &json!({"subaction": "insertText", "text": "hi"}),
                &mut state
            )
            .await
            .unwrap(),
            json!({"inserted": true})
        );
        assert_eq!(
            keyboard(&json!({"eventType": "keyUp", "key": "Enter"}), &mut state)
                .await
                .unwrap(),
            json!({"dispatched": "keyUp"})
        );
        assert_eq!(
            input_keyboard(&json!({"type": "keyDown", "key": "a"}), &mut state)
                .await
                .unwrap(),
            json!({"dispatched": "keyDown"})
        );
        assert_eq!(
            inserttext(&json!({"text": ""}), &mut state).await.unwrap(),
            json!({"inserted": true})
        );
        assert_eq!(
            inserttext(&json!({"text": "x"}), &mut state).await.unwrap(),
            json!({"inserted": true})
        );

        assert_all(
            &calls,
            &[
                "input.performActions",
                "input.performActions",
                "input.performActions",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "input.performActions",
                "input.performActions",
                "script.callFunction",
                "script.callFunction",
            ],
        );
    }

    #[tokio::test]
    async fn input_keyboard_dispatches_the_key_a_physical_code_names() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (ff, _tx, _) = fake_session_with(move |request: &Value| {
            captured.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        let mut state = state_with(ff);

        assert_eq!(
            input_keyboard(&json!({"type": "keyDown", "code": "KeyA"}), &mut state)
                .await
                .unwrap(),
            json!({"dispatched": "keyDown"})
        );
        assert_eq!(
            input_keyboard(&json!({"type": "keyUp", "code": "ShiftLeft"}), &mut state)
                .await
                .unwrap(),
            json!({"dispatched": "keyUp"})
        );
        assert_eq!(
            input_keyboard(&json!({"type": "keyDown", "code": "Launch42"}), &mut state)
                .await
                .unwrap_err(),
            "Raw key code Launch42 is not supported on Firefox"
        );
        assert_eq!(
            requests.lock().unwrap()[0]["params"]["actions"],
            json!([{ "type": "key", "id": "key", "actions": [
                { "type": "keyDown", "value": "a" }
            ] }])
        );
        assert_eq!(
            requests.lock().unwrap()[1]["params"]["actions"],
            json!([{ "type": "key", "id": "key", "actions": [
                { "type": "keyUp", "value": "\u{E008}" }
            ] }])
        );
        assert_eq!(requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn key_commands_require_their_arguments() {
        let (ff, _tx, calls) = fake_session().await;
        let mut state = state_with(ff);
        assert_eq!(
            press(&json!({}), &mut state).await.unwrap_err(),
            "Missing 'key' parameter"
        );
        assert_eq!(
            keydown(&json!({}), &mut state).await.unwrap_err(),
            "Missing 'key' parameter"
        );
        assert_eq!(
            keyup(&json!({}), &mut state).await.unwrap_err(),
            "Missing 'key' parameter"
        );
        assert_eq!(
            inserttext(&json!({}), &mut state).await.unwrap_err(),
            "Missing 'text' parameter"
        );
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn fill_and_type_resolve_snapshot_refs() {
        let (mut ff, _tx, calls) = fake_session_with(connected_node_responder).await;
        ff.refs
            .insert(("ctx-active".into(), "node-1".into(), "node-1".into()), 7);
        let mut state = state_with(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(7), "textbox", "Email", None, None);

        assert_eq!(
            fill(&json!({"selector": "@e1", "value": "a@b.c"}), &mut state)
                .await
                .unwrap(),
            json!({"filled": "@e1"})
        );
        assert_eq!(
            type_text(&json!({"selector": "@e1", "text": "ok"}), &mut state)
                .await
                .unwrap(),
            json!({"typed": "ok"})
        );
        assert_all(
            &calls,
            &[
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
            ],
        );
    }

    #[tokio::test]
    async fn typed_keys_stop_once_the_target_document_is_replaced() {
        let (ff, _tx, calls) = fake_session_with(|req| {
            if req["params"]["functionDeclaration"] == SAME_DOCUMENT_JS {
                script(json!({"type": "boolean", "value": false}))
            } else {
                node_responder(req)
            }
        })
        .await;
        let mut state = state_with(ff);

        type_text(&json!({"selector": "#a", "text": "\n"}), &mut state)
            .await
            .unwrap();

        assert!(!calls
            .lock()
            .unwrap()
            .contains(&"input.performActions".to_string()));
    }

    #[tokio::test]
    async fn clear_selectall_and_setvalue_run_the_shared_page_js() {
        let (ff, _tx, calls) = fake_session_with(node_responder).await;
        let mut state = state_with(ff);

        assert_eq!(
            clear(&json!({"selector": "#a"}), &mut state).await.unwrap(),
            json!({"cleared": "#a"})
        );
        assert_eq!(
            selectall(&json!({"selector": "#a"}), &mut state)
                .await
                .unwrap(),
            json!({"selected": "#a"})
        );
        assert_eq!(
            setvalue(&json!({"selector": "#a", "value": "v"}), &mut state)
                .await
                .unwrap(),
            json!({"set": "#a", "value": "v"})
        );
        assert_eq!(calls.lock().unwrap().len(), 6);
    }

    #[tokio::test]
    async fn select_returns_the_requested_values() {
        let (ff, _tx, calls) = fake_session_with(node_responder).await;
        let mut state = state_with(ff);
        let result = select(
            &json!({"selector": "#sizes", "values": ["s", "m"]}),
            &mut state,
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"selected": ["s", "m"]}));
        assert_all(&calls, &["script.callFunction", "script.callFunction"]);
    }

    #[tokio::test]
    async fn check_and_uncheck_click_only_when_the_state_differs() {
        let (ff, _tx, calls) = fake_session_with(checkbox_responder(false)).await;
        let mut state = state_with(ff);
        assert_eq!(
            check(&json!({"selector": "#agree"}), &mut state)
                .await
                .unwrap(),
            json!({"checked": "#agree"})
        );
        assert_all(
            &calls,
            &[
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
            ],
        );

        let (ff, _tx, calls) = fake_session_with(checkbox_responder(true)).await;
        let mut state = state_with(ff);
        assert_eq!(
            check(&json!({"selector": "#agree"}), &mut state)
                .await
                .unwrap(),
            json!({"checked": "#agree"})
        );
        assert_all(
            &calls,
            &[
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
            ],
        );

        let (ff, _tx, calls) = fake_session_with(checkbox_responder(true)).await;
        let mut state = state_with(ff);
        assert_eq!(
            uncheck(&json!({"selector": "#agree"}), &mut state)
                .await
                .unwrap(),
            json!({"unchecked": "#agree"})
        );
        assert_all(
            &calls,
            &[
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
            ],
        );
    }

    #[tokio::test]
    async fn check_retargets_label_to_checkbox_before_pointer_input() {
        let pointer_origins = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let recorded = pointer_origins.clone();
        let (ff, _tx, calls) = fake_session_with(move |req: &Value| match req["method"].as_str() {
            Some("script.callFunction") => {
                let js = req["params"]["functionDeclaration"].as_str().unwrap_or("");
                if js.starts_with("() =>") {
                    script(json!({"type": "node", "sharedId": "label-1"}))
                } else if js == ELEMENT_CHECK_CONTROL_JS {
                    assert_eq!(req["params"]["target"]["sandbox"], json!(SANDBOX));
                    script(json!({"type": "node", "sharedId": "checkbox-1"}))
                } else if js == element_check_probe_js() {
                    assert_eq!(req["params"]["this"]["sharedId"], json!("checkbox-1"));
                    assert_eq!(req["params"]["target"]["sandbox"], json!(SANDBOX));
                    script(json!({
                        "type": "object",
                        "value": [
                            ["checked", {"type": "boolean", "value": false}],
                            ["native", {"type": "boolean", "value": true}],
                            ["clickable", {"type": "boolean", "value": true}],
                        ]
                    }))
                } else if js.contains("blockerAt") {
                    assert_eq!(req["params"]["this"]["sharedId"], json!("checkbox-1"));
                    script(json!({
                        "type": "object",
                        "value": [
                            ["x", {"type": "number", "value": 10}],
                            ["y", {"type": "number", "value": 10}],
                            ["reason", {"type": "null"}],
                            ["blocker", {"type": "null"}],
                        ]
                    }))
                } else if js == ELEMENT_CHECKED_JS {
                    script(json!({"type": "boolean", "value": true}))
                } else {
                    script(json!({"type": "undefined"}))
                }
            }
            Some("input.performActions") => {
                recorded
                    .lock()
                    .unwrap()
                    .push(req["params"]["actions"].clone());
                json!({})
            }
            _ => json!({}),
        })
        .await;
        let mut state = state_with(ff);

        assert_eq!(
            check(&json!({"selector": "#wrap-label"}), &mut state)
                .await
                .unwrap(),
            json!({"checked": "#wrap-label"})
        );
        let actions = pointer_origins.lock().unwrap();
        assert!(!actions.is_empty(), "{:?}", calls.lock().unwrap());
        let serialized = serde_json::to_string(&*actions).unwrap();
        assert!(
            serialized.contains(r#""x":10.0,"y":10.0"#),
            "pointerMove must target checkbox-1's prepared coordinates: {serialized}"
        );
        assert!(!serialized.contains("label-1"), "{serialized}");
    }

    #[tokio::test]
    async fn check_and_uncheck_reject_non_checkable_elements_without_clicking() {
        let non_checkable = |req: &Value| match req["method"].as_str() {
            Some("script.callFunction") => {
                let js = req["params"]["functionDeclaration"].as_str().unwrap_or("");
                if js.starts_with("() =>") {
                    script(json!({"type": "node", "sharedId": "node-1"}))
                } else if js == ELEMENT_CHECK_CONTROL_JS {
                    script(json!({"type": "null"}))
                } else {
                    script(json!({"type": "undefined"}))
                }
            }
            _ => json!({}),
        };

        for (action, run_action) in [("check", true), ("uncheck", false)] {
            let (ff, _tx, calls) = fake_session_with(non_checkable).await;
            let mut state = state_with(ff);
            let cmd = json!({"selector": "#delete"});
            let err = if run_action {
                check(&cmd, &mut state).await.unwrap_err()
            } else {
                uncheck(&cmd, &mut state).await.unwrap_err()
            };
            assert!(
                err.contains("not a checkbox, radio button, or checkable control"),
                "{action}: {err}"
            );
            assert_all(&calls, &["script.callFunction", "script.callFunction"]);
        }
    }

    #[tokio::test]
    async fn policy_allowing_check_cannot_activate_non_checkable_target() {
        let non_checkable = |req: &Value| match req["method"].as_str() {
            Some("script.callFunction") => {
                let js = req["params"]["functionDeclaration"].as_str().unwrap_or("");
                if js.starts_with("() =>") {
                    script(json!({"type": "node", "sharedId": "node-1"}))
                } else if js == ELEMENT_CHECK_CONTROL_JS {
                    script(json!({"type": "null"}))
                } else {
                    script(json!({"type": "undefined"}))
                }
            }
            _ => json!({}),
        };

        for policy_json in [
            r#"{"default":"deny","allow":["check"],"deny":["click"]}"#,
            r#"{"default":"deny","allow":["check"],"confirm":["click"]}"#,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let policy_path = dir.path().join("policy.json");
            std::fs::write(&policy_path, policy_json).unwrap();
            let (ff, _tx, calls) = fake_session_with(non_checkable).await;
            let mut state = state_with(ff);
            state.engine = "firefox".to_string();
            state.policy = Some(
                crate::native::policy::ActionPolicy::load(policy_path.to_str().unwrap()).unwrap(),
            );

            let resp = crate::native::actions::execute_command(
                &json!({"id": "1", "action": "check", "selector": "#delete"}),
                &mut state,
            )
            .await;
            assert_eq!(resp["success"], false, "{policy_json}: {resp}");
            assert!(
                resp["error"]
                    .as_str()
                    .unwrap_or("")
                    .contains("not a checkbox, radio button, or checkable control"),
                "{policy_json}: {resp}"
            );
            assert_all(&calls, &["script.callFunction", "script.callFunction"]);
        }
    }

    #[tokio::test]
    async fn upload_resolves_files_and_reports_how_many_were_set() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.txt");
        let second = dir.path().join("second.txt");
        std::fs::write(&first, "one").unwrap();
        std::fs::write(&second, "two").unwrap();
        let (ff, _tx, calls) =
            fake_session_with(upload_responder(file_input_shape("INPUT", Some("file")))).await;
        let mut state = state_with(ff);
        let result = upload(
            &json!({
                "selector": "input[type=file]",
                "files": [first.to_string_lossy(), second.to_string_lossy()],
            }),
            &mut state,
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            json!({"uploaded": 2, "selector": "input[type=file]"})
        );
        assert_all(
            &calls,
            &[
                "script.callFunction",
                "script.callFunction",
                "input.setFiles",
            ],
        );
    }

    #[tokio::test]
    async fn upload_missing_file_returns_file_not_found_without_bidi_calls() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("ghost.txt");
        let (ff, _tx, calls) =
            fake_session_with(upload_responder(file_input_shape("INPUT", Some("file")))).await;
        let mut state = state_with(ff);
        let error = upload(
            &json!({
                "selector": "#f",
                "files": [missing.to_string_lossy()],
            }),
            &mut state,
        )
        .await
        .unwrap_err();
        assert_eq!(precondition_code(&error), Some("file_not_found"), "{error}");
        assert!(error.contains("does not exist"), "{error}");
        let made = calls.lock().unwrap().clone();
        assert!(made.is_empty(), "{made:?}");
    }

    #[tokio::test]
    async fn upload_non_file_element_returns_not_file_input() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("real.txt");
        std::fs::write(&file, "content").unwrap();
        for (shape, found) in [
            (file_input_shape("DIV", None), "<div>"),
            (file_input_shape("INPUT", Some("text")), "<input type=text>"),
        ] {
            let (ff, _tx, calls) = fake_session_with(upload_responder(shape)).await;
            let mut state = state_with(ff);
            let error = upload(
                &json!({
                    "selector": "#target",
                    "files": [file.to_string_lossy()],
                }),
                &mut state,
            )
            .await
            .unwrap_err();
            assert_eq!(precondition_code(&error), Some("not_file_input"), "{error}");
            assert!(error.contains(found), "{error}");
            assert_all(&calls, &["script.callFunction", "script.callFunction"]);
        }
    }
}
