//! Persistent session allowlist for `--allowed-domains`.
//!
//! The allowlist used to live only in the running daemon's memory, so anything
//! that replaced the daemon process (idle expiry, an OOM kill, a restart
//! triggered by a command with different daemon options) dropped containment
//! and the next command in the same session ran unfiltered. The allowlist is
//! therefore written to `{session}.allowlist` in the daemon socket directory
//! and, like `{session}.target`, intentionally survives daemon restarts: a
//! daemon that starts for a session which was given an allowlist re-applies it
//! before it serves a single command.
//!
//! The file is a containment boundary, so it fails closed. A present but
//! unreadable or corrupt file is an error, never "this session had no
//! allowlist", and a session whose allowlist cannot be written refuses to start
//! rather than run containment that would silently not survive a restart. The
//! only way to widen a session again is to drop the allowlist explicitly with
//! `--allowed-domains ""`.

use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::network::parse_domain_list;

/// Environment variable carrying the allowlist into the daemon process.
pub const ALLOWED_DOMAINS_ENV: &str = "AGENT_BROWSER_ALLOWED_DOMAINS";

/// The persisted document: `{"allowedDomains": ["example.com"]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedAllowlist {
    #[serde(rename = "allowedDomains")]
    allowed_domains: Vec<String>,
}

/// Path of the allowlist file for a session: `{socket_dir}/{session}.allowlist`.
pub fn allowlist_path(session: &str) -> PathBuf {
    crate::connection::get_socket_dir().join(format!("{}.allowlist", session))
}

/// What a starting daemon must do with the allowlist it can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupAllowlist {
    /// Neither the command nor the session carried an allowlist.
    Unrestricted,
    /// The command carried an allowlist; persist it for later restarts.
    FromEnv(Vec<String>),
    /// The command carried none but the session has one; re-apply it.
    Restored(Vec<String>),
    /// The command explicitly asked for no allowlist; forget the stored one.
    Dropped,
}

/// Decide what a starting daemon does, given the allowlist the command passed
/// in the environment and the one previously persisted for the session.
///
/// An environment variable that is set but empty is the explicit
/// `--allowed-domains ""` drop and is the only way a session loses containment;
/// an *absent* variable is the accidental path (a restarted or externally
/// started daemon) and restores the persisted allowlist instead.
pub fn decide_startup_allowlist(
    env_value: Option<&str>,
    persisted: Option<&[String]>,
) -> StartupAllowlist {
    match env_value {
        Some(raw) => {
            let domains = parse_domain_list(raw);
            if domains.is_empty() {
                StartupAllowlist::Dropped
            } else {
                StartupAllowlist::FromEnv(domains)
            }
        }
        None => match persisted {
            Some(domains) if !domains.is_empty() => StartupAllowlist::Restored(domains.to_vec()),
            _ => StartupAllowlist::Unrestricted,
        },
    }
}

/// Load the persisted allowlist for a session. `Ok(None)` means the session
/// never had one; `Err` means a file is present but unreadable or corrupt.
/// Callers must not treat `Err` as an unrestricted session: the file may have
/// carried the only record of the session's containment.
pub fn load(session: &str) -> Result<Option<Vec<String>>, String> {
    let path = allowlist_path(session);
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "cannot read session allowlist file {}: {}",
                path.display(),
                e
            ))
        }
    };
    match serde_json::from_str::<PersistedAllowlist>(&raw) {
        Ok(doc) => Ok(Some(doc.allowed_domains)),
        Err(e) => Err(format!(
            "corrupt session allowlist file {}: {}",
            path.display(),
            e
        )),
    }
}

