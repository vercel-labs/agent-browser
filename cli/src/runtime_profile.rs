use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const AGENT_BROWSER_DIR: &str = ".agent-browser";
const LEGACY_PROFILE_DIR: &str = "profile";
const RUNTIME_PROFILES_DIR: &str = "runtime-profiles";
const USER_DATA_DIR: &str = "user-data";
const RUNTIME_STATE_FILENAME: &str = "runtime-state.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub runtime_profile: Option<String>,
    pub user_data_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeState {
    pub runtime_profile: String,
    pub user_data_dir: String,
    pub browser_pid: u32,
    pub headed: bool,
    pub launch_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devtools_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ws_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeTarget {
    pub id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    pub title: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub runtime_profile: String,
    pub user_data_dir: String,
    pub state_path: String,
    pub browser_pid: Option<u32>,
    pub browser_alive: bool,
    pub headed: Option<bool>,
    pub launch_mode: Option<String>,
    pub devtools_port: Option<u16>,
    pub ws_url: Option<String>,
    pub targets: Vec<RuntimeTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProfileSummary {
    pub runtime_profile: String,
    pub user_data_dir: String,
    pub state_path: String,
    pub configured: bool,
    pub default: bool,
    pub browser_pid: Option<u32>,
    pub browser_alive: bool,
    pub headed: Option<bool>,
    pub launch_mode: Option<String>,
    pub devtools_port: Option<u16>,
    pub ws_url: Option<String>,
}

pub fn looks_like_path(value: &str) -> bool {
    value == "."
        || value == ".."
        || value.starts_with('/')
        || value.starts_with("~/")
        || value.starts_with("./")
        || value.starts_with("../")
        || value.contains('/')
        || value.contains('\\')
        || value.contains(':')
}

pub fn validate_runtime_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Runtime profile name cannot be empty".to_string());
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!(
            "Invalid runtime profile '{}'. Must match /^[a-zA-Z0-9_-]+$/",
            name
        ));
    }
    Ok(())
}

pub fn default_runtime_profile_name() -> String {
    "default".to_string()
}

pub fn runtime_profiles_root() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(AGENT_BROWSER_DIR).join(RUNTIME_PROFILES_DIR))
        .ok_or_else(|| "Could not determine home directory".to_string())
}

pub fn runtime_profile_root(name: &str) -> Result<PathBuf, String> {
    validate_runtime_profile_name(name)?;
    Ok(runtime_profiles_root()?.join(name))
}

pub fn runtime_profile_user_data_dir(name: &str) -> Result<PathBuf, String> {
    Ok(runtime_profile_root(name)?.join(USER_DATA_DIR))
}

pub fn runtime_profile_state_path(name: &str) -> Result<PathBuf, String> {
    Ok(runtime_profile_root(name)?.join(RUNTIME_STATE_FILENAME))
}

pub fn resolve_profile(
    profile: Option<&str>,
    runtime_profile: Option<&str>,
) -> Result<ResolvedProfile, String> {
    if let Some(profile) = profile {
        if looks_like_path(profile) {
            return Ok(ResolvedProfile {
                runtime_profile: runtime_profile.map(str::to_string),
                user_data_dir: expand_tilde(profile),
            });
        }

        validate_runtime_profile_name(profile)?;
        return Ok(ResolvedProfile {
            runtime_profile: Some(profile.to_string()),
            user_data_dir: resolved_runtime_profile_user_data_dir(profile)?,
        });
    }

    let runtime_name = runtime_profile.unwrap_or("default");
    validate_runtime_profile_name(runtime_name)?;
    Ok(ResolvedProfile {
        runtime_profile: Some(runtime_name.to_string()),
        user_data_dir: resolved_runtime_profile_user_data_dir(runtime_name)?,
    })
}

fn resolved_runtime_profile_user_data_dir(runtime_profile: &str) -> Result<PathBuf, String> {
    let target = runtime_profile_user_data_dir(runtime_profile)?;
    if runtime_profile == "default" && !target.exists() {
        if let Some(home) = dirs::home_dir() {
            let legacy = home.join(AGENT_BROWSER_DIR).join(LEGACY_PROFILE_DIR);
            if legacy.exists() {
                return Ok(legacy);
            }
        }
    }
    Ok(target)
}

pub fn write_runtime_state(state: &RuntimeState) -> Result<(), String> {
    let path = runtime_profile_state_path(&state.runtime_profile)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create runtime profile dir {}: {}",
                parent.display(),
                e
            )
        })?;
    }
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| format!("Failed to serialize runtime state: {}", e))?;
    fs::write(&path, json)
        .map_err(|e| format!("Failed to write runtime state {}: {}", path.display(), e))
}

