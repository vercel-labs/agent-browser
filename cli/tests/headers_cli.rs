use serde_json::{json, Value};
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;

const UNUSED_HTTP_BASE_URL: &str = "http://127.0.0.1:1";

struct TestHome {
    dir: TempDir,
}

impl TestHome {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn socket_dir(&self) -> PathBuf {
        self.path().join("sockets")
    }

    fn cli(&self, session: &str, args: &[&str]) -> Command {
        let mut command = base_command(self.path());
        command
            .arg("--json")
            .arg("--session")
            .arg(session)
            .args(args);
        command
    }

    fn cleanup(&self, session: &str, config: Option<&Path>) -> SessionCleanup {
        SessionCleanup {
            home: self.path().to_path_buf(),
            session: session.to_string(),
            config: config.map(Path::to_path_buf),
        }
    }
}

fn configure_env(command: &mut StdCommand, home: &Path) {
    let test_chrome = env::var_os("AGENT_BROWSER_TEST_CHROME");
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
            command.env_remove(key);
        }
    }
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        command.env_remove(key);
    }
    command
        .current_dir(home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("AGENT_BROWSER_SOCKET_DIR", home.join("sockets"))
        .env("NO_PROXY", "localhost,127.0.0.1")
        .env("NO_COLOR", "1");
    if let Some(path) = test_chrome {
        command.env("AGENT_BROWSER_EXECUTABLE_PATH", path);
    }
}

fn base_command(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-browser"));
    configure_env(command.as_std_mut(), home);
    command
}

struct SessionCleanup {
    home: PathBuf,
    session: String,
    config: Option<PathBuf>,
}

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        let mut command = StdCommand::new(env!("CARGO_BIN_EXE_agent-browser"));
        configure_env(&mut command, &self.home);
        if let Some(config) = &self.config {
            command.env("AGENT_BROWSER_CONFIG", config);
        }
        let Ok(mut child) = command
            .args(["--json", "--session", &self.session, "close"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
        else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
            }
        }
    }
}

async fn run(command: &mut Command, input: Option<&str>, timeout: Duration) -> Output {
    let mut child = command
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    tokio::time::timeout(timeout, async move {
        if let Some(input) = input {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input.as_bytes()).await.unwrap();
        } else {
            drop(child.stdin.take());
        }
        child.wait_with_output().await.unwrap()
    })
    .await
    .expect("agent-browser subprocess exceeded its test timeout")
}

fn output_text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[derive(Clone)]
struct RecordedRequest {
    path: String,
    headers: HashMap<String, String>,
}

struct HeaderRecorder {
    base_url: String,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: JoinHandle<()>,
}

impl HeaderRecorder {
    fn count(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.path == path)
            .count()
    }

    fn last_header(&self, path: &str, name: &str) -> Option<Option<String>> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|request| request.path == path)
            .map(|request| request.headers.get(&name.to_ascii_lowercase()).cloned())
    }
}

impl Drop for HeaderRecorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_header_recorder() -> HeaderRecorder {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded_requests = Arc::clone(&requests);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let request = tokio::time::timeout(Duration::from_secs(5), async {
                read_recorded_request(&mut stream).await
            })
            .await
            .ok()
            .flatten();
            let Some((path, headers)) = request else {
                continue;
            };
            recorded_requests
                .lock()
                .unwrap()
                .push(RecordedRequest { path, headers });

            let body = "<!doctype html><title>Header fixture</title><h1>Header fixture</h1>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    });
    HeaderRecorder {
        base_url,
        requests,
        task,
    }
}

async fn read_recorded_request(
    stream: &mut tokio::net::TcpStream,
) -> Option<(String, HashMap<String, String>)> {
    let mut request = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let bytes_read = stream.read(&mut chunk).await.ok()?;
        if bytes_read == 0 {
            return None;
        }
        request.extend_from_slice(&chunk[..bytes_read]);
        if request.len() > 16 * 1024 {
            return None;
        }
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = String::from_utf8_lossy(&request);
    let mut lines = request.lines();
    let path = lines.next()?.split_whitespace().nth(1)?.to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Some((path, headers))
}

fn mcp_input(calls: &[(&str, Value)]) -> String {
    let mut requests = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "headers-tests", "version": "1"}
            }
        }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    ];
    requests.extend(calls.iter().enumerate().map(|(index, (name, arguments))| {
        json!({
            "jsonrpc": "2.0",
            "id": index + 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        })
    }));
    requests.push(json!({
        "jsonrpc": "2.0",
        "id": calls.len() + 1,
        "method": "shutdown"
    }));
    requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect()
}

fn mcp_responses(output: &Output, calls: usize) -> Vec<Value> {
    assert!(output.status.success(), "{}", output_text(output));
    let responses: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), calls + 2, "{responses:#?}");
    responses
}

