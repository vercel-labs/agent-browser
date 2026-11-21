//! An allowlist that is supplied but lists no domain fails the command.
//!
//! Every case fails before a browser would start; `AGENT_BROWSER_EXECUTABLE_PATH`
//! points nowhere so a command that is not stopped by the allowlist check fails
//! with a different message and these tests go red.
#![cfg(unix)]

use std::fs;
use std::process::{Command, Output};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const EMPTY: &str = "Allowed domains list is empty";

fn run(home: &TempDir, envs: &[(&str, &str)], args: &[&str]) -> (Output, String) {
    let mut cmd = Command::new(BIN);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
            cmd.env_remove(key);
        }
    }
    cmd.env_remove("AI_GATEWAY_API_KEY")
        .env_remove("AI_GATEWAY_URL");
    cmd.current_dir(home.path())
        .env("HOME", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", home.path().join("sock"))
        .env("AGENT_BROWSER_SESSION", "allowlist")
        .env("AGENT_BROWSER_EXECUTABLE_PATH", "/nonexistent/chrome")
        .env("NO_COLOR", "1")
        .args(["--json"]);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let output = cmd.args(args).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output, text)
}

#[test]
fn empty_flag_values_are_rejected() {
    for value in ["", ",", " "] {
        let home = TempDir::new().unwrap();
        let (output, text) = run(
            &home,
            &[],
            &["open", "http://127.0.0.1:1/", "--allowed-domains", value],
        );
        assert!(!output.status.success(), "{value:?}: {text}");
        assert!(text.contains(EMPTY), "{value:?}: {text}");
        assert!(text.contains("--allowed-domains"), "{value:?}: {text}");
    }
}

#[test]
fn a_flag_with_no_value_is_rejected() {
    for flag in ["--allowed-domains", "--action-policy", "--confirm-actions"] {
        let home = TempDir::new().unwrap();
        let (output, text) = run(&home, &[], &["open", "http://127.0.0.1:1/", flag]);
        assert!(!output.status.success(), "{flag}: {text}");
        assert!(text.contains("needs a value"), "{flag}: {text}");
    }
}

#[test]
fn a_blank_env_var_is_rejected_by_commands_that_launch() {
    let home = TempDir::new().unwrap();
    let (output, text) = run(
        &home,
        &[("AGENT_BROWSER_ALLOWED_DOMAINS", " , ")],
        &["open", "http://127.0.0.1:1/"],
    );
    assert!(!output.status.success(), "{text}");
    assert!(text.contains(EMPTY), "{text}");
    assert!(text.contains("AGENT_BROWSER_ALLOWED_DOMAINS"), "{text}");
}

#[test]
fn a_blank_env_var_does_not_block_close_or_session_list() {
    let home = TempDir::new().unwrap();
    for args in [&["close"][..], &["session", "list"][..]] {
        let (output, text) = run(&home, &[("AGENT_BROWSER_ALLOWED_DOMAINS", "")], args);
        assert!(output.status.success(), "{args:?}: {text}");
    }
}

#[test]
fn a_non_utf8_env_var_is_rejected() {
    use std::os::unix::ffi::OsStrExt;
    let home = TempDir::new().unwrap();
    let bad = std::ffi::OsStr::from_bytes(b"example.com,\xff");
    let mut cmd = Command::new(BIN);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
            cmd.env_remove(key);
        }
    }
    let output = cmd
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", home.path().join("sock"))
        .env("AGENT_BROWSER_EXECUTABLE_PATH", "/nonexistent/chrome")
        .env("AGENT_BROWSER_ALLOWED_DOMAINS", bad)
        .args(["--json", "open", "http://127.0.0.1:1/"])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "{text}");
    assert!(text.contains("not valid UTF-8"), "{text}");
}

#[test]
fn chat_checks_the_allowlist_before_it_talks_to_the_gateway() {
    // The gateway URL points at a closed port; a run that is not stopped by the
    // allowlist check fails on the connection instead.
    for value in ["", ","] {
        let home = TempDir::new().unwrap();
        let (output, text) = run(
            &home,
            &[
                ("AI_GATEWAY_API_KEY", "test-key"),
                ("AI_GATEWAY_URL", "http://127.0.0.1:1"),
            ],
            &["chat", "--allowed-domains", value, "go"],
        );
        assert!(!output.status.success(), "{value:?}: {text}");
        assert!(text.contains(EMPTY), "{value:?}: {text}");
    }
}

