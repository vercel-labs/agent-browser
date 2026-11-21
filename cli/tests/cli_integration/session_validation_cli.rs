use serde_json::Value;
use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn command(home: &TempDir, source: &str) -> Command {
    let mut command = Command::new(BIN);
    command
        .current_dir(home.path())
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("AGENT_BROWSER_SOCKET_DIR", home.path().join("s"))
        .env("NO_COLOR", "1")
        .env_remove("AGENT_BROWSER_CONFIG")
        .env_remove("AGENT_BROWSER_SESSION")
        .env_remove("AGENT_BROWSER_DAEMON")
        .env_remove("AGENT_BROWSER_DASHBOARD");
    match source {
        "--session" => {
            command.args(["--session", "bad name"]);
        }
        "AGENT_BROWSER_SESSION" => {
            command.env("AGENT_BROWSER_SESSION", "bad name");
        }
        "config session" => {
            let config = home.path().join("selected.json");
            std::fs::write(&config, r#"{"session":"bad name"}"#).unwrap();
            command.env("AGENT_BROWSER_CONFIG", config);
        }
        _ => unreachable!(),
    }
    command
}

#[test]
fn invalid_session_does_not_block_help_version_or_doctor() {
    for source in ["--session", "AGENT_BROWSER_SESSION", "config session"] {
        let home = TempDir::new().unwrap();
        for args in [&[][..], &["--help"][..], &["--version"][..]] {
            let output = command(&home, source).args(args).output().unwrap();
            assert!(output.status.success(), "{source} {args:?}: {output:?}");
            assert!(!output.stdout.is_empty());
        }
        let output = command(&home, source)
            .args(["doctor", "--offline", "--quick", "--json"])
            .output()
            .unwrap();
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(result["summary"].is_object(), "{source}: {result}");
    }
}

#[test]
fn invalid_session_is_rejected_before_session_dependent_local_commands() {
    for source in ["--session", "AGENT_BROWSER_SESSION", "config session"] {
        let home = TempDir::new().unwrap();
        for args in [
            &["session", "info"][..],
            &["read", "https://example.com"][..],
            &["close"][..],
            &["state", "list"][..],
        ] {
            let output = command(&home, source)
                .arg("--json")
                .args(args)
                .output()
                .unwrap();
            assert!(!output.status.success(), "{source} {args:?}");
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(result["type"], "invalid_session_name", "{result}");
            assert!(result["error"]
                .as_str()
                .unwrap()
                .contains(&format!("Source: {source}.")));
            assert!(!home.path().join("s").exists());
        }
    }
}

#[test]
fn mcp_doctor_ignores_invalid_inherited_session() {
    let home = TempDir::new().unwrap();
    let output = command(&home, "AGENT_BROWSER_SESSION")
        .args([
            "mcp",
            "call",
            "agent_browser_doctor",
            r#"{"offline":true,"quick":true}"#,
        ])
        .output()
        .unwrap();
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result["structuredContent"]["response"]["summary"].is_object(),
        "{result}"
    );
}
