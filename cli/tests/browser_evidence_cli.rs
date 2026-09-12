use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const MARKDOWN: &str = "# Public docs\n\nThe phrase bearer token is ordinary technical text.\n";

struct Fixture {
    dir: TempDir,
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    server: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let server = thread::spawn(move || {
            thread::scope(|scope| {
                while !stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let received = received.clone();
                            // Chrome may preconnect without sending a request yet.
                            scope.spawn(move || {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                        let mut request = String::new();
                        while !request.ends_with("\r\n\r\n") {
                            if !matches!(reader.read_line(&mut request), Ok(n) if n > 0) {
                                return;
                            }
                        }
                        let path = request.split_whitespace().nth(1).unwrap_or("/");
                        let (content_type, body) = match path {
                            "/page" => ("text/html", "<title>Owned page</title><h1>Rendered page</h1>"),
                            "/confirm" => ("text/html", r#"<title>Confirm</title><button id="apply" onclick="this.dataset.clicked='yes'">Apply</button>"#),
                            "/motion" => ("text/html", "<title>Motion</title><style>@keyframes move{to{transform:translateX(300px)}}div{width:40px;height:40px;background:red;animation:move 0.4s infinite alternate linear}</style><div></div>"),
                            _ => ("text/markdown", MARKDOWN),
                        };
                        received.lock().unwrap().push(request);
                        let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                        let _ = stream.write_all(response.as_bytes());
                        });
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        Err(_) => break,
                    }
                }
            })
        });
        Self {
            dir,
            port,
            requests,
            stop,
            server: Some(server),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        // These tests must never inherit a developer's working browser/session.
        for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("AGENT_BROWSER_")) {
            command.env_remove(key);
        }
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(key);
        }
        command
            .current_dir(self.dir.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir.path().join("run"))
            .env("AGENT_BROWSER_CONFIG", self.dir.path().join("config.json"))
            .env("AGENT_BROWSER_SESSION", "e")
            .env("HOME", self.dir.path())
            .env_remove("XDG_RUNTIME_DIR")
            .env("NO_COLOR", "1")
            .args(["--json", "--session", "e"]);
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Value {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn info(&self) -> Value {
        self.run(&["session", "info"])["data"].clone()
    }

    fn wait_for_capture(&self, minimum: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let info = self.info();
            let current = &info["runtime"]["recording"]["current"];
            if current["capturedFrames"].as_u64().unwrap_or(0) >= minimum
                && current["frames"].as_u64().unwrap_or(0) > 0
            {
                return;
            }
            if Instant::now() >= deadline {
                let text = self.run(&["get", "text", "body"]);
                panic!(
                    "expected captured frames, got {info}; body: {text}; requests: {:?}",
                    self.requests.lock().unwrap()
                );
            }
            thread::sleep(Duration::from_millis(30));
        }
    }

    fn configure_unused_browser(&self) -> std::path::PathBuf {
        let profile = self.dir.path().join("unused-profile");
        std::fs::write(
            self.dir.path().join("unused.json"),
            json!({
                "profile": profile,
                "executablePath": self.dir.path().join("missing-chrome"),
                "headed": true,
                "restore": "unused-restore",
                "pinTab": true,
            })
            .to_string(),
        )
        .unwrap();
        profile
    }

    #[cfg(unix)]
    fn raw_command(&self, cmd: &Value) -> Value {
        let mut stream =
            std::os::unix::net::UnixStream::connect(self.dir.path().join("run/e.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        writeln!(stream, "{cmd}").unwrap();
        let mut line = String::new();
        std::io::BufReader::new(stream)
            .read_line(&mut line)
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let pid = std::fs::read_to_string(self.dir.path().join("run/e.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok());
        // The transport-error fixture fabricates a live inventory entry using
        // this test process; it is not a daemon that cleanup owns.
        if pid.is_some_and(|pid| pid != std::process::id()) {
            let _ = self.output(&["close"]);
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

#[test]
fn explicit_reads_and_read_only_batches_never_start_a_daemon() {
    let fixture = Fixture::new();
    let unused_profile = fixture.configure_unused_browser();
    let url = fixture.url("/docs");
    let read = fixture.run(&[
        "--config",
        "unused.json",
        "--headers",
        "{\"X-Read-Test\":\"preserved\"}",
        "read",
        &url,
        "--require-md",
    ]);
    assert_eq!(read["data"]["content"], MARKDOWN);
    assert_eq!(read["data"]["source"], "accept-markdown");
    assert!(fixture.requests.lock().unwrap()[0]
        .to_lowercase()
        .contains("x-read-test: preserved"));

    let mut child = fixture
        .command()
        .args(["--config", "unused.json", "batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    write!(
        child.stdin.take().unwrap(),
        "{}",
        json!([["read", url], ["read", url, "--outline"]])
    )
    .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let batch: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(batch[0]["result"]["content"], MARKDOWN);
    assert!(batch[1]["result"]["source"]
        .as_str()
        .unwrap()
        .ends_with("-outline"));
    assert!(
        !fixture.dir.path().join("run").exists(),
        "HTTP reads must not create daemon files"
    );
    assert!(!unused_profile.exists());
    assert_eq!(fixture.info()["active"], false);
}

#[test]
fn read_validation_policy_and_batch_bail_do_not_start_browser_setup() {
    let fixture = Fixture::new();
    fixture.configure_unused_browser();
    let url = fixture.url("/docs");
    let denied = fixture.output(&[
        "--config",
        "unused.json",
        "--allowed-domains",
        "other.example",
        "read",
        &url,
    ]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stdout).contains("allowed domains"));
    std::fs::write(
        fixture.dir.path().join("policy.json"),
        "{\"deny\":[\"read\"]}",
    )
    .unwrap();
    let denied = fixture.output(&["--action-policy", "policy.json", "read", &url]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stdout).contains("denied by policy"));
    let invalid = fixture.output(&["read", &url, "--timeout", "0"]);
    assert!(!invalid.status.success());
    let batch = fixture.output(&[
        "--config",
        "unused.json",
        "batch",
        "--bail",
        "read file:///not-http",
        "open https://never-reached.example",
    ]);
    assert!(!batch.status.success());
    let batch: Value = serde_json::from_slice(&batch.stdout).unwrap();
    assert_eq!(batch.as_array().unwrap().len(), 1);
    assert!(batch[0]["error"]
        .as_str()
        .unwrap()
        .contains("Unsupported read URL scheme"));
    assert!(fixture.requests.lock().unwrap().is_empty());
    assert!(!fixture.dir.path().join("run").exists());

    let mixed = fixture.output(&[
        "--config",
        "unused.json",
        "batch",
        &format!("read {url}"),
        "open about:blank",
        &format!("read {url}"),
    ]);
    assert!(!mixed.status.success());
    let mixed: Value = serde_json::from_slice(&mixed.stdout).unwrap();
    assert_eq!(
        mixed.as_array().map(Vec::len),
        Some(3),
        "each reached row must retain its result: {mixed}"
    );
    assert_eq!(mixed[0]["result"]["content"], MARKDOWN);
    assert_eq!(mixed[1]["success"], false);
    assert_eq!(mixed[2]["result"]["content"], MARKDOWN);
}

#[cfg(unix)]
#[test]
fn session_info_preserves_transport_errors() {
    let fixture = Fixture::new();
    let run = fixture.dir.path().join("run");
    std::fs::create_dir(&run).unwrap();
    std::fs::write(run.join("e.pid"), std::process::id().to_string()).unwrap();
    std::fs::write(run.join("e.version"), env!("CARGO_PKG_VERSION")).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(run.join("e.sock")).unwrap();
    let responder = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        std::io::BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&request).unwrap()["action"],
            "session_info"
        );
        stream.write_all(b"not-json\n").unwrap();
    });
    let info = fixture.info();
    responder.join().unwrap();
    std::fs::remove_file(run.join("e.pid")).unwrap();
    assert_eq!(info["active"], true);
    assert!(info["runtime"].is_null());
    assert!(info["runtimeError"]
        .as_str()
        .is_some_and(|error| !error.is_empty()));
}

#[test]
fn read_confirmation_does_not_restart_a_mismatched_daemon() {
    let fixture = Fixture::new();
    fixture.run(&["stream", "status"]);
    let pid_path = fixture.dir.path().join("run/e.pid");
    let pid = std::fs::read_to_string(&pid_path).unwrap();
    let version_path = fixture.dir.path().join("run/e.version");
    std::fs::write(&version_path, "older-version").unwrap();
    let response = fixture.output(&["--confirm-actions", "read", "read", &fixture.url("/docs")]);
    let after = std::fs::read_to_string(&pid_path).unwrap();
    std::fs::write(&version_path, env!("CARGO_PKG_VERSION")).unwrap();
    assert!(!response.status.success());
    assert!(String::from_utf8_lossy(&response.stdout).contains("different version"));
    assert_eq!(after, pid);
    assert!(fixture.requests.lock().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn read_confirmation_requires_capability_on_each_connection() {
    for disconnect_after_support in [false, true] {
        let fixture = Fixture::new();
        let run = fixture.dir.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let pid = std::process::id().to_string();
        std::fs::write(run.join("e.pid"), &pid).unwrap();
        std::fs::write(run.join("e.version"), env!("CARGO_PKG_VERSION")).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(run.join("e.sock")).unwrap();
        let responder = thread::spawn(move || {
            let mut commands = Vec::new();
            loop {
                let (mut stream, _) = listener.accept().unwrap();
                // A readiness probe may already have closed its Unix socket.
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                if std::io::BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap()
                    == 0
                {
                    continue; // The normal daemon-readiness connection has no request.
                }
                let command: Value = serde_json::from_str(&line).unwrap();
                commands.push(command.clone());
                let first_supported = disconnect_after_support && commands.len() == 1;
                let data = if command["action"] == "session_info" {
                    if first_supported {
                        json!({ "capabilities": { "readRequiresConfirmation": true } })
                    } else {
                        json!({ "browserLaunched": false })
                    }
                } else {
                    json!({ "content": MARKDOWN })
                };
                writeln!(stream, "{}", json!({ "success": true, "data": data })).unwrap();
                if first_supported && command["action"] == "session_info" {
                    stream.shutdown(std::net::Shutdown::Both).unwrap();
                    continue;
                }
                return commands;
            }
        });
        let output = fixture.output(&["--confirm-actions", "read", "read", &fixture.url("/docs")]);
        let commands = responder.join().unwrap();
        assert!(
            !output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("does not support"));
        assert!(response["data"].is_null());
        assert_eq!(commands.len(), if disconnect_after_support { 2 } else { 1 });
        assert!(commands
            .iter()
            .all(|cmd| cmd["action"] == "session_info" && cmd["capabilitiesOnly"] == true));
        assert!(fixture.requests.lock().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(run.join("e.pid")).unwrap(), pid);
        assert_eq!(
            std::fs::read_to_string(run.join("e.version")).unwrap(),
            env!("CARGO_PKG_VERSION")
        );
        assert!(!run.join("e.config").exists());
    }
}

#[test]
fn read_confirmation_keeps_the_existing_machine_callable_flow_browserless() {
    let fixture = Fixture::new();
    fixture.configure_unused_browser();
    let url = fixture.url("/docs");
    let pending = fixture.run(&[
        "--config",
        "unused.json",
        "--confirm-actions",
        "read",
        "read",
        &url,
    ]);
    assert_eq!(pending["data"]["confirmation_required"], true);
    assert_eq!(
        pending["data"]["capabilities"]["readRequiresConfirmation"],
        true
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
    let before = fixture.info();
    assert_eq!(before["active"], true);
    assert_eq!(before["runtime"]["browserLaunched"], false);
    assert_eq!(
        before["runtime"]["capabilities"]["readRequiresConfirmation"],
        true
    );
    let confirmed = fixture.run(&[
        "--config",
        "unused.json",
        "confirm",
        pending["data"]["confirmation_id"].as_str().unwrap(),
    ]);
    assert_eq!(confirmed["data"]["result"]["data"]["content"], MARKDOWN);
    assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    assert_eq!(fixture.info()["runtime"]["browserLaunched"], false);
    assert_eq!(fixture.info()["pid"], before["pid"]);
    assert!(!fixture.dir.path().join("unused-profile").exists());
}

#[test]
#[ignore = "requires Chrome; uses a generated browser and loopback page"]
fn e2e_stale_read_ids_cannot_approve_or_deny_a_newer_dom_action() {
    let fixture = Fixture::new();
    std::fs::write(
        fixture.dir.path().join("config.json"),
        r#"{"confirmActions":"read,click"}"#,
    )
    .unwrap();
    fixture.run(&["open", &fixture.url("/confirm")]);
    assert!(fixture.run(&["get", "attr", "#apply", "data-clicked"])["data"]["value"].is_null());
    let before = fixture.info();
    let read = fixture.run(&["read", &fixture.url("/docs")]);
    assert_eq!(
        read["data"]["capabilities"]["readRequiresConfirmation"],
        true
    );
    let click = fixture.run(&["click", "#apply"]);
    assert_eq!(click["data"]["confirmation_required"], true);
    assert!(click["data"]["capabilities"].is_null());
    for action in ["confirm", "deny"] {
        let output = fixture.output(&[action, read["data"]["confirmation_id"].as_str().unwrap()]);
        assert!(
            !output.status.success(),
            "stale {action}: {}; clicked attribute: {}",
            String::from_utf8_lossy(&output.stdout),
            fixture.run(&["get", "attr", "#apply", "data-clicked"])["data"]["value"]
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("does not match"));
        assert!(fixture.run(&["get", "attr", "#apply", "data-clicked"])["data"]["value"].is_null());
    }
    let after = fixture.info();
    assert_eq!(after["pid"], before["pid"]);
    assert_eq!(
        after["runtime"]["browser"]["pid"],
        before["runtime"]["browser"]["pid"]
    );
    assert_eq!(
        after["runtime"]["browser"]["tabs"],
        before["runtime"]["browser"]["tabs"]
    );
    let approved = fixture.run(&[
        "confirm",
        click["data"]["confirmation_id"].as_str().unwrap(),
    ]);
    assert_eq!(approved["data"]["result"]["success"], true);
    assert_eq!(
        fixture.run(&["get", "attr", "#apply", "data-clicked"])["data"]["value"],
        "yes"
    );
    assert!(!fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.split_whitespace().nth(1) == Some("/docs")));
}

#[test]
#[ignore = "requires Chrome; both profile paths are generated and owned by this test"]
fn e2e_profile_equals_override_reports_and_discovers_the_actual_directory() {
    let fixture = Fixture::new();
    let base = fixture.dir.path().join("unused-base-profile");
    let actual = fixture.dir.path().join("actual-profile");
    let switch = format!("--user-data-dir={}", actual.display());
    fixture.run(&[
        "--profile",
        base.to_str().unwrap(),
        "--args",
        &switch,
        "open",
        &fixture.url("/page"),
    ]);
    let info = fixture.info();
    assert_eq!(info["runtime"]["browser"]["alive"], true);
    assert_eq!(
        info["runtime"]["browser"]["userDataDir"],
        actual.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(actual.join("DevToolsActivePort").exists());
    assert!(!base.join("DevToolsActivePort").exists());
}

#[test]
#[ignore = "requires a locally installed Chrome; uses only a generated profile"]
fn e2e_warm_reads_and_confirmations_preserve_browser_identity_and_tabs() {
    let fixture = Fixture::new();
    let profile = fixture.dir.path().join("profile");
    fixture.run(&[
        "--profile",
        profile.to_str().unwrap(),
        "open",
        &fixture.url("/page"),
    ]);
    fixture.run(&["tab", "new", &fixture.url("/motion")]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let before = loop {
        let info = fixture.info();
        let tabs = info["runtime"]["browser"]["tabs"].as_array().unwrap();
        if ["Owned page", "Motion"]
            .iter()
            .all(|title| tabs.iter().any(|tab| tab["title"] == *title))
        {
            break info;
        }
        assert!(
            Instant::now() < deadline,
            "page titles did not settle: {info}"
        );
        thread::sleep(Duration::from_millis(20));
    };
    let browser = &before["runtime"]["browser"];
    assert_eq!(browser["alive"], true);
    assert_eq!(browser["ownership"], "launched");
    assert_eq!(
        browser["userDataDir"],
        profile.canonicalize().unwrap().to_str().unwrap()
    );
    assert!(browser["pid"].as_u64().unwrap() > 0);
    assert_ne!(browser["pid"], before["pid"]);
    let tabs = browser["tabs"].as_array().unwrap();
    assert_eq!(tabs.len(), 2);
    assert_eq!(tabs.iter().filter(|tab| tab["active"] == true).count(), 1);
    assert!(tabs.iter().all(
        |tab| tab["tabId"].as_str().is_some_and(|id| id.starts_with('t'))
            && tab["targetId"].as_str().is_some_and(|id| !id.is_empty())
    ));
    assert!(
        tabs.iter().any(|tab| tab["title"] == "Owned page"),
        "tabs: {tabs:?}"
    );

    fixture.configure_unused_browser();
    let url = fixture.url("/docs");
    fixture.run(&["--config", "unused.json", "read", &url]);
    fixture.run(&[
        "--config",
        "unused.json",
        "batch",
        &format!("read {url}"),
        &format!("read {url} --outline"),
    ]);
    let pending = fixture.run(&[
        "--config",
        "unused.json",
        "--confirm-actions",
        "read",
        "read",
        &url,
    ]);
    fixture.run(&[
        "--config",
        "unused.json",
        "confirm",
        pending["data"]["confirmation_id"].as_str().unwrap(),
    ]);
    let after = fixture.info();
    assert_eq!(after["pid"], before["pid"]);
    assert_eq!(after["runtime"]["browser"], *browser);
    assert!(!fixture.dir.path().join("unused-profile").exists());

    let dom = fixture.run(&["read"]);
    assert_eq!(dom["data"]["source"], "active-tab-html");
    assert_eq!(dom["data"]["url"], fixture.url("/motion"));

    #[cfg(unix)]
    {
        // Kill only the Chrome process created above; inspection must not recover it.
        let pid = browser["pid"].as_u64().unwrap() as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        thread::sleep(Duration::from_millis(100));
        let dead = fixture.info();
        assert_eq!(dead["pid"], before["pid"]);
        assert_eq!(dead["runtime"]["browserLaunched"], false);
        assert!(matches!(
            dead["runtime"]["browser"]["status"].as_str(),
            Some("disconnected" | "not-launched")
        ));
        let dead_pid = &dead["runtime"]["browser"]["pid"];
        // The daemon's normal background reaper may already have removed it.
        assert!(dead_pid.is_null() || dead_pid == &browser["pid"]);
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires Chrome and ffmpeg; all processes and output are isolated"]
fn e2e_disconnected_stop_retains_receipt_and_replay_cannot_stop_new_take() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let which = Command::new("which").arg("ffmpeg").output().unwrap();
    assert!(
        which.status.success(),
        "ffmpeg must be installed for this test"
    );
    let ffmpeg = String::from_utf8(which.stdout).unwrap().trim().to_string();
    let tools = fixture.dir.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let wrapper = tools.join("ffmpeg");
    std::fs::write(&wrapper, format!("#!/bin/sh\nif [ \"$1\" = -version ]; then exec '{ffmpeg}' \"$@\"; fi\n'{ffmpeg}' \"$@\"\nresult=$?\nsleep 1\nexit $result\n")).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path_env = format!("{}:{}", tools.display(), std::env::var("PATH").unwrap());
    let opened = fixture
        .command()
        .env("PATH", &path_env)
        .args(["open", &fixture.url("/motion")])
        .output()
        .unwrap();
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stdout)
    );
    let path = fixture.dir.path().join("take.webm");
    let started = fixture.run(&["record", "start", path.to_str().unwrap()]);
    fixture.wait_for_capture(2);
    let stop = json!({ "id": "disconnected-stop", "action": "recording_stop" });
    let stop_started = Instant::now();
    let mut stream =
        std::os::unix::net::UnixStream::connect(fixture.dir.path().join("run/e.sock")).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(30)))
        .unwrap();
    writeln!(stream, "{stop}").unwrap();
    let error = std::io::BufReader::new(stream)
        .read_line(&mut String::new())
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));

    let cli_info = fixture.info();
    let info = fixture.raw_command(&json!({ "id": "recording-info", "action": "session_info" }));
    let receipt = &info["data"]["recording"]["last"];
    assert_eq!(
        cli_info["runtime"]["recording"]["last"]["recordingId"],
        receipt["recordingId"]
    );
    assert!(
        stop_started.elapsed() >= Duration::from_secs(1),
        "real encoder exit was deliberately delayed"
    );
    assert!(info["data"]["recording"]["current"].is_null());
    assert_eq!(receipt["recordingId"], started["data"]["recordingId"]);
    assert_eq!(receipt["success"], true);
    assert_eq!(receipt["output"]["encoderSucceeded"], true);
    assert_eq!(receipt["frames"], receipt["output"]["encodedFrames"]);
    assert!(receipt["file"]["sizeBytes"].as_u64().unwrap() > 0);
    let ended =
        chrono::DateTime::parse_from_rfc3339(receipt["capture"]["endedAt"].as_str().unwrap())
            .unwrap();
    assert!(
        (chrono::Utc::now() - ended.with_timezone(&chrono::Utc)).num_milliseconds() >= 800,
        "encoding delay must not inflate capture end"
    );
    assert!(
        receipt["capture"]["lastFrameAfterMs"].as_f64().unwrap()
            <= receipt["capture"]["durationMs"].as_f64().unwrap()
    );
    assert_eq!(fixture.raw_command(&stop)["data"], *receipt);

    let next_path = fixture.dir.path().join("next.webm");
    let next = fixture.run(&["record", "start", next_path.to_str().unwrap()]);
    assert_eq!(fixture.raw_command(&stop)["data"], *receipt);
    assert_eq!(
        fixture.info()["runtime"]["recording"]["current"]["recordingId"],
        next["data"]["recordingId"]
    );
    fixture.wait_for_capture(2);
    fixture.run(&["record", "stop"]);
}

