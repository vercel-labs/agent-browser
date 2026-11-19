//! A pending `wait` must not block its session: other commands run while it
//! polls, it ends when its client disconnects, `close` ends it, and SIGTERM
//! stops the daemon and its Chrome while one is pending.

#![cfg(unix)]

use serde_json::Value;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const SESSION: &str = "wait-test";
/// Far longer than any test, so only the behavior under test can end it.
const WAIT_TIMEOUT_MS: &str = "600000";
/// Counts polls so a test can tell whether the wait is still running.
const COUNTING_FN: &str = "(window.__polls = (window.__polls || 0) + 1, false)";

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        std::fs::create_dir(fixture.0.path().join("s")).unwrap();
        fixture
    }

    fn command(&self, args: &[&str]) -> Command {
        let root = self.0.path();
        let mut command = Command::new(env!("CARGO_BIN_EXE_agent-browser"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root)
            .env("TMPDIR", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("AGENT_BROWSER_SOCKET_DIR", root.join("s"))
            .env("NO_COLOR", "1")
            .current_dir(root)
            .args(["--session", SESSION])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Run a command and return its JSON response.
    fn run(&self, args: &[&str]) -> Value {
        let output = self.command(args).arg("--json").output().unwrap();
        json_output(&output)
    }

    fn open(&self) {
        let mut args = vec!["open", "data:text/html,<p>wait</p>", "--json"];
        let executable = std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok();
        let browser_args = std::env::var("AGENT_BROWSER_ARGS").ok();
        for (flag, value) in [
            ("--executable-path", executable.as_deref()),
            ("--args", browser_args.as_deref()),
        ] {
            if let Some(value) = value {
                args.extend([flag, value]);
            }
        }
        let output = self.command(&args).output().unwrap();
        assert_eq!(json_output(&output)["success"], true, "{output:?}");
    }

    /// Start a wait that only cancellation can end, and return once it polls.
    fn start_wait(&self) -> Child {
        let child = self
            .command(&[
                "wait",
                "--fn",
                COUNTING_FN,
                "--timeout",
                WAIT_TIMEOUT_MS,
                "--json",
            ])
            .spawn()
            .unwrap();
        self.until("the wait to start polling", || {
            self.polls().is_some_and(|n| n > 0)
        });
        child
    }

    fn polls(&self) -> Option<i64> {
        self.run(&["eval", "window.__polls || 0"])["data"]["result"].as_i64()
    }

    fn daemon_pid(&self) -> i32 {
        let path = self.0.path().join(format!("s/{SESSION}.pid"));
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn until(&self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.0.path().join(format!("s/{SESSION}.pid"))) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }
}

fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"))
}

/// Wait for a client to exit, failing the test instead of hanging on it.
fn wait_output(mut child: Child, within: Duration) -> Output {
    let deadline = Instant::now() + within;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("the wait client did not exit within {within:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

/// Whether a process is running. An exited daemon or Chrome is reparented,
/// so it can linger as a zombie, which `kill(pid, 0)` still reports.
fn alive(pid: i32) -> bool {
    let output = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&output.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

fn child_pids(pid: i32) -> Vec<i32> {
    let output = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

#[test]
#[ignore = "requires Chrome"]
fn e2e_commands_run_while_a_wait_is_pending() {
    let fixture = Fixture::new();
    fixture.open();
    let wait = fixture.start_wait();

    let started = Instant::now();
    let url = fixture.run(&["get", "url"]);
    assert_eq!(url["success"], true, "{url}");
    assert_eq!(url["data"]["url"], "data:text/html,<p>wait</p>");
    let eval = fixture.run(&["eval", "1 + 1"]);
    assert_eq!(eval["data"]["result"], 2, "{eval}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "commands queued behind the wait: {:?}",
        started.elapsed()
    );

    let mut wait = wait;
    assert!(wait.try_wait().unwrap().is_none(), "the wait ended early");
    assert_eq!(fixture.run(&["close"])["success"], true);
    wait_output(wait, Duration::from_secs(10));
}

#[test]
#[ignore = "requires Chrome"]
fn e2e_a_wait_ends_when_its_client_disconnects() {
    let fixture = Fixture::new();
    fixture.open();
    let mut wait = fixture.start_wait();

    wait.kill().unwrap();
    wait.wait().unwrap();
    // Give the daemon a moment to notice, then the poll count must hold.
    thread::sleep(Duration::from_millis(500));
    let before = fixture.polls().unwrap();
    thread::sleep(Duration::from_secs(1));
    let after = fixture.polls().unwrap();
    assert_eq!(before, after, "the wait kept polling after its client left");

    assert_eq!(fixture.run(&["close"])["success"], true);
}

#[test]
#[ignore = "requires Chrome"]
fn e2e_close_ends_a_pending_wait() {
    let fixture = Fixture::new();
    fixture.open();
    let pid = fixture.daemon_pid();
    let wait = fixture.start_wait();

    let started = Instant::now();
    let close = fixture.run(&["close"]);
    assert_eq!(close["success"], true, "{close}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "close queued behind the wait: {:?}",
        started.elapsed()
    );

    let output = wait_output(wait, Duration::from_secs(10));
    let response = json_output(&output);
    assert_eq!(response["success"], false, "{response}");
    assert_eq!(
        response["error"], "Wait cancelled: the session was closed",
        "{response}"
    );
    fixture.until("the daemon to exit", || !alive(pid));
}

#[test]
#[ignore = "requires Chrome"]
fn e2e_sigterm_during_a_wait_stops_the_daemon_and_chrome() {
    let fixture = Fixture::new();
    fixture.open();
    let pid = fixture.daemon_pid();
    let browser = child_pids(pid);
    assert!(!browser.is_empty(), "Chrome should be the daemon's child");
    let wait = fixture.start_wait();

    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        assert!(
            Instant::now() < deadline,
            "the daemon outlived SIGTERM by 5s while a wait was pending"
        );
        thread::sleep(Duration::from_millis(50));
    }
    fixture.until("Chrome to exit", || {
        browser.iter().all(|&chrome| !alive(chrome))
    });

    let output = wait_output(wait, Duration::from_secs(10));
    let response = json_output(&output);
    assert_eq!(response["success"], false, "{response}");
}
