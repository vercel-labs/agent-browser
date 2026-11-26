use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct McpStdioClient {
    _home: TempDir,
    child: Child,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<Value>,
}

impl McpStdioClient {
    fn spawn(extra_args: &[&str]) -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("s")).unwrap();
        Self::spawn_in(home, extra_args)
    }

    fn spawn_in(home: TempDir, extra_args: &[&str]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent-browser"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
                cmd.env_remove(key);
            }
        }
        cmd.current_dir(home.path())
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("AGENT_BROWSER_SOCKET_DIR", home.path().join("s"))
            .env("NO_COLOR", "1")
            .arg("mcp")
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });

        Self {
            _home: home,
            child,
            stdin: Some(stdin),
            rx,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        let line = serde_json::to_string(msg).unwrap();
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&self, timeout: Duration) -> Value {
        self.rx
            .recv_timeout(timeout)
            .expect("timed out waiting for MCP message")
    }

    fn recv_for_id(&self, id: &Value, timeout: Duration) -> (Value, Vec<Value>) {
        let deadline = Instant::now() + timeout;
        let mut others = Vec::new();
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("timed out waiting for MCP response id");
            let msg = self.recv(remaining);
            if msg.get("id") == Some(id) {
                return (msg, others);
            }
            others.push(msg);
        }
    }

    fn close(mut self) -> std::process::ExitStatus {
        self.stdin.take();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                return self.child.wait().unwrap();
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn modern_meta(capabilities: Value) -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "mcp-stdio-test", "version": "1.0.0" },
        "io.modelcontextprotocol/clientCapabilities": capabilities,
    })
}

#[test]
fn modern_discover_tools_list_and_call() {
    let mut client = McpStdioClient::spawn(&[]);
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "disc-1",
        "method": "server/discover",
        "params": {
            "_meta": modern_meta(json!({}))
        }
    }));
    let resp = client.recv(Duration::from_secs(5));
    assert_eq!(resp["id"], "disc-1");
    let result = &resp["result"];
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["cacheScope"], "public");
    assert!(result["ttlMs"].as_u64().unwrap() > 0);
    let versions = result["supportedVersions"].as_array().unwrap();
    assert!(versions.contains(&json!("2026-07-28")));
    assert!(versions.contains(&json!("2025-11-25")));
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(
        result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "agent-browser"
    );

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {
            "_meta": modern_meta(json!({}))
        }
    }));
    let list_resp = client.recv(Duration::from_secs(5));
    assert_eq!(list_resp["result"]["resultType"], "complete");
    assert_eq!(list_resp["result"]["cacheScope"], "public");
    assert!(!list_resp["result"]["tools"].as_array().unwrap().is_empty());

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_tools_profiles",
            "arguments": {},
            "_meta": modern_meta(json!({}))
        }
    }));
    let call_resp = client.recv(Duration::from_secs(5));
    assert_eq!(call_resp["id"], 3);
    assert_eq!(call_resp["result"]["resultType"], "complete");
    assert_eq!(call_resp["result"]["isError"], false);
    assert_eq!(
        call_resp["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "agent-browser"
    );

    let status = client.close();
    assert!(status.success());
}

#[test]
fn modern_version_mismatch_and_missing_meta_errors() {
    let mut client = McpStdioClient::spawn(&[]);
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": {
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2099-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}
            }
        }
    }));
    let mismatch = client.recv(Duration::from_secs(5));
    assert_eq!(mismatch["error"]["code"], -32022);
    assert_eq!(mismatch["error"]["data"]["requested"], "2099-01-01");
    assert!(mismatch["error"]["data"]["supported"]
        .as_array()
        .unwrap()
        .contains(&json!("2026-07-28")));

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "server/discover",
        "params": {}
    }));
    let missing = client.recv(Duration::from_secs(5));
    assert_eq!(missing["error"]["code"], -32602);

    let status = client.close();
    assert!(status.success());
}

