#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use tempfile::TempDir;

const COMMANDS: &str = r#"[["stream","status"],["PUBLIC_SENTINEL"],["stream","status"]]"#;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        std::fs::create_dir(fixture.0.path().join("s")).unwrap();
        fixture
    }

    fn run(&self, args: &[&str], input: &str, code: i32) -> Value {
        let output = self.run_raw(args, input, code);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"))
    }

    fn run_raw(&self, args: &[&str], input: &str, code: i32) -> Output {
        let root = self.0.path();
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent-browser"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", root)
            .env("USERPROFILE", root)
            .env("TMPDIR", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("AGENT_BROWSER_SOCKET_DIR", root.join("s"))
            .env("NO_COLOR", "1")
            .current_dir(root)
            .args(["--session", "batch-test"])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        output
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(pid) = std::fs::read_to_string(self.0.path().join("s/batch-test.pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
    }
}

fn assert_identity(row: &Value, command: &Value, index: usize, omit: bool) {
    if omit {
        assert_eq!(row["index"], index);
        assert!(row.get("command").is_none(), "{row}");
    } else {
        assert_eq!(&row["command"], command);
        assert!(row.get("index").is_none(), "{row}");
    }
}

#[test]
fn batch_preserves_results_and_parse_errors_with_optional_command_omission() {
    let fixture = Fixture::new();
    let commands: Value = serde_json::from_str(COMMANDS).unwrap();
    for omit in [false, true] {
        for bail in [false, true] {
            let mut args = vec!["batch", "--json"];
            if omit {
                args.push("--omit-command");
            }
            if bail {
                args.push("--bail");
            }
            let rows = fixture.run(&args, &commands.to_string(), 1);
            assert_eq!(rows.as_array().unwrap().len(), if bail { 2 } else { 3 });
            for (index, row) in rows.as_array().unwrap().iter().enumerate() {
                assert_identity(row, &commands[index], index, omit);
                assert_eq!(row["success"], index != 1);
                if index == 1 {
                    assert_eq!(row["error"], "Unknown command: PUBLIC_SENTINEL");
                } else {
                    assert!(row["result"].is_object(), "{row}");
                    assert!(row["error"].is_null(), "{row}");
                }
            }
        }
        let mut args = vec!["batch", "--json"];
        if omit {
            args.push("--omit-command");
        }
        let success = fixture.run(&args, r#"[["stream","status"]]"#, 0);
        assert_identity(&success[0], &commands[0], 0, omit);
        assert_eq!(success[0]["success"], true);
        let mut args = vec![
            "batch",
            "--json",
            "stream 'status'",
            "'PUBLIC_SENTINEL with spaces'",
            "stream \"status\"",
        ];
        if omit {
            args.push("--omit-command");
        }
        let rows = fixture.run(&args, "", 1);
        assert_eq!(rows.as_array().unwrap().len(), 3);
        assert_identity(&rows[0], &commands[0], 0, omit);
        assert_identity(&rows[1], &json!(["PUBLIC_SENTINEL with spaces"]), 1, omit);
        assert_identity(&rows[2], &commands[2], 2, omit);
        assert_eq!(rows[0]["success"], true);
        assert_eq!(rows[2]["success"], true);
        assert_eq!(
            rows[1]["error"],
            "Unknown command: PUBLIC_SENTINEL with spaces"
        );
    }
}

#[test]
#[ignore = "requires Chrome"]
fn e2e_batch_omits_fill_secret_from_cli_and_mcp_output() {
    const SECRET_VALUE: &str = "SECRET_VALUE";
    let fixture = Fixture::new();
    let executable = std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok();
    let browser_args = std::env::var("AGENT_BROWSER_ARGS").ok();
    let mut args = vec![
        "open",
        "data:text/html,<input id=x>",
        "--engine",
        "chrome",
        "--json",
    ];
    for (flag, value) in [
        ("--executable-path", executable.as_deref()),
        ("--args", browser_args.as_deref()),
    ] {
        if let Some(value) = value {
            args.extend([flag, value]);
        }
    }
    assert_eq!(fixture.run(&args, "", 0)["success"], true);
    let commands = json!([["fill", "#x", SECRET_VALUE]]);
    for mcp in [false, true] {
        assert_eq!(
            fixture.run(&["fill", "#x", "", "--json"], "", 0)["success"],
            true
        );
        let mut args = vec!["batch", "--json", "--omit-command"];
        let input = if mcp {
            args = vec!["mcp", "--tools", "all"];
            let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "agent_browser_batch", "arguments": {
                    "commands": commands, "session": "batch-test", "omitCommand": true}}});
            format!("{request}\n")
        } else {
            commands.to_string()
        };
        let output = fixture.run_raw(&args, &input, 0);
        let raw = [output.stdout.as_slice(), output.stderr.as_slice()].concat();
        assert!(!String::from_utf8_lossy(&raw).contains(SECRET_VALUE));
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        let rows = if mcp {
            assert_eq!(response["result"]["isError"], false, "{response}");
            assert_eq!(response["result"]["structuredContent"]["exitCode"], 0);
            &response["result"]["structuredContent"]["response"]
        } else {
            &response
        };
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_identity(&rows[0], &commands[0], 0, true);
        assert_eq!(rows[0]["success"], true, "{rows}");
        let value = fixture.run(&["get", "value", "#x", "--json"], "", 0);
        assert_eq!(value["success"], true);
        assert_eq!(value["data"]["value"], SECRET_VALUE);
    }
    assert_eq!(fixture.run(&["close", "--json"], "", 0)["success"], true);
}

