use serde_json::{json, Value};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "sec-cli";
const PAGE: &str = "data:text/html,<title>browser-security</title>";
const ADVISORY_VERSION: &str = "999.0.9999.999";
const ALLOW_ENV: &str = "AGENT_BROWSER_ALLOW_OUTDATED_BROWSER";

fn env_flag(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | ""
        )
    })
}

fn browser_executable() -> Option<String> {
    [
        "AGENT_BROWSER_TEST_BROWSER",
        "AGENT_BROWSER_EXECUTABLE_PATH",
    ]
    .iter()
    .find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

struct Harness {
    home: TempDir,
    sockets: TempDir,
    executable: Option<String>,
    no_sandbox: bool,
    headed: bool,
    headless_display: bool,
    closed: bool,
}

impl Harness {
    fn new() -> Self {
        let home = TempDir::new().unwrap();
        let sockets = TempDir::new().unwrap();
        let config_dir = home.path().join(".agent-browser");
        std::fs::create_dir_all(&config_dir).unwrap();
        let fetched_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let chrome_advisory = |platform: &str| {
            json!({
                "fetched_at": fetched_at,
                "latest_stable": ADVISORY_VERSION,
                "chrome_desktop_release": true,
                "chrome_platform": platform,
                "cves": [
                    { "id": "CVE-2026-0001", "fixed_in": "999.0.9999.1" },
                    { "id": "CVE-2026-0002", "fixed_in": ADVISORY_VERSION }
                ]
            })
        };
        let fixture = json!({
            "families": {
                "v2-chrome-linux": chrome_advisory("linux"),
                "v2-chrome-mac": chrome_advisory("mac"),
                "v2-chrome-windows": chrome_advisory("windows")
            }
        });
        std::fs::write(
            config_dir.join("security-advisories.json"),
            serde_json::to_string_pretty(&fixture).unwrap(),
        )
        .unwrap();
        Self {
            home,
            sockets,
            executable: browser_executable(),
            no_sandbox: env_flag("AGENT_BROWSER_TEST_NO_SANDBOX"),
            headed: env_flag("AGENT_BROWSER_TEST_HEADED"),
            headless_display: env_flag("AGENT_BROWSER_TEST_HEADLESS_DISPLAY"),
            closed: false,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(["--session", SESSION])
            .current_dir(self.home.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.sockets.path())
            .env("NO_COLOR", "1")
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AGENT_BROWSER_CONFIG")
            .env_remove("AGENT_BROWSER_SESSION")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env_remove("AGENT_BROWSER_DAEMON")
            .env_remove("AGENT_BROWSER_DASHBOARD")
            .env_remove("AGENT_BROWSER_CDP")
            .env_remove("AGENT_BROWSER_CDP_HEADERS")
            .env_remove("AGENT_BROWSER_AUTO_CONNECT")
            .env_remove("AGENT_BROWSER_PROVIDER")
            .env_remove("AGENT_BROWSER_ENGINE")
            .env_remove("AGENT_BROWSER_PLUGINS")
            .env_remove("AGENT_BROWSER_PROFILE")
            .env_remove("AGENT_BROWSER_STATE")
            .env_remove("AGENT_BROWSER_RESTORE")
            .env_remove("AGENT_BROWSER_RESTORE_SAVE")
            .env_remove("AGENT_BROWSER_RESTORE_CHECK_URL")
            .env_remove("AGENT_BROWSER_RESTORE_CHECK_TEXT")
            .env_remove("AGENT_BROWSER_RESTORE_CHECK_FN")
            .env_remove("AGENT_BROWSER_EXTENSIONS")
            .env_remove("AGENT_BROWSER_INIT_SCRIPTS")
            .env_remove("AGENT_BROWSER_ENABLE")
            .env_remove("AGENT_BROWSER_HEADED")
            .env_remove("AGENT_BROWSER_USER_AGENT")
            .env_remove("AGENT_BROWSER_PROXY")
            .env_remove("AGENT_BROWSER_PROXY_BYPASS")
            .env_remove("AGENT_BROWSER_ALLOW_FILE_ACCESS")
            .env_remove("AGENT_BROWSER_USE_SYSTEM_CA")
            .env_remove("SSL_CERT_FILE")
            .env_remove("AGENT_BROWSER_PIN_TAB")
            .env_remove("AGENT_BROWSER_HIDE_SCROLLBARS")
            .env_remove(ALLOW_ENV);
        match self.executable.as_deref() {
            Some(executable) => {
                command.env("AGENT_BROWSER_EXECUTABLE_PATH", executable);
            }
            None => {
                command.env_remove("AGENT_BROWSER_EXECUTABLE_PATH");
            }
        }
        if self.headed {
            command.env("AGENT_BROWSER_HEADED", "1");
        }
        let mut browser_args = Vec::new();
        if self.no_sandbox {
            browser_args.push("--no-sandbox");
        }
        if self.headed && self.headless_display {
            browser_args.push("--ozone-platform=headless");
        }
        if browser_args.is_empty() {
            command.env_remove("AGENT_BROWSER_ARGS");
        } else {
            command.env("AGENT_BROWSER_ARGS", browser_args.join(","));
        }
        command
    }

    fn run(&self, allow_env: bool, args: &[&str]) -> Output {
        let mut command = self.command();
        if allow_env {
            command.env(ALLOW_ENV, "1");
        }
        command
            .args(args)
            .output()
            .expect("failed to run agent-browser")
    }

    fn daemon_pid(&self) -> String {
        std::fs::read_to_string(self.sockets.path().join(format!("{SESSION}.pid")))
            .expect("daemon pid file should exist")
            .trim()
            .to_string()
    }

    fn close(&mut self, allow_env: bool) -> Output {
        let output = self.run(allow_env, &["--json", "close"]);
        if output.status.success() {
            self.closed = true;
        }
        output
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !self.closed {
            let _ = self.run(true, &["--json", "close"]);
        }
    }
}

fn describe(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn response(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("stdout was not JSON: {error}\n{}", describe(output)))
}

fn assert_success(output: &Output, context: &str) -> Value {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{context}\n{}",
        describe(output)
    );
    let value = response(output);
    assert_eq!(value["success"], true, "{context}: {value}");
    value
}

fn assert_blocked(output: &Output, context: &str) {
    assert_eq!(
        output.status.code(),
        Some(1),
        "{context}\n{}",
        describe(output)
    );
    let value = response(output);
    assert_eq!(value["success"], false, "{context}: {value}");
    let error = value["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{context}: missing error text: {value}"));
    assert!(
        error.contains("Refusing to drive Chrome"),
        "{context}: {error}"
    );
    assert!(error.contains(ADVISORY_VERSION), "{context}: {error}");
    assert!(error.contains("CVE-2026-0001"), "{context}: {error}");
    assert!(
        error.contains("--allow-outdated-browser"),
        "{context}: {error}"
    );
    assert!(
        !error.contains("Could not refresh browser security advisories"),
        "{context}: {error}"
    );
}