#[test]
fn legacy_initialize_keeps_legacy_response_shape_and_coexists_with_modern() {
    let mut client = McpStdioClient::spawn(&[]);
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "legacy-client", "version": "1.0.0" }
        }
    }));
    let init = client.recv(Duration::from_secs(5));
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert!(init["result"].get("resultType").is_none());

    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_tools_profiles",
            "arguments": {}
        }
    }));
    let legacy_call = client.recv(Duration::from_secs(5));
    assert_eq!(legacy_call["id"], 2);
    assert!(legacy_call["result"].get("resultType").is_none());
    assert_eq!(legacy_call["result"]["isError"], false);

    // Same server process also serves modern requests statelessly.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "ping",
        "params": {
            "_meta": modern_meta(json!({}))
        }
    }));
    let modern_ping = client.recv(Duration::from_secs(5));
    assert_eq!(modern_ping["id"], 3);
    assert_eq!(modern_ping["result"]["resultType"], "complete");

    let status = client.close();
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn concurrent_ping_progress_and_cancellation_over_stdio() {
    use std::os::unix::net::UnixListener;

    let home = tempfile::tempdir().unwrap();
    let sockets = home.path().join("s");
    std::fs::create_dir_all(&sockets).unwrap();

    // Bootstrap the session config/version metadata via a real `stream status` call,
    // then replace the daemon socket with a deterministic mock daemon.
    let bootstrap = Command::new(env!("CARGO_BIN_EXE_agent-browser"))
        .env("HOME", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", &sockets)
        .env("NO_COLOR", "1")
        .args(["--session", "mcp-conc", "stream", "status", "--json"])
        .output()
        .unwrap();
    assert!(bootstrap.status.success(), "{bootstrap:?}");

    if let Ok(pid_str) = std::fs::read_to_string(sockets.join("mcp-conc.pid")) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    let _ = std::fs::remove_file(sockets.join("mcp-conc.pid"));
    let sock_path = sockets.join("mcp-conc.sock");
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();

    let daemon_thread = thread::spawn(move || {
        // First command: sleep 1.2s then respond so we observe concurrent ping + progress.
        // Second command: sleep 5s so cancellation kills the CLI subprocess promptly.
        let delays = [1200_u64, 5000_u64];
        let mut idx = 0;
        while idx < delays.len() {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let delay_ms = delays[idx];
            idx += 1;
            let req: Value = serde_json::from_str(&line).unwrap_or_default();
            let req_id = req["id"].clone();
            thread::sleep(Duration::from_millis(delay_ms));
            let resp = json!({
                "id": req_id,
                "success": true,
                "data": { "waited": true }
            });
            let _ = writeln!(stream, "{}", resp);
            let _ = stream.flush();
        }
    });

    let mut client = McpStdioClient::spawn_in(home, &[]);
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "conc-test", "version": "1.0.0" }
        }
    }));
    let _ = client.recv(Duration::from_secs(5));

    // Start a slow tool call with progressToken, then immediately send ping.
    let started = Instant::now();
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_wait_ms",
            "arguments": { "ms": 1200, "session": "mcp-conc" },
            "_meta": { "progressToken": "prog-10" }
        }
    }));
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "ping",
        "params": {}
    }));

    // Ping must answer well before the 1.2s wait finishes.
    let (ping_resp, pre_msgs) = client.recv_for_id(&json!(11), Duration::from_secs(5));
    assert_eq!(ping_resp["id"], 11);
    assert!(
        started.elapsed() < Duration::from_millis(800),
        "ping was blocked by in-flight tool call: {:?}",
        started.elapsed()
    );

    let (wait_resp, mut mid_msgs) = client.recv_for_id(&json!(10), Duration::from_secs(5));
    mid_msgs.extend(pre_msgs);
    assert_eq!(wait_resp["result"]["isError"], false);
    assert!(
        mid_msgs.iter().any(|m| {
            m.get("method") == Some(&json!("notifications/progress"))
                && m["params"]["progressToken"] == "prog-10"
        }),
        "expected notifications/progress for prog-10, saw: {:?}",
        mid_msgs
    );

    // Start a 5s wait and cancel it via notifications/cancelled.
    let cancel_started = Instant::now();
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 20,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_wait_ms",
            "arguments": { "ms": 5000, "session": "mcp-conc" }
        }
    }));
    thread::sleep(Duration::from_millis(150));
    client.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 20, "reason": "test cancel" }
    }));

    // Verify server stays responsive and does NOT emit a response for cancelled id 20.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 21,
        "method": "ping",
        "params": {}
    }));
    let (ping2, between) = client.recv_for_id(&json!(21), Duration::from_secs(5));
    assert_eq!(ping2["id"], 21);
    assert!(
        between.iter().all(|m| m.get("id") != Some(&json!(20))),
        "cancelled request 20 should not produce a response: {:?}",
        between
    );
    assert!(
        cancel_started.elapsed() < Duration::from_secs(2),
        "cancellation took too long: {:?}",
        cancel_started.elapsed()
    );

    // Shutdown exits promptly even while mock daemon thread is still sleeping.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 22,
        "method": "shutdown",
        "params": {}
    }));
    let (shutdown_resp, _) = client.recv_for_id(&json!(22), Duration::from_secs(5));
    assert_eq!(shutdown_resp["id"], 22);
    let status = client.close();
    assert!(status.success());
    drop(daemon_thread);
}

