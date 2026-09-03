//! Check the Chrome install: binary path, version, cache dirs, user-data
//! dir, and the optional lightpanda engine.

use std::env;
use std::path::{Path, PathBuf};

use super::helpers::which_exists;
use super::{Check, Status};

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Chrome";

    let chrome = crate::native::cdp::chrome::find_chrome();
    match chrome {
        Some(path) => {
            let label = path.display().to_string();
            match query_chrome_version(&path) {
                Some(version) => checks.push(Check::new(
                    "chrome.installed",
                    category,
                    Status::Pass,
                    format!("{} at {}", version, label),
                )),
                None => checks.push(Check::new(
                    "chrome.installed",
                    category,
                    Status::Pass,
                    format!("Chrome at {} (version unknown)", label),
                )),
            }
        }
        None => checks.push(
            Check::new(
                "chrome.installed",
                category,
                Status::Fail,
                "No Chrome binary found",
            )
            .with_fix("agent-browser install"),
        ),
    }

    let cache_dir = crate::install::get_browsers_dir();
    if cache_dir.exists() {
        checks.push(Check::new(
            "chrome.cache_dir",
            category,
            Status::Info,
            format!("Cache dir {}", cache_dir.display()),
        ));
    }

    if let Some(puppeteer_dir) = puppeteer_cache_dir() {
        if puppeteer_dir.exists() {
            checks.push(Check::new(
                "chrome.puppeteer_cache",
                category,
                Status::Info,
                format!(
                    "Puppeteer cache also present: {} (will be used as a fallback)",
                    puppeteer_dir.display()
                ),
            ));
        }
    }

    if let Some(user_data_dir) = crate::native::cdp::chrome::find_chrome_user_data_dir() {
        let profiles = crate::native::cdp::chrome::list_chrome_profiles(&user_data_dir);
        let count = profiles.len();
        let dir_label = user_data_dir.display().to_string();
        if count == 0 {
            checks.push(Check::new(
                "chrome.user_data_dir",
                category,
                Status::Info,
                format!(
                    "Chrome user data dir found ({}), no profiles parsed",
                    dir_label
                ),
            ));
        } else {
            checks.push(Check::new(
                "chrome.user_data_dir",
                category,
                Status::Info,
                format!("{} Chrome profile(s) at {}", count, dir_label),
            ));
        }
    }

    if let Ok(engine) = env::var("AGENT_BROWSER_ENGINE") {
        if engine == "lightpanda" {
            // Best-effort PATH lookup; absence is FAIL only when the user
            // explicitly opted into the lightpanda engine.
            if which_exists("lightpanda") {
                checks.push(Check::new(
                    "chrome.engine_lightpanda",
                    category,
                    Status::Pass,
                    "Lightpanda binary on PATH",
                ));
            } else {
                checks.push(
                    Check::new(
                        "chrome.engine_lightpanda",
                        category,
                        Status::Fail,
                        "AGENT_BROWSER_ENGINE=lightpanda but no lightpanda binary on PATH",
                    )
                    .with_fix("install lightpanda or unset AGENT_BROWSER_ENGINE"),
                );
            }
        }
    }
}

/// Longest we wait for a browser to report its own version before giving up.
#[cfg(not(windows))]
const VERSION_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Read the version from the binary's own resource table. Windows Chrome does
/// not write a version to a redirected stdout, and `chrome.exe --version`
/// leaves a process tree whose inherited stdout handle never closes, so the
/// caller would wait forever.
#[cfg(windows)]
fn query_chrome_version(path: &Path) -> Option<String> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, VS_FIXEDFILEINFO,
    };

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide` is a NUL-terminated path and stays alive for the call.
    let size = unsafe { GetFileVersionInfoSizeW(wide.as_ptr(), std::ptr::null_mut()) };
    if size == 0 {
        return None;
    }

    let mut buffer = vec![0u8; size as usize];
    // SAFETY: `buffer` is `size` bytes, the size the call above asked for.
    let ok = unsafe {
        GetFileVersionInfoW(
            wide.as_ptr(),
            0,
            size,
            buffer.as_mut_ptr() as *mut std::ffi::c_void,
        )
    };
    if ok == 0 {
        return None;
    }

    let root: Vec<u16> = "\\".encode_utf16().chain(std::iter::once(0)).collect();
    let mut info: *mut std::ffi::c_void = std::ptr::null_mut();
    let mut info_len: u32 = 0;
    // SAFETY: `buffer` holds a version-info block produced by the call above.
    let ok = unsafe {
        VerQueryValueW(
            buffer.as_ptr() as *const std::ffi::c_void,
            root.as_ptr(),
            &mut info,
            &mut info_len,
        )
    };
    if ok == 0 || info.is_null() || (info_len as usize) < std::mem::size_of::<VS_FIXEDFILEINFO>() {
        return None;
    }

    // SAFETY: VerQueryValue reported at least a VS_FIXEDFILEINFO at `info`.
    let fixed = unsafe { &*(info as *const VS_FIXEDFILEINFO) };
    Some(format!(
        "{}.{}.{}.{}",
        fixed.dwFileVersionMS >> 16,
        fixed.dwFileVersionMS & 0xFFFF,
        fixed.dwFileVersionLS >> 16,
        fixed.dwFileVersionLS & 0xFFFF,
    ))
}

#[cfg(not(windows))]
fn query_chrome_version(path: &Path) -> Option<String> {
    let mut cmd = std::process::Command::new(path);
    cmd.arg("--version");
    run_version_command(cmd, VERSION_QUERY_TIMEOUT)
}

/// Run a version command, giving up after `timeout` and killing the child so
/// doctor's liveness never depends on the browser exiting.
#[cfg(not(windows))]
fn run_version_command(
    mut cmd: std::process::Command,
    timeout: std::time::Duration,
) -> Option<String> {
    use std::io::Read;

    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };

    if !status.success() {
        return None;
    }

    let mut stdout = String::new();
    child.stdout.take()?.read_to_string(&mut stdout).ok()?;
    let s = stdout.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

pub(super) fn puppeteer_cache_dir() -> Option<PathBuf> {
    if let Ok(p) = env::var("PUPPETEER_CACHE_DIR") {
        return Some(PathBuf::from(p));
    }
    dirs::home_dir().map(|h| h.join(".cache").join("puppeteer"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_puppeteer_cache_dir_returns_sensible_default() {
        // When PUPPETEER_CACHE_DIR is unset, we fall back to
        // ~/.cache/puppeteer. Mutating env vars here would race with other
        // tests, so just verify the fallback path is shaped correctly.
        if env::var("PUPPETEER_CACHE_DIR").is_err() {
            let dir = puppeteer_cache_dir().expect("home dir should resolve in tests");
            let s = dir.to_string_lossy();
            assert!(s.contains(".cache"));
            assert!(s.ends_with("puppeteer"));
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn version_query_gives_up_on_a_command_that_never_exits() {
        use std::time::{Duration, Instant};

        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 60");

        let started = Instant::now();
        let result = run_version_command(cmd, Duration::from_millis(300));
        let elapsed = started.elapsed();

        assert!(result.is_none());
        assert!(
            elapsed < Duration::from_secs(5),
            "took {:?}, so the deadline did not fire",
            elapsed
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn version_query_returns_trimmed_stdout() {
        let mut cmd = std::process::Command::new("/bin/sh");
        cmd.arg("-c").arg("echo '  Google Chrome 150.0.0.0  '");

        assert_eq!(
            run_version_command(cmd, std::time::Duration::from_secs(5)).as_deref(),
            Some("Google Chrome 150.0.0.0")
        );
    }
}
