use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::actions::session;
use super::input::{key_chord_actions, key_event_actions, key_text_actions};
use super::interact::perform;
use super::session::FirefoxSession;
use super::target::{resolve_target, Target};
use crate::native::actions::{upload_files, DaemonState};
use crate::native::element::ELEMENT_CHECKED_JS;
use crate::native::interaction::{
    CLEAR_INPUT_JS, CLEAR_VALUE_JS, CLICK_CHECKBOX_JS, FOCUS_JS, SELECT_ALL_JS, SELECT_OPTION_JS,
};

const LITERAL_EDIT_JS: &str = r#"function(text, replace, useActive) {
    let el = useActive ? document.activeElement : this;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    if (!el || !el.isConnected || el.matches(':disabled') || el.readOnly) return 'unsupported';
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
    if (!(el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement)) return 'unsupported';
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

const FOCUSED_FRAME_CONTEXT_JS: &str = r#"() => {
    let el = document.activeElement;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    return el && el.matches('iframe,frame') ? el.contentWindow : null;
}"#;

pub(super) fn require<'a>(cmd: &'a Value, key: &str) -> Result<&'a str, String> {
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

async fn literal_edit(
    ff: &FirefoxSession,
    context: &str,
    target: Option<&Target>,
    text: &str,
    replace: bool,
) -> Result<(), String> {
    let context = if target.is_none() {
        let mut focused = ff.top_context.clone();
        let mut visited = HashSet::new();
        while visited.insert(focused.clone()) {
            let child = ff
                .bidi
                .call_function_in_sandbox(
                    &focused,
                    "agent-browser",
                    FOCUSED_FRAME_CONTEXT_JS,
                    vec![],
                )
                .await?;
            let Some(child) = child.as_str() else {
                break;
            };
            focused = child.to_string();
        }
        if focused == ff.top_context {
            context.to_string()
        } else {
            focused
        }
    } else {
        context.to_string()
    };
    let result = ff
        .race_prompt(
            &context,
            ff.bidi.call_function(
                &context,
                LITERAL_EDIT_JS,
                target.map(Target::node_ref),
                vec![json!(text), json!(replace), json!(target.is_none())],
            ),
        )
        .await?;
    let Some(result) = result else {
        return Ok(());
    };
    match result.as_str() {
        Some("ok" | "canceled") => Ok(()),
        _ => Err("Text input is not supported for the focused element".to_string()),
    }
}

