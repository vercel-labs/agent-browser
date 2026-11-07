use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const CHILD_TIMEOUT: Duration = Duration::from_secs(15);

struct ChildHome {
    dir: TempDir,
    config: PathBuf,
}

impl ChildHome {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("agent-browser.json");
        fs::write(&config, "{}").unwrap();
        Self { dir, config }
    }

    fn sessions(&self) -> PathBuf {
        self.dir.path().join(".agent-browser/sessions")
    }

    fn command(&self, timezone: &str) -> Command {
        let mut command = Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
                command.env_remove(key);
            }
        }
        command
            .current_dir(self.dir.path())
            .env("HOME", self.dir.path())
            .env("USERPROFILE", self.dir.path())
            .env("AGENT_BROWSER_CONFIG", &self.config)
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir.path().join("sockets"))
            .env("NO_COLOR", "1")
            .env("TZ", timezone);
        command
    }
}

fn run(command: &mut Command) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + CHILD_TIMEOUT;

    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().unwrap(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => timeout_child(child),
            Err(error) => {
                let output = stop_child(child);
                panic!("failed to wait for CLI child: {error}; output={output:?}");
            }
        }
    }
}

fn timeout_child(child: Child) -> ! {
    let output = stop_child(child);
    panic!("CLI child exceeded {CHILD_TIMEOUT:?}; output={output:?}");
}

fn stop_child(mut child: Child) -> Output {
    let _ = child.kill();
    child.wait_with_output().unwrap()
}

fn create_state_file(sessions: &Path, filename: &str, size: usize, modified: u64) -> PathBuf {
    fs::create_dir_all(sessions).unwrap();
    let path = sessions.join(filename);
    fs::write(&path, vec![b'x'; size]).unwrap();
    File::open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(modified)))
        .unwrap();
    path
}

fn assert_state_row(stdout: &str, filename: &str, expected: &str) {
    let line = stdout
        .lines()
        .find(|line| line.contains(filename))
        .unwrap_or_else(|| panic!("missing {filename} in state list output:\n{stdout}"));
    assert!(
        line.contains(expected),
        "expected {filename} row to contain {expected:?}, got {line:?}"
    );
}

fn json_output(output: Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("invalid JSON output: {error}; stdout={stdout}; stderr={stderr}")
    })
}

fn files_by_name(response: &Value) -> BTreeMap<&str, Value> {
    response["data"]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| (file["filename"].as_str().unwrap(), file.clone()))
        .collect()
}

#[test]
fn state_list_cli_renders_numeric_modified_date() {
    let home = ChildHome::new();
    create_state_file(
        &home.sessions(),
        "timestamp-fixture.json",
        30,
        1_794_000_000,
    );
    create_state_file(&home.sessions(), "midnight-fixture.json", 30, 1_794_009_600);
    create_state_file(&home.sessions(), "epoch-zero.json", 30, 0);
    create_state_file(&home.sessions(), "vault.json.enc", 30, 1_794_000_000);

    for timezone in ["UTC", "America/Los_Angeles", "Asia/Ho_Chi_Minh"] {
        let mut command = home.command(timezone);
        let output = run(command.args(["state", "list"]));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");

        assert_state_row(&stdout, "timestamp-fixture.json", "(30B, 2026-11-06)");
        assert_state_row(&stdout, "midnight-fixture.json", "(30B, 2026-11-07)");
        assert_state_row(&stdout, "epoch-zero.json", "(30B, 1970-01-01)");
        assert_state_row(&stdout, "vault.json.enc", "(30B, 2026-11-06) [encrypted]");
    }
}

#[test]
fn state_list_cli_and_mcp_preserve_unix_seconds_metadata() {
    let home = ChildHome::new();
    let state_path = create_state_file(
        &home.sessions(),
        "timestamp-fixture.json",
        30,
        1_794_000_000,
    );
    let encrypted_path = create_state_file(&home.sessions(), "vault.json.enc", 30, 1_794_000_000);

    let mut cli_command = home.command("UTC");
    cli_command.args(["--json", "state", "list"]);
    let cli_response = json_output(run(&mut cli_command));

    let mut mcp_command = home.command("UTC");
    mcp_command.args(["mcp", "call", "agent_browser_state_list", "{}"]);
    let mcp_result = json_output(run(&mut mcp_command));
    assert_eq!(mcp_result["isError"], false);

    let mcp_response = &mcp_result["structuredContent"]["response"];
    let text_response: Value =
        serde_json::from_str(mcp_result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(files_by_name(&cli_response), files_by_name(mcp_response));
    assert_eq!(files_by_name(mcp_response), files_by_name(&text_response));

    let cli_files = files_by_name(&cli_response);
    let timestamp = &cli_files["timestamp-fixture.json"];
    assert_eq!(timestamp["path"], state_path.to_string_lossy().as_ref());
    assert_eq!(timestamp["size"].as_u64(), Some(30));
    assert_eq!(timestamp["modified"].as_u64(), Some(1_794_000_000));
    assert_eq!(timestamp["encrypted"], false);

    let encrypted = &cli_files["vault.json.enc"];
    assert_eq!(encrypted["path"], encrypted_path.to_string_lossy().as_ref());
    assert_eq!(encrypted["size"].as_u64(), Some(30));
    assert_eq!(encrypted["modified"].as_u64(), Some(1_794_000_000));
    assert_eq!(encrypted["encrypted"], true);
}

#[test]
fn state_list_cli_keeps_the_empty_directory_message() {
    let home = ChildHome::new();
    fs::create_dir_all(home.sessions()).unwrap();

    let mut command = home.command("UTC");
    let output = run(command.args(["state", "list"]));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stdout={stdout}\nstderr={stderr}");
    assert!(stdout.contains("No state files found"), "stdout={stdout}");
}
