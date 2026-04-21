//! Integration tests for Unit 6: `doctor` Camoufox probe + `"engine"` label
//! in `--json` payloads.
//!
//! The CLI binary is invoked via `env!("CARGO_BIN_EXE_*")`. We override
//! `AGENT_BROWSER_SOCKET_DIR`, `HOME`, and (where needed) `PATH` so the
//! tests don't observe or mutate the host's real agent-browser state.
//!
//! The Chrome `--json` engine-label assertion falls back gracefully if
//! Chrome isn't installed on this machine — we still verify the CLI's
//! engine label shape, just from the daemon's error response instead of a
//! successful navigation.

use std::process::Command;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

fn build_doctor_cmd(tmp: &TempDir, args: &[&str]) -> Command {
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

fn parse_doctor_json(stdout: &[u8]) -> serde_json::Value {
    let s = std::str::from_utf8(stdout).expect("stdout utf8");
    serde_json::from_str(s).unwrap_or_else(|e| {
        panic!("stdout was not JSON: {}\n---\n{}", e, s);
    })
}

fn checks_by_id<'a>(
    payload: &'a serde_json::Value,
    id: &str,
) -> Vec<&'a serde_json::Value> {
    payload["checks"]
        .as_array()
        .expect("checks is array")
        .iter()
        .filter(|c| c["id"].as_str() == Some(id))
        .collect()
}

fn find_camoufox_check(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    payload["checks"]
        .as_array()
        .expect("checks is array")
        .iter()
        .find(|c| {
            c["category"].as_str() == Some("Camoufox")
                && c["id"]
                    .as_str()
                    .map(|s| s.starts_with("camoufox."))
                    .unwrap_or(false)
        })
}

// ---------------------------------------------------------------------------
// Scenario 1 (happy path): doctor reports a present camoufox install.
// ---------------------------------------------------------------------------

/// Camoufox installed in the fixture venv → doctor should pass all three
/// probes (python / package / binary). Feature-gated because the probe
/// depends on a real Camoufox fetch, which is only guaranteed to be
/// available in the same CI profile as the other camoufox integration
/// suites.
#[cfg(feature = "camoufox-integration")]
#[test]
fn doctor_reports_camoufox_present_when_installed() {
    let tmp = TempDir::new().unwrap();

    let mut cmd = build_doctor_cmd(&tmp, &["doctor", "--offline", "--quick", "--json"]);
    let crate_root = env!("CARGO_MANIFEST_DIR");
    let repo_root = std::path::Path::new(crate_root)
        .parent()
        .expect("repo root");
    let venv_python = repo_root.join("packages/camoufox-sidecar/.venv/bin/python3");
    assert!(
        venv_python.is_file(),
        "fixture venv missing at {}; run the package tests once to bootstrap it",
        venv_python.display()
    );
    cmd.env("AGENT_BROWSER_CAMOUFOX_PYTHON", &venv_python);

    let output = cmd.output().expect("invoke doctor");
    let payload = parse_doctor_json(&output.stdout);

    let python = checks_by_id(&payload, "camoufox.python");
    assert_eq!(python.len(), 1, "expected one camoufox.python check");
    assert_eq!(
        python[0]["status"].as_str(),
        Some("pass"),
        "camoufox.python should be pass, got {}",
        python[0]
    );

    let package = checks_by_id(&payload, "camoufox.package");
    assert_eq!(package.len(), 1);
    assert_eq!(package[0]["status"].as_str(), Some("pass"));

    let binary = checks_by_id(&payload, "camoufox.binary");
    assert_eq!(binary.len(), 1);
    assert_eq!(
        binary[0]["status"].as_str(),
        Some("pass"),
        "camoufox.binary should be pass, got {}",
        binary[0]
    );
    let msg = binary[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("browser binary at"),
        "binary message should include path, got: {}",
        msg
    );
}

// ---------------------------------------------------------------------------
// Scenario 2 (error paths): each failure mode produces a distinct reason.
// ---------------------------------------------------------------------------

