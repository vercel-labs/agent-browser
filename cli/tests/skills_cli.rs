//! These tests isolate HOME; Windows resolves the profile through the Known Folder API.
#![cfg(unix)]

use std::fs;
use std::process::Command;

#[test]
fn standalone_binary_ignores_unrelated_skills_and_loads_home_skill_through_cli_and_mcp() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("skills")).unwrap();
    let home = temp.path().join("home");
    let binary = temp.path().join("agent-browser");
    let skill = home.join(".agent-browser/skill-data/core/SKILL.md");
    let marker = "home-fallback-skill-marker-1989";
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_agent-browser"), &binary).unwrap();
    fs::write(
        &skill,
        format!("---\nname: core\ndescription: test skill\n---\n{marker}\n"),
    )
    .unwrap();

    let cli = Command::new(&binary)
        .args(["skills", "get", "core"])
        .current_dir(temp.path())
        .env("HOME", &home)
        .env_remove("AGENT_BROWSER_SKILLS_DIR")
        .output()
        .unwrap();
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert!(String::from_utf8_lossy(&cli.stdout).contains(marker));

    let mcp = Command::new(&binary)
        .args([
            "mcp",
            "call",
            "agent_browser_skills_get",
            r#"{"names":["core"]}"#,
        ])
        .current_dir(temp.path())
        .env("HOME", &home)
        .env_remove("AGENT_BROWSER_SKILLS_DIR")
        .output()
        .unwrap();
    assert!(
        mcp.status.success(),
        "{}",
        String::from_utf8_lossy(&mcp.stderr)
    );
    assert!(String::from_utf8_lossy(&mcp.stdout).contains(marker));
}
