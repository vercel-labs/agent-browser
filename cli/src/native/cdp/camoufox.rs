//! Camoufox Python sidecar: process lifecycle + launch pipeline.
//!
//! The sidecar is a long-lived `python3` child that holds a
//! Playwright+Camoufox browser open and speaks the JSON-line protocol
//! documented in `packages/camoufox-sidecar/camoufox_sidecar/protocol.py`.
//! This module mirrors `lightpanda.rs` in shape (process ownership, bounded
//! log drainer, structured readiness error) and adds the Python-specific
//! dispatch logic: `python3 -m camoufox_sidecar` first, with a fallback to
//! `python3 <extracted-dir>/__main__.py` + `PYTHONPATH` when the package is
//! not pip-installed.
//!
//! `CamoufoxProcess` owns the `Child` (and kills it on drop). The
//! `CamoufoxClient` that rides on top is constructed inside
//! `launch_camoufox_sidecar` from the child's stdio and returned alongside.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::native::camoufox_client::CamoufoxClient;
use crate::native::camoufox_embed;

const READY_TIMEOUT: Duration = Duration::from_secs(15);
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_LOG_LINES: usize = 40;
const GRACEFUL_EXIT_WAIT: Duration = Duration::from_millis(500);

#[cfg(unix)]
const SIGNAL_TERMINATE: i32 = libc::SIGTERM;
#[cfg(unix)]
const SIGNAL_FORCE_KILL: i32 = libc::SIGKILL;
#[cfg(not(unix))]
const SIGNAL_TERMINATE: i32 = 15;
#[cfg(not(unix))]
const SIGNAL_FORCE_KILL: i32 = 9;

/// Send `signal` to the process group led by `pid`. Because the sidecar is
/// spawned with `setpgid(0, 0)`, its pid and pgid are the same and this
/// hits every descendant (Python → Camoufox → plugin-container helpers).
#[cfg(unix)]
fn send_signal_to_group(pid: u32, signal: i32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, signal);
    }
}

#[cfg(not(unix))]
fn send_signal_to_group(_pid: u32, _signal: i32) {
    // Windows path not yet supported. The sidecar is not expected to run on
    // Windows in Unit 3 (E2B is Linux, dev is Linux/macOS); when Windows
    // support is added, use `TerminateProcess` + job objects here.
}

/// Owns the Python sidecar subprocess and its stderr log drainer.
///
/// `CamoufoxClient` owns the stdio half of the relationship (writer +
/// demultiplexing reader). `CamoufoxProcess` owns the OS-level child: it is
/// responsible for killing the process on drop so a panicking daemon cannot
/// leak a Python+Firefox grandchild tree.
pub struct CamoufoxProcess {
    child: Option<Child>,
    /// PID reported by the sidecar's `ready` event. Mostly useful for
    /// integration tests that assert the process tree is gone after close.
    pub sidecar_pid: Option<u32>,
    _stderr_drainer: Option<tokio::task::JoinHandle<()>>,
    stderr_log: SharedLog,
}

impl CamoufoxProcess {
    /// Best-effort terminate. Sends SIGTERM to the sidecar's process group
    /// so the entire descendant tree (Python → Camoufox → plugin-container
    /// helpers) shuts down together. Call `wait_or_kill` afterwards if you
    /// need the OS-level reap to complete before returning.
    pub fn kill(&mut self) {
        if let Some(pid) = self.child.as_ref().and_then(|c| c.id()) {
            send_signal_to_group(pid, SIGNAL_TERMINATE);
        }
    }