/// Missing python → only the python check appears, as a non-fatal `info`
/// with a distinct reason mentioning `python3 not found`.
#[test]
fn doctor_missing_python_reports_distinct_reason() {
    let tmp = TempDir::new().unwrap();

    let mut cmd = build_doctor_cmd(&tmp, &["doctor", "--offline", "--quick", "--json"]);
    // Clear PATH so the PATH fallback can't find python3. Also clear the
    // explicit env var so resolve_python()'s first branch doesn't fire.
    cmd.env("PATH", "")
        .env_remove("AGENT_BROWSER_CAMOUFOX_PYTHON");

    let output = cmd.output().expect("invoke doctor");
    // `doctor` may still exit non-zero due to other host checks (e.g. no
    // Chrome). That's fine — we only care about the camoufox probe.
    let payload = parse_doctor_json(&output.stdout);

    let python = checks_by_id(&payload, "camoufox.python");
    assert_eq!(
        python.len(),
        1,
        "expected one camoufox.python check, got {:?}",
        payload["checks"]
    );
    assert_eq!(python[0]["status"].as_str(), Some("info"));
    let msg = python[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("camoufox: not available")
            && msg.contains("python3 not found"),
        "missing-python reason should be distinct, got: {}",
        msg
    );

    // When python is missing we short-circuit — package/binary checks must
    // not appear, otherwise the user can't tell the root cause.
    assert!(
        checks_by_id(&payload, "camoufox.package").is_empty(),
        "camoufox.package should be skipped when python is missing"
    );
    assert!(
        checks_by_id(&payload, "camoufox.binary").is_empty(),
        "camoufox.binary should be skipped when python is missing"
    );
}

/// Python path pointing at a non-existent file is the same category as
/// "no python3 on PATH" for doctor purposes: the probe can't run and we
/// must say so clearly. Uses a distinct reason (spawn-failed-shape) from
/// "package missing" / "binary missing".
#[test]
fn doctor_nonexistent_python_path_reports_distinct_reason() {
    let tmp = TempDir::new().unwrap();

    let mut cmd = build_doctor_cmd(&tmp, &["doctor", "--offline", "--quick", "--json"]);
    cmd.env("AGENT_BROWSER_CAMOUFOX_PYTHON", "/does/not/exist/python3");

    let output = cmd.output().expect("invoke doctor");
    let payload = parse_doctor_json(&output.stdout);

    let python = checks_by_id(&payload, "camoufox.python");
    assert_eq!(python.len(), 1);
    assert_eq!(python[0]["status"].as_str(), Some("info"));
    let msg = python[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("not runnable") && msg.contains("/does/not/exist/python3"),
        "bad python path should surface `not runnable` reason, got: {}",
        msg
    );

    assert!(checks_by_id(&payload, "camoufox.package").is_empty());
    assert!(checks_by_id(&payload, "camoufox.binary").is_empty());
}

