#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::{Builder, TempDir};

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "sf";
const TIMEOUT: Duration = Duration::from_secs(15);

struct Daemon {
    child: Child,
    dir: TempDir,
}

impl Daemon {
    fn start() -> Self {
        // Keep the path short: Unix socket paths are limited to ~104 bytes.
        let dir = Builder::new().prefix("ab-sf").tempdir_in("/tmp").unwrap();
        let child = Command::new(BIN)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", SESSION)
            .env("AGENT_BROWSER_SOCKET_DIR", dir.path())
            .env("AGENT_BROWSER_IDLE_TIMEOUT_MS", "0")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env_remove("XDG_RUNTIME_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Self { child, dir };
        daemon.wait_for(|| daemon.path("sock").exists() && daemon.path("pid").exists());
        daemon
    }

    fn path(&self, ext: &str) -> PathBuf {
        self.dir.path().join(format!("{SESSION}.{ext}"))
    }

    fn wait_for(&self, cond: impl Fn() -> bool) {
        let start = Instant::now();
        while !cond() {
            assert!(start.elapsed() < TIMEOUT, "timed out waiting for condition");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn request_shutdown(&self) {
        let mut stream = UnixStream::connect(self.path("sock")).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        stream
            .write_all(b"{\"id\":\"t\",\"action\":\"__agent_browser_internal_shutdown\"}\n")
            .unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert!(line.contains("\"t\""), "unexpected response: {line}");
    }

    fn wait_for_exit(&mut self) {
        let start = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(start.elapsed() < TIMEOUT, "daemon did not exit");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

#[test]
fn test_daemon_shutdown_removes_daemon_files_and_keeps_persistent_ones() {
    let mut d = Daemon::start();
    for ext in ["config", "engine", "provider", "extensions"] {
        fs::write(d.path(ext), format!("dummy-{ext}")).unwrap();
    }
    fs::write(d.path("target"), "target-content").unwrap();
    fs::write(d.path("log"), "log-content").unwrap();

    d.request_shutdown();
    d.wait_for_exit();

    for ext in [
        "sock",
        "pid",
        "version",
        "config",
        "stream",
        "engine",
        "provider",
        "extensions",
    ] {
        assert!(!d.path(ext).exists(), "{ext} should be removed");
    }
    assert_eq!(read(&d.path("target")), "target-content");
    assert_eq!(read(&d.path("log")), "log-content");
}

#[test]
fn test_daemon_shutdown_keeps_files_of_replacement_daemon() {
    let mut d = Daemon::start();
    fs::write(d.path("pid"), "999999").unwrap();
    fs::write(d.path("config"), "replacement-config").unwrap();
    fs::write(d.path("engine"), "replacement-engine").unwrap();
    fs::write(d.path("version"), "replacement-version").unwrap();

    d.request_shutdown();
    d.wait_for_exit();

    assert_eq!(read(&d.path("pid")), "999999");
    assert_eq!(read(&d.path("config")), "replacement-config");
    assert_eq!(read(&d.path("engine")), "replacement-engine");
    assert_eq!(read(&d.path("version")), "replacement-version");
    assert!(
        d.path("sock").exists(),
        "replacement socket path should remain"
    );
}