#[tokio::test]
async fn invalid_page_header_cli_arguments_fail_before_dispatch() {
    let home = TestHome::new();
    let session = "headers-parser-only";
    let _cleanup = home.cleanup(session, None);

    let cases = [
        (
            vec![
                "open".to_string(),
                format!("{UNUSED_HTTP_BASE_URL}/invalid-open-array"),
                "--headers".to_string(),
                "[\"cli-secret-open\"]".to_string(),
            ],
            "cli-secret-open",
        ),
        (
            vec![
                "read".to_string(),
                format!("{UNUSED_HTTP_BASE_URL}/invalid-read-number"),
                "--headers".to_string(),
                "42".to_string(),
            ],
            "cli-secret-read",
        ),
        (
            vec![
                "set".to_string(),
                "headers".to_string(),
                "{\"X-Valid\":\"ok\",\"cli-secret-key\":false}".to_string(),
            ],
            "cli-secret-key",
        ),
    ];

    for (args, secret) in cases {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut command = home.cli(session, &borrowed);
        command.env(
            "AGENT_BROWSER_EXECUTABLE_PATH",
            home.path().join("missing-chrome"),
        );
        let output = run(&mut command, None, Duration::from_secs(10)).await;
        let text = output_text(&output);
        assert!(
            !output.status.success(),
            "invalid headers unexpectedly passed: {text}"
        );
        assert!(
            text.contains("headers must be a JSON object with string values"),
            "expected a page-header type diagnostic, got: {text}"
        );
        assert!(
            !text.contains(secret),
            "caller input leaked in diagnostic: {text}"
        );
    }
    assert!(
        !home.socket_dir().exists(),
        "invalid parser input must fail before starting a daemon"
    );
}

#[tokio::test]
async fn mcp_open_and_read_reject_invalid_operator_headers_before_side_effects() {
    let home = TestHome::new();
    let session = "headers-mcp-config";
    let config = home.path().join("headers-config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&json!({ "headers": "{\"X-Header-Secret\":false}" })).unwrap(),
    )
    .unwrap();
    let _cleanup = home.cleanup(session, Some(&config));
    let calls = [
        (
            "agent_browser_open",
            json!({
                "session": session,
                "url": format!("{UNUSED_HTTP_BASE_URL}/invalid-config-open"),
            }),
        ),
        (
            "agent_browser_read",
            json!({
                "session": session,
                "url": format!("{UNUSED_HTTP_BASE_URL}/invalid-config-read"),
            }),
        ),
    ];
    let mut command = base_command(home.path());
    command
        .env("AGENT_BROWSER_CONFIG", &config)
        .env(
            "AGENT_BROWSER_EXECUTABLE_PATH",
            home.path().join("missing-chrome"),
        )
        .args(["mcp", "--tools", "core,network"]);
    let input = mcp_input(&calls);
    let output = run(&mut command, Some(&input), Duration::from_secs(20)).await;
    let responses = mcp_responses(&output, calls.len());

    for index in 0..calls.len() {
        let response = &responses[index + 1]["result"];
        assert_eq!(
            response["isError"], true,
            "invalid config should fail: {response}"
        );
        assert_eq!(
            response["structuredContent"]["response"]["success"], false,
            "invalid config should fail in the canonical CLI parser: {response}"
        );
        assert!(
            response
                .to_string()
                .contains("headers must be a JSON object with string values"),
            "{response}"
        );
        assert!(
            !response.to_string().contains("X-Header-Secret"),
            "operator-config key leaked in error: {response}"
        );
    }
    assert!(
        !home.socket_dir().exists(),
        "invalid MCP open/read config must fail before daemon startup"
    );
}