#[test]
fn skills_over_mcp_discover_list_get_read_and_directory_read() {
    use sha2::{Digest, Sha256};

    let mut client = McpStdioClient::spawn(&[]);

    // 2026-07-28 server/discover advertises resources and io.modelcontextprotocol/skills.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "disc-skills",
        "method": "server/discover",
        "params": {
            "_meta": modern_meta(json!({}))
        }
    }));
    let disc = client.recv(Duration::from_secs(5));
    assert!(disc["result"]["capabilities"]["resources"].is_object());
    assert_eq!(
        disc["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/skills"]
            ["directoryRead"],
        true
    );

    // skills/list returns all non-hidden bundled skills with complete manifests.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "skills/list",
        "params": {
            "_meta": modern_meta(json!({}))
        }
    }));
    let list_resp = client.recv(Duration::from_secs(5));
    assert_eq!(list_resp["result"]["resultType"], "complete");
    assert_eq!(list_resp["result"]["cacheScope"], "public");
    let skills = list_resp["result"]["skills"].as_array().unwrap();
    let core = skills
        .iter()
        .find(|s| s["uri"] == "skill://core/SKILL.md")
        .expect("core skill in skills/list");
    assert_eq!(core["frontmatter"]["name"], "core");

    let core_manifest = core["resources"].as_array().unwrap();
    let skill_md_entry = core_manifest
        .iter()
        .find(|r| r["uri"] == "skill://core/SKILL.md")
        .expect("SKILL.md in manifest");

    // resources/read returns exact bytes matching the manifest's digest and size.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "resources/read",
        "params": {
            "uri": "skill://core/SKILL.md",
            "_meta": modern_meta(json!({}))
        }
    }));
    let read_resp = client.recv(Duration::from_secs(5));
    assert_eq!(read_resp["result"]["resultType"], "complete");
    let text = read_resp["result"]["contents"][0]["text"].as_str().unwrap();
    assert_eq!(
        skill_md_entry["size"].as_u64().unwrap() as usize,
        text.len()
    );
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let actual_digest = format!("sha256:{}", hex::encode(hasher.finalize()));
    assert_eq!(skill_md_entry["digest"], actual_digest);

    // skills/get resolves individual skills including hidden discovery stub and errors on unknown.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "skills/get",
        "params": {
            "uri": "skill://agent-browser/SKILL.md",
            "_meta": modern_meta(json!({}))
        }
    }));
    let stub = client.recv(Duration::from_secs(5));
    assert_eq!(
        stub["result"]["skill"]["frontmatter"]["name"],
        "agent-browser"
    );

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "skills/get",
        "params": {
            "uri": "skill://does-not-exist/SKILL.md",
            "_meta": modern_meta(json!({}))
        }
    }));
    let missing = client.recv(Duration::from_secs(5));
    assert_eq!(missing["error"]["code"], -32602);

    // resources/directory/read lists direct children and marks subdirectories as inode/directory.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "resources/directory/read",
        "params": {
            "uri": "skill://core",
            "_meta": modern_meta(json!({}))
        }
    }));
    let dir_resp = client.recv(Duration::from_secs(5));
    let dir_items = dir_resp["result"]["resources"].as_array().unwrap();
    assert!(dir_items
        .iter()
        .any(|r| { r["uri"] == "skill://core/references" && r["mimeType"] == "inode/directory" }));

    // Core resources/list also works for legacy initialize clients.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "legacy-skills", "version": "1.0.0" }
        }
    }));
    let init = client.recv(Duration::from_secs(5));
    assert_eq!(
        init["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/skills"]
            ["directoryRead"],
        true
    );
    assert!(init["result"]["capabilities"]["resources"].is_object());

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "resources/list",
        "params": {}
    }));
    let res_list = client.recv(Duration::from_secs(5));
    assert!(res_list["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["uri"] == "skill://core/SKILL.md"));

    let status = client.close();
    assert!(status.success());
}

#[cfg(unix)]
fn setup_mock_daemon(home: &TempDir, session: &str, delays_ms: Vec<u64>) -> thread::JoinHandle<()> {
    use std::os::unix::net::UnixListener;

    let sockets = home.path().join("s");
    std::fs::create_dir_all(&sockets).unwrap();
    let bootstrap = Command::new(env!("CARGO_BIN_EXE_agent-browser"))
        .env("HOME", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", &sockets)
        .env("NO_COLOR", "1")
        .args(["--session", session, "stream", "status", "--json"])
        .output()
        .unwrap();
    assert!(bootstrap.status.success(), "{bootstrap:?}");

    if let Ok(pid_str) = std::fs::read_to_string(sockets.join(format!("{session}.pid"))) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    let _ = std::fs::remove_file(sockets.join(format!("{session}.pid")));
    let sock_path = sockets.join(format!("{session}.sock"));
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();

    thread::spawn(move || {
        let mut idx = 0;
        while idx < delays_ms.len() {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let delay = delays_ms[idx];
            idx += 1;
            let req: Value = serde_json::from_str(&line).unwrap_or_default();
            let req_id = req["id"].clone();
            if delay > 0 {
                thread::sleep(Duration::from_millis(delay));
            }
            let resp = json!({
                "id": req_id,
                "success": true,
                "data": { "waited": true }
            });
            let _ = writeln!(stream, "{}", resp);
            let _ = stream.flush();
        }
    })
}

#[cfg(unix)]
#[test]
fn modern_tasks_create_poll_subscribe_and_cancel_over_stdio() {
    let home = tempfile::tempdir().unwrap();
    let daemon = setup_mock_daemon(&home, "mcp-tasks-mod", vec![600, 5000]);

    let mut client = McpStdioClient::spawn_in(home, &["--task-grace-ms", "100"]);
    let caps_with_tasks = json!({
        "extensions": { "io.modelcontextprotocol/tasks": {} }
    });

    // server/discover advertises io.modelcontextprotocol/tasks.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "server/discover",
        "params": { "_meta": modern_meta(caps_with_tasks.clone()) }
    }));
    let disc = client.recv(Duration::from_secs(5));
    assert!(
        disc["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/tasks"].is_object()
    );

    // Slow wait exceeds 100ms grace window and returns CreateTaskResult (`resultType: "task"`).
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_wait_ms",
            "arguments": { "ms": 600, "session": "mcp-tasks-mod" },
            "_meta": modern_meta(caps_with_tasks.clone())
        }
    }));
    let (created, _) = client.recv_for_id(&json!(2), Duration::from_secs(5));
    assert_eq!(created["result"]["resultType"], "task");
    assert_eq!(created["result"]["status"], "working");
    let task_id = created["result"]["taskId"].as_str().unwrap().to_string();

    // Subscribe to notifications/tasks for this task.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": "sub-1",
        "method": "subscriptions/listen",
        "params": {
            "notifications": { "taskIds": [&task_id] },
            "_meta": modern_meta(caps_with_tasks.clone())
        }
    }));
    let ack = client.recv(Duration::from_secs(5));
    assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");

    // Wait for notifications/tasks completed push, then verify tasks/get also returns completed.
    let notif = client.recv(Duration::from_secs(5));
    assert_eq!(notif["method"], "notifications/tasks");
    assert_eq!(notif["params"]["taskId"], task_id);
    assert_eq!(notif["params"]["status"], "completed");
    assert_eq!(notif["params"]["result"]["isError"], false);

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tasks/get",
        "params": {
            "taskId": &task_id,
            "_meta": modern_meta(caps_with_tasks.clone())
        }
    }));
    let (polled, _) = client.recv_for_id(&json!(3), Duration::from_secs(5));
    assert_eq!(polled["result"]["resultType"], "complete");
    assert_eq!(polled["result"]["status"], "completed");

    // Calling tasks/get without declaring io.modelcontextprotocol/tasks returns -32021.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tasks/get",
        "params": {
            "taskId": &task_id,
            "_meta": modern_meta(json!({}))
        }
    }));
    let (missing_cap, _) = client.recv_for_id(&json!(4), Duration::from_secs(5));
    assert_eq!(missing_cap["error"]["code"], -32021);

    // Second long wait: cancel it via tasks/cancel and confirm cancelled state promptly.
    let cancel_start = Instant::now();
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_wait_ms",
            "arguments": { "ms": 5000, "session": "mcp-tasks-mod" },
            "_meta": modern_meta(caps_with_tasks.clone())
        }
    }));
    let (created2, _) = client.recv_for_id(&json!(5), Duration::from_secs(5));
    let task_id2 = created2["result"]["taskId"].as_str().unwrap().to_string();

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tasks/cancel",
        "params": {
            "taskId": &task_id2,
            "_meta": modern_meta(caps_with_tasks.clone())
        }
    }));
    let (cancel_ack, _) = client.recv_for_id(&json!(6), Duration::from_secs(5));
    assert_eq!(cancel_ack["result"]["resultType"], "complete");

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tasks/get",
        "params": {
            "taskId": &task_id2,
            "_meta": modern_meta(caps_with_tasks)
        }
    }));
    let (cancelled, _) = client.recv_for_id(&json!(7), Duration::from_secs(5));
    assert_eq!(cancelled["result"]["status"], "cancelled");
    assert!(cancel_start.elapsed() < Duration::from_secs(2));

    let status = client.close();
    assert!(status.success());
    drop(daemon);
}

