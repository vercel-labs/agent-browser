//! Rust-level parity + command-surface tests for the Camoufox engine (Unit 4
//! of the engine plan).
//!
//! The *happy-path* tests drive an actual Camoufox browser via the `--engine
//! camoufox` CLI surface and compare the snapshot output against the Chrome
//! golden at `cli/tests/fixtures/form-chrome-golden.json`. They're gated on
//! `--features camoufox-integration` to match the existing Unit 3 suite at
//! `cli/tests/camoufox_launch.rs`.
//!
//! Structural parity is the contract we care about:
//!
//!   - same number of `@eN` refs,
//!   - same set of `(role, name)` pairs,
//!
//! **not** identical ref ordering. Chrome's accessibility tree walk ends up
//! visiting cursor-interactive elements after AX-native ones, so the Submit
//! button lands at `e3` on Chrome and `e6` on Camoufox for this fixture.
//! Comparing anything finer than "did both engines see the same set of
//! interactive things?" is a recipe for flakes on engine upgrades.

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

#[cfg(feature = "camoufox-integration")]
mod integration {
    use super::*;
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::thread::sleep;
    use std::time::Duration;

    /// Camoufox integration tests cannot run in parallel — they share the
    /// same Camoufox browser cache and any overlapping ``ps`` probes (the
    /// ``leak`` assertions in Unit 3) would race. Mirror the
    /// ``camoufox_launch.rs`` INTEGRATION_LOCK so Cargo's default parallel
    /// runner doesn't wedge the suite.
    static INTEGRATION_LOCK: Mutex<()> = Mutex::new(());

    fn acquire() -> std::sync::MutexGuard<'static, ()> {
        match INTEGRATION_LOCK.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn fixture_python() -> Option<PathBuf> {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let repo_root = std::path::Path::new(crate_root).parent()?;
        let venv_python = repo_root.join("packages/camoufox-sidecar/.venv/bin/python3");
        if venv_python.is_file() {
            return Some(venv_python);
        }
        std::env::var("AGENT_BROWSER_CAMOUFOX_PYTHON")
            .ok()
            .map(PathBuf::from)
    }

    fn cmd_with_python(tmp: &TempDir, args: &[&str]) -> Command {
        let mut cmd = build_cmd(tmp, args);
        if let Some(py) = fixture_python() {
            cmd.env("AGENT_BROWSER_CAMOUFOX_PYTHON", py);
        }
        cmd
    }

    fn fixture_url() -> String {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let p = std::path::Path::new(crate_root).join("tests/fixtures/form.html");
        format!("file://{}", p.display())
    }

    fn chrome_golden() -> Value {
        let crate_root = env!("CARGO_MANIFEST_DIR");
        let p = std::path::Path::new(crate_root).join("tests/fixtures/form-chrome-golden.json");
        let raw = std::fs::read_to_string(p).expect("read chrome golden");
        serde_json::from_str(&raw).expect("parse chrome golden")
    }

    fn role_name_set(refs: &Value) -> BTreeSet<(String, String)> {
        let obj = refs.as_object().expect("refs is object");
        obj.values()
            .map(|entry| {
                let role = entry
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = entry
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                (role, name)
            })
            .collect()
    }

