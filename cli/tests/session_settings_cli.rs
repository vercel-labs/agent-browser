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

    /// Like `touch`, from another working directory.
    fn touch_in(&self, cwd: &std::path::Path, flags: &[&str]) -> Output {
        let mut args = flags.to_vec();
        args.extend(["read", "http://127.0.0.1:1/", "--timeout", "1000"]);
        self.command()
            .current_dir(cwd)
            .args(&args)
            .output()
            .unwrap()
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

const BLOCKED: &str = "not in the allowed domains list";

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
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
    let after_kill = session.touch(&[]);
    assert_ne!(session.pid(), pid, "the daemon was replaced");
    assert_eq!(fs::read_to_string(session.settings()).unwrap(), stored);
    assert!(text(&after_kill).contains(BLOCKED), "{}", text(&after_kill));

    assert_eq!(session.run(&["close"])["success"], true);
    assert!(!session.settings().exists());
    let after_close = session.touch(&[]);
    assert!(
        !text(&after_close).contains(BLOCKED),
        "{}",
        text(&after_close)
    );
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

#[test]
fn a_cli_driven_restart_keeps_the_settings_and_the_filter() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    let stored = fs::read_to_string(session.settings()).unwrap();
    let pid = session.pid();

    // A changed daemon option makes the CLI stop the daemon and start another.
    let out = session.touch(&["--debug"]);

    assert_ne!(session.pid(), pid, "the daemon was restarted");
    assert_eq!(fs::read_to_string(session.settings()).unwrap(), stored);
    assert!(text(&out).contains(BLOCKED), "{}", text(&out));
}

#[test]
fn a_version_restart_keeps_the_settings_and_the_filter() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    let stored = fs::read_to_string(session.settings()).unwrap();
    let pid = session.pid();
    fs::write(session.dir().join(format!("{SESSION}.version")), "0.0.0").unwrap();

    let out = session.touch(&[]);

    assert_ne!(session.pid(), pid, "the daemon was restarted");
    assert_eq!(fs::read_to_string(session.settings()).unwrap(), stored);
    assert!(text(&out).contains(BLOCKED), "{}", text(&out));
}

#[test]
fn a_close_that_policy_denies_keeps_the_settings() {
    let session = Session::new();
    let policy = session.tmp.path().join("policy.json");
    fs::write(&policy, r#"{"deny":["close"]}"#).unwrap();
    session.touch(&[
        "--allowed-domains",
        "example.com",
        "--action-policy",
        policy.to_str().unwrap(),
    ]);
    assert!(session.settings().exists());

    let out = session.output(&["close"]);

    assert!(!out.status.success(), "{}", text(&out));
    assert!(session.settings().exists(), "a denied close clears nothing");
    let stored = fs::read_to_string(session.settings()).unwrap();
    assert!(stored.contains("example.com") && stored.contains("policy.json"));
    session.kill_daemon();
}

#[test]
fn a_close_parked_for_confirmation_keeps_the_settings() {
    let session = Session::new();
    session.touch(&[
        "--allowed-domains",
        "example.com",
        "--confirm-actions",
        "close",
    ]);
    assert!(session.settings().exists());

    let out = session.output(&["close"]);

    assert!(text(&out).contains("confirm"), "{}", text(&out));
    assert!(session.settings().exists(), "a parked close clears nothing");
    session.kill_daemon();
}

#[test]
fn a_batch_stopped_before_its_close_keeps_the_settings() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);

    let out = session.output(&[
        "batch",
        "--bail",
        "read http://127.0.0.1:1/ --timeout 1000",
        "close",
    ]);

    assert!(!out.status.success(), "{}", text(&out));
    assert!(session.settings().exists(), "the close never ran");
    session.kill_daemon();
}