#[cfg(unix)]
#[test]
fn legacy_2025_tasks_lifecycle_list_result_and_non_task_rejection() {
    let home = tempfile::tempdir().unwrap();
    let daemon = setup_mock_daemon(&home, "mcp-tasks-leg", vec![400]);

    let mut client = McpStdioClient::spawn_in(home, &[]);
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "legacy-tasks", "version": "1.0.0" }
        }
    }));
    let init = client.recv(Duration::from_secs(5));
    assert!(init["result"]["capabilities"]["tasks"]["requests"]["tools"]["call"].is_object());

    // Non-task tool rejects params.task with -32601.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_tools_profiles",
            "arguments": {},
            "task": { "ttl": 60000 }
        }
    }));
    let (rejected, _) = client.recv_for_id(&json!(2), Duration::from_secs(5));
    assert_eq!(rejected["error"]["code"], -32601);

    // Task-eligible tool returns CreateTaskResult immediately.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_wait_ms",
            "arguments": { "ms": 400, "session": "mcp-tasks-leg" },
            "task": { "ttl": 60000 }
        }
    }));
    let (created, _) = client.recv_for_id(&json!(3), Duration::from_secs(5));
    let task_id = created["result"]["task"]["taskId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(created["result"]["task"]["status"], "working");

    // tasks/list includes the created task.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tasks/list",
        "params": {}
    }));
    let (listed, _) = client.recv_for_id(&json!(4), Duration::from_secs(5));
    assert!(listed["result"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["taskId"] == task_id));

    // tasks/result blocks until completion and includes related-task metadata.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tasks/result",
        "params": { "taskId": &task_id }
    }));
    let (final_res, _) = client.recv_for_id(&json!(5), Duration::from_secs(5));
    assert_eq!(final_res["result"]["isError"], false);
    assert_eq!(
        final_res["result"]["_meta"]["io.modelcontextprotocol/related-task"]["taskId"],
        task_id
    );

    let status = client.close();
    assert!(status.success());
    drop(daemon);
}