/// Requires a real Chrome build and a host that can launch it. Set
/// `AGENT_BROWSER_TEST_BROWSER` (or `AGENT_BROWSER_EXECUTABLE_PATH`) when the
/// binary cannot find Chrome, and `AGENT_BROWSER_TEST_NO_SANDBOX=1` when the
/// sandbox needs `--no-sandbox`. Set `AGENT_BROWSER_TEST_HEADED=1` to exercise
/// headed mode. On Linux without a display, also set
/// `AGENT_BROWSER_TEST_HEADLESS_DISPLAY=1` to use the headless Ozone platform.
/// The advisory cache is a local fixture, so the check needs no network access.
#[test]
#[ignore]
fn e2e_outdated_browser_opt_out_is_scoped_to_each_cli_invocation() {
    let mut harness = Harness::new();

    let first = harness.run(true, &["--json", "open", PAGE]);
    assert_success(
        &first,
        "first launch with AGENT_BROWSER_ALLOW_OUTDATED_BROWSER=1",
    );
    let daemon = harness.daemon_pid();

    let without_env = harness.run(false, &["--json", "open", PAGE]);
    assert_blocked(&without_env, "later launch without the environment");
    assert_eq!(
        harness.daemon_pid(),
        daemon,
        "the refusal must reuse the warm daemon"
    );

    let explicit_false = harness.run(
        true,
        &["--json", "--allow-outdated-browser", "false", "open", PAGE],
    );
    assert_blocked(
        &explicit_false,
        "explicit --allow-outdated-browser false with the environment set",
    );
    assert_eq!(
        harness.daemon_pid(),
        daemon,
        "the explicit refusal must reuse the warm daemon"
    );

    let explicit_true = harness.run(
        false,
        &["--json", "--allow-outdated-browser", "true", "open", PAGE],
    );
    assert_success(&explicit_true, "explicit --allow-outdated-browser true");
    assert_eq!(
        harness.daemon_pid(),
        daemon,
        "the explicit allow must reuse the warm daemon"
    );

    let closed = harness.close(false);
    assert_success(&closed, "close without an outdated-browser override");
}

/// The advisory fixture holds a blocking verdict for the default trust. An
/// explicit invalid CA must be reported, and system trust must not reuse that fixture.
#[test]
#[ignore]
fn e2e_custom_trust_never_consumes_the_default_advisory_cache() {
    let harness = Harness::new();
    let malformed = harness.home.path().join("invalid-ca.pem");
    std::fs::write(&malformed, b"not a certificate").unwrap();
    let invalid = harness.run(
        false,
        &[
            "--json",
            "--ca-cert",
            malformed.to_str().unwrap(),
            "open",
            PAGE,
        ],
    );
    assert_eq!(invalid.status.code(), Some(1), "{}", describe(&invalid));
    let value = response(&invalid);
    assert_eq!(value["success"], false, "{value}");
    let error = value["error"]
        .as_str()
        .unwrap_or_else(|| panic!("missing error text: {value}"));
    assert!(error.contains("invalid-ca.pem"), "{error}");
    assert!(!error.contains("Refusing to drive Chrome"), "{error}");

    let custom = harness
        .command()
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("https_proxy", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("all_proxy", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .args(["--json", "--use-system-ca", "open", PAGE])
        .output()
        .expect("failed to run agent-browser");
    let value = assert_success(&custom, "custom trust without network access");
    let warning = value["warning"].as_str().unwrap_or_default();
    assert!(
        warning.contains("no current platform-specific security verdict"),
        "{value}"
    );
    let text = describe(&custom);
    assert!(!text.contains(ADVISORY_VERSION), "{text}");
    assert!(!text.contains("CVE-2026-0001"), "{text}");
    assert!(!text.contains("older cached verdict"), "{text}");
}