#[test]
fn an_unusable_allowlist_in_the_config_fails_instead_of_being_skipped() {
    let configs = [
        r#"{"allowedDomains": null}"#,
        r#"{"allowedDomains": ""}"#,
        r#"{"allowedDomains": []}"#,
        r#"{"allowedDomains": ["a.example"], "headed": "yes"}"#,
        r#"{"allowedDomains": [","]}"#,
        r#"{"allowedDomains": ["a.example"],}"#,
        "// note\n{\"allowedDomains\": [\"a.example\"]}",
        r#"{"allowedDomains": ["a.example"]} trailing"#,
        r#"[{"allowedDomains": ["a.example"]}]"#,
        r#"{"allowedDomains": ["a.example"]"#,
        r#"{"\u0061llowedDomains": ["a.example"],}"#,
    ];
    for config in configs {
        let home = TempDir::new().unwrap();
        let dir = home.path().join(".agent-browser");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.json"), config).unwrap();
        let (output, text) = run(&home, &[], &["open", "http://127.0.0.1:1/"]);
        assert!(!output.status.success(), "{config}: {text}");
        assert!(
            text.contains("allowedDomains") || text.contains("security settings"),
            "{config}: {text}"
        );
        assert!(!text.contains("nonexistent"), "{config}: {text}");
    }
}

