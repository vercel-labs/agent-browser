use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

struct TestServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestServer {
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
                        let mut request = [0u8; 2048];
                        let read = stream.read(&mut request).unwrap_or(0);
                        let request = String::from_utf8_lossy(&request[..read]);
                        let path = request.split_whitespace().nth(1).unwrap_or("/");
                        let body = match path {
                            "/shared" => "shared",
                            "/mine" => "mine",
                            _ => "not found",
                        };
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Sessions {
    socket_dir: TempDir,
    names: Vec<&'static str>,
}

impl Sessions {
    fn new() -> Self {
        Self {
            socket_dir: TempDir::new().unwrap(),
            names: vec!["pin-tab-cli-host", "pin-tab-cli-victim"],
        }
    }

    fn command(&self, session: &str) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(["--session", session])
            .env("AGENT_BROWSER_SOCKET_DIR", self.socket_dir.path())
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env_remove("AGENT_BROWSER_PIN_TAB")
            .env("NO_COLOR", "1");
        cmd
    }

    fn run_json(&self, session: &str, args: &[&str]) -> Value {
        let output = self
            .command(session)
            .args(args)
            .arg("--json")
            .output()
            .expect("failed to run agent-browser");
        assert!(
            output.status.success(),
            "command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout was not JSON: {}\n{}",
                e,
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn run_json_failure(&self, session: &str, args: &[&str]) -> Value {
        let output = self
            .command(session)
            .args(args)
            .arg("--json")
            .output()
            .expect("failed to run agent-browser");
        assert_eq!(
            output.status.code(),
            Some(1),
            "command unexpectedly succeeded or exited abnormally\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout was not JSON: {}\n{}",
                e,
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn pid(&self, session: &str) -> String {
        std::fs::read_to_string(self.socket_dir.path().join(format!("{}.pid", session)))
            .unwrap()
            .trim()
            .to_string()
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for session in &self.names {
            let _ = self.command(session).arg("close").output();
        }
    }
}

#[test]
#[ignore]
fn cdp_pin_tab_precedes_attach_in_existing_daemon() {
    let server = TestServer::start();
    let sessions = Sessions::new();
    let host = "pin-tab-cli-host";
    let victim = "pin-tab-cli-victim";
    let shared_url = server.url("/shared");
    let mine_url = server.url("/mine");

    sessions.run_json(host, &["open", &shared_url]);
    let cdp = sessions.run_json(host, &["get", "cdp-url"]);
    let ws_url = cdp["data"]["cdpUrl"].as_str().unwrap();

    sessions.run_json(victim, &["stream", "status"]);
    let pid_before = sessions.pid(victim);
    assert!(!sessions
        .socket_dir
        .path()
        .join(format!("{}.target", victim))
        .exists());

    sessions.run_json(victim, &["--cdp", ws_url, "--pin-tab", "open", &mine_url]);

    assert_eq!(sessions.pid(victim), pid_before);
    let tabs = sessions.run_json(host, &["tab", "list"]);
    let tabs = tabs["data"]["tabs"].as_array().unwrap();
    let shared = tabs
        .iter()
        .find(|tab| tab["url"] == shared_url)
        .expect("shared target should remain unchanged");
    let mine = tabs
        .iter()
        .find(|tab| tab["url"] == mine_url)
        .expect("pinned session should navigate a distinct target");
    assert_ne!(shared["targetId"], mine["targetId"]);

    let binding: Value = serde_json::from_str(
        &std::fs::read_to_string(
            sessions
                .socket_dir
                .path()
                .join(format!("{}.target", victim)),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(binding["pinned"], true);
    assert_eq!(binding["targetId"], mine["targetId"]);
}

#[test]
#[ignore]
fn tab_gone_exposes_safe_recovery_data_in_cli_and_batch() {
    let server = TestServer::start();
    let sessions = Sessions::new();
    let host = "pin-tab-cli-host";
    let victim = "pin-tab-cli-victim";
    let shared_url = server.url("/shared");
    let safe_url = server.url("/mine");
    let navigated_url = format!("{}?token=secret#fragment", safe_url);

    sessions.run_json(host, &["open", &shared_url]);
    let cdp = sessions.run_json(host, &["get", "cdp-url"]);
    let ws_url = cdp["data"]["cdpUrl"].as_str().unwrap();
    sessions.run_json(
        victim,
        &["--cdp", ws_url, "--pin-tab", "open", &navigated_url],
    );

    let binding: Value = serde_json::from_str(
        &std::fs::read_to_string(
            sessions
                .socket_dir
                .path()
                .join(format!("{}.target", victim)),
        )
        .unwrap(),
    )
    .unwrap();
    let target_id = binding["targetId"].as_str().unwrap();
    assert_eq!(binding["url"], safe_url);

    sessions.run_json(host, &["tab", "close", target_id]);

    let single = sessions.run_json_failure(victim, &["get", "url"]);
    assert_eq!(single["code"], "tab_gone");
    assert_eq!(single["data"]["targetId"], target_id);
    assert_eq!(single["data"]["lastUrl"], safe_url);

    let batch = sessions.run_json_failure(victim, &["batch", "get url"]);
    assert_eq!(batch[0]["code"], "tab_gone");
    assert_eq!(batch[0]["result"]["targetId"], target_id);
    assert_eq!(batch[0]["result"]["lastUrl"], safe_url);
}
