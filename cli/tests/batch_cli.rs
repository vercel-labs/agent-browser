//! Integration tests for omitting raw commands from batch JSON output.

use serde_json::Value;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "batch-omit-command-test";

struct BatchCli {
    socket_dir: TempDir,
    home: TempDir,
}

impl BatchCli {
    fn new() -> Self {
        Self {
            socket_dir: TempDir::new().unwrap(),
            home: TempDir::new().unwrap(),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(["--session", SESSION])
            .env("AGENT_BROWSER_SOCKET_DIR", self.socket_dir.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env("NO_COLOR", "1");
        command
    }

    fn run(&self, input: &str, args: &[&str]) -> Output {
        let mut child = self
            .command()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to start agent-browser batch");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .expect("failed to write batch input");
        child
            .wait_with_output()
            .expect("batch command failed to wait")
    }
}

impl Drop for BatchCli {
    fn drop(&mut self) {
        let _ = self.command().arg("close").output();
    }
}

#[test]
fn batch_omit_command_hides_input_and_preserves_row_identity() {
    let cli = BatchCli::new();
    let input = r#"[["stream","status"],["definitely-unknown","PUBLIC_SENTINEL"]]"#;
    let output = cli.run(input, &["batch", "--json", "--omit-command"]);

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let stderr = String::from_utf8(output.stderr).expect("stderr should be utf8");
    assert!(!stdout.contains("PUBLIC_SENTINEL"));
    assert!(!stderr.contains("PUBLIC_SENTINEL"));

    let rows: Value = serde_json::from_str(&stdout).expect("stdout should be JSON");
    let rows = rows.as_array().expect("batch output should be an array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["index"], 0);
    assert_eq!(rows[0]["success"], true);
    assert!(rows[0].get("command").is_none());
    assert_eq!(rows[1]["index"], 1);
    assert_eq!(rows[1]["success"], false);
    assert!(rows[1].get("command").is_none());
}

#[test]
fn batch_omit_command_preserves_default_and_bail_behavior() {
    let cli = BatchCli::new();
    let input =
        r##"[["definitely-unknown","PUBLIC_SENTINEL"],["fill","#password","SECOND_SENTINEL"]]"##;

    let default = cli.run(input, &["batch", "--json"]);
    assert_eq!(default.status.code(), Some(1));
    let default_stdout = String::from_utf8(default.stdout).expect("stdout should be utf8");
    let rows: Value = serde_json::from_str(&default_stdout).expect("stdout should be JSON");
    assert_eq!(rows[0]["command"][1], "PUBLIC_SENTINEL");
    assert!(rows[0].get("index").is_none());

    let omitted = cli.run(input, &["batch", "--json", "--omit-command", "--bail"]);
    assert_eq!(omitted.status.code(), Some(1));
    let stdout = String::from_utf8(omitted.stdout).expect("stdout should be utf8");
    let stderr = String::from_utf8(omitted.stderr).expect("stderr should be utf8");
    assert!(!stdout.contains("PUBLIC_SENTINEL"));
    assert!(!stdout.contains("SECOND_SENTINEL"));
    assert!(!stderr.contains("PUBLIC_SENTINEL"));
    assert!(!stderr.contains("SECOND_SENTINEL"));
    let rows: Value = serde_json::from_str(&stdout).expect("stdout should be JSON");
    let rows = rows.as_array().expect("batch output should be an array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["index"], 0);
    assert!(rows[0].get("command").is_none());
}
