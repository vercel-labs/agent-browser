//! Command dispatch for `--engine firefox`.

use serde_json::Value;

use super::{capture, forms, interact, page, query, snapshot, storage, tabs};
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
    "snapshot",
    "diff_snapshot",
    "click",
    "dblclick",
    "hover",
    "focus",
    "tap",
    "swipe",
    "drag",
    "mouse",
    "mousemove",
    "mousedown",
    "mouseup",
    "wheel",
    "input_mouse",
    "input_touch",
    "scroll",
    "scrollintoview",
    "highlight",
    "dispatch",
    "fill",
    "type",
    "press",
    "keydown",
    "keyup",
    "keyboard",
    "input_keyboard",
    "inserttext",
    "clear",
    "selectall",
    "select",
    "check",
    "uncheck",
    "setvalue",
    "upload",
    "gettext",
    "getattribute",
    "innertext",
    "innerhtml",
    "inputvalue",
    "count",
    "boundingbox",
    "styles",
    "isvisible",
    "isenabled",
    "ischecked",
    "find",
    "frame",
    "mainframe",
    "getbyrole",
    "getbytext",
    "getbylabel",
    "getbyplaceholder",
    "getbyalttext",
    "getbytitle",
    "getbytestid",
    "nth",
    "evalhandle",
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

/// The reason Firefox cannot run `action`, when it cannot. Consulted before a launch so an
/// action Firefox cannot run never starts a browser.
pub fn unsupported_error(action: &str) -> Option<String> {
    if CHROME_ONLY_ACTIONS.contains(&action) {
        Some(format!(
            "`{action}` is Chrome-only and is not available with --engine firefox"
        ))
    } else if !SUPPORTED_ACTIONS.contains(&action) {
        Some(format!("`{action}` is not supported on Firefox"))
    } else {
        None
    }
}

/// The `tab_gone` error for a page command once the pinned tab is gone, or `None` when the action
/// may run. Recovery actions mirror Chrome so the caller can list tabs or rebind explicitly.
fn pin_gone_error(action: &str, cmd: &Value, state: &DaemonState) -> Option<String> {
    let (context, last_url) = state.firefox.as_ref()?.contexts.pinned_gone()?;
    let recovery = matches!(
        action,
        "tab_list" | "tab_new" | "tab_switch" | "window_new" | "close" | "deny"
    ) || (action == "tab_close" && cmd.get("tabId").is_some())
        || (action == "dialog" && cmd.get("response").and_then(Value::as_str) == Some("status"));
    (!recovery).then(|| super::super::browser::tab_gone_error(context, last_url))
}

