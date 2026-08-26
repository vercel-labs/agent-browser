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
    #[serde(skip)]
    pub browser_context_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IsolatedTargetBinding {
    target_id: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IsolatedBindingV1 {
    format: String,
    version: u32,
    browser_context_id: String,
    target: IsolatedTargetBinding,
}

const ISOLATED_BINDING_FORMAT: &str = "agent-browser-isolated-context";
const ISOLATED_BINDING_VERSION: u32 = 1;

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
    let value: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("corrupt tab binding file {}: {}", path.display(), e))?;
    if value.get("format").is_some() || value.get("version").is_some() {
        let isolated: IsolatedBindingV1 = serde_json::from_value(value)
            .map_err(|e| format!("corrupt tab binding file {}: {}", path.display(), e))?;
        if isolated.format != ISOLATED_BINDING_FORMAT
            || isolated.version != ISOLATED_BINDING_VERSION
        {
            return Err(format!(
                "unsupported isolated tab binding in {}: format {}, version {}",
                path.display(),
                isolated.format,
                isolated.version
            ));
        }
        return Ok(Some(TabBinding {
            target_id: isolated.target.target_id,
            url: isolated.target.url,
            pinned: isolated.target.pinned,
            browser_context_id: Some(isolated.browser_context_id),
        }));
    }
    let mut binding: TabBinding = serde_json::from_value(value)
        .map_err(|e| format!("corrupt tab binding file {}: {}", path.display(), e))?;
    binding.browser_context_id = None;
    Ok(Some(binding))
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
    let raw = if let Some(browser_context_id) = safe_binding.browser_context_id.clone() {
        serde_json::to_string(&IsolatedBindingV1 {
            format: ISOLATED_BINDING_FORMAT.to_string(),
            version: ISOLATED_BINDING_VERSION,
            browser_context_id,
            target: IsolatedTargetBinding {
                target_id: safe_binding.target_id,
                url: safe_binding.url,
                pinned: safe_binding.pinned,
            },
        })
    } else {
        serde_json::to_string(&safe_binding)
    }
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

pub fn ensure_writable(session: &str) -> Result<(), String> {
    let path = binding_path(session);
    let dir = path
        .parent()
        .ok_or_else(|| format!("invalid tab binding path {}", path.display()))?;
    fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create socket dir {}: {}", dir.display(), e))?;
    let probe = path.with_extension(format!("target.probe.{}", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&probe)
        .map_err(|e| format!("cannot create tab binding probe {}: {}", probe.display(), e))?;
    file.sync_all()
        .map_err(|e| format!("cannot sync tab binding probe {}: {}", probe.display(), e))?;
    drop(file);
    fs::remove_file(&probe)
        .map_err(|e| format!("cannot remove tab binding probe {}: {}", probe.display(), e))
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
                browser_context_id: None,
            };
            save("agent-1", &binding).unwrap();
            assert_eq!(load("agent-1"), Ok(Some(binding)));
            clear("agent-1");
            assert_eq!(load("agent-1"), Ok(None));
        });
    }

    #[test]
    fn test_isolated_binding_round_trip_uses_versioned_shape() {
        with_socket_dir(|| {
            let binding = TabBinding {
                target_id: "AAAA".to_string(),
                url: "https://example.com/account?token=secret".to_string(),
                pinned: true,
                browser_context_id: Some("CONTEXT-A".to_string()),
            };

            save("isolated", &binding).unwrap();

            let raw = fs::read_to_string(binding_path("isolated")).unwrap();
            let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(value["format"], ISOLATED_BINDING_FORMAT);
            assert_eq!(value["version"], ISOLATED_BINDING_VERSION);
            assert_eq!(value["browserContextId"], "CONTEXT-A");
            assert!(value.get("targetId").is_none());
            assert_eq!(value["target"]["targetId"], "AAAA");
            assert_eq!(value["target"]["url"], "https://example.com/account");

            let mut expected = binding;
            expected.url = "https://example.com/account".to_string();
            assert_eq!(load("isolated"), Ok(Some(expected)));
        });
    }

    #[test]
    fn test_old_binding_shape_cannot_parse_isolated_binding() {
        #[derive(Deserialize)]
        struct LegacyBinding {
            #[serde(rename = "targetId")]
            _target_id: String,
        }

        with_socket_dir(|| {
            save(
                "isolated-old-reader",
                &TabBinding {
                    target_id: "AAAA".to_string(),
                    url: String::new(),
                    pinned: false,
                    browser_context_id: Some("CONTEXT-A".to_string()),
                },
            )
            .unwrap();

            let raw = fs::read_to_string(binding_path("isolated-old-reader")).unwrap();
            assert!(
                serde_json::from_str::<LegacyBinding>(&raw).is_err(),
                "pre-isolation readers must fail instead of silently dropping context ownership"
            );
        });
    }

    #[test]
    fn test_load_rejects_unknown_isolated_binding_version() {
        with_socket_dir(|| {
            let path = binding_path("future");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(
                &path,
                r#"{"format":"agent-browser-isolated-context","version":2,"browserContextId":"C","target":{"targetId":"T"}}"#,
            )
            .unwrap();

            let error = load("future").unwrap_err();
            assert!(error.contains("unsupported isolated tab binding"));
        });
    }

    #[test]
    fn test_save_drops_opaque_url_payload() {
        with_socket_dir(|| {
            let binding = TabBinding {
                target_id: "AAAA".to_string(),
                url: "data:text/html,persisted-secret".to_string(),
                pinned: true,
                browser_context_id: None,
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
            browser_context_id: None,
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
                browser_context_id: None,
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
                browser_context_id: None,
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
                browser_context_id: None,
            };
            let b = TabBinding {
                target_id: "BBBB".to_string(),
                url: String::new(),
                pinned: true,
                browser_context_id: None,
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
}
