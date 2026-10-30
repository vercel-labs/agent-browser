use serde_json::Value;

use super::session::dialog_blocks_tab;
use super::session::FirefoxSession;
use super::{capture, emulation, forms, interact, network, page, query, snapshot, storage, tabs};
use crate::native::actions::{self as chrome, DaemonState};

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
    "deny",
];

/// The launched Firefox session, or the standard not-launched error.
pub fn session(state: &mut DaemonState) -> Result<&mut FirefoxSession, String> {
    state.firefox.as_mut().ok_or("Browser not launched".into())
}

/// Fails when a dialog on the active tab would block `action`; a dialog on a background tab does not.
fn dialog_guard(action: &str, state: &DaemonState) -> Result<(), String> {
    let (Some(dialog), Some(ff)) = (&state.pending_dialog, &state.firefox) else {
        return Ok(());
    };
    if dialog_blocks_tab(dialog.owner_context.as_deref(), &ff.top_context)
        && !SAFE_DURING_DIALOG.contains(&action)
    {
        return Err(format!(
            "A JavaScript {} dialog is blocking the page: \"{}\". Resolve it with `dialog accept` or `dialog dismiss`, then retry `{}`.",
            dialog.dialog_type, dialog.message, action
        ));
    }
    Ok(())
}

macro_rules! firefox_actions {
    ($action:ident, $cmd:ident, $state:ident; $($($name:literal)|+ => $handler:expr),+ $(,)?) => {
        /// True when Firefox handles the action, including a Chrome-only error.
        pub fn handles(action: &str) -> bool {
            matches!(action, $($($name)|+)|+) || CHROME_ONLY_ACTIONS.contains(&action)
        }

        /// Runs a Firefox command after capability and dialog checks.
        pub async fn execute(
            $action: &str,
            $cmd: &Value,
            $state: &mut DaemonState,
        ) -> Result<Value, String> {
            if CHROME_ONLY_ACTIONS.contains(&$action) {
                return Err(format!(
                    "`{}` is Chrome-only and is not available with --engine firefox",
                    $action
                ));
            }
            match $action {
                $($($name)|+ => {
                    dialog_guard($action, $state)?;
                    $handler
                },)+
                _ => Err(format!("`{}` is not supported on Firefox", $action)),
            }
        }
    };
}

firefox_actions! { action, cmd, state;
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
    "dialog" => interact::dialog(cmd, state).await,
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
    "route" => network::route(cmd, state).await,
    "unroute" => network::unroute(cmd, state).await,
    "headers" => network::headers(cmd, state).await,
    "credentials" => network::credentials(cmd, state).await,
    "requests" => chrome::handle_requests(cmd, state).await,
    "request_detail" => network::request_detail(cmd, state).await,
    "responsebody" => network::responsebody(cmd, state).await,
    "har_start" => network::har_start(cmd, state).await,
    "har_stop" => chrome::handle_har_stop(cmd, state).await,
    "console" => chrome::handle_console(cmd, state).await,
    "errors" => chrome::handle_errors(cmd, state).await,
    "download" => network::download(cmd, state).await,
    "waitfordownload" => network::waitfordownload(cmd, state).await,
    "viewport" => emulation::set_viewport(cmd, state).await,
    "device" => emulation::set_device(cmd, state).await,
    "useragent" | "user_agent" => emulation::set_user_agent(cmd, state).await,
    "locale" => emulation::set_locale(cmd, state).await,
    "timezone" => emulation::set_timezone(cmd, state).await,
    "geolocation" => emulation::set_geolocation(cmd, state).await,
    "permissions" => emulation::set_permissions(cmd, state).await,
    "offline" => emulation::set_offline(cmd, state).await,
    "set_media" | "emulatemedia" => emulation::set_media(cmd, state).await,
    "close" => chrome::handle_close(state).await,
    "deny" => chrome::handle_deny(cmd, state).await,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::actions::PendingDialog;
    use crate::native::bidi::session::tests::fake_session;
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
    async fn dialog_blocks_only_the_active_tab() {
        let mut state = DaemonState::new();
        let (mut ff, _tx, calls) = fake_session().await;
        ff.tab_ids.insert("ctx-background".to_string(), 2);
        state.firefox = Some(ff);
        state.pending_dialog = Some(dialog("ctx-active", Some("ctx-active")));
        let e = execute("navigate", &json!({}), &mut state)
            .await
            .unwrap_err();
        assert!(e.contains("confirm dialog is blocking the page"), "{e}");
        assert_eq!(
            execute("trace_start", &json!({}), &mut state)
                .await
                .unwrap_err(),
            "`trace_start` is Chrome-only and is not available with --engine firefox"
        );
        assert_eq!(
            execute("unknown_action", &json!({}), &mut state)
                .await
                .unwrap_err(),
            "`unknown_action` is not supported on Firefox"
        );
        assert!(calls.lock().unwrap().is_empty());
        assert!(dialog_guard("tab_list", &state).is_ok());
        state.pending_dialog = Some(dialog("ctx-iframe", Some("ctx-background")));
        assert!(dialog_guard("navigate", &state).is_ok());
        state.pending_dialog = Some(dialog("ctx-iframe", Some("ctx-active")));
        assert!(dialog_guard("navigate", &state).is_err());
        state.pending_dialog = Some(dialog("ctx-iframe", None));
        assert!(dialog_guard("navigate", &state).is_err());
        state.pending_dialog = Some(dialog("ctx-background", Some("ctx-background")));
        assert!(dialog_guard("navigate", &state).is_ok());
    }

    #[tokio::test]
    async fn user_agent_aliases_dispatch_and_update_session() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        for action in ["useragent", "user_agent"] {
            let cmd = json!({"userAgent": action});
            assert_eq!(execute(action, &cmd, &mut state).await.unwrap(), cmd);
            assert_eq!(
                state
                    .firefox
                    .as_ref()
                    .unwrap()
                    .emulation
                    .read()
                    .unwrap()
                    .user_agent
                    .as_deref(),
                Some(action)
            );
        }
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "emulation.setUserAgentOverride",
                "emulation.setUserAgentOverride"
            ]
        );
    }
}