/// Persist the allowlist for a session, atomically (owner-only temp file,
/// fsync, rename) so a crash never leaves a truncated allowlist behind. An
/// empty list clears the file: containment is gone only when it is dropped
/// explicitly.
pub fn save(session: &str, domains: &[String]) -> Result<(), String> {
    if domains.is_empty() {
        clear(session);
        return Ok(());
    }

    let path = allowlist_path(session);
    let doc = PersistedAllowlist {
        allowed_domains: domains.to_vec(),
    };
    let raw = serde_json::to_string(&doc)
        .map_err(|e| format!("cannot serialize session allowlist: {}", e))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create socket dir {}: {}", dir.display(), e))?;
    }
    let tmp = path.with_extension(format!("allowlist.tmp.{}", std::process::id()));
    let write_result = (|| -> Result<(), String> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).map_err(|e| {
            format!(
                "cannot create session allowlist file {}: {}",
                tmp.display(),
                e
            )
        })?;
        file.write_all(raw.as_bytes()).map_err(|e| {
            format!(
                "cannot write session allowlist file {}: {}",
                tmp.display(),
                e
            )
        })?;
        file.sync_all().map_err(|e| {
            format!(
                "cannot sync session allowlist file {}: {}",
                tmp.display(),
                e
            )
        })?;
        drop(file);
        fs::rename(&tmp, &path).map_err(|e| {
            format!(
                "cannot rename session allowlist file {} to {}: {}",
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

/// Remove the persisted allowlist for a session.
pub fn clear(session: &str) {
    let _ = fs::remove_file(allowlist_path(session));
}

/// Re-apply the session's allowlist to this process's environment before any
/// command is served. Called once at daemon start-up, before the async runtime
/// exists, so the environment write has no other threads to race.
///
/// Fails closed: a corrupt allowlist file, or an allowlist that cannot be
/// stored for the next restart, aborts daemon start-up instead of serving the
/// session unfiltered.
pub fn apply_to_process_env(session: &str) -> Result<(), String> {
    let persisted = load(session).map_err(|e| {
        format!(
            "{} — refusing to start an unrestricted daemon for a session that was given \
             --allowed-domains. Delete the file to reset the session's allowlist, then re-run \
             with --allowed-domains.",
            e
        )
    })?;

    let env_value = env::var(ALLOWED_DOMAINS_ENV).ok();
    match decide_startup_allowlist(env_value.as_deref(), persisted.as_deref()) {
        StartupAllowlist::FromEnv(domains) => save(session, &domains).map_err(|e| {
            format!(
                "{} — refusing to start because --allowed-domains containment would not survive a \
                 daemon restart.",
                e
            )
        }),
        StartupAllowlist::Restored(domains) => {
            env::set_var(ALLOWED_DOMAINS_ENV, domains.join(","));
            Ok(())
        }
        StartupAllowlist::Dropped => {
            clear(session);
            Ok(())
        }
        StartupAllowlist::Unrestricted => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_socket_dir<F: FnOnce(&crate::test_utils::EnvGuard)>(f: F) {
        let guard = crate::test_utils::EnvGuard::new(&[
            "AGENT_BROWSER_SOCKET_DIR",
            "XDG_RUNTIME_DIR",
            "AGENT_BROWSER_NAMESPACE",
            ALLOWED_DOMAINS_ENV,
        ]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        guard.remove("XDG_RUNTIME_DIR");
        guard.remove("AGENT_BROWSER_NAMESPACE");
        guard.remove(ALLOWED_DOMAINS_ENV);
        f(&guard);
    }

    #[test]
    fn test_allowlist_round_trip() {
        with_socket_dir(|_guard| {
            let domains = vec!["allowed.test".to_string(), "*.allowed.test".to_string()];
            save("agent-1", &domains).unwrap();
            assert_eq!(load("agent-1"), Ok(Some(domains)));
            clear("agent-1");
            assert_eq!(load("agent-1"), Ok(None));
        });
    }

    #[test]
    fn test_load_missing_returns_none() {
        with_socket_dir(|_guard| {
            assert_eq!(load("no-such-session"), Ok(None));
        });
    }

    #[test]
    fn test_load_corrupt_file_is_an_error_not_an_empty_allowlist() {
        with_socket_dir(|_guard| {
            let path = allowlist_path("bad");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{\"allowedDomains\":[\"allowed.te").unwrap();
            let err = load("bad").unwrap_err();
            assert!(err.contains("corrupt"), "unexpected error: {}", err);
        });
    }

    #[test]
    fn test_save_empty_clears_the_file() {
        with_socket_dir(|_guard| {
            save("drop", &["allowed.test".to_string()]).unwrap();
            save("drop", &[]).unwrap();
            assert_eq!(load("drop"), Ok(None));
        });
    }

    #[test]
    fn test_restart_without_the_flag_restores_the_session_allowlist() {
        // The reported bypass: a session is configured with an allowlist, the
        // daemon is replaced (kill, idle expiry, or a restart forced by
        // different daemon options), and the next command carries no
        // allowlist. The restarted daemon must not end up unfiltered.
        let persisted = vec!["allowed.test".to_string()];
        assert_eq!(
            decide_startup_allowlist(None, Some(&persisted)),
            StartupAllowlist::Restored(persisted)
        );
    }

    #[test]
    fn test_command_allowlist_is_persisted_for_the_next_restart() {
        assert_eq!(
            decide_startup_allowlist(Some("allowed.test, *.allowed.test"), None),
            StartupAllowlist::FromEnv(vec![
                "allowed.test".to_string(),
                "*.allowed.test".to_string()
            ])
        );
    }

    #[test]
    fn test_command_allowlist_wins_over_the_persisted_one() {
        let persisted = vec!["allowed.test".to_string()];
        assert_eq!(
            decide_startup_allowlist(Some("other.test"), Some(&persisted)),
            StartupAllowlist::FromEnv(vec!["other.test".to_string()])
        );
    }

    #[test]
    fn test_explicit_empty_allowlist_drops_the_persisted_one() {
        let persisted = vec!["allowed.test".to_string()];
        assert_eq!(
            decide_startup_allowlist(Some(""), Some(&persisted)),
            StartupAllowlist::Dropped
        );
    }

    #[test]
    fn test_session_without_an_allowlist_stays_unrestricted() {
        assert_eq!(
            decide_startup_allowlist(None, None),
            StartupAllowlist::Unrestricted
        );
        assert_eq!(
            decide_startup_allowlist(None, Some(&[])),
            StartupAllowlist::Unrestricted
        );
    }

    #[test]
    fn test_apply_to_process_env_restores_allowlist_after_restart() {
        with_socket_dir(|_guard| {
            save("restarted", &["allowed.test".to_string()]).unwrap();
            apply_to_process_env("restarted").unwrap();
            assert_eq!(
                env::var(ALLOWED_DOMAINS_ENV).ok().as_deref(),
                Some("allowed.test")
            );
        });
    }

    #[test]
    fn test_apply_to_process_env_persists_a_new_allowlist() {
        with_socket_dir(|_guard| {
            _guard.set(ALLOWED_DOMAINS_ENV, "allowed.test");
            apply_to_process_env("fresh").unwrap();
            assert_eq!(load("fresh"), Ok(Some(vec!["allowed.test".to_string()])));
        });
    }

    #[test]
    fn test_apply_to_process_env_refuses_a_corrupt_allowlist() {
        with_socket_dir(|_guard| {
            let path = allowlist_path("corrupt");
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "not json").unwrap();
            let err = apply_to_process_env("corrupt").unwrap_err();
            assert!(err.contains("corrupt"), "unexpected error: {}", err);
            assert!(env::var(ALLOWED_DOMAINS_ENV).is_err());
        });
    }

    #[test]
    fn test_apply_to_process_env_forgets_an_explicitly_dropped_allowlist() {
        with_socket_dir(|_guard| {
            save("dropped", &["allowed.test".to_string()]).unwrap();
            _guard.set(ALLOWED_DOMAINS_ENV, "");
            apply_to_process_env("dropped").unwrap();
            assert_eq!(load("dropped"), Ok(None));
        });
    }
}
