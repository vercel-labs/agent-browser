//! Integration tests for the read-only Chrome readiness probe.

use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn command(tmp: &TempDir, args: &[&str]) -> Command {
    let home = tmp.path().join("home");
    let local_app_data = tmp.path().join("local-app-data");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&local_app_data).unwrap();

    let mut command = Command::new(BIN);
    command
        .args(args)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("LOCALAPPDATA", &local_app_data)
        .env("NO_COLOR", "1");
    command
}

#[test]
fn chrome_status_json_reports_local_readiness_without_creating_state() {
    let tmp = TempDir::new().unwrap();
    let output = command(&tmp, &["chrome", "status", "--json"])
        .output()
        .expect("failed to run chrome status");

    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = payload["status"].as_str().unwrap();
    assert!(["candidate", "not-running"].contains(&status));
    assert_eq!(payload["success"], status == "candidate");
    assert_eq!(output.status.success(), status == "candidate");
    if status == "not-running" {
        assert!(payload["actionRequired"]
            .as_str()
            .unwrap()
            .contains("chrome://inspect/#remote-debugging"));
    }
    assert!(!tmp.path().join("home/.config/google-chrome").exists());
    assert!(!tmp.path().join("local-app-data/Google/Chrome").exists());
}

#[test]
fn chrome_status_help_documents_side_effect_free_probe() {
    let tmp = TempDir::new().unwrap();
    let output = command(&tmp, &["chrome", "--help"])
        .output()
        .expect("failed to run chrome --help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("agent-browser chrome status"));
    assert!(stdout.contains("without"));
    assert!(stdout.contains("attaching"));
}
