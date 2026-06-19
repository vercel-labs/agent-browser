//! Check the Chrome install: binary path, version, cache dirs, user-data
//! dir, and the optional lightpanda engine.

use std::env;
use std::path::{Path, PathBuf};

use super::helpers::which_exists;
use super::{Check, Status};

pub(super) fn check(checks: &mut Vec<Check>, executable_path: Option<&str>) {
    let category = "Chrome";

    let configured_path = executable_path.filter(|path| !path.trim().is_empty());
    let using_configured_path = configured_path.is_some();
    let chrome = configured_path
        .map(PathBuf::from)
        .or_else(crate::native::cdp::chrome::find_chrome);

    match chrome {
        Some(path) => {
            let label = path.display().to_string();
            if !path.exists() {
                checks.push(
                    Check::new(
                        "chrome.installed",
                        category,
                        Status::Fail,
                        format!("Configured browser executable not found at {}", label),
                    )
                    .with_fix("update executablePath or AGENT_BROWSER_EXECUTABLE_PATH"),
                );
            } else if let Some(version) = query_chrome_version(&path) {
                checks.push(Check::new(
                    "chrome.installed",
                    category,
                    Status::Pass,
                    format!("{} at {}", version, label),
                ));
            } else {
                let status = if using_configured_path {
                    Status::Warn
                } else {
                    Status::Pass
                };
                let mut check = Check::new(
                    "chrome.installed",
                    category,
                    status,
                    if using_configured_path {
                        format!("Browser executable at {} (version unknown)", label)
                    } else {
                        format!("Chrome at {} (version unknown)", label)
                    },
                );
                if using_configured_path {
                    check = check
                        .with_fix("verify executablePath points to a runnable browser executable");
                }
                checks.push(check);
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

fn query_chrome_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
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
    use std::io::Write;
    use tempfile::TempDir;

    fn known_executable_path() -> &'static str {
        if cfg!(target_os = "windows") {
            "C:\\Windows\\System32\\cmd.exe"
        } else {
            "/bin/sh"
        }
    }

    #[test]
    fn check_reports_configured_executable_path() {
        let executable = known_executable_path();
        let mut checks = Vec::new();

        check(&mut checks, Some(executable));

        let chrome_check = checks
            .iter()
            .find(|check| check.id == "chrome.installed")
            .expect("chrome check should be present");
        assert!(
            matches!(chrome_check.status, Status::Pass | Status::Warn),
            "configured executable path should not fail: {}",
            chrome_check.message
        );
        assert!(
            chrome_check.message.contains(executable),
            "configured executable path should appear in message: {}",
            chrome_check.message
        );
    }

    #[test]
    fn check_warns_when_configured_executable_version_is_unknown() {
        let dir = TempDir::new().expect("temp dir should be created");
        let executable = dir.path().join("not-a-browser");
        let mut file = std::fs::File::create(&executable).expect("file should be created");
        writeln!(file, "not executable").expect("file should be written");
        let mut checks = Vec::new();

        check(&mut checks, executable.to_str());

        let chrome_check = checks
            .iter()
            .find(|check| check.id == "chrome.installed")
            .expect("chrome check should be present");
        assert_eq!(chrome_check.status, Status::Warn);
        assert!(
            chrome_check.message.contains("version unknown"),
            "unknown version should produce a warning message: {}",
            chrome_check.message
        );
        assert!(
            chrome_check.fix.is_some(),
            "unknown configured executable should include a fix"
        );
    }

    #[test]
    fn check_fails_when_configured_executable_path_is_missing() {
        let missing = std::env::temp_dir().join(format!(
            "agent-browser-missing-browser-{}",
            std::process::id()
        ));
        let mut checks = Vec::new();

        check(&mut checks, missing.to_str());

        let chrome_check = checks
            .iter()
            .find(|check| check.id == "chrome.installed")
            .expect("chrome check should be present");
        assert_eq!(chrome_check.status, Status::Fail);
        assert!(
            chrome_check
                .message
                .contains("Configured browser executable"),
            "missing executable should produce a targeted message: {}",
            chrome_check.message
        );
    }

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
}