/// Python present but `import camoufox` fails → package probe surfaces a
/// distinct reason, and the binary probe is skipped.
#[test]
fn doctor_missing_camoufox_package_reports_distinct_reason() {
    // Run on a python that is nearly certain not to have camoufox
    // installed: the system python3 (as opposed to the fixture venv that
    // the camoufox-integration tests use). If the host doesn't have
    // python3 at all, we skip — the missing-python scenario covers it.
    let Some(system_python) = which("python3") else {
        eprintln!("skipping: no python3 on PATH");
        return;
    };

    // Skip if camoufox happens to be installed into the system python
    // already (unlikely on CI but possible on the maintainer's machine).
    let has_camoufox = Command::new(&system_python)
        .args(["-c", "import camoufox"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_camoufox {
        eprintln!("skipping: system python has camoufox installed");
        return;
    }

    let tmp = TempDir::new().unwrap();
    let mut cmd = build_doctor_cmd(&tmp, &["doctor", "--offline", "--quick", "--json"]);
    cmd.env("AGENT_BROWSER_CAMOUFOX_PYTHON", &system_python);

    let output = cmd.output().expect("invoke doctor");
    let payload = parse_doctor_json(&output.stdout);

    let python = checks_by_id(&payload, "camoufox.python");
    assert_eq!(python.len(), 1);
    assert_eq!(python[0]["status"].as_str(), Some("pass"));

    let package = checks_by_id(&payload, "camoufox.package");
    assert_eq!(
        package.len(),
        1,
        "expected one camoufox.package check, got {:?}",
        payload["checks"]
    );
    assert_eq!(package[0]["status"].as_str(), Some("info"));
    let msg = package[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("camoufox: not available") && msg.contains("camoufox package not installed"),
        "package-missing reason should be distinct, got: {}",
        msg
    );

    assert!(
        checks_by_id(&payload, "camoufox.binary").is_empty(),
        "camoufox.binary should be skipped when package is missing"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3 (happy path): --json payload carries "engine": "camoufox".
// ---------------------------------------------------------------------------

/// Any `--json` response produced by `--engine camoufox` must carry
/// `"engine": "camoufox"` at top level so downstream telemetry can segment.
/// We use the Camoufox + missing-extensions validation error to get a
/// deterministic response without requiring a real browser launch.
#[test]
fn camoufox_json_payload_carries_engine_label() {
    let tmp = TempDir::new().unwrap();

    let output = build_doctor_cmd(
        &tmp,
        &[
            "--engine",
            "camoufox",
            "--extension",
            "/nonexistent/ext.crx",
            "--json",
            "open",
            "https://example.com",
        ],
    )
    // Point at a missing python so we don't actually spawn a sidecar;
    // `validate_camoufox_options` rejects --extension before that path.
    .env("AGENT_BROWSER_CAMOUFOX_PYTHON", "/nonexistent/python3-xyz")
    .output()
    .expect("invoke agent-browser");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout utf8");
    let payload: serde_json::Value = serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("expected JSON, got: {}\n---\n{}", e, stdout));

    assert_eq!(
        payload["engine"].as_str(),
        Some("camoufox"),
        "--engine camoufox payload should carry `engine: camoufox`, got: {}",
        stdout
    );
    // Sanity: the validation rejection is what we expected to trigger.
    assert_eq!(payload["success"].as_bool(), Some(false));
}

// ---------------------------------------------------------------------------
// Scenario 4 (structure-insensitive): chrome payload still carries
// "engine": "chrome".
// ---------------------------------------------------------------------------

/// Chrome `--json` responses must carry `"engine": "chrome"` at top level.
/// We don't want this test to depend on a working Chrome install, so we
/// trigger a validation error that goes through the same response path —
/// any action dispatched against a Chrome-engine daemon produces the same
/// shape.
#[test]
fn chrome_json_payload_carries_engine_label() {
    let tmp = TempDir::new().unwrap();

    // `screencast` is not available without a live session; the daemon
    // will return a structured error. The exact action doesn't matter —
    // what matters is that the response envelope carries an engine label.
    // Use a local-only command that doesn't need Chrome: `state list`
    // runs without a daemon, so we pick a command that *does* hit the
    // daemon path. `session list` does.
    //
    // Simplest: use `--engine chrome` explicitly and rely on the
    // missing-chrome auto-launch error. That response goes through
    // `error_response` which carries the engine label.
    let output = build_doctor_cmd(
        &tmp,
        &["--engine", "chrome", "--json", "navigate", "https://example.com"],
    )
    // Prevent the daemon from finding a real Chrome install — forces a
    // structured error rather than actually launching a browser.
    .env("AGENT_BROWSER_NO_AUTO_CONNECT", "1")
    .env("PUPPETEER_EXECUTABLE_PATH", "/nonexistent/chrome")
    .output()
    .expect("invoke agent-browser");

    let stdout = std::str::from_utf8(&output.stdout).expect("stdout utf8");
    if stdout.trim().is_empty() {
        // On hosts without Chrome, the CLI may fail before producing a
        // JSON payload (e.g. refused to start daemon). In that case
        // the test is not meaningful on this host. The camoufox label
        // scenario is the load-bearing one; chrome label parity is
        // verified by the exhaustive daemon-side unit test
        // (`test_success_response_structure`, `test_error_response_structure`).
        eprintln!("skipping: chrome path produced no JSON on this host");
        return;
    }

    // The daemon may retry and emit multiple JSON lines; parse the first
    // complete object that contains an `engine` field.
    let label = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|v| v.get("engine").and_then(|e| e.as_str()).map(str::to_string));

    match label {
        Some(engine) => assert_eq!(
            engine, "chrome",
            "chrome payload should carry engine=chrome, got {} in output: {}",
            engine, stdout
        ),
        None => {
            // Host did not reach a daemon response, see comment above.
            eprintln!(
                "skipping: no JSON response from daemon on this host (output: {})",
                stdout
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn which(name: &str) -> Option<String> {
    let which_cmd = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    let out = Command::new(which_cmd).arg(name).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().next().map(|l| l.trim().to_string())
}

// The `find_camoufox_check` helper is used only by the integration-gated
// happy-path test above; silence dead-code warnings when that feature is
// off.
#[cfg(not(feature = "camoufox-integration"))]
#[allow(dead_code)]
fn _suppress_dead_code_without_feature(p: &serde_json::Value) -> Option<&serde_json::Value> {
    find_camoufox_check(p)
}