pub fn read_runtime_state(runtime_profile: &str) -> Result<Option<RuntimeState>, String> {
    let path = runtime_profile_state_path(runtime_profile)?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "Failed to read runtime state {}: {}",
                path.display(),
                e
            ))
        }
    };

    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| format!("Failed to parse runtime state {}: {}", path.display(), e))
}

pub fn clear_runtime_state(runtime_profile: &str) -> Result<(), String> {
    let path = runtime_profile_state_path(runtime_profile)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "Failed to remove runtime state {}: {}",
            path.display(),
            e
        )),
    }
}

pub fn read_devtools_port(user_data_dir: &Path) -> Option<u16> {
    for path in [
        user_data_dir.join("DevToolsActivePort"),
        user_data_dir.join("Default").join("DevToolsActivePort"),
    ] {
        let raw = fs::read_to_string(path).ok()?;
        let port = raw.lines().next()?.trim().parse::<u16>().ok()?;
        return Some(port);
    }
    None
}

/// Resolve runtime profile status, using a config-provided user-data-dir when
/// no runtime-state file exists yet for that profile.
pub fn runtime_status_with_user_data_dir(
    runtime_profile: &str,
    configured_user_data_dir: Option<&Path>,
) -> Result<RuntimeStatus, String> {
    validate_runtime_profile_name(runtime_profile)?;
    let state_path = runtime_profile_state_path(runtime_profile)?;
    let state = read_runtime_state(runtime_profile)?;
    let user_data_dir = state
        .as_ref()
        .map(|s| PathBuf::from(&s.user_data_dir))
        .or_else(|| configured_user_data_dir.map(Path::to_path_buf))
        .unwrap_or(resolved_runtime_profile_user_data_dir(runtime_profile)?);
    let browser_pid = state.as_ref().map(|s| s.browser_pid);
    let browser_alive = browser_pid.is_some_and(pid_is_running);
    let devtools_port = state
        .as_ref()
        .and_then(|s| s.devtools_port)
        .or_else(|| read_devtools_port(&user_data_dir));
    let targets = if browser_alive {
        devtools_port
            .and_then(|port| fetch_runtime_targets(port).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(RuntimeStatus {
        runtime_profile: runtime_profile.to_string(),
        user_data_dir: user_data_dir.display().to_string(),
        state_path: state_path.display().to_string(),
        browser_pid,
        browser_alive,
        headed: state.as_ref().map(|s| s.headed),
        launch_mode: state.as_ref().map(|s| s.launch_mode.clone()),
        devtools_port,
        ws_url: state.and_then(|s| s.ws_url),
        targets,
    })
}

/// Merge configured runtime profiles with any on-disk managed profiles so
/// callers can inspect the full runtime-profile namespace in one command.
pub fn list_runtime_profiles(
    configured_profiles: &[(String, Option<PathBuf>)],
    default_runtime_profile: Option<&str>,
) -> Result<Vec<RuntimeProfileSummary>, String> {
    let mut names = BTreeSet::new();

    for (name, _) in configured_profiles {
        validate_runtime_profile_name(name)?;
        names.insert(name.clone());
    }

    if let Some(default_name) = default_runtime_profile {
        validate_runtime_profile_name(default_name)?;
        names.insert(default_name.to_string());
    }

    if let Ok(root) = runtime_profiles_root() {
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if !file_type.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if validate_runtime_profile_name(&name).is_ok() {
                    names.insert(name);
                }
            }
        }
    }

    if names.is_empty() {
        names.insert(default_runtime_profile_name());
    }

    let default_name = default_runtime_profile.unwrap_or("default");
    let mut items = Vec::with_capacity(names.len());
    for name in names {
        let configured_user_data_dir = configured_profiles
            .iter()
            .find(|(profile_name, _)| profile_name == &name)
            .and_then(|(_, path)| path.clone());
        let status = runtime_status_with_user_data_dir(&name, configured_user_data_dir.as_deref())?;
        items.push(RuntimeProfileSummary {
            runtime_profile: status.runtime_profile,
            user_data_dir: status.user_data_dir,
            state_path: status.state_path,
            configured: configured_profiles
                .iter()
                .any(|(profile_name, _)| profile_name == &name),
            default: name == default_name,
            browser_pid: status.browser_pid,
            browser_alive: status.browser_alive,
            headed: status.headed,
            launch_mode: status.launch_mode,
            devtools_port: status.devtools_port,
            ws_url: status.ws_url,
        });
    }

    Ok(items)
}

