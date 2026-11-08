//! Firefox's view of the shared daemon state: the live session and the page state tied to the active tab.

use super::session::FirefoxSession;
use crate::native::actions::DaemonState;

/// The launched Firefox session, or the standard not-launched error.
pub fn session(state: &mut DaemonState) -> Result<&mut FirefoxSession, String> {
    state.firefox.as_mut().ok_or("Browser not launched".into())
}

/// Drops element refs and frame scope after the active document is replaced or the active tab
/// changes, and points the active context back at the top-level tab.
pub fn reset_page(state: &mut DaemonState) {
    state.ref_map.invalidate_all_documents();
    state.active_iframe_sessions.clear();
    state.active_frame_id = None;
    if let Some(ff) = state.firefox.as_mut() {
        ff.context = ff.top_context.clone();
        ff.refs.clear();
    }
}

/// Forgets a browsing context that is gone, including any prompt it owned. Returns true when the
/// active tab was among the contexts dropped.
pub fn forget_context(state: &mut DaemonState, context: &str) -> Result<bool, String> {
    let ff = state.firefox.as_mut().ok_or("Browser not launched")?;
    Ok(ff.forget_context(context, &mut state.pending_dialog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::session::tests::fake_session;

    #[tokio::test]
    async fn reset_page_drops_frame_refs_and_keeps_monotonic_node_ids() {
        let (mut ff, _, _) = fake_session().await;
        ff.context = "ctx-frame".into();
        ff.next_node_id = 42;
        ff.refs.insert(("ctx-frame".into(), "node".into()), 17);
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.active_iframe_sessions.insert("frame-session".into());
        state.active_frame_id = Some("frame-id".into());
        state.ref_map.add_with_frame(
            "e1".into(),
            Some(17),
            "button",
            "OK",
            None,
            Some("frame-id"),
        );
        state.ref_map.set_next_ref_num(2);

        reset_page(&mut state);

        assert!(state.ref_map.get("e1").is_none());
        assert_eq!(state.ref_map.next_ref_num(), 2);
        assert!(state.active_iframe_sessions.is_empty());
        assert!(state.active_frame_id.is_none());
        let ff = state.firefox.as_ref().unwrap();
        assert_eq!(ff.context, ff.top_context);
        assert!(ff.refs.is_empty());
        assert_eq!(ff.next_node_id, 42);
    }
}