#[cfg(unix)]
fn setup_confirm_mock_daemon(
    home: &TempDir,
    session: &str,
    expected_commands: usize,
) -> (
    thread::JoinHandle<Vec<Value>>,
    std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
) {
    use std::os::unix::net::UnixListener;
    use std::sync::{Arc, Mutex};

    let sockets = home.path().join("s");
    std::fs::create_dir_all(&sockets).unwrap();
    let bootstrap = Command::new(env!("CARGO_BIN_EXE_agent-browser"))
        .env("HOME", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", &sockets)
        .env("NO_COLOR", "1")
        .args(["--session", session, "stream", "status", "--json"])
        .output()
        .unwrap();
    assert!(bootstrap.status.success(), "{bootstrap:?}");

    if let Ok(pid_str) = std::fs::read_to_string(sockets.join(format!("{session}.pid"))) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    let _ = std::fs::remove_file(sockets.join(format!("{session}.pid")));
    let sock_path = sockets.join(format!("{session}.sock"));
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();

    let recorded = Arc::new(Mutex::new(Vec::new()));
    let rec_clone = Arc::clone(&recorded);
    let handle = thread::spawn(move || {
        let mut count = 0;
        while count < expected_commands {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let req: Value = serde_json::from_str(&line).unwrap_or_default();
            rec_clone.lock().unwrap().push(req.clone());
            count += 1;
            let req_id = req["id"].clone();
            let action = req["action"].as_str().unwrap_or("");
            let resp = match action {
                "evaluate" | "wait" => json!({
                    "id": req_id,
                    "success": true,
                    "data": {
                        "confirmation_required": true,
                        "confirmation_id": "c_42",
                        "action": action
                    }
                }),
                "confirm" => json!({
                    "id": req_id,
                    "success": true,
                    "data": {
                        "confirmed": true,
                        "action": "eval",
                        "result": {
                            "success": true,
                            "data": { "result": "confirmed-ok" }
                        }
                    }
                }),
                "deny" => json!({
                    "id": req_id,
                    "success": true,
                    "data": { "denied": true, "confirmation_id": req["confirmation_id"] }
                }),
                _ => json!({
                    "id": req_id,
                    "success": true,
                    "data": { "ok": true }
                }),
            };
            let _ = writeln!(stream, "{}", resp);
            let _ = stream.flush();
        }
        rec_clone.lock().unwrap().clone()
    });
    (handle, recorded)
}

#[cfg(unix)]
#[test]
fn modern_elicitation_mrtr_preflight_daemon_bridge_and_fallback() {
    let home = tempfile::tempdir().unwrap();
    let (daemon, recorded) = setup_confirm_mock_daemon(&home, "mcp-elicit-mod", 3);

    let mut client = McpStdioClient::spawn_in(home, &["--elicit", "eval"]);
    let elicit_caps = json!({ "elicitation": { "form": {} } });

    // 1. Pre-flight MRTR: first call returns InputRequiredResult with signed requestState.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "document.title", "session": "mcp-elicit-mod" },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (ir, _) = client.recv_for_id(&json!(1), Duration::from_secs(5));
    assert_eq!(ir["result"]["resultType"], "input_required");
    let state_token = ir["result"]["requestState"].as_str().unwrap().to_string();
    let req_key = ir["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert_eq!(
        ir["result"]["inputRequests"][&req_key]["method"],
        "elicitation/create"
    );

    // 2. Retry with decline: refused without touching the daemon.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "document.title", "session": "mcp-elicit-mod" },
            "requestState": &state_token,
            "inputResponses": {
                &req_key: { "action": "decline" }
            },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (declined, _) = client.recv_for_id(&json!(2), Duration::from_secs(5));
    assert_eq!(declined["result"]["isError"], true);
    assert!(recorded.lock().unwrap().is_empty());

    // 3. Replaying the already-consumed requestState fails with -32602.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "document.title", "session": "mcp-elicit-mod" },
            "requestState": &state_token,
            "inputResponses": {
                &req_key: { "action": "accept", "content": { "approve": true } }
            },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (replayed, _) = client.recv_for_id(&json!(3), Duration::from_secs(5));
    assert_eq!(replayed["error"]["code"], -32602);

    // 4. Approve pre-flight -> daemon parks confirmation_required -> MRTR daemon bridge -> approve -> confirm c_42!
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "1+1", "session": "mcp-elicit-mod" },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (ir_pre, _) = client.recv_for_id(&json!(4), Duration::from_secs(5));
    let tok_pre = ir_pre["result"]["requestState"]
        .as_str()
        .unwrap()
        .to_string();
    let key_pre = ir_pre["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "1+1", "session": "mcp-elicit-mod" },
            "requestState": &tok_pre,
            "inputResponses": {
                &key_pre: { "action": "accept", "content": { "approve": true } }
            },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (ir_daemon, _) = client.recv_for_id(&json!(5), Duration::from_secs(5));
    assert_eq!(ir_daemon["result"]["resultType"], "input_required");
    let tok_daemon = ir_daemon["result"]["requestState"]
        .as_str()
        .unwrap()
        .to_string();
    let key_daemon = ir_daemon["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "1+1", "session": "mcp-elicit-mod" },
            "requestState": &tok_daemon,
            "inputResponses": {
                &key_daemon: { "action": "accept", "content": { "approve": true } }
            },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (confirmed, _) = client.recv_for_id(&json!(6), Duration::from_secs(5));
    assert_eq!(confirmed["result"]["resultType"], "complete");
    assert_eq!(confirmed["result"]["isError"], false);
    assert_eq!(
        confirmed["result"]["structuredContent"]["response"]["data"]["confirmed"],
        true
    );

    // 5. Client WITHOUT elicitation capability (and no --require-elicitation) keeps current behavior unchanged.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "2+2", "session": "mcp-elicit-mod" },
            "_meta": modern_meta(json!({}))
        }
    }));
    let (unchanged, _) = client.recv_for_id(&json!(7), Duration::from_secs(5));
    assert_eq!(unchanged["result"]["resultType"], "complete");
    assert_eq!(
        unchanged["result"]["structuredContent"]["response"]["data"]["confirmation_required"],
        true
    );

    let status = client.close();
    assert!(status.success());
    let commands = daemon.join().unwrap();
    assert_eq!(commands.len(), 3);
}

