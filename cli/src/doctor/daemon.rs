//! Check running daemons: inventory of sessions, version match with the
//! CLI, and stale sidecar files cleaned up as a side effect of the walk.

use super::{Check, Status};
use crate::connection::{walk_daemons, CleanReason};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Daemons";
    let cli_version = env!("CARGO_PKG_VERSION");

    let inventory = walk_daemons();

    for cleaned in &inventory.cleaned {
        let reason = match cleaned.reason {
            CleanReason::ProcessGone | CleanReason::DashboardGone => "process gone",
            CleanReason::UnreadablePidFile => "unreadable pid file",
            CleanReason::OrphanedSocket => "orphaned socket",
        };
        checks.push(Check::new(
            format!("daemon.cleaned.{}", cleaned.name),
            category,
            Status::Warn,
            format!("Cleaned stale files: {} ({})", cleaned.name, reason),
        ));
    }

    if inventory.sessions.is_empty() {
        checks.push(Check::new(
            "daemon.active",
            category,
            Status::Pass,
            "No active daemons",
        ));
    } else {
        for session in &inventory.sessions {
            let version_match = session.version.as_deref() == Some(cli_version);
            let status = if version_match {
                Status::Pass
            } else {
                Status::Warn
            };
            let suffix = if version_match {
                String::new()
            } else {
                format!(" (version mismatch with CLI {})", cli_version)
            };
            let mut check = Check::new(
                format!("daemon.session.{}", session.name),
                category,
                status,
                format!("Session {} (pid {}){}", session.name, session.pid, suffix),
            );
            if !version_match {
                check = check.with_fix(format!("agent-browser --session {} close", session.name));
            }
            checks.push(check);
        }
    }

    if let Some(dashboard) = inventory.dashboard {
        if dashboard.alive {
            checks.push(Check::new(
                "daemon.dashboard",
                category,
                Status::Pass,
                format!("Dashboard server running (pid {})", dashboard.pid),
            ));
        }
    }
}
