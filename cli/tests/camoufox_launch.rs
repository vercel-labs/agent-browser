//! Camoufox engine integration tests (Unit 3 of the engine plan).
//!
//! Feature-gated: requires `--features camoufox-integration` to run, since
//! they spawn a real Python sidecar + Camoufox browser. On a development
//! machine set `AGENT_BROWSER_CAMOUFOX_PYTHON` to the venv under
//! `packages/camoufox-sidecar/.venv/bin/python3` so the tests don't depend
//! on the system Python.
//!
//! The non-gated tests in this file (error/validation paths that don't need
//! Camoufox installed) always run so regressions in Rust-side wiring surface
//! in CI.

#![cfg_attr(
    not(feature = "camoufox-integration"),
    allow(dead_code, unused_imports)
)]

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

/// These tests run unconditionally — they exercise error paths that don't
/// depend on Camoufox being installed, so they catch plumbing regressions
/// without the integration harness.
mod rust_only {
    use super::*;

    /// `--engine camoufox --extension foo.crx` must be rejected by
    /// `validate_camoufox_options` with a clear message. This is the
    /// "Error path" R4-parity test from the plan.
    #[test]
    fn rejects_extensions_with_camoufox() {
        let tmp = TempDir::new().unwrap();
        let output = build_cmd(
            &tmp,
            &[
                "--engine",
                "camoufox",
                "--extension",
                "/nonexistent/ext",
                "--json",
                "open",
                "https://example.com",
            ],
        )
        // Pointing at a missing python short-circuits the launch path on
        // test environments that don't have Camoufox installed, so the
        // error comes from `validate_camoufox_options` rather than the
        // sidecar spawn probe.
        .env(
            "AGENT_BROWSER_CAMOUFOX_PYTHON",
            "/definitely/not/a/real/python3",
        )
        .output()
        .expect("invoke agent-browser");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Extensions are not supported with Camoufox"),
            "expected extensions-rejection error message, got: {}",
            stdout
        );
    }

    /// `AGENT_BROWSER_CAMOUFOX_PYTHON=/nonexistent` must surface an
    /// actionable error and not partially start any process.
    #[test]
    fn missing_python_surfaces_actionable_error() {
        let tmp = TempDir::new().unwrap();
        let output = build_cmd(
            &tmp,
            &["--engine", "camoufox", "--json", "open", "https://example.com"],
        )
        .env("AGENT_BROWSER_CAMOUFOX_PYTHON", "/nonexistent/python3-xyz")
        .output()
        .expect("invoke agent-browser");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("does not exist")
                || stdout.contains("AGENT_BROWSER_CAMOUFOX_PYTHON"),
            "expected python-not-found error, got: {}",
            stdout
        );
        // Must return a structured error (non-panic, non-signal exit).
        assert_ne!(output.status.code(), Some(101), "should not panic");
    }
}

// -----------------------------------------------------------------------------
// Feature-gated integration tests. These require a real Camoufox install.
// -----------------------------------------------------------------------------

#[cfg(feature = "camoufox-integration")]
mod integration {
    use super::*;
    use std::sync::Mutex;
    use std::thread::sleep;
    use std::time::Duration;

    /// Integration tests share the Camoufox browser binary cache and can
    /// each leak stray sidecar / Firefox processes if they run concurrently.
    /// Cargo's default parallel runner would also make "no process leaked"
    /// assertions non-deterministic because each test's ps snapshot would
    /// see other tests' in-flight sidecars. Serialise the whole integration
    /// suite behind this mutex so each test sees a clean slate.
    static INTEGRATION_LOCK: Mutex<()> = Mutex::new(());

    fn acquire() -> std::sync::MutexGuard<'static, ()> {
        match INTEGRATION_LOCK.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn fixture_python() -> Option<std::path::PathBuf> {
        // Prefer the package's dev venv if it exists — faster than spinning up
        // a new environment per run.
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let repo_root = std::path::Path::new(crate_root).parent()?;
        let venv_python = repo_root
            .join("packages/camoufox-sidecar/.venv/bin/python3");
        if venv_python.is_file() {
            return Some(venv_python);
        }
        std::env::var("AGENT_BROWSER_CAMOUFOX_PYTHON")
            .ok()
            .map(std::path::PathBuf::from)
    }

    fn cmd_with_python(tmp: &TempDir, args: &[&str]) -> Command {
        let mut cmd = build_cmd(tmp, args);
        if let Some(py) = fixture_python() {
            cmd.env("AGENT_BROWSER_CAMOUFOX_PYTHON", py);
        }
        cmd
    }

