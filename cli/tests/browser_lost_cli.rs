//! A browser that dies between commands is relaunched, not silently replaced.
//!
//! Regression test for #2505: when Chrome exits on its own (the daemon's
//! background tick notices and drops it), the next command launches a fresh
//! browser on a fresh profile, so the page is gone. That relaunch must be
//! reported: `lifecycle.relaunchedBrowser` is true in `--json` output, and
//! the human output notes on stderr that the browser was relaunched. Before
//! the fix the command reported `relaunchedBrowser: false` and the swap was
//! invisible.
//!
//! Needs a real Chrome: run with `cargo test --test browser_lost_cli --
//! --ignored --test-threads=1`. The binary finds Chrome through
//! `AGENT_BROWSER_EXECUTABLE_PATH` when set, else the puppeteer cache.
#![cfg(unix)]

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "lost";

struct Session {
    tmp: TempDir,
}

impl Session {
    fn new() -> Self {
        Self {
            tmp: TempDir::new().unwrap(),
        }
    }

    fn dir(&self) -> PathBuf {
        self.tmp.path().join("sockets")
    }

    fn page(&self) -> PathBuf {
        let page = self.tmp.path().join("page.html");
        fs::write(
            &page,
            "<html><body><h1>lost browser page</h1></body></html>",
        )
        .unwrap();
        page
    }

    fn command(&self) -> Command {
        self.command_with_json(true)
    }

    fn command_with_json(&self, json: bool) -> Command {
        let mut cmd = Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AGENT_BROWSER_") {
                cmd.env_remove(key);
            }
        }
        // Keep the real puppeteer cache visible so the binary can find
        // Chrome; HOME points at the temp dir to isolate user config.
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("PUPPETEER_CACHE_DIR", format!("{home}/.cache/puppeteer"));
        }
        cmd.current_dir(self.tmp.path())
            .env("HOME", self.tmp.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir())
            .env("NO_COLOR", "1")
            .args(["--session", SESSION]);
        if json {
            cmd.arg("--json");
        }
        if let Ok(executable) = std::env::var("AGENT_BROWSER_EXECUTABLE_PATH") {
            cmd.args(["--executable-path", &executable]);
        }
        cmd
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Value {
        let out = self.output(args);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "args={args:?} stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    fn pid(&self) -> u32 {
        fs::read_to_string(self.dir().join(format!("{SESSION}.pid")))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    /// The Chrome main process: the daemon's direct child whose command line
    /// names the Chrome binary.
    fn chrome_pid(&self) -> u32 {
        let daemon = self.pid();
        let mut found = Vec::new();
        if let Ok(tasks) = fs::read_dir(format!("/proc/{daemon}/task")) {
            for task in tasks.flatten() {
                let children = fs::read_to_string(task.path().join("children")).unwrap_or_default();
                for child in children.split_whitespace() {
                    let Ok(pid) = child.parse::<u32>() else {
                        continue;
                    };
                    let cmdline =
                        fs::read_to_string(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                    if cmdline.contains("chrome") {
                        found.push(pid);
                    }
                }
            }
        }
        assert_eq!(
            found.len(),
            1,
            "expected exactly one Chrome child of daemon {daemon}, found {found:?}"
        );
        found[0]
    }

    /// Kill Chrome and let the daemon's background tick notice (it runs
    /// every ~100ms while the daemon is idle).
    fn kill_chrome(&self) {
        let chrome = self.chrome_pid();
        assert!(Command::new("kill")
            .args(["-9", &chrome.to_string()])
            .status()
            .unwrap()
            .success());
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.command().arg("close").output();
    }
}

#[test]
#[ignore]
fn lost_browser_relaunch_is_reported() {
    let session = Session::new();
    let page = session.page();
    let file_url = format!("file://{}", page.display());

    let open = session.run(&[
        "--allow-file-access",
        "--args",
        "--disable-dev-shm-usage",
        "open",
        &file_url,
    ]);
    assert_eq!(open["success"], true, "open failed: {open}");
    assert_eq!(open["data"]["url"], file_url);

    let url = session.run(&["get", "url"]);
    assert_eq!(url["success"], true, "get url failed: {url}");
    assert_eq!(url["data"]["url"], file_url);

    // The browser dies on its own between commands. The next command
    // relaunches it; the JSON reports the relaunch.
    session.kill_chrome();
    let after = session.run(&["get", "url"]);
    assert_eq!(after["success"], true, "get url after loss failed: {after}");
    assert_eq!(
        after["data"]["url"], "about:blank",
        "a fresh browser starts on about:blank: {after}"
    );
    assert_eq!(
        after["data"]["lifecycle"]["launched"], true,
        "the command launched a fresh browser: {after}"
    );
    assert_eq!(
        after["data"]["lifecycle"]["relaunchedBrowser"], true,
        "replacing a lost browser is a relaunch, not a fresh launch: {after}"
    );

    // Same loss again, in human mode: stderr notes the relaunch.
    session.kill_chrome();
    let out = session
        .command_with_json(false)
        .args(["get", "url"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("about:blank"),
        "a fresh browser starts on about:blank: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("relaunched browser"),
        "stderr should say the browser was relaunched: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
