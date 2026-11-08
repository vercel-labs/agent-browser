//! Command dispatch for `--engine firefox`.

use serde_json::Value;

use super::daemon::session;
use super::{capture, page, storage, tabs};
use crate::native::actions::{self as chrome, DaemonState};

/// Actions that run on Firefox.
pub const SUPPORTED_ACTIONS: &[&str] = &[
    "navigate",
    "back",
    "forward",
    "reload",
    "url",
    "title",
    "content",
    "evaluate",
    "wait",
    "waitforurl",
    "waitforloadstate",
    "waitforfunction",
    "tab_new",
    "tab_list",
    "tab_switch",
    "tab_close",
    "window_new",
    "cookies_get",
    "cookies_set",
    "cookies_clear",
    "storage_get",
    "storage_set",
    "storage_clear",
    "state_save",
    "state_load",
    "dialog",
    "screenshot",
    "pdf",
    "close",
    "confirm",
    "deny",
];

/// Actions that exist only on Chrome and fail on Firefox with a clear reason.
pub const CHROME_ONLY_ACTIONS: &[&str] = &[
    "trace_start",
    "trace_stop",
    "profiler_start",
    "profiler_stop",
    "webmcp_list",
    "webmcp_invoke",
    "webmcp_result",
    "webmcp_cancel",
    "screencast_start",
    "screencast_stop",
    "recording_start",
    "recording_stop",
    "recording_restart",
    "video_start",
    "video_stop",
    "a11y",
    "react_tree",
    "react_inspect",
    "react_renders_start",
    "react_renders_stop",
    "react_suspense",
];

/// True when the Firefox module answers `action` itself, including with a Chrome-only error.
pub fn handles(action: &str) -> bool {
    SUPPORTED_ACTIONS.contains(&action) || CHROME_ONLY_ACTIONS.contains(&action)
}

/// Runs one command on the Firefox session.
pub async fn execute(action: &str, cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if CHROME_ONLY_ACTIONS.contains(&action) {
        return Err(format!(
            "`{action}` is Chrome-only and is not available with --engine firefox"
        ));
    }
    if !SUPPORTED_ACTIONS.contains(&action) {
        return Err(format!("`{action}` is not supported on Firefox"));
    }
    if let Some(error) = chrome::dialog_block_error(action, cmd, state) {
        return Err(error);
    }
    match action {
        "navigate" => page::navigate(cmd, state).await,
        "back" => page::back(cmd, state).await,
        "forward" => page::forward(cmd, state).await,
        "reload" => page::reload(cmd, state).await,
        "url" => page::url(state).await,
        "title" => page::title(state).await,
        "content" => page::content(state).await,
        "evaluate" => page::evaluate(cmd, state).await,
        "wait" => page::wait(cmd, state).await,
        "waitforurl" => page::waitforurl(cmd, state).await,
        "waitforloadstate" => page::waitforloadstate(cmd, state).await,
        "waitforfunction" => page::waitforfunction(cmd, state).await,
        "tab_new" => tabs::tab_new(cmd, state).await,
        "tab_list" => tabs::tab_list(state).await,
        "tab_switch" => tabs::tab_switch(cmd, state).await,
        "tab_close" => tabs::tab_close(cmd, state).await,
        "window_new" => tabs::window_new(cmd, state).await,
        "cookies_get" => storage::cookies_get(cmd, state).await,
        "cookies_set" => storage::cookies_set(cmd, state).await,
        "cookies_clear" => storage::cookies_clear(cmd, state).await,
        "storage_get" => storage::storage_get(cmd, state).await,
        "storage_set" => storage::storage_set(cmd, state).await,
        "storage_clear" => storage::storage_clear(cmd, state).await,
        "state_save" => storage::state_save(cmd, state).await,
        "state_load" => storage::state_load(cmd, state).await,
        "dialog" => handle_dialog(cmd, state).await,
        "screenshot" => capture::screenshot(cmd, state).await,
        "pdf" => capture::pdf(cmd, state).await,
        "close" => chrome::handle_close(state).await,
        "deny" => chrome::handle_deny(cmd, state).await,
        _ => Err(format!("`{action}` is not an action Firefox can run")),
    }
}

