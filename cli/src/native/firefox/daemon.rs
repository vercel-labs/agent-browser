//! Firefox's view of the shared daemon state: the live session and the page state tied to the active tab.

use super::session::FirefoxSession;
use crate::native::actions::DaemonState;

/// The launched Firefox session, or the standard not-launched error.
pub fn session(state: &mut DaemonState) -> Result<&mut FirefoxSession, String> {
    state.firefox.as_mut().ok_or("Browser not launched".into())
}

/// Drops element refs and frame scope after the active document is replaced or the active tab changes.
pub fn reset_page(state: &mut DaemonState) {
    state.ref_map.invalidate_all_documents();
    state.active_iframe_sessions.clear();
    state.active_frame_id = None;
    if let Some(ff) = state.firefox.as_mut() {
        ff.refs.clear();
    }
}

/// Forgets a browsing context that is gone, including any prompt it owned. Returns true when it
/// was the active tab, which is then replaced by the lowest tab id.
pub fn forget_context(state: &mut DaemonState, context: &str) -> Result<bool, String> {
    let ff = state.firefox.as_mut().ok_or("Browser not launched")?;
    Ok(ff.forget_context(context, &mut state.pending_dialog))
}
