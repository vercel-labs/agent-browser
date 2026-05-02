//! Integration tests for `agent-browser doctor`.
//!
//! These tests spawn the real CLI binary via `env!("CARGO_BIN_EXE_*")` and
//! verify the doctor command produces sane output. They override
//! `AGENT_BROWSER_SOCKET_DIR` and `HOME` / `USERPROFILE` so the doctor
//! inspects a throwaway directory and never touches the user's real state.

use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn build_doctor_cmd(tmp: &TempDir, args: &[&str]) -> Command {
    let socket_dir = tmp.path().join("sockets");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("AGENT_BROWSER_SOCKET_DIR", &socket_dir)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        // Keep the launch test's skip-logic deterministic across hosts.
        .env_remove("AGENT_BROWSER_PROVIDER")
        .env_remove("AGENT_BROWSER_CDP")
        .env_remove("AGENT_BROWSER_EXECUTABLE_PATH")
        // Don't emit color codes into captured stdout.
        .env("NO_COLOR", "1");
    cmd
}

#[test]
fn doctor_offline_quick_json_emits_valid_payload() {
    let tmp = TempDir::new().unwrap();

    let output = build_doctor_cmd(&tmp, &["doctor", "--offline", "--quick", "--json"])
        .output()
        .expect("failed to invoke agent-browser doctor");

    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    // Exit code 0 (all pass) or 1 (one or more fails) are both valid outcomes;
    // the doctor may legitimately report a failure on a host without Chrome.
    assert!(
        code == 0 || code == 1,
        "unexpected exit code {}\nstdout:\n{}\nstderr:\n{}",
        code,
        stdout,
        stderr,
    );

    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not JSON: {}\n---\n{}", e, stdout));

    assert!(payload.get("success").is_some(), "missing success field");
    assert!(payload.get("summary").is_some(), "missing summary field");
    assert!(payload.get("fixed").is_some(), "missing fixed field");

    let summary = &payload["summary"];
    assert!(summary["pass"].is_number());
    assert!(summary["warn"].is_number());
    assert!(summary["fail"].is_number());

    let checks = payload["checks"]
        .as_array()
        .expect("checks should be an array");
    assert!(!checks.is_empty(), "checks array should not be empty");

    // Every check must have a non-empty id / category / status / message.
    for c in checks {
        assert!(
            c["id"].as_str().is_some_and(|s| !s.is_empty()),
            "check missing id: {}",
            c
        );
        assert!(
            c["category"].as_str().is_some_and(|s| !s.is_empty()),
            "check missing category: {}",
            c
        );
        let status = c["status"].as_str().expect("status should be string");
        assert!(
            ["pass", "warn", "fail", "info"].contains(&status),
            "unexpected status {:?}",
            status
        );
        assert!(
            c["message"].as_str().is_some_and(|s| !s.is_empty()),
            "check missing message: {}",
            c
        );
    }

    // Check IDs must be unique now that providers / sessions / skipped-launch
    // states each carry their own ID suffix.
    let mut seen = std::collections::HashSet::new();
    for c in checks {
        let id = c["id"].as_str().unwrap();
        assert!(
            seen.insert(id.to_string()),
            "duplicate check id in JSON output: {}\nfull payload:\n{}",
            id,
            stdout
        );
    }
}

#[test]
fn doctor_uses_configured_executable_path() {
    let tmp = TempDir::new().unwrap();
    let config_dir = tmp.path().join("home").join(".agent-browser");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.json"),
        serde_json::json!({ "executablePath": BIN }).to_string(),
    )
    .unwrap();

    let output = build_doctor_cmd(&tmp, &["doctor", "--offline", "--quick", "--json"])
        .output()
        .expect("failed to invoke agent-browser doctor");

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not JSON: {}\n---\n{}", e, stdout));

    let checks = payload["checks"]
        .as_array()
        .expect("checks should be an array");
    let chrome_check = checks
        .iter()
        .find(|check| check["id"] == "chrome.installed")
        .expect("chrome check should be present");

    assert_eq!(chrome_check["status"], "pass");
    assert!(
        chrome_check["message"]
            .as_str()
            .is_some_and(|message| message.contains(BIN)),
        "chrome check should mention configured executable path: {}",
        chrome_check
    );
}

#[test]
fn doctor_help_describes_flags_and_examples() {
    let tmp = TempDir::new().unwrap();

    let output = build_doctor_cmd(&tmp, &["doctor", "--help"])
        .output()
        .expect("failed to invoke agent-browser doctor --help");

    assert!(
        output.status.success(),
        "doctor --help should exit 0; got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");

    for needle in [
        "agent-browser doctor",
        "--offline",
        "--quick",
        "--fix",
        "--json",
        "Exit codes",
    ] {
        assert!(
            stdout.contains(needle),
            "doctor --help output missing {:?}\n---\n{}",
            needle,
            stdout
        );
    }
}
