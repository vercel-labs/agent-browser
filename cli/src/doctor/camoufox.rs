//! Probe the Camoufox engine availability.
//!
//! Three checks, reported independently so the user can tell exactly which
//! step is missing on a partial install:
//!   1. A Python 3 runtime (either `AGENT_BROWSER_CAMOUFOX_PYTHON` or
//!      `python3` on PATH).
//!   2. The `camoufox` Python package imports cleanly.
//!   3. The Camoufox browser binary has been fetched.
//!
//! All failures are non-fatal: we report the distinct reason as `Info` so
//! `doctor` continues and users can still confidently use `--engine chrome`
//! / `--engine lightpanda`. Dependent checks short-circuit: if Python is
//! missing we skip the package and binary probes, since running them would
//! fail for an unrelated reason.

use std::env;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use super::{Check, Status};

const CATEGORY: &str = "Camoufox";
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn check(checks: &mut Vec<Check>) {
    let python = match resolve_python() {
        Some(p) => p,
        None => {
            push_not_available(
                checks,
                "camoufox.python",
                "python3 not found",
                "install python3 and `pip install camoufox`, or set AGENT_BROWSER_CAMOUFOX_PYTHON",
            );
            return;
        }
    };

    match probe_python_version(&python) {
        PythonProbe::Ok(version_label) => checks.push(Check::new(
            "camoufox.python",
            CATEGORY,
            Status::Pass,
            format!("python3 at {} ({})", python, version_label),
        )),
        PythonProbe::Unusable(reason) => {
            push_not_available(
                checks,
                "camoufox.python",
                &format!("python3 at {} is not runnable ({})", python, reason),
                "install python3 and `pip install camoufox`, or set AGENT_BROWSER_CAMOUFOX_PYTHON",
            );
            return;
        }
    }

    match import_camoufox(&python) {
        ProbeOutcome::Ok(detail) => checks.push(Check::new(
            "camoufox.package",
            CATEGORY,
            Status::Pass,
            format!("camoufox package importable{}", detail),
        )),
        ProbeOutcome::Missing(reason) => {
            push_not_available(checks, "camoufox.package", &reason, "pip install camoufox");
            return;
        }
    }

    match camoufox_binary_path(&python) {
        ProbeOutcome::Ok(path) => checks.push(Check::new(
            "camoufox.binary",
            CATEGORY,
            Status::Pass,
            format!("camoufox browser binary at {}", path),
        )),
        ProbeOutcome::Missing(reason) => {
            push_not_available(checks, "camoufox.binary", &reason, "python3 -m camoufox fetch");
        }
    }
}

fn push_not_available(checks: &mut Vec<Check>, id: &str, reason: &str, fix: &str) {
    checks.push(
        Check::new(
            id.to_string(),
            CATEGORY,
            Status::Info,
            format!("camoufox: not available (reason: {})", reason),
        )
        .with_fix(fix.to_string()),
    );
}

enum PythonProbe {
    /// Version string from `<python> --version`, already trimmed. Empty is
    /// allowed and surfaced as `(version unknown)`.
    Ok(String),
    /// Spawn failed, non-zero exit, or probe timed out. The caller treats
    /// this as equivalent to "python not found" for `doctor` purposes.
    Unusable(String),
}

enum ProbeOutcome {
    Ok(String),
    Missing(String),
}

fn resolve_python() -> Option<String> {
    if let Ok(explicit) = env::var("AGENT_BROWSER_CAMOUFOX_PYTHON") {
        if !explicit.trim().is_empty() {
            return Some(explicit);
        }
    }
    if super::helpers::which_exists("python3") {
        return Some("python3".to_string());
    }
    None
}

