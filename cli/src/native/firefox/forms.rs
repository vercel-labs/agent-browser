//! Firefox keyboard and form commands.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::actions::session;
use super::input::{key_chord_actions, key_event_actions, key_text_actions};
use super::session::FirefoxSession;
use super::target::{resolve_target, Target};
use crate::native::actions::DaemonState;
use crate::native::element::ELEMENT_CHECKED_JS;
use crate::native::interaction::{
    CLEAR_INPUT_TYPING_JS, CLEAR_VALUE_JS, CLICK_CHECKBOX_JS, FOCUS_JS, SELECT_ALL_JS,
    SELECT_OPTION_JS,
};

fn require<'a>(cmd: &'a Value, key: &str) -> Result<&'a str, String> {
    cmd.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing '{key}' parameter"))
}

fn node_ref(target: &Target) -> Value {
    json!({ "sharedId": target.shared_id })
}

async fn run(
    ff: &FirefoxSession,
    target: &Target,
    js: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    ff.bidi
        .call_function(&target.context, js, Some(node_ref(target)), args)
        .await
}

async fn perform(ff: &FirefoxSession, context: &str, actions: Value) -> Result<(), String> {
    let params = json!({ "context": context, "actions": actions });
    let _ = ff
        .race_prompt(context, ff.bidi.send("input.performActions", params))
        .await?;
    Ok(())
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
                perform(ff, context, key_text_actions(&ch.to_string())).await?;
                tokio::time::sleep(Duration::from_millis(delay)).await;
            }
            Ok(())
        }
        _ => perform(ff, context, key_text_actions(text)).await,
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

fn upload_files(cmd: &Value) -> Vec<String> {
    cmd.get("files")
        .and_then(Value::as_array)
        .map(|files| {
            files
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .or_else(|| {
            cmd.get("file")
                .and_then(Value::as_str)
                .map(|file| vec![file.to_string()])
        })
        .unwrap_or_default()
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

async fn is_checked(ff: &FirefoxSession, target: &Target) -> Result<bool, String> {
    Ok(run(ff, target, ELEMENT_CHECKED_JS, vec![])
        .await?
        .as_bool()
        .unwrap_or(false))
}

async fn click_control(ff: &FirefoxSession, target: &Target) -> Result<(), String> {
    let _ = ff
        .race_prompt(&target.context, run(ff, target, CLICK_CHECKBOX_JS, vec![]))
        .await?;
    Ok(())
}

/// Focuses the element, clears it through the shared page JS, then types `value` as key actions.
pub async fn fill(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let value = require(cmd, "value")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    run(ff, &target, FOCUS_JS, vec![]).await?;
    run(ff, &target, SELECT_ALL_JS, vec![]).await?;
    if value.is_empty() {
        perform(ff, &target.context, key_chord_actions("Backspace")).await?;
    } else {
        send_text(ff, &target.context, value, None).await?;
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
    run(ff, &target, FOCUS_JS, vec![]).await?;
    if clear {
        run(ff, &target, CLEAR_INPUT_TYPING_JS, vec![]).await?;
    }
    send_text(ff, &target.context, text, delay).await?;
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
        Some("insertText") => {
            let text = require(cmd, "text")?;
            let ff = session(state)?;
            send_text(ff, &ff.context, text, None).await?;
            Ok(json!({ "inserted": true }))
        }
        _ => {
            let event_type = cmd
                .get("eventType")
                .and_then(Value::as_str)
                .unwrap_or("keyDown");
            let key = cmd
                .get("key")
                .and_then(Value::as_str)
                .or_else(|| cmd.get("text").and_then(Value::as_str))
                .unwrap_or("");
            let ff = session(state)?;
            perform(ff, &ff.context, key_event_actions(event_type, key)).await?;
            Ok(json!({ "dispatched": event_type }))
        }
    }
}

/// Sends one raw key event from the `input_keyboard` options.
pub async fn input_keyboard(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let event_type = cmd.get("type").and_then(Value::as_str).unwrap_or("keyDown");
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
    send_text(ff, &ff.context, text, None).await?;
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
    let result = run(ff, &target, SELECT_OPTION_JS, vec![json!(values)]).await?;
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
    if !is_checked(ff, &target).await? {
        click_control(ff, &target).await?;
    }
    Ok(json!({ "checked": selector }))
}

/// Unchecks a checkbox or radio only when it is checked.
pub async fn uncheck(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    if is_checked(ff, &target).await? {
        click_control(ff, &target).await?;
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
        "element": node_ref(&target),
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
    use crate::native::firefox::session::tests::{fake_session, fake_session_with, Calls};

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
                if js.starts_with("() =>") {
                    script(json!({"type": "node", "sharedId": "node-1"}))
                } else {
                    script(json!({"type": "undefined"}))
                }
            }
            _ => json!({}),
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
        let files = vec![
            "fixture.txt".to_string(),
            "/already/absolute.txt".to_string(),
        ];
        let paths = absolute_files(&files);
        assert!(Path::new(&paths[0]).is_absolute(), "{}", paths[0]);
        assert!(paths[0].ends_with("fixture.txt"));
        assert_eq!(paths[1], "/already/absolute.txt");
    }

    #[tokio::test]
    async fn key_commands_send_bidi_key_actions_and_return_chrome_shapes() {
        let (ff, _tx, calls) = fake_session().await;
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
            inserttext(&json!({"text": "x"}), &mut state).await.unwrap(),
            json!({"inserted": true})
        );

        assert_eq!(calls.lock().unwrap().len(), 8);
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .all(|method| method == "input.performActions"));
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
    async fn fill_focuses_selects_all_then_types() {
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
                "input.performActions",
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

    #[tokio::test]
    async fn upload_resolves_files_and_reports_how_many_were_set() {
        let (ff, _tx, calls) = fake_session_with(node_responder).await;
        let mut state = state_with(ff);
        let result = upload(
            &json!({"selector": "input[type=file]", "files": ["a.txt", "/b.txt"]}),
            &mut state,
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            json!({"uploaded": 2, "selector": "input[type=file]"})
        );
        assert_all(&calls, &["script.callFunction", "input.setFiles"]);
    }
}
