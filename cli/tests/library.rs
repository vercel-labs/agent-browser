//! The embeddable library: a host process builds its own state and drives
//! the same command dispatcher the daemon uses.
//!
//! The dispatcher launches a browser before any command that is not on its
//! skip list, so this sends an empty action, which is on it: the test needs no
//! browser and never starts one.

use agent_browser::{execute_command, DaemonState, StateOptions};
use serde_json::json;

#[tokio::test]
async fn embedded_state_dispatches_commands_without_a_daemon() {
    let mut state = DaemonState::with_options(StateOptions {
        session_id: "library-test".to_string(),
        ..StateOptions::default()
    });
    let reply = execute_command(&json!({"id": "1", "action": ""}), &mut state).await;
    assert_eq!(reply["id"], "1");
    assert_eq!(reply["success"], false);
    assert!(reply["error"].as_str().is_some_and(|e| !e.is_empty()));
}
