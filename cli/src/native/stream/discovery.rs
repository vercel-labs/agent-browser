use serde_json::{json, Value};
use std::path::Path;

use crate::connection::{get_socket_dir, is_pid_alive};
use crate::session_files::SessionFile;

pub(super) fn discover_sessions() -> String {
    let dir = get_socket_dir();
    let mut sessions = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(session) = name_str.strip_suffix(".stream") {
                if let Ok(port_str) = std::fs::read_to_string(entry.path()) {
                    if let Ok(port) = port_str.trim().parse::<u16>() {
                        let pid_path = SessionFile::Pid.path_in(&dir, session);
                        if is_process_alive(&pid_path) {
                            let engine_path = SessionFile::Engine.path_in(&dir, session);
                            let engine = std::fs::read_to_string(&engine_path)
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or_else(|| "chrome".to_string());

                            let provider_path = SessionFile::Provider.path_in(&dir, session);
                            let provider = std::fs::read_to_string(&provider_path)
                                .ok()
                                .filter(|s| !s.trim().is_empty());

                            let extensions = read_extensions_metadata(&dir, session);

                            let mut entry = json!({
                                "session": session,
                                "port": port,
                                "engine": engine.trim(),
                            });
                            if let Some(ref p) = provider {
                                entry["provider"] = json!(p.trim());
                            }
                            if !extensions.is_empty() {
                                entry["extensions"] = json!(extensions);
                            }
                            sessions.push(entry);
                        } else {
                            let _ = std::fs::remove_file(entry.path());
                        }
                    }
                }
            }
        }
    }

    serde_json::to_string(&sessions).unwrap_or_else(|_| "[]".to_string())
}

fn read_extensions_metadata(dir: &std::path::Path, session: &str) -> Vec<Value> {
    let ext_path = SessionFile::Extensions.path_in(dir, session);
    let ext_str = match std::fs::read_to_string(&ext_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    ext_str
        .split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .filter_map(|path| {
            let manifest_path = std::path::Path::new(path).join("manifest.json");
            let manifest_str = std::fs::read_to_string(&manifest_path).ok()?;
            let manifest: Value = serde_json::from_str(&manifest_str).ok()?;

            let name = manifest
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string();
            let version = manifest
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let description = manifest
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let mut ext = json!({
                "name": name,
                "version": version,
                "path": path,
            });
            if let Some(desc) = description {
                ext["description"] = json!(desc);
            }
            Some(ext)
        })
        .collect()
}

fn is_process_alive(pid_path: &Path) -> bool {
    let pid_str = match std::fs::read_to_string(pid_path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => return false,
    };
    // PID zero and negative values have process-group semantics on Unix.
    // A PID file must identify one process, not a group selected by a cast.
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    if i32::try_from(pid).is_err() {
        return false;
    }
    is_pid_alive(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_liveness_rejects_invalid_pid_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.pid");
        assert!(!is_process_alive(&path));
        for contents in ["", "invalid", "-1", "0"] {
            std::fs::write(&path, contents).unwrap();
            assert!(!is_process_alive(&path), "PID file: {contents:?}");
        }
        #[cfg(unix)]
        for pid in [i32::MAX as u32 + 1, u32::MAX] {
            std::fs::write(&path, pid.to_string()).unwrap();
            assert!(!is_process_alive(&path), "PID file: {pid}");
        }
    }

    #[test]
    fn process_liveness_detects_running_and_exited_processes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.pid");
        std::fs::write(&path, format!("{}\n", std::process::id())).unwrap();
        assert!(is_process_alive(&path));

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        std::fs::write(&path, pid.to_string()).unwrap();
        assert!(!is_process_alive(&path));
    }
}
