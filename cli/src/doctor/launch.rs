//! Live launch test: spawn a scratch daemon session, launch headless
//! Chrome, navigate to `about:blank`, then close. Skipped under `--quick`.
//!
//! A `LaunchGuard` Drop impl ensures the scratch session is closed and its
//! sidecar files cleaned even on panic or early return.

use std::env;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use super::helpers::new_id;
use super::{Check, Status};
use crate::connection::{cleanup_stale_files, ensure_daemon, send_command, DaemonOptions};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Launch test";

    if env::var("AGENT_BROWSER_PROVIDER").is_ok() {
        checks.push(Check::new(
            "launch.skipped.provider",
            category,
            Status::Info,
            "Skipped (AGENT_BROWSER_PROVIDER is set; would consume cloud quota)",
        ));
        return;
    }
    if env::var("AGENT_BROWSER_CDP").is_ok() {
        checks.push(Check::new(
            "launch.skipped.cdp",
            category,
            Status::Info,
            "Skipped (AGENT_BROWSER_CDP is set; would attach to a real browser)",
        ));
        return;
    }

    let session = format!(
        "doctor-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );

    // Armed after `ensure_daemon` succeeds so we don't send a stray `close`
    // or delete sidecar files for a daemon that never started. On every early
    // return past the `Some(...)` assignment below, Drop runs one close and
    // one `cleanup_stale_files`.
    let mut _guard: Option<LaunchGuard> = None;

    let opts = DaemonOptions {
        headed: false,
        debug: false,
        executable_path: None,
        extensions: &[],
        init_scripts: &[],
        enable: &[],
        args: None,
        user_agent: None,
        proxy: None,
        proxy_bypass: None,
        proxy_username: None,
        proxy_password: None,
        ignore_https_errors: false,
        allow_file_access: false,
        profile: None,
        state: None,
        provider: None,
        device: None,
        session_name: None,
        download_path: None,
        allowed_domains: None,
        action_policy: None,
        confirm_actions: None,
        engine: None,
        auto_connect: false,
        idle_timeout: None,
        default_timeout: None,
        cdp: None,
        no_auto_dialog: false,
    };

    let started = Instant::now();
    if let Err(e) = ensure_daemon(&session, &opts) {
        checks.push(
            Check::new(
                "launch.daemon",
                category,
                Status::Fail,
                format!("Could not start daemon: {}", e),
            )
            .with_fix("check Chrome install and re-run with --debug"),
        );
        return;
    }
    _guard = Some(LaunchGuard {
        session: session.clone(),
    });

    let launch_cmd = json!({
        "id": new_id(),
        "action": "launch",
        "headless": true,
    });
    if let Err(e) = send_json(launch_cmd, &session) {
        checks.push(
            Check::new(
                "launch.launch",
                category,
                Status::Fail,
                format!("Browser launch failed: {}", e),
            )
            .with_fix("agent-browser install   # or check --debug output"),
        );
        return;
    }

    let open_cmd = json!({
        "id": new_id(),
        "action": "navigate",
        "url": "about:blank",
    });
    if let Err(e) = send_json(open_cmd, &session) {
        checks.push(
            Check::new(
                "launch.navigate",
                category,
                Status::Fail,
                format!("Navigation to about:blank failed: {}", e),
            )
            .with_fix("re-run with --debug for full launch logs"),
        );
        return;
    }

    // Close + stale-file cleanup happen exactly once via LaunchGuard::drop at
    // end of scope; no explicit close here.
    let elapsed = started.elapsed();
    let secs = elapsed.as_secs_f64();
    if elapsed > Duration::from_secs(5) {
        checks.push(Check::new(
            "launch.elapsed",
            category,
            Status::Warn,
            format!(
                "Headless launch + about:blank in {:.2}s (slow; expected < 5s)",
                secs
            ),
        ));
    } else {
        checks.push(Check::new(
            "launch.elapsed",
            category,
            Status::Pass,
            format!("Headless launch + about:blank in {:.2}s", secs),
        ));
    }
}

fn send_json(cmd: Value, session: &str) -> Result<(), String> {
    match send_command(cmd, session) {
        Ok(resp) => {
            if resp.success {
                Ok(())
            } else {
                Err(resp.error.unwrap_or_else(|| "unknown error".to_string()))
            }
        }
        Err(e) => Err(e),
    }
}

/// Best-effort cleanup when the launch test panics or returns early.
struct LaunchGuard {
    session: String,
}

impl Drop for LaunchGuard {
    fn drop(&mut self) {
        let close_cmd = json!({ "id": new_id(), "action": "close" });
        let _ = send_command(close_cmd, &self.session);
        cleanup_stale_files(&self.session);
    }
}
