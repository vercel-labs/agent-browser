use serde_json::{json, Value};
use std::process::{Output, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

fn command(home: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-browser"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", home.path().join("sockets"))
        .env("NO_COLOR", "1");
    command
}

fn run(command: &mut Command, input: &str) -> Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut child = command
                .kill_on_drop(true)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(10), async move {
                let mut stdin = child.stdin.take().unwrap();
                stdin.write_all(input.as_bytes()).await.unwrap();
                drop(stdin);
                child.wait_with_output().await.unwrap()
            })
            .await
            .expect("CLI/MCP subprocess exceeded 10 seconds")
        })
}

fn mcp_call(args: &[&str], stdin: Option<&str>) -> (i32, Value) {
    let home = tempfile::tempdir().unwrap();
    let output = run(
        command(&home).args(["mcp", "call"]).args(args),
        stdin.unwrap_or(""),
    );
    let value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"));
    (output.status.code().unwrap(), value)
}

fn mcp_stdio(command: &mut Command, arguments: &[Value]) -> (Output, Vec<Value>) {
    let mut requests = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "config-tests", "version": "1"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ];
    requests.extend(arguments.iter().enumerate().map(|(index, arguments)| {
        json!({
            "jsonrpc": "2.0",
            "id": index + 1,
            "method": "tools/call",
            "params": {"name": "agent_browser_session", "arguments": arguments}
        })
    }));
    requests.push(json!({
        "jsonrpc": "2.0", "id": arguments.len() + 1, "method": "shutdown"
    }));
    let input: String = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect();
    let output = run(command, &input);
    assert!(output.status.success(), "{output:?}");
    let responses: Vec<Value> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), arguments.len() + 2, "{responses:?}");
    for (id, response) in responses.iter().enumerate() {
        assert_eq!(response["jsonrpc"], "2.0", "{response}");
        assert_eq!(response["id"], json!(id), "{response}");
    }
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "agent-browser"
    );
    assert_eq!(responses.last().unwrap()["result"], json!({}));
    let calls = responses
        .into_iter()
        .skip(1)
        .take(arguments.len())
        .collect();
    (output, calls)
}

fn assert_project_warning(stderr: &str, count: usize) {
    let warning = format!("ignoring {count} unsupported settings from ./agent-browser.json");
    assert!(stderr.contains(&warning), "{stderr}");
    assert!(stderr.len() < 1024, "warning is {} bytes", stderr.len());
    assert!(stderr.contains("~/.agent-browser/config.json"), "{stderr}");
    assert!(stderr.contains("--config"), "{stderr}");
    assert!(stderr.contains("AGENT_BROWSER_CONFIG"), "{stderr}");
    assert!(stderr.contains("MCP server"), "{stderr}");
}

fn assert_mcp_session(result: &Value, session: &str) {
    assert_eq!(result["isError"], false, "{result}");
    let response = &result["structuredContent"]["response"];
    assert_eq!(response["success"], true, "{result}");
    assert_eq!(response["data"]["session"], session, "{result}");
}

