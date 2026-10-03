//! agent-browser as a library.
//!
//! The command-line program and the daemon drive the browser through one
//! dispatcher, [`execute_command`], which takes the same JSON commands the
//! daemon receives over its socket and returns the same JSON responses. A
//! process that embeds the engine calls it directly with its own
//! [`DaemonState`], built from [`StateOptions`] rather than from the
//! `AGENT_BROWSER_*` environment variables the daemon reads.
//!
//! ```no_run
//! # async fn demo() {
//! use agent_browser::{execute_command, DaemonState, StateOptions};
//! use serde_json::json;
//!
//! let mut state = DaemonState::with_options(StateOptions {
//!     session_id: "embedded".to_string(),
//!     ..StateOptions::default()
//! });
//! let reply = execute_command(
//!     &json!({"id": "1", "action": "navigate", "url": "https://example.com"}),
//!     &mut state,
//! )
//! .await;
//! assert_eq!(reply["success"], true);
//! # }
//! ```

mod ca_bundle;
mod chat;
#[doc(hidden)]
pub mod cli;
mod color;
mod commands;
mod connection;
mod doctor;
mod flags;
mod install;
mod mcp;
mod native;
mod output;
mod plugins;
mod read;
mod skills;
#[cfg(test)]
mod test_utils;
mod upgrade;
mod validation;

pub use native::actions::{execute_command, DaemonState, StateOptions};
pub use native::policy::{ActionPolicy, ConfirmActions};