    /// Graceful-then-forceful shutdown. Sends SIGTERM, waits up to
    /// `timeout` for the sidecar (and its descendants) to exit, then
    /// SIGKILLs the process group if anything is still alive.
    ///
    /// Purely synchronous — the caller is expected to invoke this from a
    /// blocking context (e.g. `tokio::task::spawn_blocking`). We use raw
    /// `libc::waitpid` rather than tokio's `Child::wait` because wiring
    /// futures through a potentially-detached `tokio::spawn` was a
    /// persistent source of racy teardown where the process wasn't
    /// actually gone by the time `mgr.close()` returned. Synchronous
    /// waitpid blocks on the kernel and returns deterministically.
    pub fn wait_or_kill(&mut self, timeout: Duration) {
        let Some(child) = self.child.take() else {
            return;
        };
        let Some(pid) = child.id() else {
            // Already reaped; tokio may have taken the exit status.
            return;
        };
        // We own the Child here; dropping it at the end of this function
        // is fine because we've already reaped the kernel-level process
        // entry below. We don't need to hold it across the wait.
        drop(child);

        send_signal_to_group(pid, SIGNAL_TERMINATE);

        #[cfg(unix)]
        {
            const POLL: Duration = Duration::from_millis(100);
            let start = std::time::Instant::now();
            while start.elapsed() < timeout {
                let mut status: libc::c_int = 0;
                let ret = unsafe {
                    libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG)
                };
                if ret == pid as libc::pid_t || ret == -1 {
                    return;
                }
                std::thread::sleep(POLL);
            }

            send_signal_to_group(pid, SIGNAL_FORCE_KILL);
            let mut status: libc::c_int = 0;
            unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
        }
    }

    /// Snapshot of the last few stderr lines — used to build a detailed
    /// error message when readiness times out or the child exits early.
    pub fn snapshot_stderr(&self) -> Vec<String> {
        self.stderr_log
            .lock()
            .expect("stderr log poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Non-blocking probe: has the sidecar subprocess exited? Also reaps
    /// the zombie if so, matching Chrome/Lightpanda semantics.
    pub fn has_exited(&mut self) -> bool {
        let Some(child) = self.child.as_mut() else {
            return true;
        };
        matches!(child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for CamoufoxProcess {
    /// Synchronous cleanup path for the ungraceful case (daemon panic, the
    /// `BrowserManager` being dropped without a `close()` call). Sends
    /// SIGTERM to the sidecar's process group, waits briefly for its
    /// asyncio cleanup + Playwright Firefox teardown to complete, and
    /// escalates to SIGKILL if that times out. We use `libc::waitpid`
    /// directly rather than `Child::wait` so this stays cheap in Drop —
    /// spinning up a fresh tokio runtime from a destructor has historically
    /// been a source of subtle deadlocks.
    fn drop(&mut self) {
        let Some(pid) = self.child.as_ref().and_then(|c| c.id()) else {
            return;
        };

        send_signal_to_group(pid, SIGNAL_TERMINATE);

        #[cfg(unix)]
        {
            const DROP_GRACEFUL_WAIT: Duration = Duration::from_secs(3);
            const DROP_POLL: Duration = Duration::from_millis(100);

            let start = std::time::Instant::now();
            let mut reaped = false;
            while start.elapsed() < DROP_GRACEFUL_WAIT {
                let mut status: libc::c_int = 0;
                let ret = unsafe {
                    libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG)
                };
                if ret == pid as libc::pid_t {
                    reaped = true;
                    break;
                }
                if ret == -1 {
                    // ECHILD = already reaped (e.g. by tokio), which is fine.
                    reaped = true;
                    break;
                }
                std::thread::sleep(DROP_POLL);
            }

            if !reaped {
                send_signal_to_group(pid, SIGNAL_FORCE_KILL);
                let mut status: libc::c_int = 0;
                unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
            }
        }
    }
}

type SharedLog = Arc<Mutex<VecDeque<String>>>;

fn empty_log() -> SharedLog {
    Arc::new(Mutex::new(VecDeque::with_capacity(MAX_LOG_LINES)))
}

fn push_bounded(log: &SharedLog, line: String) {
    let mut g = log.lock().expect("stderr log poisoned");
    if g.len() >= MAX_LOG_LINES {
        g.pop_front();
    }
    g.push_back(line);
}

/// Validated Camoufox launch kwargs passed through to the sidecar `launch`
/// command. `args` contains exactly the object the sidecar will feed into
/// `AsyncCamoufox(**kwargs)`; the Python side re-validates against its own
/// allowlist so new options can be rolled out from the sidecar without a
/// Rust release.
#[derive(Debug, Default, Clone)]
pub struct CamoufoxLaunchOptions {
    pub headless: bool,
    pub executable_path: Option<String>,
    pub proxy: Option<Value>,
    /// Extra allowed kwargs forwarded verbatim to the sidecar. Left open
    /// (instead of strongly typed) because the sidecar already enforces the
    /// allowlist; adding fields here just duplicates validation.
    pub extra: serde_json::Map<String, Value>,
}

impl CamoufoxLaunchOptions {
    fn to_launch_args(&self) -> Value {
        let mut args = serde_json::Map::new();
        args.insert("headless".to_string(), json!(self.headless));
        if let Some(path) = &self.executable_path {
            args.insert("executable_path".to_string(), json!(path));
        }
        if let Some(proxy) = &self.proxy {
            args.insert("proxy".to_string(), proxy.clone());
        }
        for (k, v) in &self.extra {
            args.insert(k.clone(), v.clone());
        }
        Value::Object(args)
    }
}

/// Launch the Python sidecar, wait for its `ready` event, then send the
/// `launch` command to bring up the Camoufox browser. Returns the owning
/// process handle paired with the client that the rest of the daemon drives.
///
/// Failure cleans up the subprocess before returning; callers never receive
/// a `CamoufoxProcess` whose `ready` handshake did not complete.
pub async fn launch_camoufox_sidecar(
    options: &CamoufoxLaunchOptions,
) -> Result<(CamoufoxProcess, Arc<CamoufoxClient>), String> {
    let python = resolve_python_executable()?;
    let extracted = camoufox_embed::ensure_extracted()
        .map_err(|e| format!("Failed to extract embedded camoufox-sidecar: {}", e))?;

    let (mut child, dispatch) = spawn_sidecar(&python, &extracted).await?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Failed to capture camoufox-sidecar stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Failed to capture camoufox-sidecar stdout".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Failed to capture camoufox-sidecar stderr".to_string())?;

    let stderr_log = empty_log();
    let stderr_drainer = spawn_stderr_drainer(stderr, stderr_log.clone());

    let (client, ready_pid) = match CamoufoxClient::start(stdin, stdout, READY_TIMEOUT).await {
        Ok(c) => c,
        Err(e) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let stderr = snapshot(&stderr_log);
            return Err(decorate_error(
                format!("camoufox-sidecar failed readiness handshake: {}", e),
                dispatch,
                &stderr,
            ));
        }
    };

    let launch_args = options.to_launch_args();
    let launch_result = tokio::time::timeout(LAUNCH_TIMEOUT, client.call("launch", launch_args))
        .await
        .map_err(|_| "Camoufox launch timed out after 60s".to_string())
        .and_then(|r| r);

    if let Err(err) = launch_result {
        // Attempt a graceful close; if that fails, kill.
        let _ = tokio::time::timeout(GRACEFUL_EXIT_WAIT, client.close()).await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        let stderr = snapshot(&stderr_log);
        return Err(decorate_error(
            format!("Camoufox launch failed: {}", err),
            dispatch,
            &stderr,
        ));
    }

    Ok((
        CamoufoxProcess {
            child: Some(child),
            sidecar_pid: ready_pid,
            _stderr_drainer: Some(stderr_drainer),
            stderr_log,
        },
        client,
    ))
}

/// Describes which invocation path the sidecar used. Retained only for the
/// error message — callers don't care beyond that.
#[derive(Debug, Clone)]
enum SidecarDispatch {
    Module(String),
    Script { script: PathBuf },
}

impl SidecarDispatch {
    fn describe(&self) -> String {
        match self {
            SidecarDispatch::Module(m) => format!("python3 -m {}", m),
            SidecarDispatch::Script { script } => {
                format!("python3 {}", script.display())
            }
        }
    }
}

/// Spawn the sidecar, trying `-m camoufox_sidecar` first (works when the
/// package is pip-installed, as in E2B) and falling back to the embedded
/// copy extracted to the user cache (works when only the Rust binary is
/// installed).
async fn spawn_sidecar(
    python: &Path,
    extracted: &Path,
) -> Result<(Child, SidecarDispatch), String> {
    // Probe: can Python find `camoufox_sidecar` on its own? We do a cheap
    // `-c "import camoufox_sidecar"` first so the fallback doesn't require
    // swallowing a startup crash.
    let probe_ok = tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(python)
            .args(["-c", "import camoufox_sidecar"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await
    .ok()
    .and_then(|r| r.ok())
    .map(|s| s.success())
    .unwrap_or(false);

    if probe_ok {
        let child = build_command(python)
            .args(["-m", "camoufox_sidecar"])
            .spawn()
            .map_err(|e| format!("Failed to spawn `python3 -m camoufox_sidecar`: {}", e))?;
        return Ok((
            child,
            SidecarDispatch::Module("camoufox_sidecar".to_string()),
        ));
    }

    // Fallback: `extracted` is the PYTHONPATH root — it contains a
    // `camoufox_sidecar/` package directory. We set PYTHONPATH and invoke
    // `python3 -m camoufox_sidecar` so Python loads the module as a proper
    // package (relative imports like `from .protocol import ...` resolve).
    let package_init = extracted.join("camoufox_sidecar").join("__main__.py");
    if !package_init.is_file() {
        return Err(format!(
            "Embedded camoufox-sidecar is missing __main__.py at {}",
            package_init.display()
        ));
    }
    let pythonpath = prepend_pythonpath(extracted);

    let child = build_command(python)
        .args(["-m", "camoufox_sidecar"])
        .env("PYTHONPATH", pythonpath)
        .spawn()
        .map_err(|e| {
            format!(
                "Failed to spawn fallback `python3 -m camoufox_sidecar` (PYTHONPATH={}): {}",
                extracted.display(),
                e
            )
        })?;
    Ok((
        child,
        SidecarDispatch::Script {
            script: package_init,
        },
    ))
}

fn build_command(python: &Path) -> Command {
    let mut cmd = Command::new(python);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Python must flush stdout after every frame — otherwise the sidecar
        // protocol deadlocks on buffered output. The sidecar itself calls
        // `sys.stdout.flush()` but we set this too as belt-and-braces.
        .env("PYTHONUNBUFFERED", "1");

    // Make the sidecar the leader of its own process group so we can signal
    // the entire descendant tree (Python → Camoufox → plugin-container
    // helpers) with one kill. Without this, on macOS the Firefox
    // grandchildren survive when we SIGKILL only the Python parent and
    // leak across test runs. `kill_on_drop` is deliberately NOT set — it
    // uses SIGKILL, which gives the sidecar no chance to run its asyncio
    // cleanup (which is how Playwright closes Firefox cleanly).
    #[cfg(unix)]
    {
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    cmd
}

fn prepend_pythonpath(dir: &Path) -> std::ffi::OsString {
    let existing = std::env::var_os("PYTHONPATH");
    let sep = if cfg!(windows) { ";" } else { ":" };
    let mut out = std::ffi::OsString::from(dir.as_os_str());
    if let Some(v) = existing {
        if !v.is_empty() {
            out.push(sep);
            out.push(v);
        }
    }
    out
}

/// Discovery order per the plan: env var → `python3` on PATH → error.
fn resolve_python_executable() -> Result<PathBuf, String> {
    if let Ok(v) = std::env::var("AGENT_BROWSER_CAMOUFOX_PYTHON") {
        if !v.is_empty() {
            let p = PathBuf::from(v);
            if p.exists() {
                return Ok(p);
            }
            return Err(format!(
                "AGENT_BROWSER_CAMOUFOX_PYTHON points to a path that does not exist: {}",
                p.display()
            ));
        }
    }

    #[cfg(unix)]
    {
        for candidate in ["python3", "python"] {
            if let Ok(output) = std::process::Command::new("which").arg(candidate).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Ok(PathBuf::from(path));
                    }
                }
            }
        }
    }
    #[cfg(windows)]
    {
        for candidate in ["python3", "python"] {
            if let Ok(output) = std::process::Command::new("where").arg(candidate).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if !path.is_empty() {
                        return Ok(PathBuf::from(path));
                    }
                }
            }
        }
    }

    Err(
        "Camoufox requires a Python 3 runtime with the `camoufox` package installed. \
         Set AGENT_BROWSER_CAMOUFOX_PYTHON to your python3 binary or install python3 on PATH. \
         See docs/engines/camoufox.md."
            .to_string(),
    )
}