fn probe_python_version(python: &str) -> PythonProbe {
    let out = match run_with_timeout(Command::new(python).arg("--version")) {
        RunOutcome::Ok(o) => o,
        RunOutcome::SpawnFailed(e) => {
            return PythonProbe::Unusable(format!("spawn failed: {}", e));
        }
        RunOutcome::Timeout => {
            return PythonProbe::Unusable("probe timed out".to_string());
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let msg = first_line(&stderr).unwrap_or_else(|| format!("exit {}", exit_code_label(&out)));
        return PythonProbe::Unusable(msg);
    }
    // Python writes `--version` to stdout on 3.4+ and stderr on older;
    // prefer stdout, fall back to stderr.
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !stdout.is_empty() {
        return PythonProbe::Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if stderr.is_empty() {
        PythonProbe::Ok("version unknown".to_string())
    } else {
        PythonProbe::Ok(stderr)
    }
}

fn import_camoufox(python: &str) -> ProbeOutcome {
    // `camoufox.__version__` is a submodule (not a string), so use
    // importlib.metadata to fetch the installed version instead.
    let probe = r#"
import sys, importlib
importlib.import_module('camoufox')
try:
    from importlib.metadata import version
    print(version('camoufox'), end='')
except Exception:
    print('', end='')
"#;
    let out = match run_with_timeout(Command::new(python).arg("-c").arg(probe)) {
        RunOutcome::Ok(o) => o,
        RunOutcome::SpawnFailed(e) => {
            return ProbeOutcome::Missing(format!("python probe spawn failed: {}", e));
        }
        RunOutcome::Timeout => {
            return ProbeOutcome::Missing("camoufox import probe timed out".to_string());
        }
    };
    if out.status.success() {
        let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let detail = if version.is_empty() {
            String::new()
        } else {
            format!(" (version {})", version)
        };
        return ProbeOutcome::Ok(detail);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr.contains("ModuleNotFoundError") || stderr.contains("No module named 'camoufox'") {
        ProbeOutcome::Missing("camoufox package not installed".to_string())
    } else {
        ProbeOutcome::Missing(format!(
            "camoufox import failed: {}",
            first_line(&stderr).unwrap_or_else(|| "unknown error".to_string())
        ))
    }
}

fn camoufox_binary_path(python: &str) -> ProbeOutcome {
    // Prefer the package's own path resolver so we don't hardcode the cache
    // layout, then fall back to the canonical linux cache dir so an
    // upstream rename to `pkgman` / etc. can't make a working install look
    // broken.
    let probe = r#"
import sys
from pathlib import Path
try:
    from camoufox.pkgman import installed_verstr, get_path
    ver = installed_verstr()
    if not ver:
        print('__AB_NOT_FETCHED__', end='')
        sys.exit(0)
    base = Path(get_path('cache'))
    candidates = [base, base / ver]
    for c in candidates:
        if c.exists():
            print(str(c), end='')
            sys.exit(0)
    print('__AB_NOT_FETCHED__', end='')
except Exception as exc:
    home = Path.home()
    fallback = home / '.cache' / 'camoufox'
    if fallback.exists() and any(fallback.iterdir()):
        print(str(fallback), end='')
        sys.exit(0)
    sys.stderr.write(f'{type(exc).__name__}: {exc}')
    sys.exit(2)
"#;

    let out = match run_with_timeout(Command::new(python).arg("-c").arg(probe)) {
        RunOutcome::Ok(o) => o,
        RunOutcome::SpawnFailed(e) => {
            return ProbeOutcome::Missing(format!("python probe spawn failed: {}", e));
        }
        RunOutcome::Timeout => {
            return ProbeOutcome::Missing("camoufox path probe timed out".to_string());
        }
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return ProbeOutcome::Missing(format!(
            "camoufox path probe failed: {}",
            first_line(&stderr).unwrap_or_else(|| "unknown error".to_string())
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if stdout.is_empty() || stdout == "__AB_NOT_FETCHED__" {
        return ProbeOutcome::Missing(
            "camoufox browser binary not fetched (run `python3 -m camoufox fetch`)".to_string(),
        );
    }
    ProbeOutcome::Ok(stdout)
}

enum RunOutcome {
    Ok(Output),
    SpawnFailed(String),
    Timeout,
}

fn run_with_timeout(cmd: &mut Command) -> RunOutcome {
    // A deadlocked child (e.g. probe hanging on import of a broken module)
    // must not hang the whole `doctor` run. Spawn, then poll with a wall
    // clock; if the deadline fires, kill and return `Timeout`.
    let mut child = match cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return RunOutcome::SpawnFailed(e.to_string()),
    };

    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => {
                return match child.wait_with_output() {
                    Ok(out) => RunOutcome::Ok(out),
                    Err(e) => RunOutcome::SpawnFailed(e.to_string()),
                };
            }
            Ok(None) => {
                if start.elapsed() >= PROBE_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return RunOutcome::Timeout;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return RunOutcome::SpawnFailed(e.to_string()),
        }
    }
}

fn exit_code_label(out: &Output) -> String {
    match out.status.code() {
        Some(c) => c.to_string(),
        None => "signal".to_string(),
    }
}

fn first_line(s: &str) -> Option<String> {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_line_skips_blank_leading_lines() {
        assert_eq!(first_line(""), None);
        assert_eq!(first_line("\n\n"), None);
        assert_eq!(first_line("hello"), Some("hello".to_string()));
        assert_eq!(first_line("\n  first\nsecond"), Some("first".to_string()));
    }
}
