#![cfg(target_os = "linux")]

use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use tempfile::TempDir;

#[test]
fn conflicting_angle_backend_has_the_same_cli_and_mcp_error() {
    let binary = env!("CARGO_BIN_EXE_agent-browser");
    let session = format!("webgpu-angle-test-{}", std::process::id());

    let cli = Command::new(binary)
        .args([
            "--json",
            "--session",
            &session,
            "--webgpu",
            "--executable-path",
            "/usr/bin/true",
            "open",
            "about:blank",
        ])
        .env("AGENT_BROWSER_ARGS", "--use-angle=swiftshader")
        .output()
        .expect("CLI should run");
    assert!(!cli.status.success());
    let cli_response: Value = serde_json::from_slice(&cli.stdout).expect("CLI JSON response");
    let cli_error = cli_response["error"].as_str().expect("CLI error text");
    assert!(cli_error.contains("--use-angle=swiftshader"));
    assert!(cli_error.contains("--webgpu false"));

    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "agent_browser_open",
            "arguments": {
                "webgpu": true,
                "session": session,
                "extraArgs": ["--executable-path", "/usr/bin/true"]
            }
        }
    });
    let mut mcp = Command::new(binary)
        .args(["mcp", "--tools", "all"])
        .env("AGENT_BROWSER_ARGS", "--use-angle=swiftshader")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("MCP server should run");
    writeln!(mcp.stdin.take().unwrap(), "{request}").expect("MCP request should be sent");
    let mcp_output = mcp
        .wait_with_output()
        .expect("MCP server should exit on EOF");
    assert!(mcp_output.status.success());
    let mcp_response: Value =
        serde_json::from_slice(&mcp_output.stdout).expect("MCP JSON response");
    assert_eq!(mcp_response["result"]["isError"], true);
    assert_eq!(
        mcp_response["result"]["structuredContent"]["response"]["error"],
        cli_error
    );

    let _ = Command::new(binary)
        .args(["--session", &session, "close"])
        .output();
}

#[test]
fn repeated_disabled_features_merge_after_cli_and_env_arg_parsing() {
    let binary = env!("CARGO_BIN_EXE_agent-browser");
    let temp = TempDir::new().expect("temporary test directory");
    let fake_chrome = temp.path().join("chrome");
    fs::write(
        &fake_chrome,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$AGENT_BROWSER_TEST_CAPTURE\"\nexit 9\n",
    )
    .expect("fake Chrome script");
    fs::set_permissions(&fake_chrome, fs::Permissions::from_mode(0o755))
        .expect("fake Chrome permissions");

    for source in ["cli", "env"] {
        let session = format!("disabled-features-{source}-{}", std::process::id());
        let capture = temp.path().join(format!("{source}-args.txt"));
        let mut command = Command::new(binary);
        command.args(["--json", "--session", &session, "--executable-path"]);
        command.arg(&fake_chrome);
        if source == "cli" {
            command.args(["--args", "--disable-features=Foo,--disable-features=Bar"]);
            command.env_remove("AGENT_BROWSER_ARGS");
        } else {
            command.env(
                "AGENT_BROWSER_ARGS",
                "--disable-features=Foo,--disable-features=Bar",
            );
        }
        let output = command
            .args(["open", "about:blank"])
            .env("AGENT_BROWSER_TEST_CAPTURE", &capture)
            .output()
            .expect("CLI should run");
        assert!(!output.status.success(), "fake Chrome must fail to launch");
        let args = fs::read_to_string(&capture).expect("fake Chrome captured its arguments");
        let disabled: Vec<&str> = args
            .lines()
            .filter(|arg| arg.starts_with("--disable-features="))
            .collect();
        assert_eq!(
            disabled,
            vec!["--disable-features=Translate,Foo,Bar"],
            "{source} args should produce one combined Chrome switch"
        );
        let _ = Command::new(binary)
            .args(["--session", &session, "close"])
            .output();
    }
}