    /// Happy path: open + close completes, and the child Python/Firefox
    /// processes belonging to our daemon are gone afterwards.
    #[test]
    fn open_and_close_cleans_up_children() {
        let _guard = acquire();
        let tmp = TempDir::new().unwrap();

        let open = cmd_with_python(
            &tmp,
            &[
                "--engine",
                "camoufox",
                "--session",
                "ce_open",
                "--json",
                "open",
                "https://example.com",
            ],
        )
        .output()
        .expect("open");
        assert!(
            open.status.success(),
            "open failed: stdout={} stderr={}",
            String::from_utf8_lossy(&open.stdout),
            String::from_utf8_lossy(&open.stderr)
        );
        let out = String::from_utf8_lossy(&open.stdout);
        assert!(
            out.contains("\"success\":true") || out.contains("\"success\": true"),
            "open output did not indicate success: {}",
            out
        );

        let close = cmd_with_python(&tmp, &["--session", "ce_open", "close"])
            .output()
            .expect("close");
        assert!(close.status.success(), "close failed");

        // Give the OS a moment to reap the grandchildren.
        sleep(Duration::from_secs(2));
        let daemon_pids = pgrep_contains("agent-browser --daemon");
        assert!(
            daemon_pids.is_empty(),
            "daemon process survived close: {:?}",
            daemon_pids
        );
        let sidecar_pids = pgrep_contains("camoufox_sidecar");
        assert!(
            sidecar_pids.is_empty(),
            "camoufox_sidecar process survived close: {:?}",
            sidecar_pids
        );
    }

    /// Loop smoke test from the plan: open → close → reopen 10× with no
    /// process leak between iterations.
    #[test]
    fn loop_smoke_no_process_leaks() {
        let _guard = acquire();
        let tmp = TempDir::new().unwrap();

        for iteration in 0..10 {
            let open = cmd_with_python(
                &tmp,
                &[
                    "--engine",
                    "camoufox",
                    "--session",
                    "ce_loop",
                    "--json",
                    "open",
                    "about:blank",
                ],
            )
            .output()
            .unwrap_or_else(|e| panic!("iter {}: open failed: {}", iteration, e));
            assert!(
                open.status.success(),
                "iter {}: open non-zero: {}",
                iteration,
                String::from_utf8_lossy(&open.stdout)
            );

            let close = cmd_with_python(&tmp, &["--session", "ce_loop", "close"])
                .output()
                .unwrap_or_else(|e| panic!("iter {}: close failed: {}", iteration, e));
            assert!(close.status.success(), "iter {}: close non-zero", iteration);

            sleep(Duration::from_secs(2));
            let sidecar_pids = pgrep_contains("camoufox_sidecar");
            assert!(
                sidecar_pids.is_empty(),
                "iter {}: camoufox_sidecar survived close: {:?}",
                iteration,
                sidecar_pids
            );
        }
    }

    /// `--stealth --engine camoufox` should still succeed. The warning
    /// itself is emitted from the daemon process (not the CLI client), so
    /// asserting on its text would require parsing the daemon debug log —
    /// we leave the warning's string contents locked in by the unit tests
    /// on `initialize_camoufox_manager` and limit this integration check
    /// to the observable outcome: the combination does not fail.
    #[test]
    fn stealth_plus_camoufox_still_succeeds() {
        let _guard = acquire();
        let tmp = TempDir::new().unwrap();
        let out = cmd_with_python(
            &tmp,
            &[
                "--engine",
                "camoufox",
                "--stealth",
                "--session",
                "ce_stealth",
                "--json",
                "open",
                "about:blank",
            ],
        )
        .output()
        .expect("open");
        assert!(
            out.status.success(),
            "open with --stealth failed: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        let _ = cmd_with_python(&tmp, &["--session", "ce_stealth", "close"]).output();
    }
}

/// `pgrep -f <pat>` returning the matching PIDs as strings. We prefer
/// `pgrep` over parsing `ps -A` output because `pgrep`'s exit code is
/// unambiguous (0 = found, 1 = none) and its matching scope is the full
/// command line, which is what we need to pick up `python -m camoufox_sidecar`.
#[cfg(feature = "camoufox-integration")]
fn pgrep_contains(needle: &str) -> Vec<String> {
    let output = Command::new("pgrep")
        .args(["-f", needle])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}
