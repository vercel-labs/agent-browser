use serde_json::Value;
use std::process::{Command, Output};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn run(args: &[&str]) -> (TempDir, Output) {
    let temp = tempfile::tempdir().unwrap();
    let mut command = Command::new(BIN);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
            command.env_remove(key);
        }
    }
    let socket_dir = temp.path().join("sockets");
    let output = command
        .args(args)
        .current_dir(temp.path())
        .env("HOME", temp.path())
        .env("USERPROFILE", temp.path())
        .env("AGENT_BROWSER_SOCKET_DIR", socket_dir)
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    (temp, output)
}

#[test]
fn help_only_succeeds_without_a_command_or_for_known_commands() {
    let cases: &[(&[&str], bool, &str)] = &[
        (&["unknown-command", "--help"], false, "Unknown command"),
        (&["--help", "unknown-command"], false, "Unknown command"),
        (&["unknown-command", "-h"], false, "Unknown command"),
        (
            &["--json", "unknown-command", "--help"],
            false,
            "unknown_command",
        ),
        (&["--unknown", "--help"], true, "Usage:"),
        (&["--help", "open"], true, "agent-browser open"),
        (&["open", "--help"], true, "agent-browser open"),
        (&["--json", "--help"], true, "Usage:"),
    ];

    for (args, succeeds, expected) in cases {
        let (temp, output) = run(args);
        assert_eq!(output.status.success(), *succeeds, "{args:?}");
        if args.contains(&"--json") && !succeeds {
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["success"], false, "{args:?}: {result}");
            assert_eq!(result["type"], *expected, "{args:?}: {result}");
        } else {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(text.contains(expected), "{args:?}: {text}");
        }
        assert!(!temp.path().join("sockets").exists(), "{args:?}");
    }
}

#[test]
fn set_offline_rejects_an_invalid_value_without_starting_a_daemon() {
    let (temp, output) = run(&["--json", "set", "offline", "of"]);
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["type"], "invalid_value");
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("expects on or off"));
    assert!(!temp.path().join("sockets").exists());
}

#[test]
fn mcp_set_offline_rejects_non_boolean_values_without_starting_a_daemon() {
    let (temp, output) = run(&[
        "mcp",
        "call",
        "agent_browser_set_offline",
        r#"{"enabled":"of"}"#,
    ]);
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], -32602);
    assert_eq!(result["error"]["message"], "enabled must be a boolean");
    assert!(!temp.path().join("sockets").exists());
}

#[test]
fn cookies_rejects_unknown_operations_without_starting_a_daemon() {
    let (temp, output) = run(&["--json", "cookies", "typo"]);
    assert!(!output.status.success());
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["type"], "unknown_subcommand");
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("Valid options: get, set, clear"));
    assert!(!temp.path().join("sockets").exists());
}
