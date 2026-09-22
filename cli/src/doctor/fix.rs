//! Destructive repair actions behind `--fix`: reinstall Chrome, close
//! version-mismatched daemons, purge expired state files, and generate a
//! missing encryption key.

use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use super::helpers::new_id;
use super::{Check, Status};
use crate::connection::{cleanup_stale_files, send_command, walk_daemons};
use crate::native::state::{get_state_dir, state_clean, state_expiration_days};

pub(super) fn run(checks: &mut [Check], fixed: &mut Vec<String>) {
    // `close_all_sessions` is expensive and closes every session at once, so
    // only fire it on the first daemon.session.* Warn we encounter. Subsequent
    // daemon.session.* Warn checks piggy-back on the same result.
    let mut daemons_closed: Option<usize> = None;

    for c in checks.iter_mut() {
        match c.id.as_str() {
            "chrome.installed" if c.status == Status::Fail => {
                let installed = attempt_chrome_install();
                if installed {
                    fixed.push("Reinstalled Chrome".to_string());
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            id if id.starts_with("daemon.session.") && c.status == Status::Warn => {
                let killed = *daemons_closed.get_or_insert_with(|| {
                    let n = close_all_sessions();
                    if n > 0 {
                        fixed.push(format!("Closed {} version-mismatched daemon(s)", n));
                    }
                    n
                });
                if killed > 0 {
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            "security.state_count" if c.status == Status::Warn => {
                let removed = purge_old_state();
                if removed > 0 {
                    fixed.push(format!("Deleted {} expired state file(s)", removed));
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            "security.encryption_key" if c.status == Status::Info => {
                let generated = create_encryption_key();
                if generated {
                    fixed.push("Generated encryption key".to_string());
                    c.status = Status::Pass;
                    c.message = format!("{} (fixed by --fix)", c.message);
                    c.fix = None;
                }
            }
            _ => {}
        }
    }
}

fn attempt_chrome_install() -> bool {
    // run_install() uses process::exit on failure, so we shell out to ourselves
    // to avoid taking down the doctor process if the install fails.
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    std::process::Command::new(exe)
        .arg("install")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn close_all_sessions() -> usize {
    let mut killed = 0;
    for session in &walk_daemons().sessions {
        let cmd = json!({ "id": new_id(), "action": "close" });
        if send_command(cmd, &session.name).is_ok() {
            killed += 1;
        }
        cleanup_stale_files(&session.name);
    }
    killed
}

fn purge_old_state() -> usize {
    let Some(days) = state_expiration_days() else {
        return 0;
    };
    state_clean(days)
        .ok()
        .and_then(|result| result.get("cleaned").and_then(|value| value.as_u64()))
        .unwrap_or(0) as usize
}

fn create_encryption_key() -> bool {
    create_encryption_key_at(&get_state_dir())
}

fn create_encryption_key_at(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(".encryption-key");
    if path.exists() {
        return false;
    }
    let mut buf = [0u8; 32];
    if getrandom::getrandom(&mut buf).is_err() {
        return false;
    }
    let hex: String = buf.iter().map(|b| format!("{:02x}", b)).collect();
    if fs::write(&path, format!("{}\n", hex)).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_create_encryption_key_at_writes_64_char_hex_key() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("state");

        assert!(create_encryption_key_at(&dir));

        let key = dir.join(".encryption-key");
        assert!(key.exists(), "key file should be created");

        let contents = fs::read_to_string(&key).unwrap();
        let trimmed = contents.trim();
        assert_eq!(trimmed.len(), 64, "key should be 64 hex chars");
        assert!(
            trimmed.chars().all(|c| c.is_ascii_hexdigit()),
            "key should be all hex digits"
        );
    }

    #[test]
    fn test_create_encryption_key_at_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("state");
        assert!(create_encryption_key_at(&dir));

        let original = fs::read_to_string(dir.join(".encryption-key")).unwrap();

        // Second call returns false and must not overwrite the existing key.
        assert!(!create_encryption_key_at(&dir));
        let after = fs::read_to_string(dir.join(".encryption-key")).unwrap();
        assert_eq!(original, after);
    }

    #[cfg(unix)]
    #[test]
    fn test_create_encryption_key_at_sets_0600_perms() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("state");

        assert!(create_encryption_key_at(&dir));

        let key = dir.join(".encryption-key");
        let mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file should be 0600, got {:o}", mode);
    }

    #[cfg(unix)]
    #[test]
    fn test_run_fixes_generates_missing_encryption_key() {
        // Reaches the Info-status arm in run_fixes that was previously
        // unreachable due to an early-continue guard. Overrides HOME so
        // get_state_dir() resolves under a temp dir.
        let guard = crate::test_utils::EnvGuard::new(&["HOME"]);
        let tmp = TempDir::new().unwrap();
        guard.set("HOME", tmp.path().to_str().unwrap());

        let mut checks = vec![Check::new(
            "security.encryption_key",
            "Security",
            Status::Info,
            "No encryption key set",
        )
        .with_fix("export AGENT_BROWSER_ENCRYPTION_KEY=...")];
        let mut fixed = Vec::new();

        run(&mut checks, &mut fixed);

        assert_eq!(
            checks[0].status,
            Status::Pass,
            "Info check should transition to Pass after --fix"
        );
        assert!(
            checks[0].fix.is_none(),
            "fix hint should be cleared after repair"
        );
        assert!(
            fixed.iter().any(|s| s.contains("encryption key")),
            "fixed summary should mention the key generation"
        );
        assert!(
            tmp.path().join(".agent-browser/.encryption-key").exists(),
            "key file should exist at ~/.agent-browser/.encryption-key"
        );
    }

    #[test]
    fn test_purge_old_state_does_not_delete_when_expiration_is_disabled() {
        let guard = crate::test_utils::EnvGuard::new(&["HOME", "AGENT_BROWSER_STATE_EXPIRE_DAYS"]);
        let tmp = TempDir::new().unwrap();
        guard.set("HOME", tmp.path().to_str().unwrap());
        guard.set("AGENT_BROWSER_STATE_EXPIRE_DAYS", "0");

        let sessions = crate::native::state::get_sessions_dir();
        fs::create_dir_all(&sessions).unwrap();
        let state = sessions.join("keep.json");
        fs::write(&state, "{}").unwrap();

        assert_eq!(purge_old_state(), 0);
        assert!(state.exists());
    }
}
