//! Commands that start the same session at the same time must share one
//! daemon instead of spawning competing daemons that remove each other's
//! socket and pid files (#2074).

use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const CONCURRENT_CALLERS: usize = 4;
const ROUNDS: usize = 5;

struct MarkdownServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl MarkdownServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut request = [0u8; 2048];
                        let _ = stream.read(&mut request);
                        let body = "# startup race\n";
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
}

impl Drop for MarkdownServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Sessions {
    tmp: TempDir,
    started: Vec<String>,
}

impl Sessions {
    fn new() -> Self {
        Self {
            tmp: TempDir::new().unwrap(),
            started: Vec::new(),
        }
    }

    fn command(&self, session: &str) -> Command {
        let mut cmd = Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
                cmd.env_remove(key);
            }
        }
        cmd.current_dir(self.tmp.path())
            .env("HOME", self.tmp.path())
            .env("USERPROFILE", self.tmp.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.tmp.path().join("sockets"))
            .env_remove("XDG_RUNTIME_DIR")
            .env("NO_COLOR", "1")
            .args(["--session", session, "--json"]);
        cmd
    }

    fn read(&self, session: &str, url: &str) -> Output {
        self.command(session)
            .args(["read", url, "--timeout", "5000"])
            .output()
            .unwrap()
    }

    fn pid(&self, session: &str) -> String {
        std::fs::read_to_string(self.tmp.path().join(format!("sockets/{session}.pid")))
            .unwrap_or_default()
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for session in &self.started {
            let _ = self.command(session).arg("close").output();
        }
    }
}

fn assert_read_succeeded(output: &Output, context: &str) {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{context}: stdout was not JSON\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        response["success"],
        true,
        "{context}: {response}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn concurrent_commands_that_start_a_session_share_one_daemon() {
    let server = MarkdownServer::start();
    let mut sessions = Sessions::new();

    // Each round starts a session that has never run, so every caller races
    // to start its daemon.
    for round in 0..ROUNDS {
        let session = format!("startup-race-{round}");
        sessions.started.push(session.clone());
        let outputs: Vec<Output> = thread::scope(|scope| {
            let callers: Vec<_> = (0..CONCURRENT_CALLERS)
                .map(|_| scope.spawn(|| sessions.read(&session, &server.url())))
                .collect();
            callers
                .into_iter()
                .map(|caller| caller.join().unwrap())
                .collect()
        });
        for (caller, output) in outputs.iter().enumerate() {
            assert_read_succeeded(output, &format!("round {round}, caller {caller}"));
        }

        // Every caller reached the same daemon, so a follow-up command reuses
        // it rather than finding the session unreachable and respawning.
        let pid = sessions.pid(&session);
        assert!(!pid.trim().is_empty(), "round {round}: no daemon pid file");
        assert_read_succeeded(
            &sessions.read(&session, &server.url()),
            &format!("round {round}, follow-up"),
        );
        assert_eq!(
            sessions.pid(&session),
            pid,
            "round {round}: the follow-up command started another daemon"
        );
    }
}