#[test]
#[ignore = "requires Chrome"]
fn e2e_eval_preserves_unserializable_values_across_cli_and_mcp() {
    let fixture = Fixture::new();
    let executable = std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok();
    let browser_args = std::env::var("AGENT_BROWSER_ARGS").ok();
    let mut open_args = vec![
        "open",
        "data:text/html,%3Ctitle%3EEval%20Test%3C/title%3E",
        "--engine",
        "chrome",
        "--json",
    ];
    for (flag, value) in [
        ("--executable-path", executable.as_deref()),
        ("--args", browser_args.as_deref()),
    ] {
        if let Some(value) = value {
            open_args.extend([flag, value]);
        }
    }
    assert_eq!(fixture.run(&open_args, "", 0)["success"], true);

    let mcp_eval = |script: &str| {
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "agent_browser_eval", "arguments": {
                "script": script, "session": "batch-test"}}});
        fixture.run(&["mcp", "--tools", "all"], &format!("{request}\n"), 0)
    };

    for token in ["NaN", "Infinity", "-Infinity", "-0", "1n", "undefined"] {
        let cli = fixture.run(&["eval", "--stdin", "--json"], token, 0);
        assert_eq!(cli["data"]["result"], Value::Null, "{cli}");
        assert_eq!(cli["data"]["unserializable"], token, "{cli}");
        let plain = fixture.run_raw(&["eval", "--stdin"], token, 0).stdout;
        assert_eq!(String::from_utf8(plain).unwrap().trim(), token);

        let mcp = mcp_eval(token);
        let result = &mcp["result"];
        assert_eq!(result["isError"], false, "{mcp}");
        assert_eq!(result["structuredContent"]["response"], cli);
        assert_eq!(result["content"][0]["text"], token);
    }

    let null = fixture.run(&["eval", "--stdin", "--json"], "null", 0);
    assert_eq!(null["data"]["result"], Value::Null);
    assert!(null["data"].get("unserializable").is_none());
    let mcp_null = mcp_eval("null");
    assert_eq!(mcp_null["result"]["structuredContent"]["response"], null);
    assert_eq!(mcp_null["result"]["content"][0]["text"], "null");

    let ordinary_script = r#"({unserializable: "NaN"})"#;
    let ordinary = fixture.run(&["eval", "--stdin", "--json"], ordinary_script, 0);
    assert_eq!(ordinary["data"]["result"], json!({"unserializable": "NaN"}));
    assert!(ordinary["data"].get("unserializable").is_none());
    let ordinary_text = String::from_utf8(
        fixture
            .run_raw(&["eval", "--stdin"], ordinary_script, 0)
            .stdout,
    )
    .unwrap();
    assert!(ordinary_text.contains("\"unserializable\": \"NaN\""));
    let ordinary_mcp = mcp_eval(ordinary_script);
    assert_eq!(
        ordinary_mcp["result"]["structuredContent"]["response"],
        ordinary
    );
    assert!(ordinary_mcp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("\"unserializable\": \"NaN\""));

    let awaited = fixture.run(&["eval", "--stdin", "--json"], "Promise.resolve(1)", 0);
    assert_eq!(awaited["data"]["result"], 1);
    let awaited_mcp = mcp_eval("Promise.resolve(1)");
    assert_eq!(
        awaited_mcp["result"]["structuredContent"]["response"],
        awaited
    );
    assert_eq!(awaited_mcp["result"]["content"][0]["text"], "1");

    let exception = fixture.run(&["eval", "--stdin", "--json"], "throw new Error('boom')", 1);
    assert_eq!(exception["success"], false);
    assert!(exception["error"]
        .as_str()
        .unwrap()
        .contains("Evaluation error"));
    let exception_mcp = mcp_eval("throw new Error('boom')");
    assert_eq!(exception_mcp["result"]["isError"], true);
    assert_eq!(
        exception_mcp["result"]["structuredContent"]["response"],
        exception
    );
    assert!(exception_mcp["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("Evaluation error"));

    let title = fixture.run(&["get", "title", "--json"], "", 0);
    assert_eq!(title["data"]["title"], "Eval Test");
    assert_eq!(fixture.run(&["close", "--json"], "", 0)["success"], true);
}

#[test]
fn mcp_batch_typed_option_matches_cli_and_rejects_non_booleans() {
    let fixture = Fixture::new();
    let commands: Value = serde_json::from_str(COMMANDS).unwrap();
    for omit in [None, Some(false), Some(true)] {
        let mut arguments = json!({"commands": commands, "session": "batch-test", "bail": true});
        if let Some(value) = omit {
            arguments["omitCommand"] = json!(value);
        }
        let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "agent_browser_batch", "arguments": arguments}});
        let response = fixture.run(&["mcp", "--tools", "all"], &format!("{request}\n"), 0);
        let result = &response["result"];
        assert_eq!(result["isError"], true, "{response}");
        assert_eq!(result["structuredContent"]["exitCode"], 1);
        let rows = &result["structuredContent"]["response"];
        assert_eq!(rows.as_array().unwrap().len(), 2);
        assert_identity(&rows[0], &commands[0], 0, omit == Some(true));
        assert_identity(&rows[1], &commands[1], 1, omit == Some(true));
        assert_eq!(rows[0]["success"], true);
        assert_eq!(rows[1]["error"], "Unknown command: PUBLIC_SENTINEL");
    }
    let request = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": {"name": "agent_browser_batch", "arguments": {
            "commands": commands, "omitCommand": "true"}}});
    let response = fixture.run(&["mcp", "--tools", "all"], &format!("{request}\n"), 0);
    assert_eq!(response["error"]["code"], -32602, "{response}");
    assert_eq!(
        response["error"]["message"],
        "omitCommand must be a boolean"
    );
}