/// Runs one command on the Firefox session.
pub async fn execute(action: &str, cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    if let Some(error) = unsupported_error(action) {
        return Err(error);
    }
    if let Some(error) = pin_gone_error(action, cmd, state) {
        return Err(error);
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
        "dialog" => interact::dialog(cmd, state).await,
        "screenshot" => capture::screenshot(cmd, state).await,
        "pdf" => capture::pdf(cmd, state).await,
        "snapshot" => Box::pin(snapshot::snapshot(cmd, state)).await,
        "diff_snapshot" => Box::pin(snapshot::diff_snapshot(cmd, state)).await,
        "click" => interact::click(cmd, state).await,
        "dblclick" => interact::dblclick(cmd, state).await,
        "hover" => interact::hover(cmd, state).await,
        "focus" => interact::focus(cmd, state).await,
        "tap" => interact::tap(cmd, state).await,
        "swipe" => interact::swipe(cmd, state).await,
        "drag" => interact::drag(cmd, state).await,
        "mouse" => interact::mouse(cmd, state).await,
        "mousemove" => interact::mousemove(cmd, state).await,
        "mousedown" => interact::mousedown(cmd, state).await,
        "mouseup" => interact::mouseup(cmd, state).await,
        "wheel" => interact::wheel(cmd, state).await,
        "input_mouse" => interact::input_mouse(cmd, state).await,
        "input_touch" => interact::input_touch(cmd, state).await,
        "scroll" => interact::scroll(cmd, state).await,
        "scrollintoview" => interact::scrollintoview(cmd, state).await,
        "highlight" => interact::highlight(cmd, state).await,
        "dispatch" => interact::dispatch(cmd, state).await,
        "fill" => forms::fill(cmd, state).await,
        "type" => forms::type_text(cmd, state).await,
        "press" => forms::press(cmd, state).await,
        "keydown" => forms::keydown(cmd, state).await,
        "keyup" => forms::keyup(cmd, state).await,
        "keyboard" => forms::keyboard(cmd, state).await,
        "input_keyboard" => forms::input_keyboard(cmd, state).await,
        "inserttext" => forms::inserttext(cmd, state).await,
        "clear" => forms::clear(cmd, state).await,
        "selectall" => forms::selectall(cmd, state).await,
        "select" => forms::select(cmd, state).await,
        "check" => forms::check(cmd, state).await,
        "uncheck" => forms::uncheck(cmd, state).await,
        "setvalue" => forms::setvalue(cmd, state).await,
        "upload" => forms::upload(cmd, state).await,
        "gettext" => query::gettext(cmd, state).await,
        "getattribute" => query::getattribute(cmd, state).await,
        "innertext" => query::innertext(cmd, state).await,
        "innerhtml" => query::innerhtml(cmd, state).await,
        "inputvalue" => query::inputvalue(cmd, state).await,
        "count" => query::count(cmd, state).await,
        "boundingbox" => query::boundingbox(cmd, state).await,
        "styles" => query::styles(cmd, state).await,
        "isvisible" => query::isvisible(cmd, state).await,
        "isenabled" => query::isenabled(cmd, state).await,
        "ischecked" => query::ischecked(cmd, state).await,
        "find" => query::find(cmd, state).await,
        "frame" => query::frame(cmd, state).await,
        "mainframe" => query::mainframe(state).await,
        "getbyrole" => query::getbyrole(cmd, state).await,
        "getbytext" => query::getbytext(cmd, state).await,
        "getbylabel" => query::getbylabel(cmd, state).await,
        "getbyplaceholder" => query::getbyplaceholder(cmd, state).await,
        "getbyalttext" => query::getbyalttext(cmd, state).await,
        "getbytitle" => query::getbytitle(cmd, state).await,
        "getbytestid" => query::getbytestid(cmd, state).await,
        "nth" => query::nth(cmd, state).await,
        "evalhandle" => query::evalhandle(cmd, state).await,
        "close" => chrome::handle_close(state).await,
        "deny" => chrome::handle_deny(cmd, state).await,
        _ => Err(format!("`{action}` is not an action Firefox can run")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::actions::PendingDialog;
    use crate::native::firefox::session::tests::{fake_session, fake_session_with};
    use serde_json::json;

    fn dialog(context: &str, owner_context: Option<&str>) -> PendingDialog {
        PendingDialog {
            dialog_type: "confirm".into(),
            message: "sure?".into(),
            session_id: Some(context.into()),
            owner_context: owner_context.map(String::from),
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
        let e = execute("vitals", &json!({}), &mut state).await.unwrap_err();
        assert_eq!(e, "`vitals` is not supported on Firefox");
        assert!(state.firefox.is_none());
    }

    #[tokio::test]
    async fn query_actions_are_routed_to_the_query_handlers() {
        let mut state = DaemonState::new();
        for action in [
            "gettext",
            "getattribute",
            "innertext",
            "innerhtml",
            "inputvalue",
            "count",
            "boundingbox",
            "styles",
            "isvisible",
            "isenabled",
            "ischecked",
            "find",
            "frame",
            "nth",
            "getbyrole",
            "getbytext",
            "getbylabel",
            "getbyplaceholder",
            "getbyalttext",
            "getbytitle",
            "getbytestid",
        ] {
            assert!(handles(action), "{action}");
            let error = execute(action, &json!({}), &mut state).await.unwrap_err();
            assert!(
                error.starts_with("Missing '"),
                "{action} did not reach its query handler: {error}"
            );
        }
        let error = execute("mainframe", &json!({}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(error, "Browser not launched");
        let error = execute("evalhandle", &json!({}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(error, "Missing 'script' parameter");
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
        state.pending_dialog = Some(dialog("ctx-active", Some("ctx-active")));

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
    async fn dialog_blocks_only_the_active_tab() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let blocked = |state: &DaemonState, action: &str| {
            chrome::dialog_block_error(action, &json!({}), state)
                .is_some_and(|error| error.contains("confirm dialog is blocking the page"))
        };

        state.pending_dialog = Some(dialog("ctx-active", Some("ctx-active")));
        assert!(blocked(&state, "navigate"));
        assert!(!blocked(&state, "tab_list"));

        state.pending_dialog = Some(dialog("ctx-iframe", Some("ctx-background")));
        assert!(!blocked(&state, "navigate"));

        state.pending_dialog = Some(dialog("ctx-iframe", Some("ctx-active")));
        assert!(blocked(&state, "navigate"));

        state.pending_dialog = Some(PendingDialog {
            dialog_type: "confirm".into(),
            message: "sure?".into(),
            ..Default::default()
        });
        assert!(blocked(&state, "navigate"));
    }
    #[tokio::test]
    async fn dialog_command_targets_the_tab_that_opened_the_prompt() {
        use std::sync::{Arc, Mutex};
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let (mut ff, _tx, _) = fake_session_with(move |request| {
            recorded.lock().unwrap().push(request.clone());
            json!({})
        })
        .await;
        ff.contexts.activate("ctx-active");
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.pending_dialog = Some(dialog("ctx-other", Some("ctx-other")));
        execute("dialog", &json!({"response": "accept"}), &mut state)
            .await
            .unwrap();
        let params = seen
            .lock()
            .unwrap()
            .iter()
            .find(|request| request["method"] == "browsingContext.handleUserPrompt")
            .map(|request| request["params"].clone())
            .expect("the prompt command is sent");
        assert_eq!(params["context"], "ctx-other");
        assert!(state.pending_dialog.is_none());
    }

    #[tokio::test]
    async fn pin_gone_blocks_page_commands_and_allows_recovery_actions() {
        let mut state = DaemonState::new();
        let (mut ff, _tx, calls) = fake_session().await;
        ff.contexts
            .set_url("ctx-active", "https://example.com/timeout");
        ff.set_pin(true);
        ff.contexts.remove("ctx-active");
        state.firefox = Some(ff);

        for (action, cmd) in [
            ("url", json!({})),
            ("evaluate", json!({"script": "1"})),
            ("navigate", json!({"url": "https://example.com"})),
            ("tab_close", json!({})),
            ("dialog", json!({"response": "accept"})),
        ] {
            let err = execute(action, &cmd, &mut state).await.unwrap_err();
            assert!(
                err.starts_with(crate::native::browser::TAB_GONE_PREFIX),
                "{action}: {err}"
            );
        }
        assert!(calls.lock().unwrap().is_empty());

        for (action, cmd) in [
            ("tab_list", json!({})),
            ("tab_new", json!({})),
            ("tab_switch", json!({"tabId": "t1"})),
            ("tab_close", json!({"tabId": "t2"})),
            ("window_new", json!({})),
            ("close", json!({})),
            ("dialog", json!({"response": "status"})),
        ] {
            assert!(
                pin_gone_error(action, &cmd, &state).is_none(),
                "{action} should be allowed while tab_gone"
            );
        }
    }
}
