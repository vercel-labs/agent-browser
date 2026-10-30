use serde_json::Value;
use std::io::Write;
use std::process::{Command, Stdio};

fn mcp_call(args: &[&str], stdin: Option<&str>) -> (i32, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent-browser"))
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", home.path().join("s"))
        .args(["mcp", "call"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    if let Some(stdin) = stdin {
        input.write_all(stdin.as_bytes()).unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    let value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{e}: {output:?}"));
    (output.status.code().unwrap(), value)
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