#[cfg(unix)]
#[test]
fn modern_task_mid_flight_elicitation_via_tasks_update() {
    let home = tempfile::tempdir().unwrap();
    let (daemon, _) = setup_confirm_mock_daemon(&home, "mcp-elicit-task", 2);

    let mut client = McpStdioClient::spawn_in(home, &[]);
    let caps = json!({
        "elicitation": { "form": {} },
        "extensions": { "io.modelcontextprotocol/tasks": {} }
    });

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_wait_ms",
            "arguments": { "ms": 50, "session": "mcp-elicit-task" },
            "_meta": modern_meta(caps.clone())
        }
    }));
    let (created, _) = client.recv_for_id(&json!(1), Duration::from_secs(5));
    assert_eq!(created["result"]["resultType"], "task");
    assert_eq!(created["result"]["status"], "input_required");
    let task_id = created["result"]["taskId"].as_str().unwrap().to_string();

    // Poll tasks/get to inspect inputRequests.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tasks/get",
        "params": {
            "taskId": &task_id,
            "_meta": modern_meta(caps.clone())
        }
    }));
    let (polled, _) = client.recv_for_id(&json!(2), Duration::from_secs(5));
    assert_eq!(polled["result"]["status"], "input_required");
    let input_key = polled["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();

    // Submit approval via tasks/update.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tasks/update",
        "params": {
            "taskId": &task_id,
            "inputResponses": {
                &input_key: { "action": "accept", "content": { "approve": true } }
            },
            "_meta": modern_meta(caps.clone())
        }
    }));
    let (ack, _) = client.recv_for_id(&json!(3), Duration::from_secs(5));
    assert_eq!(ack["result"]["resultType"], "complete");

    // Poll until completed with confirmed daemon result.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut seq = 4;
    let final_task = loop {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for task completion"
        );
        client.send(&json!({
            "jsonrpc": "2.0",
            "id": seq,
            "method": "tasks/get",
            "params": {
                "taskId": &task_id,
                "_meta": modern_meta(caps.clone())
            }
        }));
        let (resp, _) = client.recv_for_id(&json!(seq), Duration::from_secs(5));
        seq += 1;
        if resp["result"]["status"] == "completed" {
            break resp;
        }
        thread::sleep(Duration::from_millis(30));
    };
    assert_eq!(
        final_task["result"]["result"]["structuredContent"]["response"]["data"]["confirmed"],
        true
    );

    let status = client.close();
    assert!(status.success());
    let commands = daemon.join().unwrap();
    assert_eq!(commands[0]["action"], "wait");
    assert_eq!(commands[1]["action"], "confirm");
}

#[cfg(unix)]
#[test]
fn legacy_2025_server_initiated_elicitation_and_require_elicitation_flag() {
    let home = tempfile::tempdir().unwrap();
    let (daemon, _) = setup_confirm_mock_daemon(&home, "mcp-elicit-leg", 2);

    let mut client = McpStdioClient::spawn_in(home, &[]);
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": { "elicitation": { "form": {} } },
            "clientInfo": { "name": "legacy-elicit", "version": "1.0.0" }
        }
    }));
    let _ = client.recv(Duration::from_secs(5));

    // Call eval -> daemon parks confirmation_required -> server sends elicitation/create request to client.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "document.title", "session": "mcp-elicit-leg" }
        }
    }));
    let elicit_req = client.recv(Duration::from_secs(5));
    assert_eq!(elicit_req["method"], "elicitation/create");
    assert_eq!(elicit_req["params"]["mode"], "form");
    let srv_id = elicit_req["id"].clone();

    // Client responds accept + approve: true -> server runs `confirm c_42` and finishes tools/call.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": srv_id,
        "result": {
            "action": "accept",
            "content": { "approve": true }
        }
    }));
    let (final_resp, _) = client.recv_for_id(&json!(2), Duration::from_secs(5));
    assert_eq!(final_resp["result"]["isError"], false);
    assert_eq!(
        final_resp["result"]["structuredContent"]["response"]["data"]["confirmed"],
        true
    );

    let status = client.close();
    assert!(status.success());
    let commands = daemon.join().unwrap();
    assert_eq!(commands[0]["action"], "evaluate");
    assert_eq!(commands[1]["action"], "confirm");

    // Verify --require-elicitation fails closed when client lacks elicitation capability.
    let mut strict = McpStdioClient::spawn(&["--elicit", "eval", "--require-elicitation"]);
    strict.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "1+1" },
            "_meta": modern_meta(json!({}))
        }
    }));
    let (err_resp, _) = strict.recv_for_id(&json!(1), Duration::from_secs(5));
    assert_eq!(err_resp["error"]["code"], -32021);
    assert!(err_resp["error"]["data"]["requiredCapabilities"]["elicitation"]["form"].is_object());
    let status = strict.close();
    assert!(status.success());
}

