#![cfg(unix)]

use std::fs;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const TIMEOUT: Duration = Duration::from_secs(15);

fn socket_dir() -> TempDir {
    // Keep the path short: Unix socket paths are limited to ~104 bytes.
    Builder::new().prefix("ab-acq").tempdir_in("/tmp").unwrap()
}

fn command(dir: &Path, session: &str) -> Command {
    let mut cmd = Command::new(BIN);
    cmd.env("AGENT_BROWSER_SESSION", session)
        .env("AGENT_BROWSER_SOCKET_DIR", dir)
        .env("AGENT_BROWSER_IDLE_TIMEOUT_MS", "0")
        .env_remove("AGENT_BROWSER_CDP")
        .env_remove("AGENT_BROWSER_AUTO_CONNECT")
        .env_remove("AGENT_BROWSER_PROVIDER")
        .env_remove("AGENT_BROWSER_NAMESPACE")
        .env_remove("XDG_RUNTIME_DIR");
    cmd
}

fn spawn_daemon(dir: &Path, session: &str) -> Child {
    command(dir, session)
        .env("AGENT_BROWSER_DAEMON", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn wait_for(cond: impl Fn() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < TIMEOUT, "timed out waiting for condition");
        thread::sleep(Duration::from_millis(20));
    }
}

fn kill_daemon(dir: &Path, session: &str) {
    if let Some(pid) = fs::read_to_string(dir.join(format!("{session}.pid")))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

/// A daemon that loses the bind to a live one exits and leaves that daemon's
/// pid and socket in place.
#[test]
fn second_daemon_for_a_session_leaves_the_first_one_alone() {
    let dir = socket_dir();
    let sock = dir.path().join("acq.sock");
    let pid_file = dir.path().join("acq.pid");

    let mut first = spawn_daemon(dir.path(), "acq");
    wait_for(|| sock.exists() && pid_file.exists());
    wait_for(|| UnixStream::connect(&sock).is_ok());
    let first_pid = first.id().to_string();
    assert_eq!(fs::read_to_string(&pid_file).unwrap().trim(), first_pid);

    let mut second = spawn_daemon(dir.path(), "acq");
    let start = Instant::now();
    let status = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break Some(status);
        }
        if start.elapsed() > TIMEOUT {
            let _ = second.kill();
            let _ = second.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };

    assert!(
        status.is_some_and(|s| !s.success()),
        "second daemon must exit instead of taking over the session"
    );
    assert_eq!(
        fs::read_to_string(&pid_file).unwrap().trim(),
        first_pid,
        "second daemon overwrote the pid file"
    );
    assert!(
        UnixStream::connect(&sock).is_ok(),
        "first daemon lost its socket"
    );
    assert!(
        first.try_wait().unwrap().is_none(),
        "first daemon must keep running"
    );

    let _ = first.kill();
    let _ = first.wait();
}

/// Two commands that start the same session at once both end up on one daemon.
#[test]
fn concurrent_first_commands_share_one_daemon() {
    let dir = socket_dir();

    for round in 0..16 {
        let session = format!("race{round}");
        let run = || {
            let mut cmd = command(dir.path(), &session);
            cmd.args(["get", "cdp-url"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            cmd.spawn().unwrap()
        };
        let a = run();
        let b = run();
        for child in [a, b] {
            let out = child.wait_with_output().unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                !text.contains("Daemon") && !text.contains("daemon"),
                "round {round}: {text}"
            );
        }
        kill_daemon(dir.path(), &session);
    }
}
