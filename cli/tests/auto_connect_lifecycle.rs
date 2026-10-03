// This fixture redirects Linux Chrome discovery into a temporary HOME.
// Other platforms require isolated discovery paths before enabling this test.
#![cfg(target_os = "linux")]

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::Message;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "auto-connect-lifecycle";
const RECOVERY_TAG: &str = "AUTO_CONNECT_RECOVERY_TRIGGER_7F3A";

// Auto-connect also probes common ports. Re-execute each test in its own
// network namespace before starting any fixture or daemon, even under cargo test.
fn run_in_private_network(test_name: &str) -> bool {
    let namespace = fs::read_link("/proc/self/ns/net").unwrap();
    if let Some(parent_namespace) = std::env::var_os("AGENT_BROWSER_TEST_PARENT_NET") {
        assert_ne!(namespace.as_os_str(), parent_namespace);
        return false;
    }
    let output = std::process::Command::new("unshare")
        .args(["-Urn", "sh", "-c", "ip link set lo up && exec \"$@\"", "sh"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", test_name, "--nocapture"])
        .env("AGENT_BROWSER_TEST_PARENT_NET", namespace)
        .output()
        .expect("lifecycle tests require unshare and ip for browser isolation");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Some hosted CI runners forbid unprivileged user namespaces
        // (unshare fails writing /proc/self/uid_map with EPERM). Without the
        // namespace these tests could probe the host's real browsers on the
        // common ports, so skip rather than run unisolated.
        if stderr.contains("Operation not permitted") {
            eprintln!("skipped {test_name}: user namespaces are not permitted here");
            return true;
        }
        panic!(
            "isolated lifecycle test failed (unshare and ip are required)\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            stderr
        );
    }
    true
}

#[derive(Clone)]
struct FakeTarget {
    id: String,
    url: String,
}

#[derive(Clone, Copy)]
enum SecondConnectionBehavior {
    StallTargetSetup,
    DelayTargetSetup(Duration),
}

struct RecoveryPlan {
    tagged_expression: String,
    second_connection: SecondConnectionBehavior,
    disconnected: AtomicBool,
}

struct FakeCdpServer {
    port: u16,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    recovery: Option<Arc<RecoveryPlan>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl FakeCdpServer {
    fn start() -> Self {
        Self::start_with_recovery(None)
    }

    fn start_recovery(tagged_expression: &str, behavior: SecondConnectionBehavior) -> Self {
        Self::start_with_recovery(Some(RecoveryPlan {
            tagged_expression: tagged_expression.to_string(),
            second_connection: behavior,
            disconnected: AtomicBool::new(false),
        }))
    }

    fn start_with_recovery(recovery: Option<RecoveryPlan>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));
        let recovery = recovery.map(Arc::new);
        let targets = Arc::new(Mutex::new(vec![FakeTarget {
            id: "page-1".to_string(),
            url: "about:blank".to_string(),
        }]));
        let server_stop = Arc::clone(&stop);
        let server_connections = Arc::clone(&connections);
        let server_targets = Arc::clone(&targets);
        let server_recovery = recovery.clone();
        let thread = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();

                while !server_stop.load(Ordering::Relaxed) {
                    let accepted =
                        tokio::time::timeout(Duration::from_millis(100), listener.accept()).await;
                    let Ok(Ok((stream, _))) = accepted else {
                        continue;
                    };
                    let targets = Arc::clone(&server_targets);
                    let connections = Arc::clone(&server_connections);
                    let recovery = server_recovery.clone();
                    tokio::spawn(async move {
                        serve_connection(stream, targets, connections, recovery).await;
                    });
                }
            });
        });

        Self {
            port,
            stop,
            connections,
            recovery,
            thread: Some(thread),
        }
    }

    fn connection_count(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    fn wait_for_disconnect(&self) {
        let Some(recovery) = self.recovery.as_ref() else {
            return;
        };
        for _ in 0..100 {
            if recovery.disconnected.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(20));
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!("fixture did not abort the tagged CDP WebSocket");
    }
}

