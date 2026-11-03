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

#[test]
fn skills_get_ref_loads_one_reference_through_cli_and_mcp() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let binary = temp.path().join("agent-browser");
    let core = home.join(".agent-browser/skill-data/core");
    let skill_marker = "core-skill-body-marker-2024";
    let ref_marker = "commands-reference-marker-2024";
    let other_marker = "auth-reference-marker-2024";
    fs::create_dir_all(core.join("references")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_agent-browser"), &binary).unwrap();
    fs::write(
        core.join("SKILL.md"),
        format!("---\nname: core\ndescription: test skill\n---\n{skill_marker}\n"),
    )
    .unwrap();
    fs::write(
        core.join("references/commands.md"),
        format!("{ref_marker}\n"),
    )
    .unwrap();
    fs::write(
        core.join("references/authentication.md"),
        format!("{other_marker}\n"),
    )
    .unwrap();

    let run = |args: &[&str]| {
        Command::new(&binary)
            .args(args)
            .current_dir(temp.path())
            .env("HOME", &home)
            .env_remove("AGENT_BROWSER_SKILLS_DIR")
            .output()
            .unwrap()
    };

    for args in [
        &["skills", "get", "core", "--ref", "commands"][..],
        &["skills", "get", "core", "--ref=references/commands.md"][..],
        &[
            "mcp",
            "call",
            "agent_browser_skills_get",
            r#"{"names":["core"],"refs":["commands.md"]}"#,
        ][..],
    ] {
        let output = run(args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains(ref_marker), "{args:?}: {stdout}");
        assert!(!stdout.contains(skill_marker), "{args:?}: {stdout}");
        assert!(!stdout.contains(other_marker), "{args:?}: {stdout}");
    }

    let missing = run(&["skills", "get", "core", "--ref", "nope"]);
    assert!(!missing.status.success());
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("Reference not found: nope"), "{stderr}");
    assert!(stderr.contains("references/commands.md"), "{stderr}");

    let with_full = run(&["skills", "get", "core", "--full", "--ref", "commands"]);
    assert!(!with_full.status.success());
}