fn fetch_runtime_targets(port: u16) -> Result<Vec<RuntimeTarget>, String> {
    let json = http_get_json(port, "/json/list")?;
    let list = json
        .as_array()
        .ok_or_else(|| "Invalid /json/list response".to_string())?;
    Ok(list
        .iter()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?.to_string();
            let target_type = entry.get("type")?.as_str()?.to_string();
            let title = entry
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let url = entry
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            Some(RuntimeTarget {
                id,
                target_type,
                title,
                url,
            })
        })
        .collect())
}

fn http_get_json(port: u16, path: &str) -> Result<Value, String> {
    let mut stream = TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| format!("Invalid DevTools address: {}", e))?,
        Duration::from_millis(500),
    )
    .map_err(|e| format!("Failed to connect to DevTools port {}: {}", port, e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| format!("Failed to set read timeout: {}", e))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| format!("Failed to set write timeout: {}", e))?;

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        path, port
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Failed to write HTTP request: {}", e))?;
    let _ = stream.shutdown(Shutdown::Write);

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| format!("Failed to read HTTP response: {}", e))?;
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .ok_or_else(|| "Malformed HTTP response from DevTools".to_string())?;
    serde_json::from_str(body).map_err(|e| format!("Failed to parse DevTools JSON: {}", e))
}

#[cfg(unix)]
pub fn pid_is_running(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn pid_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        return false;
    }
    unsafe { CloseHandle(handle) };
    true
}

#[cfg(not(any(unix, windows)))]
pub fn pid_is_running(_pid: u32) -> bool {
    false
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn test_looks_like_path() {
        assert!(looks_like_path("/tmp/foo"));
        assert!(looks_like_path("~/foo"));
        assert!(looks_like_path("./foo"));
        assert!(looks_like_path("../foo"));
        assert!(looks_like_path("relative/path"));
        assert!(!looks_like_path("default"));
    }

    #[test]
    fn test_validate_runtime_profile_name() {
        assert!(validate_runtime_profile_name("default").is_ok());
        assert!(validate_runtime_profile_name("work_2").is_ok());
        assert!(validate_runtime_profile_name("bad/name").is_err());
        assert!(validate_runtime_profile_name("").is_err());
    }

    #[test]
    fn test_http_get_json() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut _buf = [0u8; 1024];
                let _ = stream.read(&mut _buf);
                let body = r#"[{"id":"page-1","type":"page","title":"Example","url":"https://example.com"}]"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        let json = http_get_json(port, "/json/list").unwrap();
        assert_eq!(json[0]["id"], "page-1");
    }

    #[test]
    fn test_runtime_status_uses_configured_user_data_dir_without_state() {
        let runtime_profile = format!(
            "testcfg{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros()
        );
        let configured_user_data_dir =
            env::temp_dir().join(format!("{}-user-data", runtime_profile));

        let _ = clear_runtime_state(&runtime_profile);
        let status =
            runtime_status_with_user_data_dir(&runtime_profile, Some(&configured_user_data_dir))
                .unwrap();

        assert_eq!(
            status.user_data_dir,
            configured_user_data_dir.display().to_string()
        );
    }

    #[test]
    fn test_list_runtime_profiles_merges_config_and_disk() {
        let disk_profile = format!(
            "testdisk{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_micros()
        );
        let configured_profile = format!("{}cfg", disk_profile);
        let disk_root = runtime_profile_root(&disk_profile).unwrap();
        fs::create_dir_all(&disk_root).unwrap();

        let configured_user_data_dir =
            env::temp_dir().join(format!("{}-user-data", configured_profile));
        let items = list_runtime_profiles(
            &[(
                configured_profile.clone(),
                Some(configured_user_data_dir.clone()),
            )],
            Some(&configured_profile),
        )
        .unwrap();

        let configured = items
            .iter()
            .find(|item| item.runtime_profile == configured_profile)
            .unwrap();
        assert!(configured.configured);
        assert!(configured.default);
        assert_eq!(
            configured.user_data_dir,
            configured_user_data_dir.display().to_string()
        );

        let disk = items
            .iter()
            .find(|item| item.runtime_profile == disk_profile)
            .unwrap();
        assert!(!disk.configured);

        let _ = fs::remove_dir_all(&disk_root);
    }
}
