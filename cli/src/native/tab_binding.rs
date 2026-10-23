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
//! restarts keep the strict semantics without repeating the flag. Because a
//! lost or corrupt binding silently drops that safety boundary, writes are
//! atomic (temp file + rename), owner-only, and fsynced, and both `save` and
//! `load` report failures instead of swallowing them.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// A persisted session to tab binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabBinding {
    /// CDP target id of the bound tab. Stable across daemon restarts (unlike
    /// `t<N>` ids, which are per-daemon counters).
    #[serde(rename = "targetId")]
    pub target_id: String,
    /// Last known URL of the bound tab, used for actionable error messages
    /// when the tab is gone. Sanitized before persistence: credentials,
    /// query, and fragment are stripped (see [`sanitize_url`]).
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

/// Reduce a URL to safe diagnostic information before it is persisted or
/// included in a `tab_gone` error. HTTP and HTTPS retain their origin and
/// path while credentials, query parameters, and fragments are stripped.
/// `about:blank` is retained exactly. Other schemes and malformed URLs are
/// dropped because their opaque payloads may contain secrets.
pub fn sanitize_url(raw: &str) -> String {
    if raw == "about:blank" {
        return raw.to_string();
    }

    match url::Url::parse(raw) {
        Ok(mut parsed) if matches!(parsed.scheme(), "http" | "https") => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        _ => String::new(),
    }
}

/// Load the persisted binding for a session. `Ok(None)` means no binding
/// exists; `Err` means a binding file is present but unreadable or corrupt.
/// Callers must not treat `Err` as a first-time session: a corrupt file may
/// have carried `pinned: true`, and silently dropping it would remove the
/// strict isolation boundary.
pub fn load(session: &str) -> Result<Option<TabBinding>, String> {
    let path = binding_path(session);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "cannot read tab binding file {}: {}",
                path.display(),
                e
            ))
        }
    };
    match serde_json::from_str(&raw) {
        Ok(binding) => Ok(Some(binding)),
        Err(e) => Err(format!(
            "corrupt tab binding file {}: {}",
            path.display(),
            e
        )),
    }
}

/// Persist the binding for a session (write-on-change is the caller's
/// responsibility). The write is atomic: the JSON is serialized first, then
/// written to an owner-only temp file in the same directory, fsynced, and
/// renamed over the destination, so a crash never leaves a truncated or
/// half-written binding. Errors are returned so callers can retry on the
/// next command instead of silently losing the binding.
pub fn save(session: &str, binding: &TabBinding) -> Result<(), String> {
    let path = binding_path(session);
    let mut safe_binding = binding.clone();
    safe_binding.url = sanitize_url(&safe_binding.url);
    let raw = serde_json::to_string(&safe_binding)
        .map_err(|e| format!("cannot serialize tab binding: {}", e))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create socket dir {}: {}", dir.display(), e))?;
    }
    let tmp = path.with_extension(format!("target.tmp.{}", std::process::id()));
    let write_result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp)
            .map_err(|e| format!("cannot create tab binding file {}: {}", tmp.display(), e))?;
        file.write_all(raw.as_bytes())
            .map_err(|e| format!("cannot write tab binding file {}: {}", tmp.display(), e))?;
        file.sync_all()
            .map_err(|e| format!("cannot sync tab binding file {}: {}", tmp.display(), e))?;
        drop(file);
        fs::rename(&tmp, &path).map_err(|e| {
            format!(
                "cannot rename tab binding file {} to {}: {}",
                tmp.display(),
                path.display(),
                e
            )
        })
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    write_result
}

/// Name of another live session that holds a `--pin-tab` pin on `target_id`.
///
/// Read-only scan of `{socket_dir}/*.target`. A pin counts only while the
/// owning session's daemon is alive (`<session>.pid` parses and the process
/// exists), so pins left behind by a dead daemon do not lock the tab.
pub fn pinned_owner(target_id: &str, own_session: &str) -> Option<String> {
    let dir = crate::connection::get_socket_dir();
    let mut owners: Vec<String> = fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".target").map(str::to_string)
        })
        .filter(|session| session != own_session)
        .filter(|session| {
            matches!(
                load(session),
                Ok(Some(b)) if b.pinned && b.target_id.eq_ignore_ascii_case(target_id)
            )
        })
        .filter(|session| {
            fs::read_to_string(dir.join(format!("{}.pid", session)))
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok())
                .is_some_and(crate::connection::is_pid_alive)
        })
        .collect();
    owners.sort();
    owners.into_iter().next()
}

