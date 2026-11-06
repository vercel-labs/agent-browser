use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

#[test]
fn invalid_max_output_warns_for_cli_and_environment() {
    let cases = [
        (
            &["--max-output", "not-a-number", "--help"][..],
            None,
            "--max-output",
        ),
        (
            &["--help"][..],
            Some("not-a-number"),
            "AGENT_BROWSER_MAX_OUTPUT",
        ),
    ];
    for (args, env_value, source) in cases {
        let home = TempDir::new().unwrap();
        let mut command = Command::new(BIN);
        command
            .args(args)
            .current_dir(home.path())
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("NO_COLOR", "1")
            .env_remove("AGENT_BROWSER_CONFIG")
            .env_remove("AGENT_BROWSER_MAX_OUTPUT");
        if let Some(value) = env_value {
            command.env("AGENT_BROWSER_MAX_OUTPUT", value);
        }

        let output = command.output().expect("failed to invoke agent-browser");
        assert!(output.status.success(), "{source}: {:?}", output.status);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(source),
            "{source} warning missing from stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
