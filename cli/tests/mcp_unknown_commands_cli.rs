use serde_json::{json, Value};
use std::process::Command;
use tempfile::TempDir;

/// Winsock fails to initialize (os error 10106) without SystemRoot, so a
/// cleared environment keeps the variables Windows needs to open sockets.
fn windows_runtime_env() -> Vec<(String, std::ffi::OsString)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    ["SystemRoot", "windir", "TEMP", "TMP"]
        .into_iter()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.to_string(), value)))
        .collect()
}

#[test]
fn mcp_batch_rejects_invalid_commands() {
    let home = TempDir::new().unwrap();
    let session = "mcp-cookie-parse-test";
    let batch_args = json!({
        "session": session,
        "commands": [["cookies", "delete", "foo"], ["close", "--all"]]
    })
    .to_string();
    let output = Command::new(env!("CARGO_BIN_EXE_agent-browser"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", home.path().join("sockets"))
        .env("NO_COLOR", "1")
        .envs(windows_runtime_env())
        .current_dir(home.path())
        .args(["mcp", "call", "agent_browser_batch", &batch_args])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let batch: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(batch["isError"], true, "{batch}");
    assert_eq!(batch["structuredContent"]["exitCode"], 1, "{batch}");
    assert!(
        batch["structuredContent"]["response"][0]["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{batch}"))
            .contains("Unknown subcommand: delete"),
        "{batch}"
    );
    assert!(
        batch["structuredContent"]["response"][1]["error"]
            .as_str()
            .unwrap_or_else(|| panic!("{batch}"))
            .contains("close --all is only supported outside a batch"),
        "{batch}"
    );
}
