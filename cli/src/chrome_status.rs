//! Read-only Chrome remote-debugging readiness command.

use serde_json::json;

use crate::color;
use crate::native::cdp::chrome::{chrome_debugging_status, ChromeDebuggingStatus};

const ENABLE_HINT: &str =
    "Open chrome://inspect/#remote-debugging in Chrome and enable Remote debugging.";

/// Print whether `--auto-connect` can discover Chrome without attaching to it.
pub async fn run(json_output: bool) -> i32 {
    let status = chrome_debugging_status().await;
    let (name, message, action, exit_code) = match &status {
        ChromeDebuggingStatus::Ready { port, .. } => (
            "ready",
            format!("Chrome remote debugging is ready on loopback port {}", port),
            None,
            0,
        ),
        ChromeDebuggingStatus::Candidate { port } => (
            "candidate",
            format!(
                "Common auto-connect port {} is reachable, but this check did not verify CDP",
                port
            ),
            None,
            0,
        ),
        ChromeDebuggingStatus::NotRunning => (
            "not-running",
            "Chrome remote debugging is not active (Chrome may be closed or the setting may be disabled)".to_string(),
            Some(ENABLE_HINT),
            1,
        ),
        ChromeDebuggingStatus::Stale { port, .. } => (
            "stale",
            format!("Chrome advertised loopback port {}, but nothing is listening", port),
            Some("Restart Chrome, then check remote debugging again."),
            2,
        ),
        ChromeDebuggingStatus::Unknown { reason, .. } => (
            "unknown",
            format!("Chrome remote-debugging state could not be determined: {}", reason),
            Some("Check the Chrome user-data directory permissions, then retry."),
            2,
        ),
    };

    if json_output {
        let (user_data_dir, port) = match &status {
            ChromeDebuggingStatus::Ready {
                user_data_dir,
                port,
            }
            | ChromeDebuggingStatus::Stale {
                user_data_dir,
                port,
            } => (Some(user_data_dir.display().to_string()), Some(*port)),
            ChromeDebuggingStatus::Candidate { port } => (None, Some(*port)),
            ChromeDebuggingStatus::Unknown { user_data_dir, .. } => {
                (Some(user_data_dir.display().to_string()), None)
            }
            ChromeDebuggingStatus::NotRunning => (None, None),
        };
        println!(
            "{}",
            json!({
                "success": exit_code == 0,
                "status": name,
                "message": message,
                "actionRequired": action,
                "userDataDir": user_data_dir,
                "port": port,
            })
        );
    } else if exit_code == 0 {
        println!("{} {}", color::success_indicator(), message);
    } else {
        println!("{} {}", color::warning_indicator(), message);
        if let Some(action) = action {
            println!("  {}", action);
        }
        println!("  This check did not attach to Chrome or show an approval prompt.");
    }

    exit_code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_hint_points_to_chromes_remote_debugging_setting() {
        assert!(ENABLE_HINT.contains("chrome://inspect/#remote-debugging"));
    }
}
