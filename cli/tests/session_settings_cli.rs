//! Security settings outlive the daemon and go away with `close`.
//!
//! `read` starts a daemon without a browser, so these run without Chrome. The
//! daemon is started with the settings in its environment, as the CLI does for
//! `--allowed-domains`, and stores them at boot.
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "sec";

struct Session {
    tmp: TempDir,
}

impl Session {
    fn new() -> Self {
        Self {
            tmp: TempDir::new().unwrap(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.tmp.path().join("sockets")
    }

    fn settings(&self) -> PathBuf {
        self.dir().join(format!("{SESSION}.settings"))
    }

    fn write(&self, contents: &str) {
        fs::write(self.settings(), contents).unwrap();
        fs::set_permissions(self.settings(), fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
                cmd.env_remove(key);
            }
        }
        cmd.current_dir(self.tmp.path())
            .env("HOME", self.tmp.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir())
            .env("NO_COLOR", "1")
            .args(["--session", SESSION, "--json"]);
        cmd
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Value {
        let out = self.output(args);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "args={args:?} stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    /// Any command that reaches the daemon without a browser.
    fn touch(&self, flags: &[&str]) -> Output {
        let mut args = flags.to_vec();
        args.extend(["read", "http://127.0.0.1:1/", "--timeout", "1000"]);
        self.output(&args)
    }

    fn pid(&self) -> u32 {
        fs::read_to_string(self.dir().join(format!("{SESSION}.pid")))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn kill_daemon(&self) {
        let pid = self.pid();
        assert!(Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status()
            .unwrap()
            .success());
        while Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success()
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.command().arg("close").output();
    }
}

#[test]
fn stored_settings_survive_a_killed_daemon_until_close() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    let stored = fs::read_to_string(session.settings()).unwrap();
    assert!(stored.contains("example.com"), "{stored}");
    assert_eq!(
        fs::metadata(session.settings())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let pid = session.pid();
    session.kill_daemon();
    session.touch(&[]);
    assert_ne!(session.pid(), pid, "the daemon was replaced");
    assert_eq!(fs::read_to_string(session.settings()).unwrap(), stored);

    assert_eq!(session.run(&["close"])["success"], true);
    assert!(!session.settings().exists());
}

#[test]
fn a_file_the_daemon_refuses_is_named_and_cleared_by_close() {
    let session = Session::new();
    fs::create_dir_all(session.dir()).unwrap();
    session.write("{not json");

    let out = session.touch(&[]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "{text}");
    assert!(text.contains("sec.settings"), "{text}");
    assert!(text.contains("agent-browser --session sec close"), "{text}");
    assert_eq!(fs::read_to_string(session.settings()).unwrap(), "{not json");

    assert_eq!(session.run(&["close"])["success"], true);
    assert!(!session.settings().exists());
    let after = session.touch(&[]);
    assert!(!String::from_utf8_lossy(&after.stderr).contains("sec.settings"));
}

#[test]
fn a_file_readable_by_others_stops_the_daemon() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    session.kill_daemon();
    fs::set_permissions(session.settings(), fs::Permissions::from_mode(0o644)).unwrap();

    let out = session.touch(&[]);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("0644"), "{text}");
}

#[test]
fn close_all_clears_the_stored_settings() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    assert!(session.settings().exists());

    let out = session.output(&["close", "--all"]);

    assert!(out.status.success(), "{out:?}");
    assert!(!session.settings().exists());
}

#[test]
fn batch_close_clears_the_stored_settings() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    assert!(session.settings().exists());

    session.run(&["batch", "close"]);

    assert!(!session.settings().exists());
}

#[test]
fn close_clears_a_file_when_no_daemon_is_running() {
    let session = Session::new();
    fs::create_dir_all(session.dir()).unwrap();
    session.write("{}");

    session.run(&["close"]);

    assert!(!session.settings().exists());
}