/// Release the pin on a session's binding while keeping its target and URL,
/// so other sessions may take the tab once the owner has explicitly closed.
/// Returns the updated binding when a pin was released.
pub fn release_pin(session: &str) -> Result<Option<TabBinding>, String> {
    match load(session)? {
        Some(mut binding) if binding.pinned => {
            binding.pinned = false;
            save(session, &binding)?;
            Ok(Some(binding))
        }
        _ => Ok(None),
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
            save("agent-1", &binding).unwrap();
            assert_eq!(load("agent-1"), Ok(Some(binding)));
            clear("agent-1");
            assert_eq!(load("agent-1"), Ok(None));
        });
    }

    #[test]
    fn test_save_drops_opaque_url_payload() {
        with_socket_dir(|| {
            let binding = TabBinding {
                target_id: "AAAA".to_string(),
                url: "data:text/html,persisted-secret".to_string(),
                pinned: true,
            };

            save("opaque", &binding).unwrap();

            let saved = load("opaque").unwrap().unwrap();
            assert_eq!(saved.target_id, binding.target_id);
            assert!(saved.pinned);
            assert_eq!(saved.url, "");
            assert!(!fs::read_to_string(binding_path("opaque"))
                .unwrap()
                .contains("persisted-secret"));
        });
    }

    #[test]
    fn test_load_missing_returns_none() {
        with_socket_dir(|| {
            assert_eq!(load("no-such-session"), Ok(None));
        });
    }

    #[test]
    fn test_load_invalid_json_returns_error() {
        with_socket_dir(|| {
            let path = binding_path("bad");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "not json").unwrap();
            let err = load("bad").unwrap_err();
            assert!(err.contains("corrupt"), "unexpected error: {}", err);
        });
    }

    #[test]
    fn test_load_truncated_json_returns_error() {
        with_socket_dir(|| {
            let path = binding_path("truncated");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            // A partial write of a valid document (e.g. a crash mid-write
            // with a non-atomic writer).
            fs::write(&path, "{\"targetId\":\"AAAA\",\"pin").unwrap();
            assert!(load("truncated").is_err());
        });
    }

    #[test]
    fn test_save_unwritable_dir_returns_error() {
        let guard = crate::test_utils::EnvGuard::new(&[
            "AGENT_BROWSER_SOCKET_DIR",
            "XDG_RUNTIME_DIR",
            "AGENT_BROWSER_NAMESPACE",
        ]);
        let dir = tempfile::tempdir().unwrap();
        // Point the socket dir below a regular file so create_dir_all fails.
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, "x").unwrap();
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            blocker.join("sub").to_str().unwrap(),
        );
        guard.remove("XDG_RUNTIME_DIR");
        guard.remove("AGENT_BROWSER_NAMESPACE");
        let binding = TabBinding {
            target_id: "AAAA".to_string(),
            url: String::new(),
            pinned: true,
        };
        assert!(save("blocked", &binding).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn test_save_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        with_socket_dir(|| {
            let binding = TabBinding {
                target_id: "AAAA".to_string(),
                url: String::new(),
                pinned: false,
            };
            save("perms", &binding).unwrap();
            let mode = fs::metadata(binding_path("perms"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        });
    }

    #[test]
    fn test_save_leaves_no_temp_file() {
        with_socket_dir(|| {
            let binding = TabBinding {
                target_id: "AAAA".to_string(),
                url: String::new(),
                pinned: false,
            };
            save("tmpcheck", &binding).unwrap();
            let dir = binding_path("tmpcheck").parent().unwrap().to_path_buf();
            let leftovers: Vec<_> = fs::read_dir(dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
                .collect();
            assert!(leftovers.is_empty(), "temp files left: {:?}", leftovers);
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
            save("session-a", &a).unwrap();
            save("session-b", &b).unwrap();
            assert_eq!(load("session-a"), Ok(Some(a)));
            assert_eq!(load("session-b"), Ok(Some(b)));
        });
    }

    #[test]
    fn test_sanitize_url_strips_sensitive_components() {
        assert_eq!(
            sanitize_url("https://user:secret@example.com/reset?token=abc123#code=xyz"),
            "https://example.com/reset"
        );
        assert_eq!(
            sanitize_url("https://example.com/cb?code=4/0AX4XfWh&state=s"),
            "https://example.com/cb"
        );
        assert_eq!(
            sanitize_url("https://example.com/checkout"),
            "https://example.com/checkout"
        );
        assert_eq!(
            sanitize_url("https://example.com:8443/checkout?token=x#receipt"),
            "https://example.com:8443/checkout"
        );
        assert_eq!(sanitize_url("about:blank"), "about:blank");
        assert_eq!(sanitize_url("ABOUT:blank"), "");
        assert_eq!(sanitize_url("about:blank#secret"), "");
        assert_eq!(sanitize_url("about:config"), "");
        assert_eq!(sanitize_url("data:text/html,secret"), "");
        assert_eq!(sanitize_url("blob:https://example.com/secret"), "");
        assert_eq!(sanitize_url("javascript:secret()"), "");
        assert_eq!(sanitize_url("file:///tmp/secret"), "");
        assert_eq!(sanitize_url("not a url"), "");
        assert_eq!(sanitize_url(""), "");
    }

    fn write_pin(session: &str, target: &str, pinned: bool, pid: Option<u32>) {
        save(
            session,
            &TabBinding {
                target_id: target.to_string(),
                url: String::new(),
                pinned,
            },
        )
        .unwrap();
        if let Some(pid) = pid {
            fs::write(
                crate::connection::get_socket_dir().join(format!("{}.pid", session)),
                pid.to_string(),
            )
            .unwrap();
        }
    }

    #[test]
    fn test_pinned_owner_alive_pin_locks() {
        with_socket_dir(|| {
            write_pin("a", "TARGET1", true, Some(std::process::id()));
            assert_eq!(pinned_owner("TARGET1", "b"), Some("a".to_string()));
            assert_eq!(pinned_owner("OTHER", "b"), None);
        });
    }

    #[test]
    fn test_pinned_owner_unpinned_is_free() {
        with_socket_dir(|| {
            write_pin("a", "TARGET1", false, Some(std::process::id()));
            assert_eq!(pinned_owner("TARGET1", "b"), None);
        });
    }

    #[test]
    fn test_pinned_owner_ignores_own_session() {
        with_socket_dir(|| {
            write_pin("a", "TARGET1", true, Some(std::process::id()));
            assert_eq!(pinned_owner("TARGET1", "a"), None);
        });
    }

    #[test]
    fn test_pinned_owner_dead_or_missing_pid_is_free() {
        with_socket_dir(|| {
            write_pin("nopid", "TARGET1", true, None);
            assert_eq!(pinned_owner("TARGET1", "b"), None);
            let mut child = std::process::Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            write_pin("dead", "TARGET1", true, Some(pid));
            assert_eq!(pinned_owner("TARGET1", "b"), None);
        });
    }

    #[test]
    fn test_release_pin_unlocks_even_with_live_pid() {
        with_socket_dir(|| {
            write_pin("a", "TARGET1", true, Some(std::process::id()));
            assert_eq!(pinned_owner("TARGET1", "b"), Some("a".to_string()));
            let released = release_pin("a").unwrap().unwrap();
            assert!(!released.pinned);
            assert_eq!(released.target_id, "TARGET1");
            assert_eq!(pinned_owner("TARGET1", "b"), None);
            assert_eq!(load("a").unwrap().unwrap().pinned, false);
            assert_eq!(release_pin("a"), Ok(None));
            assert_eq!(release_pin("missing"), Ok(None));
        });
    }

    #[test]
    fn test_release_pin_corrupt_file_errors_and_is_untouched() {
        with_socket_dir(|| {
            let path = binding_path("bad");
            fs::write(&path, "{not json").unwrap();
            assert!(release_pin("bad").is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
        });
    }
}
