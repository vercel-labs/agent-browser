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