async fn send_text(
    ff: &FirefoxSession,
    context: &str,
    text: &str,
    delay_ms: Option<u64>,
) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    match delay_ms {
        Some(delay) if delay > 0 => {
            for ch in text.chars() {
                if perform(ff, context, key_text_actions(&ch.to_string())).await? {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            Ok(())
        }
        _ => perform(ff, context, key_text_actions(text))
            .await
            .map(|_| ()),
    }
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
    if run(ff, &target, FOCUS_JS, vec![]).await?.is_some() {
        literal_edit(ff, &target.context, Some(&target), value, true).await?;
    }
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
    if run(ff, &target, FOCUS_JS, vec![]).await?.is_some()
        && (!clear || run(ff, &target, CLEAR_INPUT_JS, vec![]).await?.is_some())
    {
        send_text(ff, &target.context, text, delay).await?;
    }
    Ok(json!({ "typed": text }))
}

/// Presses a key or chord such as `Control+a` on the active tab.
pub async fn press(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let key = require(cmd, "key")?;
    let ff = session(state)?;
    perform(ff, &ff.context, key_chord_actions(key)).await?;
    Ok(json!({ "pressed": key }))
}

/// Sends one keyDown event for `key`.
pub async fn keydown(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let key = require(cmd, "key")?;
    let ff = session(state)?;
    perform(ff, &ff.context, key_event_actions("keyDown", key)).await?;
    Ok(json!({ "keydown": key }))
}

/// Sends one keyUp event for `key`.
pub async fn keyup(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let key = require(cmd, "key")?;
    let ff = session(state)?;
    perform(ff, &ff.context, key_event_actions("keyUp", key)).await?;
    Ok(json!({ "keyup": key }))
}

/// Handles the `keyboard` options: typing text, inserting text or one key event.
pub async fn keyboard(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    match cmd.get("subaction").and_then(Value::as_str) {
        Some("type") => {
            let text = require(cmd, "text")?;
            let ff = session(state)?;
            send_text(ff, &ff.context, text, None).await?;
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
        literal_edit(ff, &ff.context, None, text, false).await?;
        return Ok(json!({ "dispatched": event_type }));
    }
    let key = cmd
        .get("key")
        .and_then(Value::as_str)
        .or_else(|| cmd.get("text").and_then(Value::as_str))
        .unwrap_or("");
    let ff = session(state)?;
    perform(ff, &ff.context, key_event_actions(event_type, key)).await?;
    Ok(json!({ "dispatched": event_type }))
}

/// Types `text` into the active tab without targeting an element.
pub async fn inserttext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let text = require(cmd, "text")?;
    let ff = session(state)?;
    if !text.is_empty() {
        literal_edit(ff, &ff.context, None, text, false).await?;
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

/// Checks a checkbox or radio unless it is already checked.
pub async fn check(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    if run(ff, &target, ELEMENT_CHECKED_JS, vec![]).await? == Some(json!(false)) {
        run(ff, &target, CLICK_CHECKBOX_JS, vec![]).await?;
    }
    Ok(json!({ "checked": selector }))
}

/// Unchecks a checkbox or radio only when it is checked.
pub async fn uncheck(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    if run(ff, &target, ELEMENT_CHECKED_JS, vec![]).await? == Some(json!(true)) {
        run(ff, &target, CLICK_CHECKBOX_JS, vec![]).await?;
    }
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
    let selector = require(cmd, "selector")?;
    let files = upload_files(cmd);
    let paths = absolute_files(&files);
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let params = json!({
        "context": target.context,
        "element": target.node_ref(),
        "files": paths,
    });
    let _ = ff
        .race_prompt(&target.context, ff.bidi.send("input.setFiles", params))
        .await?;
    Ok(json!({ "uploaded": files.len(), "selector": selector }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::session::tests::{fake_session, fake_session_with, Calls};

    fn state_with(ff: FirefoxSession) -> DaemonState {
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
    }

    fn script(remote: Value) -> Value {
        json!({ "type": "success", "result": remote })
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
                .is_some_and(|js| {
                    js == crate::native::element::IS_CONNECTED_JS
                        || js.contains("ownerDocument === document")
                })
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
                } else if js.contains("aria-checked") {
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
    async fn fill_focuses_then_replaces_with_literal_text() {
        let (ff, _tx, calls) = fake_session_with(node_responder).await;
        let mut state = state_with(ff);
        let result = fill(&json!({"selector": "#email", "value": "a@b.c"}), &mut state)
            .await
            .unwrap();
        assert_eq!(result, json!({"filled": "#email"}));
        assert_all(
            &calls,
            &[
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
            ],
        );
    }

    #[tokio::test]
    async fn type_with_clear_runs_the_shared_clear_js_before_typing() {
        let (ff, _tx, calls) = fake_session_with(node_responder).await;
        let mut state = state_with(ff);
        let result = type_text(
            &json!({"selector": "input", "text": "ok", "clear": true}),
            &mut state,
        )
        .await
        .unwrap();
        assert_eq!(result, json!({"typed": "ok"}));
        assert_all(
            &calls,
            &[
                "script.callFunction",
                "script.callFunction",
                "script.callFunction",
                "input.performActions",
            ],
        );
    }

    #[tokio::test]
    async fn fill_and_type_resolve_snapshot_refs() {
        let (mut ff, _tx, calls) = fake_session_with(connected_node_responder).await;
        ff.refs.insert(("ctx-active".into(), "node-1".into()), 7);
        ff.documents.insert("ctx-active".into(), "node-1".into());
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
                "input.performActions",
            ],
        );
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
        assert_eq!(calls.lock().unwrap().len(), 3);

        let (ff, _tx, calls) = fake_session_with(checkbox_responder(true)).await;
        let mut state = state_with(ff);
        assert_eq!(
            check(&json!({"selector": "#agree"}), &mut state)
                .await
                .unwrap(),
            json!({"checked": "#agree"})
        );
        assert_all(&calls, &["script.callFunction", "script.callFunction"]);

        let (ff, _tx, calls) = fake_session_with(checkbox_responder(true)).await;
        let mut state = state_with(ff);
        assert_eq!(
            uncheck(&json!({"selector": "#agree"}), &mut state)
                .await
                .unwrap(),
            json!({"unchecked": "#agree"})
        );
        assert_eq!(calls.lock().unwrap().len(), 3);
    }
}
