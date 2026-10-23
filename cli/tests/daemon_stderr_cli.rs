//! Windows daemon logging must outlive both CLI and MCP command subprocesses.
#![cfg(windows)]

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "stderr-integration";

struct DaemonCleanup(Command);

impl Drop for DaemonCleanup {
    fn drop(&mut self) {
        let _ = self
            .0
            .args(["close"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn windows_cli_and_mcp_share_daemon_with_independent_debug_log() {
    let tmp = tempfile::tempdir().unwrap();
    let command = || {
        let mut cmd = Command::new(BIN);
        cmd.current_dir(tmp.path())
            .env("AGENT_BROWSER_SOCKET_DIR", tmp.path())
            .env("AGENT_BROWSER_SESSION", SESSION)
            .env("AGENT_BROWSER_DEBUG", "1")
            .env("AGENT_BROWSER_IDLE_TIMEOUT_MS", "0")
            .env("HOME", tmp.path())
            .env("USERPROFILE", tmp.path())
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env_remove("AGENT_BROWSER_CONFIG")
            .env_remove("AGENT_BROWSER_DAEMON");
        cmd
    };
    let _cleanup = DaemonCleanup(command());
    let status = command()
        .args(["stream", "status"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    // The launching CLI has exited and dropped its daemon stderr pipe reader.
    let pid_path = tmp.path().join(format!("{SESSION}.pid"));
    let original_pid = fs::read_to_string(&pid_path).unwrap();

    let cli = command()
        .args(["--json", "stream", "status"])
        .output()
        .unwrap();
    assert!(cli.status.success(), "{cli:?}");
    let cli_response: Value = serde_json::from_slice(&cli.stdout).unwrap();
    assert_eq!(cli_response["success"], true);

    let mut mcp = command()
        .args(["mcp", "--tools", "all"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        mcp.stdin.take().unwrap(),
        "{}",
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "agent_browser_stream_status", "arguments": {"session": SESSION}
        }})
    )
    .unwrap();
    let output = mcp.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    assert_eq!(
        response["result"]["structuredContent"]["response"]["success"],
        true
    );

    assert_eq!(fs::read_to_string(&pid_path).unwrap(), original_pid);
    let log = fs::read_to_string(tmp.path().join(format!("{SESSION}.log"))).unwrap();
    assert!(log.contains(&format!("Debug logging started for session: {SESSION}")));
}
