use serde_json::{json, Value};
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

struct Session {
    dir: TempDir,
}

impl Session {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        Self { dir }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir.path())
            .env("NO_COLOR", "1")
            .current_dir(self.dir.path())
            .args(["--config", "config.json", "--session", "o", "--json"])
            .stdin(Stdio::null());
        command
    }

    fn prime(&self, bypass: Option<&str>) -> String {
        let mut command = self.command();
        if let Some(bypass) = bypass {
            command.env("AGENT_BROWSER_PROXY_BYPASS", bypass);
        }
        let output = command.args(["stream", "status"]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(payload(&output)["data"]["connected"], false);
        self.pid()
    }

    fn pid(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("o.pid")).unwrap()
    }

    fn launch(&self, engine: &str) -> Command {
        let mut command = self.command();
        command.args([
            "--engine",
            engine,
            "--executable-path",
            "/nonexistent/obscura-review-probe",
            "open",
            "about:blank",
        ]);
        command
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.dir.path().join("o.pid").exists() {
            let _ = self.command().arg("close").output();
        }
    }
}

fn payload(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid JSON: {error}; {output:?}"))
}

fn assert_bypass_rejected(output: &Output) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let response = payload(output);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("--proxy-bypass")),
        "{response}"
    );
    assert!(
        !response["error"]
            .as_str()
            .unwrap()
            .contains("Failed to launch"),
        "{response}"
    );
}

#[test]
fn obscura_existing_daemon_rejects_cli_proxy_bypass() {
    let session = Session::new();
    let pid = session.prime(None);
    let output = session
        .launch("obscura")
        .args(["--proxy-bypass", "localhost"])
        .output()
        .unwrap();
    assert_eq!(session.pid(), pid);
    assert_bypass_rejected(&output);
}

#[test]
fn obscura_existing_daemon_rejects_no_proxy() {
    let session = Session::new();
    let pid = session.prime(None);
    let output = session
        .launch("obscura")
        .env("NO_PROXY", "localhost")
        .output()
        .unwrap();
    assert_eq!(session.pid(), pid);
    assert_bypass_rejected(&output);
}

#[test]
fn obscura_existing_daemon_rejects_config_proxy_bypass() {
    let session = Session::new();
    let pid = session.prime(None);
    std::fs::write(
        session.dir.path().join("config.json"),
        json!({"proxyBypass": "localhost"}).to_string(),
    )
    .unwrap();
    let output = session.launch("obscura").output().unwrap();
    assert_eq!(session.pid(), pid);
    assert_bypass_rejected(&output);
}

#[test]
fn obscura_existing_daemon_does_not_reuse_stale_proxy_bypass() {
    let session = Session::new();
    let pid = session.prime(Some("localhost"));
    let output = session.launch("obscura").output().unwrap();
    assert_eq!(session.pid(), pid);
    assert_eq!(output.status.code(), Some(1));
    let response = payload(&output);
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("Failed to launch Obscura")),
        "{response}"
    );
}

#[test]
fn obscura_proxy_bypass_fix_preserves_other_engine_validation() {
    for engine in ["chrome", "lightpanda"] {
        let session = Session::new();
        let pid = session.prime(None);
        let output = session
            .launch(engine)
            .args(["--proxy-bypass", "localhost"])
            .output()
            .unwrap();
        assert_eq!(session.pid(), pid);
        assert_eq!(output.status.code(), Some(1));
        let response = payload(&output);
        let error = response["error"].as_str().unwrap();
        assert!(
            error.contains("/nonexistent/obscura-review-probe"),
            "{response}"
        );
        assert!(!error.contains("--proxy-bypass"), "{response}");
    }
}
