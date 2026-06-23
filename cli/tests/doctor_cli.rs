//! Integration tests for `agent-browser doctor`.
//!
//! These tests spawn the real CLI binary via `env!("CARGO_BIN_EXE_*")` and
//! verify the doctor command produces sane output. They override
//! `AGENT_BROWSER_SOCKET_DIR` and `HOME` / `USERPROFILE` so the doctor
//! inspects a throwaway directory and never touches the user's real state.

use std::process::Command;
#[cfg(windows)]
use std::process::{Child, Output, Stdio};
#[cfg(windows)]
use std::time::{Duration, Instant};
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

#[cfg(windows)]
#[test]
fn doctor_offline_json_live_launch_completes_on_windows() {
    let tmp = TempDir::new().unwrap();
    let socket_dir = tmp.path().join("sockets");

    let child = build_doctor_cmd(&tmp, &["doctor", "--offline", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to invoke agent-browser doctor");

    let output = wait_with_timeout(child, Duration::from_secs(60), &socket_dir);
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert!(
        code == 0 || code == 1,
        "unexpected exit code {}\nstdout:\n{}\nstderr:\n{}",
        code,
        stdout,
        stderr,
    );

    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not JSON: {}\n---\n{}", e, stdout));
    let checks = payload["checks"]
        .as_array()
        .expect("checks should be an array");
    assert!(
        checks.iter().any(|c| c["category"] == "Launch test")
            || checks
                .iter()
                .any(|c| { c["id"].as_str().is_some_and(|id| id.starts_with("launch.")) }),
        "doctor --offline --json should exercise the live launch check\n{}",
        stdout
    );
}

#[cfg(windows)]
fn wait_with_timeout(mut child: Child, timeout: Duration, socket_dir: &std::path::Path) -> Output {
    let started = Instant::now();
    loop {
        if child
            .try_wait()
            .expect("failed to poll agent-browser doctor")
            .is_some()
        {
            return child
                .wait_with_output()
                .expect("failed to collect agent-browser doctor output");
        }

        if started.elapsed() >= timeout {
            let _ = Command::new("taskkill")
                .args(["/PID", &child.id().to_string(), "/T", "/F"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = child.kill();
            kill_sidecar_daemons(socket_dir);
            let output = child
                .wait_with_output()
                .expect("failed to collect timed-out agent-browser doctor output");
            panic!(
                "agent-browser doctor timed out after {:?}\nstdout:\n{}\nstderr:\n{}",
                timeout,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(windows)]
fn kill_sidecar_daemons(socket_dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(socket_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pid") {
            continue;
        }
        let Ok(pid) = std::fs::read_to_string(&path) else {
            continue;
        };
        let pid = pid.trim();
        if pid.is_empty() {
            continue;
        }
        let _ = Command::new("taskkill")
            .args(["/PID", pid, "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
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