#[test]
#[ignore = "requires Chrome and ffmpeg; output stays in a generated directory"]
fn e2e_encoder_failure_retains_a_failed_receipt() {
    let fixture = Fixture::new();
    fixture.run(&["open", &fixture.url("/motion")]);
    let path = fixture.dir.path().join("take.invalid-container");
    fixture.run(&["record", "start", path.to_str().unwrap()]);
    thread::sleep(Duration::from_millis(250));
    let stopped = fixture.output(&["record", "stop"]);
    assert!(!stopped.status.success());
    let stopped: Value = serde_json::from_slice(&stopped.stdout).unwrap();
    assert_eq!(stopped["success"], false);
    assert_eq!(stopped["data"]["success"], false);
    assert_eq!(stopped["data"]["output"]["encoderSucceeded"], false);
    assert!(stopped["data"]["error"]
        .as_str()
        .unwrap()
        .contains("ffmpeg failed"));
    assert_eq!(
        fixture.info()["runtime"]["recording"]["last"],
        stopped["data"]
    );
    let repeated: Value =
        serde_json::from_slice(&fixture.output(&["record", "stop"]).stdout).unwrap();
    assert_eq!(repeated["success"], false);
    assert_eq!(repeated["data"], stopped["data"]);

    let next_path = fixture.dir.path().join("retry.webm");
    let restarted = fixture.run(&["record", "restart", next_path.to_str().unwrap()]);
    assert_eq!(restarted["data"]["previousRecording"], stopped["data"]);
    assert_ne!(
        restarted["data"]["recordingId"],
        stopped["data"]["recordingId"]
    );
    assert!(restarted["data"]["previousPath"].is_null());
    fixture.wait_for_capture(1);
    fixture.run(&["record", "stop"]);
}
