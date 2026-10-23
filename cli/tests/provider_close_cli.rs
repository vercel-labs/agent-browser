//! Closing a provider-selected session must not allocate a provider first.
#![cfg(unix)]

use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "provider-close-fixture";

struct Fixture {
    root: TempDir,
    calls: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let calls = root.path().join("provider-calls");
        let script = root.path().join("provider.sh");
        let quoted = format!("'{}'", calls.to_string_lossy().replace('\'', "'\\''"));
        std::fs::write(&script, format!(
            "#!/bin/sh\ncat >/dev/null\nprintf 'called\\n' >> {quoted}\nprintf '%s\\n' '{{\"protocol\":\"agent-browser.plugin.v1\",\"success\":false,\"error\":\"provider launch sentinel\"}}'\n"
        )).unwrap();
        std::fs::write(
            root.path().join("config.json"),
            json!({"plugins": [{
                "name": "close-fixture", "command": "/bin/sh", "args": [script],
                "capabilities": ["browser.provider"]
            }]})
            .to_string(),
        )
        .unwrap();
        Self { root, calls }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        for (name, _) in std::env::vars_os() {
            if name.to_string_lossy().starts_with("AGENT_BROWSER_") {
                command.env_remove(name);
            }
        }
        command
            .args(["--session", SESSION])
            .env("HOME", self.root.path())
            .env("USERPROFILE", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("XDG_DATA_HOME", self.root.path().join("data"))
            .env("XDG_CACHE_HOME", self.root.path().join("cache"))
            .env_remove("XDG_RUNTIME_DIR")
            .env("AGENT_BROWSER_SOCKET_DIR", self.root.path())
            .env("AGENT_BROWSER_CONFIG", self.root.path().join("config.json"))
            .env("NO_COLOR", "1");
        command
    }

    fn assert_close(&self, via_env: bool, warm: bool) {
        if warm {
            let output = self
                .command()
                .args(["stream", "status", "--json"])
                .output()
                .unwrap();
            assert!(output.status.success(), "bootstrap failed: {output:?}");
            assert!(self.root.path().join(format!("{SESSION}.pid")).exists());
        }
        let mut command = self.command();
        if via_env {
            command.env("AGENT_BROWSER_PROVIDER", "close-fixture");
        } else {
            command.args(["--provider", "close-fixture"]);
        }
        let output = command.args(["close", "--json"]).output().unwrap();
        assert!(output.status.success(), "close failed: {output:?}");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["success"], true, "{response}");
        assert!(
            !self.calls.exists(),
            "close invoked the provider launch hook"
        );
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.root.path().join(format!("{SESSION}.pid")).exists() {
            // No provider selection on failure cleanup, even on unfixed builds.
            let _ = self.command().args(["close", "--json"]).output();
        }
    }
}

#[test]
fn cold_explicit_provider_close_does_not_launch() {
    Fixture::new().assert_close(false, false);
}

#[test]
fn cold_environment_provider_close_does_not_launch() {
    Fixture::new().assert_close(true, false);
}

#[test]
fn existing_daemon_explicit_provider_close_does_not_launch() {
    Fixture::new().assert_close(false, true);
}

#[test]
fn existing_daemon_environment_provider_close_does_not_launch() {
    Fixture::new().assert_close(true, true);
}

#[test]
fn browser_commands_still_invoke_selected_provider() {
    for via_env in [false, true] {
        let fixture = Fixture::new();
        let mut command = fixture.command();
        if via_env {
            command.env("AGENT_BROWSER_PROVIDER", "close-fixture");
        } else {
            command.args(["--provider", "close-fixture"]);
        }
        let output = command
            .args(["open", "about:blank", "--json"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            fixture.calls.exists(),
            "browser command bypassed the provider"
        );
    }
}

#[test]
fn mcp_close_uses_nonlaunching_cli_path() {
    let fixture = Fixture::new();
    let mut child = fixture
        .command()
        .env("AGENT_BROWSER_PROVIDER", "close-fixture")
        .args(["mcp", "--tools", "core"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"provider-close-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agent_browser_close","arguments":{"session":SESSION}}}),
    ] {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "MCP process failed: {output:?}");
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let response = responses
        .iter()
        .find(|v| v["id"] == 2)
        .expect("missing tool response");
    assert_eq!(
        response["result"]["structuredContent"]["exitCode"], 0,
        "{response}"
    );
    assert!(!fixture.calls.exists(), "MCP close launched a provider");
}
