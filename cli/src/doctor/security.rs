//! Check security posture: encryption key presence / permissions, saved
//! state file age, and the optional action policy file.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::helpers::parse_json_file;
use super::{Check, Status};
use crate::native::state::{
    get_sessions_dir, get_state_dir, is_state_file, state_expiration_cutoff, state_expiration_days,
};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Security";

    let key_env = env::var("AGENT_BROWSER_ENCRYPTION_KEY").ok();
    let key_file = get_state_dir().join(".encryption-key");
    if let Some(hex) = &key_env {
        if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            checks.push(Check::new(
                "security.encryption_key",
                category,
                Status::Pass,
                "AGENT_BROWSER_ENCRYPTION_KEY set (64-char hex)",
            ));
        } else {
            checks.push(
                Check::new(
                    "security.encryption_key",
                    category,
                    Status::Fail,
                    "AGENT_BROWSER_ENCRYPTION_KEY is not a 64-char hex string",
                )
                .with_fix("export AGENT_BROWSER_ENCRYPTION_KEY=$(openssl rand -hex 32)"),
            );
        }
    } else if key_file.exists() {
        let mut msg = format!("Encryption key file present: {}", key_file.display());
        let mut status = Status::Pass;
        let mut fix: Option<String> = None;
        #[cfg(unix)]
        if let Ok(meta) = fs::metadata(&key_file) {
            let mode = meta.permissions().mode() & 0o777;
            if mode & 0o077 != 0 {
                status = Status::Warn;
                msg = format!(
                    "Encryption key file is too permissive ({:o}): {}",
                    mode,
                    key_file.display()
                );
                fix = Some(format!("chmod 600 {}", key_file.display()));
            }
        }
        let mut check = Check::new("security.encryption_key", category, status, msg);
        if let Some(f) = fix {
            check = check.with_fix(f);
        }
        checks.push(check);
    } else {
        checks.push(
            Check::new(
                "security.encryption_key",
                category,
                Status::Info,
                "No encryption key set (will be auto-generated on first auth save)",
            )
            .with_fix("export AGENT_BROWSER_ENCRYPTION_KEY=$(openssl rand -hex 32)"),
        );
    }

    let sessions_dir = get_sessions_dir();
    if sessions_dir.exists() {
        let mut total = 0usize;
        if let Ok(entries) = fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                    && is_state_file(&entry.path())
                {
                    total += 1;
                }
            }
        }
        if total == 0 {
            checks.push(Check::new(
                "security.state_count",
                category,
                Status::Info,
                "No saved state files",
            ));
        } else {
            match state_expiration_cutoff(SystemTime::now()) {
                Ok(None) => checks.push(Check::new(
                    "security.state_count",
                    category,
                    Status::Pass,
                    format!(
                        "{} saved state file(s); expiration disabled (0 days)",
                        total
                    ),
                )),
                Err(error) => checks.push(
                    Check::new(
                        "security.state_count",
                        category,
                        Status::Warn,
                        format!("Could not evaluate state expiration: {}", error),
                    )
                    .with_fix(
                        "set AGENT_BROWSER_STATE_EXPIRE_DAYS to 0 or a smaller positive value",
                    ),
                ),
                Ok(Some(cutoff)) => {
                    let mut old = 0usize;
                    if let Ok(entries) = fs::read_dir(&sessions_dir) {
                        for entry in entries.flatten() {
                            if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                                && is_state_file(&entry.path())
                                && entry
                                    .metadata()
                                    .and_then(|metadata| metadata.modified())
                                    .map(|modified| modified < cutoff)
                                    .unwrap_or(false)
                            {
                                old += 1;
                            }
                        }
                    }
                    if old > 0 {
                        let expire_days =
                            state_expiration_days().expect("enabled expiration has a day value");
                        checks.push(
                            Check::new(
                                "security.state_count",
                                category,
                                Status::Warn,
                                format!(
                                    "{} state file(s) older than {} days ({} total)",
                                    old, expire_days, total
                                ),
                            )
                            .with_fix(format!(
                                "agent-browser state clean --older-than {}",
                                expire_days
                            )),
                        );
                    } else {
                        checks.push(Check::new(
                            "security.state_count",
                            category,
                            Status::Pass,
                            format!("{} saved state file(s)", total),
                        ));
                    }
                }
            }
        }
    }

    if let Ok(policy_path) = env::var("AGENT_BROWSER_ACTION_POLICY") {
        let p = PathBuf::from(&policy_path);
        if !p.exists() {
            checks.push(
                Check::new(
                    "security.action_policy",
                    category,
                    Status::Fail,
                    format!(
                        "AGENT_BROWSER_ACTION_POLICY points to missing file: {}",
                        policy_path
                    ),
                )
                .with_fix("update or unset AGENT_BROWSER_ACTION_POLICY"),
            );
        } else {
            match parse_json_file(&p) {
                Ok(_) => checks.push(Check::new(
                    "security.action_policy",
                    category,
                    Status::Pass,
                    format!("Action policy: {}", policy_path),
                )),
                Err(e) => checks.push(
                    Check::new(
                        "security.action_policy",
                        category,
                        Status::Fail,
                        format!("Action policy: {}: {}", policy_path, e),
                    )
                    .with_fix(format!("edit {}", policy_path)),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn zero_expiration_does_not_report_saved_states_as_old() {
        let guard = crate::test_utils::EnvGuard::new(&[
            "HOME",
            "AGENT_BROWSER_STATE_EXPIRE_DAYS",
            "AGENT_BROWSER_ENCRYPTION_KEY",
        ]);
        let tmp = TempDir::new().unwrap();
        guard.set("HOME", tmp.path().to_str().unwrap());
        guard.set("AGENT_BROWSER_STATE_EXPIRE_DAYS", "0");
        guard.remove("AGENT_BROWSER_ENCRYPTION_KEY");

        let sessions = get_sessions_dir();
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("keep.json"), "{}").unwrap();

        let mut checks = Vec::new();
        check(&mut checks);
        let state_check = checks
            .iter()
            .find(|item| item.id == "security.state_count")
            .expect("state count check should be present");
        assert_eq!(state_check.status, Status::Pass);
        assert!(state_check.message.contains("expiration disabled"));
    }
}