#[test]
fn a_relative_policy_path_is_stored_absolute_and_keeps_its_directory() {
    let session = Session::new();
    let (a, b) = (session.tmp.path().join("a"), session.tmp.path().join("b"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("policy.json"), "{}").unwrap();
    fs::write(b.join("policy.json"), r#"{"default":"deny"}"#).unwrap();

    session.touch_in(&a, &["--action-policy", "policy.json"]);
    let expected = a.canonicalize().unwrap().join("policy.json");
    let stored: Value =
        serde_json::from_str(&fs::read_to_string(session.settings()).unwrap()).unwrap();
    assert_eq!(stored["actionPolicy"], expected.to_str().unwrap());

    // The next daemon starts in another directory and still uses a's policy.
    session.kill_daemon();
    session.touch_in(&b, &[]);
    let after: Value =
        serde_json::from_str(&fs::read_to_string(session.settings()).unwrap()).unwrap();
    assert_eq!(after["actionPolicy"], expected.to_str().unwrap());
}

#[test]
fn close_all_clears_settings_of_sessions_whose_daemon_is_gone() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    session.kill_daemon();
    assert!(session.settings().exists());
    let refused = session.dir().join("refused.settings");
    fs::write(&refused, "{not json").unwrap();

    let out = session.output(&["close", "--all"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(!session.settings().exists());
    assert!(!refused.exists());
}

#[test]
fn a_batch_close_that_policy_denies_keeps_the_settings() {
    let session = Session::new();
    let policy = session.tmp.path().join("policy.json");
    fs::write(&policy, r#"{"deny":["close"]}"#).unwrap();
    session.touch(&[
        "--allowed-domains",
        "example.com",
        "--action-policy",
        policy.to_str().unwrap(),
    ]);

    let out = session.output(&["batch", "close"]);

    assert!(!out.status.success(), "{}", text(&out));
    assert!(session.settings().exists(), "a denied close clears nothing");
    session.kill_daemon();
}

#[test]
fn close_all_does_not_clear_or_report_a_close_that_was_parked_or_denied() {
    let session = Session::new();
    let deny = session.tmp.path().join("deny.json");
    fs::write(&deny, r#"{"deny":["close"]}"#).unwrap();
    let parked_file = session.dir().join("parked.settings");
    let denied_file = session.dir().join("denied.settings");
    let run = |name: &str, extra: &[&str]| {
        let mut args = vec!["--session", name, "--allowed-domains", "example.com"];
        args.extend_from_slice(extra);
        args.extend(["read", "http://127.0.0.1:1/", "--timeout", "1000"]);
        session.command().args(&args).output().unwrap();
    };
    run("parked", &["--confirm-actions", "close"]);
    run("denied", &["--action-policy", deny.to_str().unwrap()]);
    assert!(parked_file.exists() && denied_file.exists());

    let human = session.command().args(["close", "--all"]).output().unwrap();
    let json = session.run(&["close", "--all"]);

    let human_text = text(&human);
    assert!(!human.status.success(), "{human_text}");
    assert!(!human_text.contains("Closed session"), "{human_text}");
    assert_eq!(json["data"]["closed"], 0, "{json}");
    assert_eq!(json["success"], false, "{json}");
    assert!(parked_file.exists(), "a parked close clears nothing");
    assert!(denied_file.exists(), "a denied close clears nothing");
    for name in ["parked", "denied"] {
        let pid = fs::read_to_string(session.dir().join(format!("{name}.pid"))).unwrap();
        assert!(Command::new("kill")
            .args(["-9", pid.trim()])
            .status()
            .unwrap()
            .success());
    }
}

#[test]
fn close_with_a_dead_daemon_is_still_governed_by_the_stored_policy() {
    let session = Session::new();
    let policy = session.tmp.path().join("policy.json");
    fs::write(&policy, r#"{"deny":["close"]}"#).unwrap();
    session.touch(&[
        "--allowed-domains",
        "example.com",
        "--action-policy",
        policy.to_str().unwrap(),
    ]);
    let stored = fs::read_to_string(session.settings()).unwrap();
    session.kill_daemon();

    let out = session.output(&["close"]);

    assert!(!out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("denied by policy"), "{}", text(&out));
    assert_eq!(fs::read_to_string(session.settings()).unwrap(), stored);
    let after = session.touch(&[]);
    assert!(text(&after).contains(BLOCKED), "{}", text(&after));
    session.kill_daemon();
}

#[test]
fn close_clears_a_corrupt_file_when_the_daemon_is_dead() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    session.kill_daemon();
    session.write("{not json");

    let out = session.output(&["close"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(!session.settings().exists());
}

#[test]
fn close_all_on_a_dead_session_is_governed_by_its_stored_policy() {
    let session = Session::new();
    let policy = session.tmp.path().join("policy.json");
    fs::write(&policy, r#"{"deny":["close"]}"#).unwrap();
    session.touch(&[
        "--allowed-domains",
        "example.com",
        "--action-policy",
        policy.to_str().unwrap(),
    ]);
    let stored = fs::read_to_string(session.settings()).unwrap();
    session.kill_daemon();

    let out = session.output(&["close", "--all"]);

    assert!(!out.status.success(), "{}", text(&out));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["data"]["closed"], 0, "{json}");
    assert_eq!(json["data"]["failed"][0]["session"], "sec", "{json}");
    assert!(
        json["data"]["failed"][0]["error"]
            .as_str()
            .unwrap()
            .contains("denied by policy"),
        "{json}"
    );
    assert_eq!(fs::read_to_string(session.settings()).unwrap(), stored);
    session.kill_daemon();
}

#[test]
fn close_all_closes_a_dead_session_and_removes_its_file() {
    let session = Session::new();
    session.touch(&["--allowed-domains", "example.com"]);
    session.kill_daemon();

    let out = session.output(&["close", "--all"]);

    assert!(out.status.success(), "{}", text(&out));
    let json: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["data"]["sessions"][0], "sec", "{json}");
    assert_eq!(json["data"]["closed"], 1, "{json}");
    assert!(!session.settings().exists());
    let pid = session.dir().join(format!("{SESSION}.pid"));
    assert!(!pid.exists(), "the daemon started for the close is gone");
}

#[test]
fn close_all_clears_a_corrupt_file_of_a_dead_session() {
    let session = Session::new();
    fs::create_dir_all(session.dir()).unwrap();
    session.write("{not json");

    let out = session.output(&["close", "--all"]);

    assert!(out.status.success(), "{}", text(&out));
    assert!(!session.settings().exists());
}