fn spawn_stderr_drainer(
    stderr: tokio::process::ChildStderr,
    log: SharedLog,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            push_bounded(&log, line);
        }
    })
}

fn snapshot(log: &SharedLog) -> Vec<String> {
    log.lock()
        .expect("stderr log poisoned")
        .iter()
        .cloned()
        .collect()
}

fn decorate_error(message: String, dispatch: SidecarDispatch, stderr: &[String]) -> String {
    let mut out = format!("{}\n  dispatch: {}", message, dispatch.describe());
    if !stderr.is_empty() {
        out.push_str(&format!(
            "\n  sidecar stderr (last {} lines):\n    {}",
            stderr.len(),
            stderr.join("\n    ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_options_marshals_headless() {
        let opts = CamoufoxLaunchOptions {
            headless: true,
            executable_path: Some("/tmp/cf".into()),
            proxy: None,
            extra: serde_json::Map::new(),
        };
        let args = opts.to_launch_args();
        assert_eq!(args["headless"], json!(true));
        assert_eq!(args["executable_path"], json!("/tmp/cf"));
    }

    #[test]
    fn launch_options_preserves_extra() {
        let mut extra = serde_json::Map::new();
        extra.insert("humanize".into(), json!(true));
        let opts = CamoufoxLaunchOptions {
            headless: false,
            executable_path: None,
            proxy: None,
            extra,
        };
        let args = opts.to_launch_args();
        assert_eq!(args["humanize"], json!(true));
    }

    #[test]
    fn resolve_python_returns_env_var_when_set() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::env::set_var("AGENT_BROWSER_CAMOUFOX_PYTHON", tmp.path());
        let got = resolve_python_executable().unwrap();
        assert_eq!(got, tmp.path());
        std::env::remove_var("AGENT_BROWSER_CAMOUFOX_PYTHON");
    }

    #[test]
    fn resolve_python_rejects_missing_env_path() {
        std::env::set_var(
            "AGENT_BROWSER_CAMOUFOX_PYTHON",
            "/nonexistent/python3-no-such-file",
        );
        let err = resolve_python_executable().unwrap_err();
        assert!(err.contains("does not exist"));
        std::env::remove_var("AGENT_BROWSER_CAMOUFOX_PYTHON");
    }
}
