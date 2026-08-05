//! Integration tests for failures that occur before daemon dispatch.

use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn build_cli(tmp: &TempDir) -> Command {
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.current_dir(tmp.path())
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("NO_COLOR", "1")
        .env_remove("AGENT_BROWSER_CONFIG")
        .env_remove("AGENT_BROWSER_JSON");
    cmd
}

#[test]
fn explicit_config_error_honors_json_mode() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("missing.json");
    let output = build_cli(&tmp)
        .args([
            "--json",
            "--config",
            missing.to_str().unwrap(),
            "open",
            "about:blank",
        ])
        .output()
        .expect("run CLI with missing config");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout was not JSON: {error}\n{stdout}"));

    assert_eq!(payload["success"], false);
    assert_eq!(payload["type"], "config_error");
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("config file not found")));
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

#[test]
fn explicit_config_error_preserves_text_mode() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("missing.json");
    let output = build_cli(&tmp)
        .args(["--config", missing.to_str().unwrap(), "open", "about:blank"])
        .output()
        .expect("run CLI with missing config");

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    assert!(stderr.contains("config file not found"), "stderr: {stderr}");
}

#[test]
fn malformed_explicit_config_honors_json_mode() {
    let tmp = TempDir::new().unwrap();
    let malformed = tmp.path().join("malformed.json");
    std::fs::write(&malformed, "{ not valid JSON").unwrap();
    let output = build_cli(&tmp)
        .args([
            "--json",
            "--config",
            malformed.to_str().unwrap(),
            "open",
            "about:blank",
        ])
        .output()
        .expect("run CLI with malformed config");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout was not JSON: {error}\n{stdout}"));

    assert_eq!(payload["success"], false);
    assert_eq!(payload["type"], "config_error");
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("failed to load config")));
    assert!(output.stderr.is_empty());
}

#[test]
fn config_error_honors_json_environment_variable() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("missing-from-env.json");
    let output = build_cli(&tmp)
        .args(["open", "about:blank"])
        .env("AGENT_BROWSER_CONFIG", &missing)
        .env("AGENT_BROWSER_JSON", "1")
        .output()
        .expect("run CLI with config and JSON environment variables");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout was not JSON: {error}\n{stdout}"));

    assert_eq!(payload["success"], false);
    assert_eq!(payload["type"], "config_error");
    assert!(output.stderr.is_empty());
}

#[test]
fn standalone_subcommand_error_honors_json_mode() {
    let tmp = TempDir::new().unwrap();
    let output = build_cli(&tmp)
        .args(["--json", "dashboard", "unexpected"])
        .output()
        .expect("run dashboard with unknown subcommand");

    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout should be UTF-8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout was not JSON: {error}\n{stdout}"));

    assert_eq!(payload["success"], false);
    assert_eq!(payload["type"], "unknown_subcommand");
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("Unknown dashboard subcommand")));
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}