#[tokio::test]
#[ignore]
async fn e2e_headers_cli_invalid_global_inputs_preserve_the_previous_wire_header() {
    let home = TestHome::new();
    let recorder = start_header_recorder().await;
    let session = "headers-cli-live";
    let _cleanup = home.cleanup(session, None);

    let mut open = home.cli(
        session,
        &["open", &format!("{}/initial", recorder.base_url)],
    );
    let opened = run(&mut open, None, Duration::from_secs(60)).await;
    assert!(opened.status.success(), "{}", output_text(&opened));

    let mut set = home.cli(session, &["set", "headers", "{\"X-Global\":\"retained\"}"]);
    let set_output = run(&mut set, None, Duration::from_secs(20)).await;
    assert!(set_output.status.success(), "{}", output_text(&set_output));

    let invalid = [
        ("[\"cli-array-secret\"]", "cli-array-secret"),
        ("17", "cli-number-secret"),
        (
            "{\"X-Global\":\"candidate\",\"cli-member-secret\":false}",
            "cli-member-secret",
        ),
    ];
    for (index, (raw_headers, secret)) in invalid.into_iter().enumerate() {
        let mut command = home.cli(session, &["set", "headers", raw_headers]);
        let output = run(&mut command, None, Duration::from_secs(20)).await;
        let text = output_text(&output);
        assert!(
            !output.status.success(),
            "invalid global headers succeeded: {text}"
        );
        assert!(text.contains("headers"), "missing type diagnostic: {text}");
        assert!(
            !text.contains(secret),
            "caller input leaked in error: {text}"
        );

        let path = format!("/after-invalid-global-{index}");
        let url = format!("{}{}", recorder.base_url, path);
        let mut navigate = home.cli(session, &["open", &url]);
        let output = run(&mut navigate, None, Duration::from_secs(40)).await;
        assert!(output.status.success(), "{}", output_text(&output));
        assert_eq!(
            recorder.last_header(&path, "x-global"),
            Some(Some("retained".to_string())),
            "the prior valid header must remain active after rejected input"
        );
    }

    for (command_args, path, secret) in [
        (
            vec![
                "open".to_string(),
                format!("{}/invalid-open-header", recorder.base_url),
                "--headers".to_string(),
                "{\"cli-open-secret\":null}".to_string(),
            ],
            "/invalid-open-header",
            "cli-open-secret",
        ),
        (
            vec![
                "read".to_string(),
                format!("{}/invalid-read-header", recorder.base_url),
                "--headers".to_string(),
                "{\"cli-read-secret\":null}".to_string(),
            ],
            "/invalid-read-header",
            "cli-read-secret",
        ),
    ] {
        let borrowed: Vec<&str> = command_args.iter().map(String::as_str).collect();
        let mut command = home.cli(session, &borrowed);
        let output = run(&mut command, None, Duration::from_secs(20)).await;
        let text = output_text(&output);
        assert!(
            !output.status.success(),
            "invalid CLI header flag succeeded: {text}"
        );
        assert!(text.contains("headers"), "missing type diagnostic: {text}");
        assert!(
            !text.contains(secret),
            "caller input leaked in error: {text}"
        );
        assert_eq!(recorder.count(path), 0, "invalid command reached {path}");
    }

    let mut close = home.cli(session, &["close"]);
    let closed = run(&mut close, None, Duration::from_secs(20)).await;
    assert!(closed.status.success(), "{}", output_text(&closed));
}

#[tokio::test]
#[ignore]
async fn e2e_headers_stdio_mcp_rejects_invalid_calls_and_keeps_valid_header_on_wire() {
    let home = TestHome::new();
    let recorder = start_header_recorder().await;
    let session = "headers-mcp-live";
    let _cleanup = home.cleanup(session, None);

    let calls = [
        (
            "agent_browser_open",
            json!({
                "session": session,
                "url": format!("{}/mcp-initial", recorder.base_url),
            }),
        ),
        (
            "agent_browser_set_headers",
            json!({ "session": session, "headers": { "X-Mcp": "retained" } }),
        ),
        (
            "agent_browser_set_headers",
            json!({ "session": session, "headers": ["mcp-array-secret"] }),
        ),
        (
            "agent_browser_set_headers",
            json!({ "session": session, "headers": 17 }),
        ),
        (
            "agent_browser_set_headers",
            json!({
                "session": session,
                "headers": { "X-Mcp": "candidate", "mcp-member-secret": false },
            }),
        ),
        (
            "agent_browser_open",
            json!({
                "session": session,
                "url": format!("{}/mcp-after-invalid", recorder.base_url),
            }),
        ),
        ("agent_browser_close", json!({ "session": session })),
    ];
    let mut command = base_command(home.path());
    command.args(["mcp", "--tools", "core,network"]);
    let input = mcp_input(&calls);
    let output = run(&mut command, Some(&input), Duration::from_secs(120)).await;
    let responses = mcp_responses(&output, calls.len());

    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "agent-browser"
    );
    assert_eq!(responses[1]["result"]["isError"], false, "{}", responses[1]);
    assert_eq!(responses[2]["result"]["isError"], false, "{}", responses[2]);
    for response in &responses[3..6] {
        assert_eq!(response["error"]["code"], -32602, "{response}");
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("headers"));
        assert!(!response.to_string().contains("secret"), "{response}");
    }
    assert_eq!(responses[6]["result"]["isError"], false, "{}", responses[6]);
    assert_eq!(responses[7]["result"]["isError"], false, "{}", responses[7]);
    assert_eq!(
        recorder.last_header("/mcp-after-invalid", "x-mcp"),
        Some(Some("retained".to_string())),
        "invalid MCP header calls must not clear the prior valid header"
    );
    assert_eq!(
        responses.last().unwrap()["result"],
        json!({}),
        "stdio MCP shutdown response was not observed"
    );
}