#[test]
fn mcp_call_prints_the_tool_result_and_version() {
    let (code, result) = mcp_call(&["agent_browser_tools_profiles"], None);
    assert_eq!(code, 0, "{result}");
    assert_eq!(result["isError"], false);
    assert_eq!(
        result["_meta"]["agentBrowserVersion"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(result["structuredContent"].is_object(), "{result}");
}

#[test]
fn mcp_call_reads_arguments_from_stdin() {
    let (code, result) = mcp_call(&["agent_browser_tools_profiles", "-"], Some("{}"));
    assert_eq!(code, 0, "{result}");
    assert_eq!(result["isError"], false);
}

#[test]
fn mcp_call_reports_invalid_calls_with_exit_code_two() {
    let (code, result) = mcp_call(&["agent_browser_nope"], None);
    assert_eq!(code, 2);
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Unknown tool"));
    assert_eq!(
        result["_meta"]["agentBrowserVersion"],
        env!("CARGO_PKG_VERSION")
    );

    let (code, result) = mcp_call(&["agent_browser_tools_profiles", "not json"], None);
    assert_eq!(code, 2);
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("JSON object"));

    let (code, result) = mcp_call(&["agent_browser_tools_profiles", r#"{"bogus":1}"#], None);
    assert_eq!(code, 2, "{result}");
}

#[test]
fn mcp_call_allows_every_tool_unless_profiles_limit_it() {
    let (code, result) = mcp_call(
        &["agent_browser_network_har_start", "--tools", "core"],
        None,
    );
    assert_eq!(code, 2);
    assert!(result["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not enabled"));
}

#[test]
fn cli_and_mcp_keep_user_config_when_project_settings_require_trust() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(home.path().join(".agent-browser")).unwrap();
    std::fs::write(
        home.path().join(".agent-browser/config.json"),
        json!({"session": "user-session"}).to_string(),
    )
    .unwrap();
    let long_key = format!("benignSetting{}", "x".repeat(16_384));
    std::fs::write(
        home.path().join("agent-browser.json"),
        json!({
            "$schema": "https://agent-browser.dev/schema.json",
            "json": true,
            "session": "project-session",
            "allowedDomains": [],
            "futureOption": false,
            "state": null,
            long_key: "benign-unused-value"
        })
        .to_string(),
    )
    .unwrap();

    let output = run(command(&home).arg("session"), "");
    assert!(output.status.success(), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["data"]["session"], "user-session", "{response}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_project_warning(&stderr, 4);
    for ignored in [
        "session",
        "allowedDomains",
        "futureOption",
        "$schema",
        "benignSetting",
        "benign-unused-value",
    ] {
        assert!(!stderr.contains(ignored), "{stderr}");
    }

    let (output, calls) = mcp_stdio(
        command(&home).args(["mcp", "--tools", "state"]),
        &[json!({})],
    );
    assert_project_warning(&String::from_utf8_lossy(&output.stderr), 4);
    let result = &calls[0]["result"];
    assert_mcp_session(result, "user-session");
    let stderr = result["structuredContent"]["stderr"].as_str().unwrap();
    assert_project_warning(stderr, 4);
    for ignored in ["session", "allowedDomains", "futureOption", "$schema"] {
        assert!(!stderr.contains(ignored), "{stderr}");
    }
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.len() < 2048, "tool text is {} bytes", text.len());
    for sentinel in ["benignSetting", "benign-unused-value"] {
        assert!(!stderr.contains(sentinel), "{stderr}");
        assert!(!text.contains(sentinel), "{text}");
    }
}

#[test]
fn explicitly_selected_config_remains_trusted_for_cli_and_mcp() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("agent-browser.json");
    let fallback = home.path().join("other-config.json");
    std::fs::write(
        &path,
        json!({
            "session": "selected-session",
            "json": true,
            "allowedDomains": ["example.com"]
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(&fallback, json!({"session": "other-session"}).to_string()).unwrap();

    for args in [
        &["session"][..],
        &["mcp", "call", "agent_browser_session"][..],
    ] {
        for use_flag in [false, true] {
            let mut cmd = command(&home);
            if use_flag {
                cmd.env("AGENT_BROWSER_CONFIG", &fallback)
                    .arg("--config")
                    .arg(&path);
            } else {
                cmd.env("AGENT_BROWSER_CONFIG", &path);
            }
            let output = run(cmd.args(args), "");
            assert!(output.status.success(), "{output:?}");
            let response: Value = serde_json::from_slice(&output.stdout).unwrap();
            if args[0] == "mcp" {
                assert_mcp_session(&response, "selected-session");
            } else {
                assert_eq!(
                    response["data"]["session"], "selected-session",
                    "{response}"
                );
            }
            assert!(output.stderr.is_empty(), "{output:?}");
        }
    }

    for use_flag in [false, true] {
        let mut cmd = command(&home);
        cmd.args(["mcp", "--tools", "state"]);
        if use_flag {
            cmd.env("AGENT_BROWSER_CONFIG", &fallback)
                .arg("--config")
                .arg(&path);
        } else {
            cmd.env("AGENT_BROWSER_CONFIG", &path);
        }
        let (output, calls) = mcp_stdio(
            &mut cmd,
            &[json!({}), json!({"allowedDomains": ["example.net"]})],
        );
        assert!(output.stderr.is_empty(), "{output:?}");
        let result = &calls[0]["result"];
        assert_mcp_session(result, "selected-session");
        assert_eq!(result["structuredContent"]["stderr"], "", "{result}");
        assert_eq!(calls[1]["error"]["code"], -32602, "{:?}", calls[1]);
        assert!(
            calls[1]["error"]["message"]
                .as_str()
                .unwrap()
                .contains("already set"),
            "{:?}",
            calls[1]
        );
    }

    for use_flag in [false, true] {
        let mut cmd = command(&home);
        if use_flag {
            cmd.arg("--config").arg(&fallback);
        } else {
            cmd.env("AGENT_BROWSER_CONFIG", &fallback);
        }
        let output = run(cmd.arg("session"), "");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, b"other-session\n", "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }

    std::fs::create_dir(home.path().join(".agent-browser")).unwrap();
    std::fs::write(
        home.path().join(".agent-browser/config.json"),
        json!({"session": "user-session", "allowedDomains": ["example.com"]}).to_string(),
    )
    .unwrap();
    std::fs::write(&path, "{}").unwrap();
    let (output, calls) = mcp_stdio(
        command(&home)
            .env("AGENT_BROWSER_CONFIG", &fallback)
            .args(["mcp", "--tools", "state", "--config"])
            .arg(&path),
        &[json!({}), json!({"allowedDomains": ["example.net"]})],
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    for call in calls {
        let result = &call["result"];
        assert_mcp_session(result, "default");
        assert_eq!(result["structuredContent"]["stderr"], "", "{call}");
    }
}

#[test]
fn config_flag_requires_a_path_before_cli_or_mcp_dispatch() {
    let home = tempfile::tempdir().unwrap();
    let fallback = home.path().join("selected.json");
    std::fs::write(&fallback, "{}").unwrap();
    for args in [
        &["session", "--config"][..],
        &["mcp", "--config"][..],
        &["mcp", "call", "agent_browser_session", "--config"][..],
    ] {
        let output = run(
            command(&home)
                .env("AGENT_BROWSER_CONFIG", &fallback)
                .args(args),
            "",
        );
        assert_eq!(output.status.code(), Some(1), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("--config requires a file path"), "{stderr}");
    }
}

#[test]
fn schema_and_null_project_settings_do_not_warn() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("agent-browser.json"),
        json!({
            "$schema": "https://agent-browser.dev/schema.json",
            "json": true,
            "session": null,
            "futureOption": null
        })
        .to_string(),
    )
    .unwrap();
    let (output, calls) = mcp_stdio(
        command(&home).args(["mcp", "--tools", "state"]),
        &[json!({})],
    );
    assert!(output.stderr.is_empty(), "{output:?}");
    let result = &calls[0]["result"];
    assert_mcp_session(result, "default");
    assert_eq!(result["structuredContent"]["stderr"], "", "{result}");
}

#[test]
fn automatic_config_errors_hide_values_and_trusted_errors_keep_details() {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("agent-browser.json");
    let wrong_type = "benign-preference-value".repeat(1024);
    for content in [
        json!({"headed": wrong_type}).to_string(),
        "{\"json\":".to_string(),
    ] {
        std::fs::write(&path, content).unwrap();
        let (output, calls) = mcp_stdio(
            command(&home).args(["mcp", "--tools", "state"]),
            &[json!({})],
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(
                "invalid config file ./agent-browser.json: expected a JSON object with correctly typed preferences"
            ),
            "{stderr}"
        );
        assert!(stderr.len() < 512, "error is {} bytes", stderr.len());
        assert!(!stderr.contains("benign-preference-value"), "{stderr}");
        let result = &calls[0]["result"];
        assert_mcp_session(result, "default");
        assert_eq!(result["structuredContent"]["stderr"], stderr.as_ref());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.len() < 1024, "tool text is {} bytes", text.len());
        assert!(!text.contains("benign-preference-value"), "{text}");
    }

    std::fs::write(
        &path,
        json!({"headed": "benign-preference-value"}).to_string(),
    )
    .unwrap();
    let output = run(command(&home).arg("--config").arg(&path).arg("session"), "");
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("benign-preference-value"), "{stderr}");
    assert!(stderr.contains("expected a boolean"), "{stderr}");
}