/// Reports or resolves the prompt on the active tab. The pending record is dropped before the
/// command is sent, so a prompt that is already gone cannot block every later page command.
async fn handle_dialog(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let response = cmd.get("response").and_then(Value::as_str);
    if response == Some("status") {
        return Ok(match &state.pending_dialog {
            Some(dialog) => {
                let mut result = serde_json::json!({
                    "hasDialog": true,
                    "type": dialog.dialog_type,
                    "message": dialog.message,
                });
                if let Some(prompt) = &dialog.default_prompt {
                    result["defaultPrompt"] = serde_json::json!(prompt);
                }
                result
            }
            None => serde_json::json!({ "hasDialog": false }),
        });
    }
    let accept = response
        .map(|response| response == "accept")
        .or_else(|| cmd.get("accept").and_then(Value::as_bool))
        .unwrap_or(true);
    let recorded = state
        .pending_dialog
        .as_ref()
        .and_then(|dialog| dialog.session_id.clone());
    let ff = session(state)?;
    let context = recorded.unwrap_or_else(|| ff.context.clone());
    let mut params = serde_json::json!({ "context": context, "accept": accept });
    if let Some(text) = cmd.get("promptText").and_then(Value::as_str) {
        params["userText"] = serde_json::json!(text);
    }
    state.pending_dialog = None;
    session(state)?
        .bidi
        .send("browsingContext.handleUserPrompt", params)
        .await?;
    Ok(serde_json::json!({ "handled": true, "accepted": accept }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::actions::PendingDialog;
    use crate::native::firefox::session::tests::fake_session;
    use serde_json::json;

    fn dialog(context: &str) -> PendingDialog {
        PendingDialog {
            dialog_type: "confirm".into(),
            message: "sure?".into(),
            session_id: Some(context.into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn chrome_only_and_unknown_actions_fail_without_touching_the_browser() {
        let mut state = DaemonState::new();
        for action in CHROME_ONLY_ACTIONS {
            let e = execute(action, &json!({}), &mut state).await.unwrap_err();
            assert!(e.contains(action) && e.contains("Chrome-only"), "{e}");
        }
        let e = execute("snapshot", &json!({}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(e, "`snapshot` is not supported on Firefox");
        assert!(state.firefox.is_none());
    }

    #[tokio::test]
    async fn wait_variants_are_routed_to_the_page_handlers() {
        for action in ["waitforurl", "waitforloadstate", "waitforfunction"] {
            assert!(handles(action), "{action}");
        }
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let result = execute(
            "waitforloadstate",
            &json!({"state": "none", "timeout": 50}),
            &mut state,
        )
        .await
        .unwrap();
        assert_eq!(result, json!({ "state": "none" }));
    }

    /// A prompt that is already gone must not leave a record that blocks every later command.
    #[tokio::test]
    async fn a_rejected_prompt_command_still_clears_the_pending_dialog() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let reply = json!({
                    "type": "error",
                    "id": request["id"],
                    "error": "no such alert",
                    "message": "no user prompt is open",
                });
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
        });
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        state.firefox.as_mut().unwrap().bidi = super::super::bidi::BidiClient::connect(&url)
            .await
            .unwrap()
            .into();
        state.pending_dialog = Some(dialog("ctx-active"));

        let error = execute("dialog", &json!({"response": "accept"}), &mut state)
            .await
            .unwrap_err();
        assert!(error.contains("no such alert"), "{error}");
        assert!(
            state.pending_dialog.is_none(),
            "a rejected prompt command must not keep the dialog recorded"
        );
        server.abort();
    }

    #[tokio::test]
    async fn dialog_command_targets_the_tab_that_opened_the_prompt() {
        let mut state = DaemonState::new();
        let (mut ff, _tx, calls) = fake_session().await;
        ff.context = "ctx-active".into();
        state.firefox = Some(ff);
        state.pending_dialog = Some(dialog("ctx-other"));
        execute("dialog", &json!({"response": "accept"}), &mut state)
            .await
            .unwrap();
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "browsingContext.handleUserPrompt"));
        assert!(state.pending_dialog.is_none());
    }

    #[tokio::test]
    async fn dialog_command_reports_and_resolves_pending_prompt() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        state.pending_dialog = Some(dialog("ctx-active"));
        let status = execute("dialog", &json!({"response": "status"}), &mut state)
            .await
            .unwrap();
        assert_eq!(status["type"], "confirm");
        let result = execute("dialog", &json!({"response": "dismiss"}), &mut state)
            .await
            .unwrap();
        assert_eq!(result, json!({"handled": true, "accepted": false}));
        assert!(state.pending_dialog.is_none());
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "browsingContext.handleUserPrompt"));
    }
}