    fn session_args<'a>(session: &'a str, extras: &'a [&'a str]) -> Vec<&'a str> {
        let mut v: Vec<&str> = vec!["--engine", "camoufox", "--session", session, "--json"];
        v.extend(extras);
        v
    }

    fn open_fixture(tmp: &TempDir, session: &str) {
        let url = fixture_url();
        let open_args = ["open", url.as_str()];
        let args = session_args(session, &open_args);
        let out = cmd_with_python(tmp, &args).output().expect("open");
        assert!(
            out.status.success(),
            "open failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }

    fn run_json(tmp: &TempDir, session: &str, extras: &[&str]) -> Value {
        let args: Vec<&str> = {
            let mut v: Vec<&str> = vec!["--session", session, "--json"];
            v.extend(extras);
            v
        };
        let out = cmd_with_python(tmp, &args).output().expect("run_json");
        assert!(
            out.status.success(),
            "cmd {:?} failed: stdout={} stderr={}",
            extras,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("invalid JSON response: {} — body: {}", e, stdout))
    }

    fn close(tmp: &TempDir, session: &str) {
        let _ = cmd_with_python(tmp, &["--session", session, "close"]).output();
        sleep(Duration::from_secs(1));
    }

    /// Parity: snapshot role/name set matches Chrome golden on the form fixture.
    #[test]
    fn snapshot_refs_match_chrome_golden_on_fixture() {
        let _guard = acquire();
        let tmp = TempDir::new().unwrap();
        let session = "cam_parity_refs";
        open_fixture(&tmp, session);

        let snap = run_json(&tmp, session, &["snapshot"]);
        close(&tmp, session);

        let refs = snap
            .get("data")
            .and_then(|d| d.get("refs"))
            .expect("response has data.refs");
        let got = role_name_set(refs);

        let golden = chrome_golden();
        let golden_refs = golden
            .get("data")
            .and_then(|d| d.get("refs"))
            .expect("golden has data.refs");
        let expected = role_name_set(golden_refs);

        assert_eq!(
            got, expected,
            "Camoufox snapshot refs diverge from Chrome golden (set-level parity)",
        );
    }

    /// Ref-based click+fill+gettext pipeline exercises the sidecar's ref cache
    /// via the CLI surface.
    #[test]
    fn click_fill_gettext_by_ref_roundtrip() {
        let _guard = acquire();
        let tmp = TempDir::new().unwrap();
        let session = "cam_parity_click";
        open_fixture(&tmp, session);

        let snap = run_json(&tmp, session, &["snapshot"]);
        let refs = snap
            .get("data")
            .and_then(|d| d.get("refs"))
            .and_then(|v| v.as_object())
            .expect("refs");
        let email_ref = refs
            .iter()
            .find(|(_, v)| {
                v.get("role").and_then(|r| r.as_str()) == Some("textbox")
                    && v.get("name").and_then(|n| n.as_str()).map(str::trim) == Some("Email")
            })
            .map(|(k, _)| k.clone())
            .expect("email textbox ref");
        let submit_ref = refs
            .iter()
            .find(|(_, v)| {
                v.get("role").and_then(|r| r.as_str()) == Some("button")
                    && v.get("name").and_then(|n| n.as_str()).map(str::trim) == Some("Submit")
            })
            .map(|(k, _)| k.clone())
            .expect("submit button ref");

        let email_token = format!("@{}", email_ref);
        let submit_token = format!("@{}", submit_ref);

        let _ = run_json(
            &tmp,
            session,
            &["fill", &email_token, "test@example.com"],
        );
        let _ = run_json(&tmp, session, &["click", &submit_token]);

        let status = run_json(&tmp, session, &["get", "text", "#status"]);
        close(&tmp, session);

        let text = status
            .get("data")
            .and_then(|d| d.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(text, "Submitted", "status didn't update after ref-click");
    }

    /// CSS-selector path: ``click "#submit"`` must work without a prior
    /// snapshot.
    #[test]
    fn click_by_css_selector_without_snapshot() {
        let _guard = acquire();
        let tmp = TempDir::new().unwrap();
        let session = "cam_parity_css";
        open_fixture(&tmp, session);

        let _ = run_json(&tmp, session, &["click", "#submit"]);
        let status = run_json(&tmp, session, &["get", "text", "#status"]);
        close(&tmp, session);

        let text = status
            .get("data")
            .and_then(|d| d.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert_eq!(text, "Submitted");
    }

    /// Stale-ref contract: refs from before a navigation must surface
    /// ``ref-stale`` rather than silently acting on a reloaded element.
    #[test]
    fn ref_stale_after_navigation() {
        let _guard = acquire();
        let tmp = TempDir::new().unwrap();
        let session = "cam_parity_stale";
        open_fixture(&tmp, session);

        let snap = run_json(&tmp, session, &["snapshot"]);
        let refs = snap
            .get("data")
            .and_then(|d| d.get("refs"))
            .and_then(|v| v.as_object())
            .expect("refs");
        let any_ref = refs.keys().next().cloned().expect("at least one ref");
        let token = format!("@{}", any_ref);

        // data: URL dodges the "navigating to about:blank from about:blank"
        // Playwright interruption.
        let _ = run_json(
            &tmp,
            session,
            &["navigate", "data:text/html,<html><body>after</body></html>"],
        );

        // The CLI wraps non-zero `success:false` responses into a non-zero
        // exit status, so we can't use `run_json`. Use a direct command.
        let out = cmd_with_python(&tmp, &["--session", session, "--json", "click", &token])
            .output()
            .expect("click after nav");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("ref-stale"),
            "expected ref-stale error, got: {}",
            stdout
        );
        close(&tmp, session);
    }
}
