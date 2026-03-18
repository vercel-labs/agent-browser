use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::state::get_sessions_dir;

const TAB_ASSIGNMENTS_FILE_NAME: &str = "tab-assignments.json";
const TAB_ASSIGNMENTS_SCHEMA_VERSION: u32 = 1;
const MAX_WRITE_RETRIES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TabAssignment {
    pub agent_session_id: String,
    pub tab_id: String,
    pub target_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u64>,
    pub status: String,
    pub lease_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    pub assigned_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_known_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TabAssignmentsFile {
    pub version: u32,
    pub revision: u64,
    pub session_name: String,
    pub updated_at: u64,
    #[serde(default)]
    pub assignments: HashMap<String, TabAssignment>,
    #[serde(default)]
    pub tabs: HashMap<String, Value>,
}

impl TabAssignmentsFile {
    pub fn new(session_name: impl Into<String>) -> Self {
        Self {
            version: TAB_ASSIGNMENTS_SCHEMA_VERSION,
            revision: 0,
            session_name: session_name.into(),
            updated_at: current_time_millis(),
            assignments: HashMap::new(),
            tabs: HashMap::new(),
        }
    }
}

pub fn read_tab_assignments(
    session_id: &str,
    session_name: Option<&str>,
) -> Result<TabAssignmentsFile, String> {
    let path = get_tab_assignments_path(session_id, session_name);
    if !path.exists() {
        return Ok(TabAssignmentsFile::new(resolve_session_name(
            session_id,
            session_name,
        )));
    }

    let json = fs::read_to_string(&path).map_err(|e| {
        format!(
            "Failed to read tab assignments from {}: {}",
            path.display(),
            e
        )
    })?;
    let mut file: TabAssignmentsFile = serde_json::from_str(&json).map_err(|e| {
        format!(
            "Failed to parse tab assignments from {}: {}",
            path.display(),
            e
        )
    })?;
    if file.version == 0 {
        file.version = TAB_ASSIGNMENTS_SCHEMA_VERSION;
    }
    if file.session_name.is_empty() {
        file.session_name = resolve_session_name(session_id, session_name);
    }
    Ok(file)
}

pub fn write_tab_assignments(
    session_id: &str,
    session_name: Option<&str>,
    file: &TabAssignmentsFile,
) -> Result<TabAssignmentsFile, String> {
    let path = get_tab_assignments_path(session_id, session_name);
    let parent = path
        .parent()
        .ok_or_else(|| format!("Invalid tab assignments path: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|e| {
        format!(
            "Failed to create tab assignments directory {}: {}",
            parent.display(),
            e
        )
    })?;

    let expected_revision = file.revision;
    let session_name_value = resolve_session_name(session_id, session_name);

    for attempt in 0..MAX_WRITE_RETRIES {
        let current = read_tab_assignments(session_id, session_name)?;
        if current.revision != expected_revision {
            if attempt + 1 == MAX_WRITE_RETRIES {
                return Err(format!(
                    "Tab assignments revision mismatch at {}: expected {}, found {}",
                    path.display(),
                    expected_revision,
                    current.revision
                ));
            }
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        let mut next = file.clone();
        next.version = TAB_ASSIGNMENTS_SCHEMA_VERSION;
        next.session_name = session_name_value.clone();
        next.revision = expected_revision + 1;
        next.updated_at = current_time_millis();

        let temp_path = get_temp_path(&path);
        let json = serde_json::to_string_pretty(&next)
            .map_err(|e| format!("Failed to serialize tab assignments: {}", e))?;
        fs::write(&temp_path, json).map_err(|e| {
            format!(
                "Failed to write temporary tab assignments file {}: {}",
                temp_path.display(),
                e
            )
        })?;

        let latest = read_tab_assignments(session_id, session_name)?;
        if latest.revision != expected_revision {
            let _ = fs::remove_file(&temp_path);
            if attempt + 1 == MAX_WRITE_RETRIES {
                return Err(format!(
                    "Tab assignments revision changed before commit at {}: expected {}, found {}",
                    path.display(),
                    expected_revision,
                    latest.revision
                ));
            }
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        rename_atomic(&temp_path, &path)?;
        return Ok(next);
    }

    Err(format!(
        "Failed to write tab assignments after {} attempts",
        MAX_WRITE_RETRIES
    ))
}

pub fn get_session_dir(session_id: &str, session_name: Option<&str>) -> PathBuf {
    let session_component = resolve_session_name(session_id, session_name);
    get_sessions_dir().join(session_component)
}

pub fn get_tab_assignments_path(session_id: &str, session_name: Option<&str>) -> PathBuf {
    get_session_dir(session_id, session_name).join(TAB_ASSIGNMENTS_FILE_NAME)
}

fn resolve_session_name(session_id: &str, session_name: Option<&str>) -> String {
    session_name
        .filter(|name| !name.is_empty())
        .unwrap_or(session_id)
        .to_string()
}

fn current_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn get_temp_path(path: &PathBuf) -> PathBuf {
    let suffix = format!(".tmp.{}.{}", std::process::id(), current_time_millis());
    PathBuf::from(format!("{}{}", path.display(), suffix))
}

fn rename_atomic(from: &PathBuf, to: &PathBuf) -> Result<(), String> {
    #[cfg(windows)]
    {
        if to.exists() {
            fs::remove_file(to).map_err(|e| {
                format!(
                    "Failed to replace existing tab assignments file {}: {}",
                    to.display(),
                    e
                )
            })?;
        }
    }

    fs::rename(from, to).map_err(|e| {
        format!(
            "Failed to move tab assignments file from {} to {}: {}",
            from.display(),
            to.display(),
            e
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    struct HomeGuard {
        original_home: Option<String>,
        original_userprofile: Option<String>,
    }

    impl HomeGuard {
        fn set(temp_home: &str) -> Self {
            let original_home = std::env::var("HOME").ok();
            let original_userprofile = std::env::var("USERPROFILE").ok();
            std::env::set_var("HOME", temp_home);
            std::env::set_var("USERPROFILE", temp_home);
            Self {
                original_home,
                original_userprofile,
            }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            if let Some(home) = &self.original_home {
                std::env::set_var("HOME", home);
            } else {
                std::env::remove_var("HOME");
            }

            if let Some(userprofile) = &self.original_userprofile {
                std::env::set_var("USERPROFILE", userprofile);
            } else {
                std::env::remove_var("USERPROFILE");
            }
        }
    }

    fn with_temp_home() -> (PathBuf, HomeGuard) {
        let temp_home =
            std::env::temp_dir().join(format!("agent-browser-tab-assignments-{}", Uuid::new_v4()));
        fs::create_dir_all(&temp_home).unwrap();
        let guard = HomeGuard::set(temp_home.to_string_lossy().as_ref());
        (temp_home, guard)
    }

    #[test]
    fn test_read_missing_returns_default_file() {
        let (_temp_home, _guard) = with_temp_home();
        let file = read_tab_assignments("default", Some("named")).unwrap();
        assert_eq!(file.version, TAB_ASSIGNMENTS_SCHEMA_VERSION);
        assert_eq!(file.revision, 0);
        assert_eq!(file.session_name, "named");
        assert!(file.assignments.is_empty());
        assert!(file.tabs.is_empty());
    }

    #[test]
    fn test_write_then_read_roundtrip() {
        let (_temp_home, _guard) = with_temp_home();
        let mut file = read_tab_assignments("default", Some("named")).unwrap();
        file.assignments.insert(
            "tab-1".to_string(),
            TabAssignment {
                agent_session_id: "default".to_string(),
                tab_id: "tab-1".to_string(),
                target_id: "target-1".to_string(),
                window_id: Some(1),
                status: "assigned".to_string(),
                lease_version: 1,
                connection_id: Some("conn-1".to_string()),
                assigned_at: 100,
                updated_at: 100,
                last_known_url: Some("https://example.com".to_string()),
                last_known_title: Some("Example".to_string()),
            },
        );

        let written = write_tab_assignments("default", Some("named"), &file).unwrap();
        assert_eq!(written.revision, 1);

        let loaded = read_tab_assignments("default", Some("named")).unwrap();
        assert_eq!(loaded.revision, 1);
        assert_eq!(loaded.assignments.len(), 1);
        assert_eq!(loaded.assignments["tab-1"].status, "assigned");
    }

    #[test]
    fn test_write_rejects_stale_revision() {
        let (_temp_home, _guard) = with_temp_home();

        let file = read_tab_assignments("default", Some("named")).unwrap();
        let written = write_tab_assignments("default", Some("named"), &file).unwrap();

        let mut stale = file.clone();
        stale.assignments.insert(
            "tab-2".to_string(),
            TabAssignment {
                agent_session_id: "default".to_string(),
                tab_id: "tab-2".to_string(),
                target_id: "target-2".to_string(),
                window_id: None,
                status: "assigned".to_string(),
                lease_version: 2,
                connection_id: None,
                assigned_at: 200,
                updated_at: 200,
                last_known_url: None,
                last_known_title: None,
            },
        );

        let err = write_tab_assignments("default", Some("named"), &stale).unwrap_err();
        assert!(err.contains("revision mismatch") || err.contains("revision changed"));

        let loaded = read_tab_assignments("default", Some("named")).unwrap();
        assert_eq!(loaded.revision, written.revision);
        assert!(!loaded.assignments.contains_key("tab-2"));
    }
}