#[test]
fn batch_send_error_preserves_identity_without_a_daemon_response() {
    let fixture = Fixture::new();
    let bootstrap = fixture.run(&["stream", "status", "--json"], "", 0);
    assert_eq!(bootstrap["success"], true);
    let sockets = fixture.0.path().join("s");
    assert!(sockets.join("batch-test.version").exists());
    assert!(sockets.join("batch-test.config").exists());
    let socket = sockets.join("batch-test.sock");
    std::fs::remove_file(&socket).unwrap();
    let listener = UnixListener::bind(socket).unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = 0;
        while requests < 2 {
            let (mut stream, _) = listener.accept().unwrap();
            // macOS rejects the timeout with EINVAL once a probe connection has already closed.
            if stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .is_err()
            {
                continue;
            }
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            if line.is_empty() {
                continue;
            }
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["action"], "stream_status");
            stream.write_all(b"INVALID_JSON\n").unwrap();
            requests += 1;
        }
    });
    let commands = json!([["stream", "status"]]);
    let rows = [false, true].map(|omit| {
        let mut args = vec!["batch", "--json"];
        if omit {
            args.push("--omit-command");
        }
        fixture.run(&args, &commands.to_string(), 1)
    });
    for (omit, rows) in [false, true].into_iter().zip(rows) {
        assert_eq!(rows.as_array().unwrap().len(), 1);
        let row = &rows[0];
        assert_identity(row, &commands[0], 0, omit);
        assert_eq!(row["success"], false);
        assert!(
            row["error"].as_str().unwrap().contains("Invalid response:"),
            "{row}"
        );
        assert!(row.get("result").is_none(), "{row}");
    }
    server.join().unwrap();
}
