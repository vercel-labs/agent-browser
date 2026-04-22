//! Check security posture: encryption key presence / permissions, saved
//! state file age, and the optional action policy file.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::helpers::parse_json_file;
use super::{Check, Status};
use crate::native::state::{get_sessions_dir, get_state_dir};

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
        let expire_days = env::var("AGENT_BROWSER_STATE_EXPIRE_DAYS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(30);
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(expire_days * 86_400))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut total = 0usize;
        let mut old = 0usize;
        if let Ok(entries) = fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    total += 1;
                    if let Ok(meta) = entry.metadata() {
                        if let Ok(modified) = meta.modified() {
                            if modified < cutoff {
                                old += 1;
                            }
                        }
                    }
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
        } else if old > 0 {
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
