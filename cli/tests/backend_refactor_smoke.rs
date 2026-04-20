//! Smoke + characterization tests for the `BrowserBackend` refactor (Units 1
//! and 3 of the Camoufox engine plan).
//!
//! These tests cover two things the refactor must guarantee:
//!
//! 1. `agent-browser --engine camoufox open <url>` does **not** panic. It
//!    must exit cleanly with a structured JSON error whose message
//!    mentions Camoufox — either the Unit 1 `not-yet-implemented` stub or
//!    the Unit 3 launch-failure message (Python missing / sidecar failed
//!    readiness) depending on how much of the plan has landed.
//!
//! 2. Unknown engines are rejected with a message that enumerates
//!    `chrome, lightpanda, camoufox` — proves the launch dispatch table
//!    has the new arm wired up.
//!
//! Both tests spawn the real CLI binary (no Chrome required) so they run in
//! CI without infrastructure. Chrome + Lightpanda happy-path parity is covered
//! by the existing `#[ignore]`d integration suite in `cli/src/native/e2e_tests.rs`
//! which we ran manually against this refactor to produce the characterization
//! baseline — the invariant those tests enforce (execute_command returns the
//! same response shape before/after Unit 1) is what this smoke file locks in
//! cheaply.

use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn build_cmd(tmp: &TempDir, args: &[&str]) -> Command {
    let socket_dir = tmp.path().join("sockets");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&socket_dir).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .env("AGENT_BROWSER_SOCKET_DIR", &socket_dir)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env_remove("AGENT_BROWSER_PROVIDER")
        .env_remove("AGENT_BROWSER_CDP")
        .env_remove("AGENT_BROWSER_AUTO_CONNECT")
        .env_remove("AGENT_BROWSER_ENGINE")
        .env("NO_COLOR", "1");
    cmd
}

#[test]
fn camoufox_engine_returns_structured_error_without_panic() {
    let tmp = TempDir::new().unwrap();

    // Point the sidecar at a non-existent python so Unit 3's launch path
    // exits cleanly on environments that don't have Camoufox installed —
    // CI will otherwise spend minutes in the Python probe.
    let output = build_cmd(
        &tmp,
        &["--engine", "camoufox", "--json", "open", "https://example.com"],
    )
    .env(
        "AGENT_BROWSER_CAMOUFOX_PYTHON",
        "/definitely/not/a/real/python3",
    )
    .output()
    .expect("failed to invoke agent-browser");

    // The command must not panic. A panic surfaces as signal-death (exit code
    // 101 for explicit panics, 134/137/139 for signals, or None on Unix signal
    // termination). A non-zero but structured exit is fine.
    assert!(
        !matches!(output.status.code(), Some(101)),
        "--engine camoufox open panicked (exit 101)\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.code().is_some(),
        "--engine camoufox open died from a signal (no exit code)\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");

    // JSON output must parse and carry a failure payload.
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not JSON: {}\n---\n{}", e, stdout));

    assert_eq!(
        payload.get("success").and_then(|v| v.as_bool()),
        Some(false),
        "expected success:false for camoufox launch failure, got payload:\n{}",
        stdout
    );

    let error = payload
        .get("error")
        .and_then(|v| v.as_str())
        .expect("payload must contain an error string");
    // Accept either the Unit 1 stub shape or the Unit 3 "python missing"
    // shape; both are characterised by a mention of camoufox or the python
    // env var we set above.
    assert!(
        error.to_lowercase().contains("camoufox")
            || error.contains("AGENT_BROWSER_CAMOUFOX_PYTHON"),
        "error message did not mention camoufox/python: {:?}",
        error
    );
}

#[test]
fn unknown_engine_lists_camoufox_in_supported_engines() {
    let tmp = TempDir::new().unwrap();

    let output = build_cmd(
        &tmp,
        &["--engine", "nonsense", "--json", "open", "https://example.com"],
    )
    .output()
    .expect("failed to invoke agent-browser");

    let stdout = String::from_utf8(output.stdout).expect("stdout should be utf8");

    // Either the flag layer rejects it or the launch layer does; both should
    // surface a user-visible message that enumerates the valid engines,
    // including `camoufox` now that Unit 1 has wired it in.
    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout was not JSON: {}\n---\n{}", e, stdout));
    let error = payload
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    assert!(
        error.contains("camoufox"),
        "unknown-engine error should enumerate `camoufox` among supported engines, got: {:?}",
        error
    );
}