#[cfg(unix)]
#[test]
fn batch_elicitation_gates_subcommands_and_binds_immutable_batch() {
    let home = tempfile::tempdir().unwrap();
    let (daemon, recorded) = setup_confirm_mock_daemon(&home, "mcp-batch-elicit", 1);

    let mut client = McpStdioClient::spawn_in(
        home,
        &[
            "--tools",
            "all",
            "--elicit",
            "high-risk",
            "--require-elicitation",
        ],
    );
    let elicit_caps = json!({ "elicitation": { "form": {} } });

    // 1. Without client elicitation support + --require-elicitation, a batch with a gated
    // subcommand fails closed with -32021 and never reaches the daemon.
    let gated_batch_args = json!({
        "commands": [
            ["stream", "status"],
            ["eval", "document.cookie"]
        ],
        "session": "mcp-batch-elicit"
    });
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_batch",
            "arguments": &gated_batch_args,
            "_meta": modern_meta(json!({}))
        }
    }));
    let (no_cap, _) = client.recv_for_id(&json!(1), Duration::from_secs(5));
    assert_eq!(no_cap["error"]["code"], -32021);
    assert!(recorded.lock().unwrap().is_empty());

    // 2. With form-mode elicitation, the server returns InputRequiredResult naming the gated op.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_batch",
            "arguments": &gated_batch_args,
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (ir, _) = client.recv_for_id(&json!(2), Duration::from_secs(5));
    assert_eq!(ir["result"]["resultType"], "input_required");
    let token = ir["result"]["requestState"].as_str().unwrap().to_string();
    let req_key = ir["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert!(ir["result"]["inputRequests"][&req_key]["params"]["message"]
        .as_str()
        .unwrap()
        .contains("eval"));

    // 3. Mutating ANY subcommand in the batch on retry invalidates the signed requestState (-32602).
    let mutated_batch_args = json!({
        "commands": [
            ["stream", "status"],
            ["eval", "fetch('https://evil.example')"]
        ],
        "session": "mcp-batch-elicit"
    });
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_batch",
            "arguments": &mutated_batch_args,
            "requestState": &token,
            "inputResponses": {
                &req_key: { "action": "accept", "content": { "approve": true } }
            },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (mutated_err, _) = client.recv_for_id(&json!(3), Duration::from_secs(5));
    assert_eq!(mutated_err["error"]["code"], -32602);
    assert!(recorded.lock().unwrap().is_empty());

    // 4. Declining the original batch refuses without executing any subcommand.
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_batch",
            "arguments": &gated_batch_args,
            "requestState": &token,
            "inputResponses": {
                &req_key: { "action": "decline" }
            },
            "_meta": modern_meta(elicit_caps)
        }
    }));
    let (declined, _) = client.recv_for_id(&json!(4), Duration::from_secs(5));
    assert_eq!(declined["result"]["isError"], true);
    assert!(recorded.lock().unwrap().is_empty());

    let status = client.close();
    assert!(status.success());
    drop(daemon);
}

