//! Integration tests for top-level CLI help output.

use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn build_help_cmd(tmp: &TempDir, args: &[&str]) -> Command {
    let socket_dir = tmp.path().join("sockets");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("AGENT_BROWSER_SOCKET_DIR", &socket_dir)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("NO_COLOR", "1");
    cmd
}

#[test]
fn top_level_help_environment_entries_are_unique() {
    let tmp = TempDir::new().unwrap();

    let output = build_help_cmd(&tmp, &["--help"])
        .output()
        .expect("failed to invoke agent-browser --help");

    assert!(
        output.status.success(),
        "--help should exit 0; got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");
    let environment = stdout
        .split_once("Environment:\n")
        .and_then(|(_, rest)| rest.split_once("\nInstall:").map(|(section, _)| section))
        .expect("top-level help should include Environment and Install sections");

    let mut seen = std::collections::HashSet::new();
    for line in environment.lines() {
        let Some(name) = line.split_whitespace().next() else {
            continue;
        };
        if !name.starts_with("AGENT_BROWSER_") && !name.starts_with("AI_GATEWAY_") {
            continue;
        }
        assert!(
            seen.insert(name.to_string()),
            "top-level help Environment section lists {} more than once\n---\n{}",
            name,
            environment
        );
    }
}
