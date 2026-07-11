//! Persistent session to tab binding.
//!
//! Each daemon session can be bound to a single CDP target (tab). The binding
//! is written to `{session}.target` in the daemon socket directory as a small
//! JSON document and, unlike the other per-session files (`.pid`, `.sock`,
//! `.stream`, ...), it intentionally survives daemon restarts. On the next
//! attach over CDP the daemon re-selects the bound target by `targetId`
//! instead of adopting whatever tab happens to be first in
//! `Target.getTargets` (the most recently active one, which in a shared
//! browser is usually another session's tab).
//!
//! The `pinned` field makes `--pin-tab` sticky for the session: once a
//! session is created with `--pin-tab`, subsequent commands and daemon
//! restarts keep the strict semantics without repeating the flag.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// A persisted session to tab binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabBinding {
    /// CDP target id of the bound tab. Stable across daemon restarts (unlike
    /// `t<N>` ids, which are per-daemon counters).
    #[serde(rename = "targetId")]
    pub target_id: String,
    /// Last known URL of the bound tab, used for actionable error messages
    /// when the tab is gone.
    #[serde(default)]
    pub url: String,
    /// Whether strict pin-tab semantics are enabled for this session.
    #[serde(default)]
    pub pinned: bool,
}

/// Path of the binding file for a session: `{socket_dir}/{session}.target`.
pub fn binding_path(session: &str) -> PathBuf {
    crate::connection::get_socket_dir().join(format!("{}.target", session))
}

/// Load the persisted binding for a session, if any. Unreadable or invalid
/// files are treated as no binding.
pub fn load(session: &str) -> Option<TabBinding> {
    let raw = fs::read_to_string(binding_path(session)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist the binding for a session (write-on-change is the caller's
/// responsibility). Errors are ignored: a failed write degrades to the old
/// re-attach behavior instead of failing the command.
pub fn save(session: &str, binding: &TabBinding) {
    let path = binding_path(session);
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Ok(raw) = serde_json::to_string(binding) {
        let _ = fs::write(path, raw);
    }
}

/// Remove the persisted binding for a session.
pub fn clear(session: &str) {
    let _ = fs::remove_file(binding_path(session));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_socket_dir<F: FnOnce()>(f: F) {
        let guard = crate::test_utils::EnvGuard::new(&[
            "AGENT_BROWSER_SOCKET_DIR",
            "XDG_RUNTIME_DIR",
            "AGENT_BROWSER_NAMESPACE",
        ]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        guard.remove("XDG_RUNTIME_DIR");
        guard.remove("AGENT_BROWSER_NAMESPACE");
        f();
    }

    #[test]
    fn test_binding_round_trip() {
        with_socket_dir(|| {
            let binding = TabBinding {
                target_id: "4A0B7C4E1F2D3A4B5C6D7E8F90A1B2C3".to_string(),
                url: "https://example.com/checkout".to_string(),
                pinned: true,
            };
            save("agent-1", &binding);
            assert_eq!(load("agent-1"), Some(binding));
            clear("agent-1");
            assert_eq!(load("agent-1"), None);
        });
    }

    #[test]
    fn test_load_missing_returns_none() {
        with_socket_dir(|| {
            assert_eq!(load("no-such-session"), None);
        });
    }

    #[test]
    fn test_load_invalid_json_returns_none() {
        with_socket_dir(|| {
            let path = binding_path("bad");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "not json").unwrap();
            assert_eq!(load("bad"), None);
        });
    }

    #[test]
    fn test_bindings_are_per_session() {
        with_socket_dir(|| {
            let a = TabBinding {
                target_id: "AAAA".to_string(),
                url: String::new(),
                pinned: false,
            };
            let b = TabBinding {
                target_id: "BBBB".to_string(),
                url: String::new(),
                pinned: true,
            };
            save("session-a", &a);
            save("session-b", &b);
            assert_eq!(load("session-a"), Some(a));
            assert_eq!(load("session-b"), Some(b));
        });
    }
}
