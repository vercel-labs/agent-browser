//! Stateless helpers shared across doctor submodules.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde_json::Value;

pub(super) fn is_writable_dir(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| !m.permissions().readonly())
        .unwrap_or(false)
}

pub(super) fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(unix)]
pub(super) fn disk_free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::PathBuf;

    // Walk up to the first existing ancestor (for fresh installs where the
    // state dir hasn't been created yet).
    let mut probe: PathBuf = path.to_path_buf();
    while !probe.exists() {
        match probe.parent() {
            Some(p) => probe = p.to_path_buf(),
            None => return None,
        }
    }
    let c_path = CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize)
}

#[cfg(windows)]
pub(super) fn disk_free_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(not(any(unix, windows)))]
pub(super) fn disk_free_bytes(_path: &Path) -> Option<u64> {
    None
}

pub(super) fn which_exists(name: &str) -> bool {
    let probe = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    std::process::Command::new(probe)
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(super) fn parse_json_file(path: &Path) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| format!("read failed: {}", e))?;
    serde_json::from_str::<Value>(&content).map_err(|e| format!("invalid JSON: {}", e))?;
    Ok(())
}

/// Generate a unique `doctor-<pid>-<micros>-<sequence>` id for JSON command envelopes.
pub(super) fn new_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    format!(
        "doctor-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_micros())
            .unwrap_or(0),
        sequence
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_human_size_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.0 GB");
        assert_eq!(human_size(1_500_000), "1.4 MB");
    }

    #[test]
    fn test_disk_free_walks_up_to_existing_ancestor() {
        let dir = TempDir::new().unwrap();
        let nested = dir.path().join("a/b/c/d");
        let bytes = disk_free_bytes(&nested);
        if cfg!(unix) {
            assert!(bytes.is_some());
            assert!(bytes.unwrap() > 0);
        }
    }

    #[test]
    fn test_is_writable_dir_matches_metadata() {
        let dir = TempDir::new().unwrap();
        assert!(is_writable_dir(dir.path()));

        let missing = dir.path().join("does-not-exist");
        assert!(!is_writable_dir(&missing));
    }

    #[test]
    fn test_which_exists_matches_common_binaries() {
        // `sh` exists on every unix; `cmd` exists on windows.
        let probe = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        assert!(which_exists(probe));
        assert!(!which_exists(
            "agent-browser-this-does-not-exist-please-dont-install-it"
        ));
    }

    #[test]
    fn test_parse_json_file_valid_and_invalid() {
        let dir = TempDir::new().unwrap();
        let valid = dir.path().join("ok.json");
        fs::write(&valid, r#"{"k": 1}"#).unwrap();
        assert!(parse_json_file(&valid).is_ok());

        let invalid = dir.path().join("bad.json");
        fs::write(&invalid, "{not json}").unwrap();
        let err = parse_json_file(&invalid).unwrap_err();
        assert!(err.contains("invalid JSON"));

        let missing = dir.path().join("nope.json");
        let err = parse_json_file(&missing).unwrap_err();
        assert!(err.contains("read failed"));
    }

    #[test]
    fn test_parse_json_file_accepts_arrays() {
        // The config parser rejects arrays at the Config type level, but
        // doctor only checks syntactic JSON validity so it should accept
        // both arrays and objects.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("arr.json");
        fs::write(&path, r#"[1, 2, 3]"#).unwrap();
        assert!(parse_json_file(&path).is_ok());
    }

    #[test]
    fn test_new_id_is_unique_per_call() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert!(a.starts_with("doctor-"));
    }
}