#[test]
fn an_unreadable_or_non_utf8_config_fails_instead_of_being_skipped() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().unwrap();
    let dir = home.path().join(".agent-browser");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");

    let mut bytes = br#"{"allowedDomains": ["a.example"], "userAgent": ""#.to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe]);
    bytes.extend_from_slice(br#""}"#);
    fs::write(&path, bytes).unwrap();
    let (output, text) = run(&home, &[], &["open", "http://127.0.0.1:1/"]);
    assert!(!output.status.success(), "non-utf8: {text}");
    assert!(!text.contains("nonexistent"), "non-utf8: {text}");

    fs::write(&path, r#"{"allowedDomains": ["a.example"]}"#).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let (output, text) = run(&home, &[], &["open", "http://127.0.0.1:1/"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(!output.status.success(), "mode 000: {text}");
    assert!(text.contains("cannot read config"), "mode 000: {text}");
}

/// A one-shot gateway: the first request answers with a tool call, the next
/// with plain text. Nothing leaves the machine.
fn fake_gateway(command: &'static str) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = std::thread::spawn(move || {
        for turn in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if buf.len() >= split + 4 + length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let delta = if turn == 0 {
                serde_json::json!({"tool_calls": [{"index": 0, "id": "c1", "type": "function",
                    "function": {"name": "agent_browser",
                        "arguments": serde_json::json!({"command": command}).to_string()}}]})
            } else {
                serde_json::json!({"content": "done"})
            };
            let body = format!(
                "data: {}\n\ndata: [DONE]\n\n",
                serde_json::json!({"choices": [{"delta": delta}]})
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    (url, handle)
}

#[test]
fn chat_passes_the_allowlist_to_the_commands_it_runs() {
    // `read` enforces the allowlist in the daemon without starting a browser.
    // With the list forwarded, the host is refused; without it the fetch would
    // fail on the connection instead.
    let (url, gateway) = fake_gateway("agent-browser read http://127.0.0.1:1/");
    let home = TempDir::new().unwrap();
    let (_, text) = run(
        &home,
        &[("AI_GATEWAY_API_KEY", "test-key"), ("AI_GATEWAY_URL", &url)],
        &["chat", "--allowed-domains", "example.com", "go"],
    );
    let _ = run(&home, &[], &["close"]);
    gateway.join().unwrap();
    assert!(text.contains("not in the allowed domains list"), "{text}");
}

#[test]
fn a_broken_security_config_does_not_block_close() {
    let home = TempDir::new().unwrap();
    let dir = home.path().join(".agent-browser");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config.json"),
        r#"{"allowedDomains": ["a.example"],}"#,
    )
    .unwrap();
    let (output, text) = run(&home, &[], &["close"]);
    assert!(output.status.success(), "{text}");
    assert!(text.contains("invalid config file"), "{text}");
    let (output, _) = run(&home, &[], &["open", "http://127.0.0.1:1/"]);
    assert!(!output.status.success());
}

#[test]
fn the_chat_model_cannot_confirm_or_deny_actions() {
    for command in [
        "confirm c_1234",
        "deny c_1234",
        "batch 'confirm c_1234'",
        "batch 'open about:blank' 'deny c_1234'",
        "batch",
    ] {
        let command: &'static str = Box::leak(format!("agent-browser {command}").into_boxed_str());
        let (url, gateway) = fake_gateway(command);
        let home = TempDir::new().unwrap();
        let (_, text) = run(
            &home,
            &[("AI_GATEWAY_API_KEY", "test-key"), ("AI_GATEWAY_URL", &url)],
            &["chat", "go"],
        );
        gateway.join().unwrap();
        assert!(
            text.contains("is not a valid agent-browser command")
                || text.contains("batch needs its commands"),
            "{command}: {text}"
        );
    }
}

#[test]
fn a_broken_security_config_stops_only_commands_that_run_a_browser() {
    let home = TempDir::new().unwrap();
    let dir = home.path().join(".agent-browser");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.json"), r#"{"allowedDomains": []}"#).unwrap();

    for args in [
        &["--version"][..],
        &["--help"][..],
        &["skills", "list"][..],
        &["session", "list"][..],
        &["close"][..],
    ] {
        let (output, text) = run(&home, &[], args);
        assert!(output.status.success(), "{args:?}: {text}");
    }

    let (_, text) = run(&home, &[], &["doctor", "--offline", "--quick"]);
    assert!(
        text.contains("allowedDomains"),
        "doctor should report the config problem and keep running: {text}"
    );

    let (output, text) = run(&home, &[], &["open", "http://127.0.0.1:1/"]);
    assert!(!output.status.success(), "open: {text}");
    assert!(!text.contains("nonexistent"), "open: {text}");
}

#[test]
fn help_like_option_values_do_not_make_a_command_browserless() {
    // `--user-agent --help` and friends give the option a value; the command is
    // still `read` and must refuse the empty allowlist it inherited.
    let values: [(&str, &str); 5] = [
        ("--user-agent", "--help"),
        ("--user-agent", "-h"),
        ("--proxy", "-V"),
        ("--user-agent", "--version"),
        ("--session-name", "--help"),
    ];
    for (option, value) in values {
        let home = TempDir::new().unwrap();
        let (output, text) = run(
            &home,
            &[("AGENT_BROWSER_ALLOWED_DOMAINS", " , ")],
            &["read", "http://127.0.0.1:1/", option, value],
        );
        assert!(!output.status.success(), "{option} {value}: {text}");
        assert!(text.contains(EMPTY), "{option} {value}: {text}");
        // A refusal, not the warning a browserless command prints.
        assert!(!text.contains('\u{26a0}'), "{option} {value}: {text}");
    }
}

#[test]
fn mcp_extra_args_cannot_turn_off_containment_with_help_like_values() {
    for extra in [
        r#"["--user-agent","--help"]"#,
        r#"["--user-agent","-V"]"#,
        r#"["--user-agent","--version"]"#,
        r#"["--user-agent","-h"]"#,
    ] {
        let home = TempDir::new().unwrap();
        let args = format!(r#"{{"url":"http://127.0.0.1:1/","extraArgs":{extra}}}"#);
        let (_, text) = run(
            &home,
            &[("AGENT_BROWSER_ALLOWED_DOMAINS", " , ")],
            &["mcp", "call", "agent_browser_open", &args],
        );
        assert!(text.contains(EMPTY), "{extra}: {text}");
        assert!(!text.contains("nonexistent"), "{extra}: {text}");
    }
}

#[test]
fn close_still_works_with_an_empty_allowlist_flag_or_env() {
    let home = TempDir::new().unwrap();
    let (output, text) = run(&home, &[], &["close", "--allowed-domains", ","]);
    assert!(output.status.success(), "{text}");
    let (output, text) = run(&home, &[("AGENT_BROWSER_ALLOWED_DOMAINS", " ")], &["close"]);
    assert!(output.status.success(), "{text}");
}
