//! Exercise concurrent cold starts through the real CLI and MCP without Chrome.
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("CLI timed out during session startup");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

struct Session {
    dir: TempDir,
}

impl Session {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("config.json"), "{}").unwrap();
        Self { dir }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.dir.path())
            .env("USERPROFILE", self.dir.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir.path())
            // Bound orphan lifetime if the regression fails before cleanup.
            .env("AGENT_BROWSER_IDLE_TIMEOUT_MS", "15000")
            .env("NO_COLOR", "1")
            .current_dir(self.dir.path())
            .args(["--config", "config.json", "--session", "race", "--json"])
            .stdin(Stdio::null());
        command
    }

    fn spawn_status(&self, index: usize, mcp: bool) -> Child {
        let mut command = self.command();
        if mcp {
            command
                .args(["mcp", "--tools", "all"])
                .stdin(Stdio::piped());
        } else {
            command.args(["stream", "status"]);
        }
        // Files avoid inherited output pipes keeping Windows readers blocked.
        let mut child = command
            .stdout(fs::File::create(self.dir.path().join(format!("{index}.out"))).unwrap())
            .stderr(fs::File::create(self.dir.path().join(format!("{index}.err"))).unwrap())
            .spawn()
            .unwrap();
        if mcp {
            writeln!(
                child.stdin.take().unwrap(),
                "{}",
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {
                        "name": "agent_browser_stream_status",
                        "arguments": { "session": "race" }
                    }
                })
            )
            .unwrap();
        }
        child
    }

    fn assert_status(&self, index: usize, status: ExitStatus, mcp: bool) -> u64 {
        let stdout = fs::read_to_string(self.dir.path().join(format!("{index}.out"))).unwrap();
        let stderr = fs::read_to_string(self.dir.path().join(format!("{index}.err"))).unwrap();
        assert!(status.success(), "{status}: {stdout}\n{stderr}");
        let mut response: Value = serde_json::from_str(&stdout).unwrap();
        if mcp {
            assert!(response.get("error").is_none(), "{response}");
            assert_eq!(response["result"]["isError"], false, "{response}");
            response = response["result"]["structuredContent"]["response"].take();
        }
        assert_eq!(response["success"], true, "{response}");
        assert_eq!(response["data"]["connected"], false, "{response}");
        response["data"]["port"]
            .as_u64()
            .expect("daemon has no stream listener")
    }

    fn pid(&self) -> String {
        fs::read_to_string(self.dir.path().join("race.pid")).unwrap()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.dir.path().join("race.pid").exists() {
            if let Ok(mut child) = self
                .command()
                .arg("close")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                let deadline = Instant::now() + Duration::from_secs(5);
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn concurrent_cold_start(mcp: bool) {
    let session = Session::new();
    let barrier = Arc::new(Barrier::new(2));
    let mut children = thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|index| {
                let barrier = barrier.clone();
                let session = &session;
                scope.spawn(move || {
                    barrier.wait();
                    session.spawn_status(index, mcp)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    // Reap both CLI processes before checking their output, even on failure.
    let statuses: Vec<_> = children.iter_mut().map(wait).collect();
    let ports: Vec<_> = statuses
        .into_iter()
        .enumerate()
        .map(|(index, status)| session.assert_status(index, status, mcp))
        .collect();
    assert_eq!(ports[0], ports[1], "cold callers reached different daemons");
    let pid = session.pid();
    let mut follow_up = session.spawn_status(2, false);
    assert_eq!(
        session.assert_status(2, wait(&mut follow_up), false),
        ports[0]
    );
    assert_eq!(session.pid(), pid, "a warm command replaced the daemon");
}

#[test]
fn concurrent_cold_start_reuses_one_daemon() {
    concurrent_cold_start(false);
}

#[test]
fn concurrent_mcp_cold_start_reuses_one_daemon() {
    concurrent_cold_start(true);
}

#[test]
fn startup_waits_for_lock_before_creating_sidecars() {
    let session = Session::new();
    let path = session.dir.path().join("race.startup.lock");
    let lock = fs::File::create(&path).unwrap();
    lock.lock().unwrap();
    let mut child = session.spawn_status(0, false);
    thread::sleep(Duration::from_millis(200));
    let still_waiting = child.try_wait().unwrap().is_none();
    let pid_created = session.dir.path().join("race.pid").exists();
    drop(lock);
    let status = wait(&mut child);
    assert!(still_waiting, "CLI ignored the held startup lock");
    assert!(!pid_created, "CLI started a daemon without owning the lock");
    session.assert_status(0, status, false);
    assert!(path.exists(), "startup removed the shared lock inode");
}

// Run this helper in a separate test process so abrupt termination, rather
// than Rust's normal guard drop, must release the OS lock.
#[test]
#[ignore = "helper process for startup_recovers_after_lock_owner_is_killed"]
fn startup_lock_holder_process() {
    let path =
        std::env::var_os("AGENT_BROWSER_STARTUP_LOCK_TEST").expect("missing helper lock path");
    let path = std::path::PathBuf::from(path);
    let lock = fs::File::create(&path).unwrap();
    lock.lock().unwrap();
    fs::write(path.with_extension("ready"), "ready").unwrap();
    // Bound helper lifetime even if the parent test unexpectedly exits.
    thread::sleep(Duration::from_secs(30));
    drop(lock);
}

#[test]
fn startup_recovers_after_lock_owner_is_killed() {
    let session = Session::new();
    let path = session.dir.path().join("race.startup.lock");
    let mut owner = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "startup_lock_holder_process"])
        .env("AGENT_BROWSER_STARTUP_LOCK_TEST", &path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.with_extension("ready").exists() {
        if Instant::now() >= deadline || owner.try_wait().unwrap().is_some() {
            let _ = owner.kill();
            let _ = owner.wait();
            panic!("lock holder did not become ready");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let mut child = session.spawn_status(0, false);
    thread::sleep(Duration::from_millis(200));
    let was_blocked = child.try_wait().unwrap().is_none();
    owner.kill().unwrap();
    owner.wait().unwrap();
    let status = wait(&mut child);
    assert!(was_blocked, "CLI ignored the other process's lock");
    session.assert_status(0, status, false);
    assert!(path.exists(), "recovery removed the shared lock inode");
}
