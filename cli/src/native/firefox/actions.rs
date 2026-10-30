//! Command dispatch for `--engine firefox`.

use serde_json::Value;

use super::session::FirefoxSession;
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

const SAFE_DURING_DIALOG: &[&str] = &[
    "dialog",
    "screenshot",
    "url",
    "title",
    "tab_list",
    "tab_new",
    "tab_switch",
    "tab_close",
    "close",
    "confirm",
    "deny",
];

/// True when the Firefox module answers `action` itself, including with a Chrome-only error.
pub fn handles(action: &str) -> bool {
    SUPPORTED_ACTIONS.contains(&action) || CHROME_ONLY_ACTIONS.contains(&action)
}

/// The launched Firefox session, or the standard not-launched error.
pub fn session(state: &mut DaemonState) -> Result<&mut FirefoxSession, String> {
    state.firefox.as_mut().ok_or("Browser not launched".into())
}

/// Fails when a dialog on the active tab would block `action`; a dialog on a background tab does not.
fn dialog_guard(action: &str, state: &DaemonState) -> Result<(), String> {
    let (Some(dialog), Some(ff)) = (&state.pending_dialog, &state.firefox) else {
        return Ok(());
    };
    let on_active_tab = dialog.session_id.as_deref().is_none_or(|c| c == ff.context);
    if on_active_tab && !SAFE_DURING_DIALOG.contains(&action) {
        return Err(format!(
            "A JavaScript {} dialog is blocking the page: \"{}\". Resolve it with `dialog accept` or `dialog dismiss`, then retry `{}`.",
            dialog.dialog_type, dialog.message, action
        ));
    }
    Ok(())
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
    dialog_guard(action, state)?;
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
        "screenshot" => capture::screenshot(cmd, state).await,
        "pdf" => capture::pdf(cmd, state).await,
        "close" => chrome::handle_close(state).await,
        _ => chrome::handle_deny(cmd, state).await,
    }
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

    #[tokio::test]
    async fn dialog_blocks_only_the_active_context() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        state.pending_dialog = Some(dialog("ctx-active"));
        let e = dialog_guard("navigate", &state).unwrap_err();
        assert!(e.contains("confirm dialog is blocking the page"), "{e}");
        assert!(dialog_guard("tab_list", &state).is_ok());
        state.pending_dialog = Some(dialog("ctx-background"));
        assert!(dialog_guard("navigate", &state).is_ok());
    }
}
