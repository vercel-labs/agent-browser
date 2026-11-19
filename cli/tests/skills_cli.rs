//! These tests isolate HOME; Windows resolves the profile through the Known Folder API.
#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;

/// Serializes writing a binary copy and forking children within this test process.
///
/// A fork inherits every open fd until its exec. If another test thread forks while
/// the copy's write fd is open, executing the copy fails with ETXTBSY on Linux.
/// Holding this lock for the whole copy and for each spawn (which returns only after
/// the child's exec) guarantees no child can hold the write fd.
static FORK_LOCK: Mutex<()> = Mutex::new(());

fn copy_binary(dest: &Path) {
    let _guard = FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    fs::copy(env!("CARGO_BIN_EXE_agent-browser"), dest).unwrap();
}

fn run_output(command: &mut Command) -> Output {
    let child = {
        let _guard = FORK_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    child.wait_with_output().unwrap()
}

#[test]
fn standalone_binary_ignores_unrelated_skills_and_loads_home_skill_through_cli_and_mcp() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("skills")).unwrap();
    let home = temp.path().join("home");
    let binary = temp.path().join("agent-browser");
    let skill = home.join(".agent-browser/skill-data/core/SKILL.md");
    let marker = "home-fallback-skill-marker-1989";
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    copy_binary(&binary);
    fs::write(
        &skill,
        format!("---\nname: core\ndescription: test skill\n---\n{marker}\n"),
    )
    .unwrap();

    let cli = run_output(
        Command::new(&binary)
            .args(["skills", "get", "core"])
            .current_dir(temp.path())
            .env("HOME", &home)
            .env_remove("AGENT_BROWSER_SKILLS_DIR"),
    );
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stderr)
    );
    assert!(String::from_utf8_lossy(&cli.stdout).contains(marker));

    let mcp = run_output(
        Command::new(&binary)
            .args([
                "mcp",
                "call",
                "agent_browser_skills_get",
                r#"{"names":["core"]}"#,
            ])
            .current_dir(temp.path())
            .env("HOME", &home)
            .env_remove("AGENT_BROWSER_SKILLS_DIR"),
    );
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
    copy_binary(&binary);
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
        run_output(
            Command::new(&binary)
                .args(args)
                .current_dir(temp.path())
                .env("HOME", &home)
                .env_remove("AGENT_BROWSER_SKILLS_DIR"),
        )
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

#[test]
fn skills_get_unknown_name_points_to_skills_list() {
    let temp = tempfile::tempdir().unwrap();
    let output = run_output(
        Command::new(env!("CARGO_BIN_EXE_agent-browser"))
            .args(["skills", "get", "missing-skill"])
            .env("AGENT_BROWSER_SKILLS_DIR", temp.path()),
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Skill not found: missing-skill"),
        "{stderr}"
    );
    assert!(stderr.contains("agent-browser skills list"), "{stderr}");
}