impl Drop for FakeCdpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    targets: Arc<Mutex<Vec<FakeTarget>>>,
    connections: Arc<AtomicUsize>,
    recovery: Option<Arc<RecoveryPlan>>,
) {
    let mut ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(_error) => {
            // The private network namespace keeps common-port discovery
            // probes isolated from any browser outside this fixture.
            return;
        }
    };
    let connection_number = connections.fetch_add(1, Ordering::SeqCst) + 1;

    while let Some(Ok(message)) = ws.next().await {
        match message {
            Message::Text(text) => {
                let Ok(request) = serde_json::from_str::<Value>(text.as_ref()) else {
                    continue;
                };
                let id = request.get("id").cloned().unwrap_or_else(|| json!(0));
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if connection_number == 2 && method == "Target.setDiscoverTargets" {
                    if let Some(recovery) = recovery.as_ref() {
                        match recovery.second_connection {
                            SecondConnectionBehavior::StallTargetSetup => {
                                std::future::pending::<()>().await;
                            }
                            SecondConnectionBehavior::DelayTargetSetup(delay) => {
                                tokio::time::sleep(delay).await;
                            }
                        }
                    }
                }
                let is_tagged_evaluation = method == "Runtime.evaluate"
                    && request
                        .get("params")
                        .and_then(|params| params.get("expression"))
                        .and_then(Value::as_str)
                        == recovery
                            .as_ref()
                            .map(|plan| plan.tagged_expression.as_str());
                let abort_after_response = connection_number == 1 && is_tagged_evaluation;
                let result = fake_cdp_result(method, &request, &targets);
                let response = json!({"id": id, "result": result});
                if ws.send(Message::Text(response.to_string())).await.is_err() {
                    break;
                }
                if abort_after_response {
                    // The tagged evaluation response is complete. Abort the
                    // accepted WebSocket itself so the daemon's next command
                    // observes a dead connection.
                    let _ = ws.close(None).await;
                    if let Some(recovery) = recovery.as_ref() {
                        recovery.disconnected.store(true, Ordering::SeqCst);
                    }
                    return;
                }
            }
            Message::Ping(payload) => {
                if ws.send(Message::Pong(payload)).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

fn fake_cdp_result(method: &str, request: &Value, targets: &Arc<Mutex<Vec<FakeTarget>>>) -> Value {
    match method {
        "Target.getTargets" => {
            let targets = targets.lock().unwrap();
            json!({
                "targetInfos": targets.iter().map(|target| json!({
                    "targetId": target.id,
                    "type": "page",
                    "title": "",
                    "url": target.url,
                    "attached": false,
                })).collect::<Vec<_>>()
            })
        }
        "Target.createTarget" => {
            let mut targets = targets.lock().unwrap();
            let id = format!("page-{}", targets.len() + 1);
            targets.push(FakeTarget {
                id: id.clone(),
                url: "about:blank".to_string(),
            });
            json!({"targetId": id})
        }
        "Target.attachToTarget" => {
            let target_id = request
                .get("params")
                .and_then(|params| params.get("targetId"))
                .and_then(Value::as_str)
                .unwrap_or("page-1");
            json!({"sessionId": format!("session-{}", target_id)})
        }
        "Runtime.evaluate" => json!({
            "result": {
                "type": "number",
                "value": 1,
            }
        }),
        _ => json!({}),
    }
}

struct Harness {
    server: Option<FakeCdpServer>,
    home: TempDir,
    socket_dir: TempDir,
}

impl Harness {
    fn new() -> Self {
        Self::with_server(FakeCdpServer::start())
    }

    fn new_recovery(tagged_expression: &str, behavior: SecondConnectionBehavior) -> Self {
        Self::with_server(FakeCdpServer::start_recovery(tagged_expression, behavior))
    }

    fn with_server(server: FakeCdpServer) -> Self {
        let home = TempDir::new().unwrap();
        let chrome_dir = home.path().join(".config/chromium");
        fs::create_dir_all(&chrome_dir).unwrap();
        fs::write(
            chrome_dir.join("DevToolsActivePort"),
            format!("{}\n/devtools/browser/test-browser\n", server.port),
        )
        .unwrap();

        Self {
            server: Some(server),
            home,
            socket_dir: TempDir::new().unwrap(),
        }
    }

    fn command(&self, timeout: Option<&str>) -> std::process::Command {
        let mut command = std::process::Command::new(BIN);
        command
            .env("HOME", self.home.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.socket_dir.path())
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env_remove("AGENT_BROWSER_DAEMON")
            .env_remove("AGENT_BROWSER_CONFIG")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PIN_TAB")
            .env_remove("AGENT_BROWSER_NO_AUTO_DIALOG")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_PROFILE")
            .env_remove("AGENT_BROWSER_STATE")
            .env_remove("AGENT_BROWSER_ARGS")
            .env_remove("AGENT_BROWSER_EXECUTABLE_PATH")
            .env_remove("AGENT_BROWSER_ENGINE")
            .env_remove("AGENT_BROWSER_HEADED")
            .env_remove("AGENT_BROWSER_HEADLESS")
            .env_remove("AGENT_BROWSER_RESTORE")
            .env_remove("AGENT_BROWSER_SESSION_NAME")
            .env_remove("AGENT_BROWSER_ACTION_POLICY")
            .env_remove("AGENT_BROWSER_CONFIRM_ACTIONS")
            .env_remove("AGENT_BROWSER_DEFAULT_TIMEOUT")
            .env_remove("AGENT_BROWSER_IDLE_TIMEOUT_MS")
            .env_remove("AGENT_BROWSER_PLUGINS")
            .env_remove("AGENT_BROWSER_INIT_SCRIPTS")
            .env_remove("AGENT_BROWSER_ENABLE")
            .env_remove("AGENT_BROWSER_NO_WEBMCP")
            .env_remove("AGENT_BROWSER_WEBGPU")
            .env_remove("AGENT_BROWSER_CA_CERT")
            .env_remove("AGENT_BROWSER_CLEAR_CA_CERT")
            .env_remove("AGENT_BROWSER_PROXY")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env("NO_COLOR", "1");
        if let Some(timeout) = timeout {
            command.env("AGENT_BROWSER_AUTO_CONNECT_TIMEOUT", timeout);
        } else {
            command.env_remove("AGENT_BROWSER_AUTO_CONNECT_TIMEOUT");
        }
        command
    }

    fn run_command(
        &self,
        timeout: Option<&str>,
        args: &[&str],
        stdin_body: Option<&str>,
    ) -> std::process::Output {
        let mut command = self.command(timeout);
        command
            .args(["--session", SESSION])
            .args(args)
            .arg("--json");
        if let Some(stdin_body) = stdin_body {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to run agent-browser");
            child
                .stdin
                .take()
                .expect("agent-browser stdin was not piped")
                .write_all(stdin_body.as_bytes())
                .expect("failed to write agent-browser stdin");
            return child
                .wait_with_output()
                .expect("failed to wait for agent-browser");
        }
        command.output().expect("failed to run agent-browser")
    }

    fn response(output: &std::process::Output) -> Value {
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout was not JSON: {}\n{}",
                error,
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn run(&self, timeout: Option<&str>, args: &[&str]) -> Value {
        let output = self.run_command(timeout, args, None);
        assert!(
            output.status.success(),
            "command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Self::response(&output)
    }

    fn run_failure(&self, timeout: Option<&str>, args: &[&str]) -> Value {
        let output = self.run_command(timeout, args, None);
        assert!(
            !output.status.success(),
            "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Self::response(&output)
    }

    fn run_batch(&self, timeout: Option<&str>, input: &str) -> Value {
        let output = self.run_command(timeout, &["batch"], Some(input));
        assert!(
            output.status.success(),
            "batch command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Self::response(&output)
    }

    fn pid(&self) -> u32 {
        fs::read_to_string(self.socket_dir.path().join(format!("{}.pid", SESSION)))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn daemon_env(&self, key: &str) -> Option<String> {
        let pid = self.pid();
        fs::read(format!("/proc/{pid}/environ"))
            .ok()?
            .split(|byte| *byte == 0)
            .find_map(|entry| {
                let entry = std::str::from_utf8(entry).ok()?;
                entry
                    .strip_prefix(&format!("{key}="))
                    .map(ToString::to_string)
            })
    }

    fn binding(&self) -> Value {
        serde_json::from_str(
            &fs::read_to_string(self.socket_dir.path().join(format!("{}.target", SESSION)))
                .unwrap(),
        )
        .unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self
            .command(None)
            .args(["--session", SESSION, "close", "--json"])
            .output();
        self.server.take();
    }
}

#[test]
fn auto_connect_reuses_daemon_and_cdp_connection_across_timeout_changes() {
    if run_in_private_network(
        "auto_connect_reuses_daemon_and_cdp_connection_across_timeout_changes",
    ) {
        return;
    }
    let harness = Harness::new();

    harness.run(Some("60000"), &["--auto-connect", "--pin-tab", "tab", "t1"]);
    let first_pid = harness.pid();
    assert_eq!(harness.binding()["pinned"], true);
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 1);

    harness.run(Some("60000"), &["--auto-connect", "eval", "document.title"]);
    assert_eq!(
        harness.pid(),
        first_pid,
        "stable options should reuse the daemon"
    );
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 1);

    harness.run(None, &["--auto-connect", "eval", "document.title"]);
    assert_eq!(
        harness.pid(),
        first_pid,
        "omitting the per-connect timeout should not restart the daemon"
    );
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 1);

    harness.run(Some("30000"), &["--auto-connect", "eval", "document.title"]);
    assert_eq!(
        harness.pid(),
        first_pid,
        "changing the per-connect timeout should not restart the daemon"
    );
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 1);
    assert_eq!(
        harness.binding()["pinned"],
        true,
        "pin-tab must remain sticky when omitted"
    );

    harness.run(
        Some("30000"),
        &[
            "--auto-connect",
            "--no-auto-dialog",
            "eval",
            "document.title",
        ],
    );
    assert_ne!(
        harness.pid(),
        first_pid,
        "changing daemon-owned dialog handling must restart the daemon"
    );
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 2);
}

#[test]
fn auto_connect_recovery_uses_current_timeout_without_auto_connect_flag() {
    if run_in_private_network(
        "auto_connect_recovery_uses_current_timeout_without_auto_connect_flag",
    ) {
        return;
    }
    let harness = Harness::new_recovery(RECOVERY_TAG, SecondConnectionBehavior::StallTargetSetup);

    harness.run(Some("1200"), &["--auto-connect", "eval", RECOVERY_TAG]);
    harness.server.as_ref().unwrap().wait_for_disconnect();
    let first_pid = harness.pid();
    assert_eq!(
        harness.daemon_env("AGENT_BROWSER_AUTO_CONNECT"),
        Some("1".to_string())
    );
    assert_eq!(
        harness.daemon_env("AGENT_BROWSER_AUTO_CONNECT_TIMEOUT"),
        Some("1200".to_string())
    );
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 1);

    let response = harness.run_failure(Some("180"), &["eval", "1"]);
    assert_eq!(harness.pid(), first_pid, "recovery must keep the daemon");
    assert_eq!(
        harness.server.as_ref().unwrap().connection_count(),
        2,
        "recovery must open a second CDP WebSocket"
    );
    assert!(
        response["error"]
            .as_str()
            .is_some_and(|error| error.contains("timed out after 180ms")),
        "unexpected recovery error: {}",
        response
    );

    // The stalled attempt is discarded. A later command retries implicit
    // recovery and succeeds on the fixture's normal third connection.
    harness.run(Some("500"), &["eval", "1"]);
    assert_eq!(harness.pid(), first_pid);
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 3);
}

#[test]
fn omitted_timeout_uses_default_for_implicit_recovery() {
    if run_in_private_network("omitted_timeout_uses_default_for_implicit_recovery") {
        return;
    }
    let harness = Harness::new_recovery(
        RECOVERY_TAG,
        SecondConnectionBehavior::DelayTargetSetup(Duration::from_millis(400)),
    );

    harness.run(Some("200"), &["--auto-connect", "eval", RECOVERY_TAG]);
    harness.server.as_ref().unwrap().wait_for_disconnect();
    let started = std::time::Instant::now();
    harness.run(None, &["eval", "1"]);
    assert!(
        started.elapsed() >= Duration::from_millis(300),
        "recovery completed before the delayed setup, so the timeout budget was not exercised"
    );
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 2);
}

#[test]
fn omitted_timeout_is_propagated_through_batch_recovery() {
    if run_in_private_network("omitted_timeout_is_propagated_through_batch_recovery") {
        return;
    }
    let harness = Harness::new_recovery(
        RECOVERY_TAG,
        SecondConnectionBehavior::DelayTargetSetup(Duration::from_millis(400)),
    );

    harness.run(Some("200"), &["--auto-connect", "eval", RECOVERY_TAG]);
    harness.server.as_ref().unwrap().wait_for_disconnect();
    let result = harness.run_batch(None, r#"[["eval", "1"]]"#);
    assert_eq!(result[0]["success"], true);
    assert_eq!(harness.server.as_ref().unwrap().connection_count(), 2);
}