#[cfg(unix)]
#[test]
fn chained_daemon_confirmations_elicit_every_gate_in_modern_and_legacy() {
    use std::os::unix::net::UnixListener;

    let home = tempfile::tempdir().unwrap();
    let sockets = home.path().join("s");
    std::fs::create_dir_all(&sockets).unwrap();

    for (session, expected_cmds) in [("mcp-chain-mod", 3_usize), ("mcp-chain-leg", 3_usize)] {
        let bootstrap = Command::new(env!("CARGO_BIN_EXE_agent-browser"))
            .env("HOME", home.path())
            .env("AGENT_BROWSER_SOCKET_DIR", &sockets)
            .env("NO_COLOR", "1")
            .args(["--session", session, "stream", "status", "--json"])
            .output()
            .unwrap();
        assert!(bootstrap.status.success(), "{bootstrap:?}");
        if let Ok(pid_str) = std::fs::read_to_string(sockets.join(format!("{session}.pid"))) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
        let _ = std::fs::remove_file(sockets.join(format!("{session}.pid")));
        let sock_path = sockets.join(format!("{session}.sock"));
        let _ = std::fs::remove_file(&sock_path);
        let listener = UnixListener::bind(&sock_path).unwrap();

        thread::spawn(move || {
            let mut count = 0;
            while count < expected_cmds {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                count += 1;
                let req: Value = serde_json::from_str(&line).unwrap_or_default();
                let req_id = req["id"].clone();
                let action = req["action"].as_str().unwrap_or("");
                let cid = req["confirmationId"].as_str().unwrap_or("");
                let resp = match (action, cid) {
                    ("evaluate", _) => json!({
                        "id": req_id,
                        "success": true,
                        "data": {
                            "confirmation_required": true,
                            "confirmation_id": "c_first",
                            "action": "eval"
                        }
                    }),
                    ("confirm", "c_first") => json!({
                        "id": req_id,
                        "success": true,
                        "data": {
                            "confirmed": true,
                            "confirmation_id": "c_first",
                            "action": "eval",
                            "result": {
                                "success": true,
                                "data": {
                                    "confirmation_required": true,
                                    "confirmation_id": "c_second",
                                    "action": "plugin:vault:credential.read"
                                }
                            }
                        }
                    }),
                    ("confirm", "c_second") => json!({
                        "id": req_id,
                        "success": true,
                        "data": {
                            "confirmed": true,
                            "confirmation_id": "c_second",
                            "action": "plugin:vault:credential.read",
                            "result": {
                                "success": true,
                                "data": { "result": "all-gates-approved" }
                            }
                        }
                    }),
                    _ => json!({
                        "id": req_id,
                        "success": false,
                        "error": format!("unexpected confirm id: {cid}")
                    }),
                };
                let _ = writeln!(stream, "{}", resp);
                let _ = stream.flush();
            }
        });
    }

    let mut client = McpStdioClient::spawn_in(home, &[]);
    let elicit_caps = json!({ "elicitation": { "form": {} } });

    // Modern 2026-07-28 MRTR: gate 1 -> approve -> gate 2 -> approve -> completed!
    let mod_args = json!({ "script": "1+1", "session": "mcp-chain-mod" });
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": &mod_args,
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (ir1, _) = client.recv_for_id(&json!(1), Duration::from_secs(5));
    assert_eq!(ir1["result"]["resultType"], "input_required");
    let tok1 = ir1["result"]["requestState"].as_str().unwrap().to_string();
    let key1 = ir1["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": &mod_args,
            "requestState": &tok1,
            "inputResponses": {
                &key1: { "action": "accept", "content": { "approve": true } }
            },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    let (ir2, _) = client.recv_for_id(&json!(2), Duration::from_secs(5));
    assert_eq!(
        ir2["result"]["resultType"], "input_required",
        "second parked approval must trigger a second InputRequiredResult: {ir2}"
    );
    let tok2 = ir2["result"]["requestState"].as_str().unwrap().to_string();
    let key2 = ir2["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    assert!(ir2["result"]["inputRequests"][&key2]["params"]["message"]
        .as_str()
        .unwrap()
        .contains("c_second"));

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": &mod_args,
            "requestState": &tok2,
            "inputResponses": {
                &key2: { "action": "accept", "content": { "approve": true } }
            },
            "_meta": modern_meta(elicit_caps)
        }
    }));
    let (done_mod, _) = client.recv_for_id(&json!(3), Duration::from_secs(5));
    assert_eq!(done_mod["result"]["resultType"], "complete");
    assert_eq!(done_mod["result"]["isError"], false);
    assert_eq!(
        done_mod["result"]["structuredContent"]["response"]["data"]["result"]["data"]["result"],
        "all-gates-approved"
    );

    // Legacy 2025-11-25: elicits c_first, then elicits c_second, then returns final result!
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 10,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": { "elicitation": { "form": {} } },
            "clientInfo": { "name": "legacy-chain", "version": "1.0.0" }
        }
    }));
    let _ = client.recv(Duration::from_secs(5));

    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 11,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_eval",
            "arguments": { "script": "1+1", "session": "mcp-chain-leg" }
        }
    }));
    let elicit1 = client.recv(Duration::from_secs(5));
    assert_eq!(elicit1["method"], "elicitation/create");
    assert!(elicit1["params"]["message"]
        .as_str()
        .unwrap()
        .contains("c_first"));
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": elicit1["id"],
        "result": { "action": "accept", "content": { "approve": true } }
    }));

    let elicit2 = client.recv(Duration::from_secs(5));
    assert_eq!(elicit2["method"], "elicitation/create");
    assert!(elicit2["params"]["message"]
        .as_str()
        .unwrap()
        .contains("c_second"));
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": elicit2["id"],
        "result": { "action": "accept", "content": { "approve": true } }
    }));

    let (done_leg, _) = client.recv_for_id(&json!(11), Duration::from_secs(5));
    assert_eq!(done_leg["result"]["isError"], false);
    assert_eq!(
        done_leg["result"]["structuredContent"]["response"]["data"]["result"]["data"]["result"],
        "all-gates-approved"
    );

    let status = client.close();
    assert!(status.success());
}

#[test]
fn batch_eval_stdin_and_curl_dev_stdin_never_block_mcp_protocol_reader() {
    let mut client = McpStdioClient::spawn(&["--tools", "all", "--elicit", "high-risk,state"]);
    let elicit_caps = json!({ "elicitation": { "form": {} } });

    // Send a batch containing `eval --stdin` and `cookies set --curl /dev/stdin` while
    // keeping MCP stdio open, followed immediately by a `ping` request. If classification
    // ever touched process stdin or opened `/dev/stdin`, this would deadlock or swallow `ping`.
    let started = Instant::now();
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_batch",
            "arguments": {
                "commands": [
                    ["eval", "--stdin"],
                    ["cookies", "set", "--curl", "/dev/stdin"]
                ]
            },
            "_meta": modern_meta(elicit_caps.clone())
        }
    }));
    client.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "ping",
        "params": { "_meta": modern_meta(elicit_caps) }
    }));

    let (ir, _) = client.recv_for_id(&json!(1), Duration::from_secs(2));
    assert_eq!(ir["result"]["resultType"], "input_required");
    let req_key = ir["result"]["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let msg = ir["result"]["inputRequests"][&req_key]["params"]["message"]
        .as_str()
        .unwrap();
    assert!(msg.contains("eval --stdin"), "unexpected prompt: {msg}");
    assert!(msg.contains("cookies set"), "unexpected prompt: {msg}");

    let (ping_resp, _) = client.recv_for_id(&json!(2), Duration::from_secs(2));
    assert_eq!(ping_resp["id"], 2);
    assert_eq!(ping_resp["result"]["resultType"], "complete");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "classification blocked protocol reader: {:?}",
        started.elapsed()
    );

    let status = client.close();
    assert!(status.success());
}
