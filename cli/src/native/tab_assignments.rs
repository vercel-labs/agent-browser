use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;

#[cfg(unix)]
use std::os::fd::AsRawFd;

use super::state::get_sessions_dir;

const TAB_ASSIGNMENTS_FILE_NAME: &str = "tab-assignments.json";
const TAB_ASSIGNMENTS_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TabAssignmentStatus {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "assigned")]
    Assigned,
    #[serde(rename = "attached")]
    Attached,
    #[serde(rename = "detached")]
    Detached,
    #[serde(rename = "closed")]
    Closed,
    #[serde(rename = "orphaned")]
    Orphaned,
}

impl TabAssignmentStatus {
    pub fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "open" => Ok(Self::Open),
            "assigned" => Ok(Self::Assigned),
            "attached" => Ok(Self::Attached),
            "detached" => Ok(Self::Detached),
            "closed" => Ok(Self::Closed),
            "orphaned" => Ok(Self::Orphaned),
            other => Err(format!("Invalid tab assignment status '{}'", other)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TabAssignment {
    pub agent_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<usize>,
    pub status: TabAssignmentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    pub assigned_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_known_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_known_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_ordinal: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TabInfo {
    pub tab_id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    pub status: TabAssignmentStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_known_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_known_title: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TabAssignmentsProfile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_data_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_directory: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TabAssignmentsTransport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relay_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TabAssignmentsFile {
    pub version: u8,
    pub revision: u64,
    pub session_name: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<TabAssignmentsProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<TabAssignmentsTransport>,
    #[serde(default)]
    pub assignments: HashMap<String, TabAssignment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs: Option<HashMap<String, TabInfo>>,
}

impl TabAssignmentsFile {
    pub fn new(session_name: impl Into<String>) -> Self {
        Self {
            version: TAB_ASSIGNMENTS_SCHEMA_VERSION,
            revision: 0,
            session_name: session_name.into(),
            updated_at: current_timestamp(),
            profile: None,
            transport: None,
            assignments: HashMap::new(),
            tabs: None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonEncryptedPayload {
    version: u8,
    encrypted: bool,
    iv: String,
    auth_tag: String,
    data: String,
}

pub fn current_timestamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

pub fn read_tab_assignments(
    session_id: &str,
    session_name: Option<&str>,
) -> Result<TabAssignmentsFile, String> {
    read_tab_assignments_in(&get_sessions_dir(), session_id, session_name)
}

pub fn write_tab_assignments(
    session_id: &str,
    session_name: Option<&str>,
    file: &TabAssignmentsFile,
) -> Result<TabAssignmentsFile, String> {
    write_tab_assignments_in(&get_sessions_dir(), session_id, session_name, file)
}

pub fn get_session_dir(session_id: &str, session_name: Option<&str>) -> Result<PathBuf, String> {
    get_session_dir_in(&get_sessions_dir(), session_id, session_name)
}

pub fn get_tab_assignments_path(
    session_id: &str,
    session_name: Option<&str>,
) -> Result<PathBuf, String> {
    get_tab_assignments_path_in(&get_sessions_dir(), session_id, session_name)
}

fn read_tab_assignments_in(
    root: &Path,
    session_id: &str,
    session_name: Option<&str>,
) -> Result<TabAssignmentsFile, String> {
    let resolved_session_name = resolve_session_name(session_id, session_name)?;
    let path = get_tab_assignments_path_in(root, session_id, session_name)?;
    if !path.exists() {
        return Ok(TabAssignmentsFile::new(resolved_session_name));
    }

    let json = read_json_state_file(&path)?;
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
        file.session_name = resolved_session_name;
    }
    Ok(file)
}

fn write_tab_assignments_in(
    root: &Path,
    session_id: &str,
    session_name: Option<&str>,
    file: &TabAssignmentsFile,
) -> Result<TabAssignmentsFile, String> {
    let path = get_tab_assignments_path_in(root, session_id, session_name)?;
    let lock_path = path.with_extension("json.lock");
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

    with_exclusive_lock(&lock_path, || {
        let current = read_tab_assignments_in(root, session_id, session_name)?;
        if current.revision != file.revision {
            return Err(format!(
                "Tab assignments revision mismatch at {}: expected {}, found {}",
                path.display(),
                file.revision,
                current.revision
            ));
        }

        let mut next = file.clone();
        next.version = TAB_ASSIGNMENTS_SCHEMA_VERSION;
        next.session_name = resolve_session_name(session_id, session_name)?;
        if next.updated_at.is_empty() {
            next.updated_at = current_timestamp();
        }
        next.revision = current.revision + 1;

        let json = serde_json::to_string_pretty(&next)
            .map_err(|e| format!("Failed to serialize tab assignments: {}", e))?;
        let temp_path = get_temp_path(&path);
        write_json_state_file(&temp_path, &json)?;
        rename_atomic(&temp_path, &path)?;
        Ok(next)
    })
}

fn get_session_dir_in(
    root: &Path,
    session_id: &str,
    session_name: Option<&str>,
) -> Result<PathBuf, String> {
    let session_component = resolve_session_name(session_id, session_name)?;
    Ok(root.join(session_component))
}

fn get_tab_assignments_path_in(
    root: &Path,
    session_id: &str,
    session_name: Option<&str>,
) -> Result<PathBuf, String> {
    Ok(get_session_dir_in(root, session_id, session_name)?.join(TAB_ASSIGNMENTS_FILE_NAME))
}

fn resolve_session_name(session_id: &str, session_name: Option<&str>) -> Result<String, String> {
    let candidate = session_name
        .filter(|name| !name.is_empty())
        .unwrap_or(session_id);
    validate_session_name(candidate)?;
    Ok(candidate.to_string())
}

fn validate_session_name(name: &str) -> Result<(), String> {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "Invalid session name '{}': expected only [a-zA-Z0-9_-]",
            name
        ))
    }
}

fn get_temp_path(path: &Path) -> PathBuf {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(TAB_ASSIGNMENTS_FILE_NAME);
    path.with_file_name(format!(".{}.tmp-{}", filename, Uuid::new_v4()))
}

fn rename_atomic(from: &Path, to: &Path) -> Result<(), String> {
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

fn read_json_state_file(path: &Path) -> Result<String, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read state file {}: {}", path.display(), e))?;
    let parsed: Value =
        serde_json::from_str(&content).map_err(|e| format!("Invalid JSON state file: {}", e))?;

    if let Ok(payload) = serde_json::from_value::<JsonEncryptedPayload>(parsed.clone()) {
        if payload.encrypted {
            let key = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").map_err(|_| {
                "Encrypted state file requires AGENT_BROWSER_ENCRYPTION_KEY".to_string()
            })?;
            return decrypt_json_payload(&payload, &key);
        }
    }

    Ok(content)
}

fn write_json_state_file(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create state directory {}: {}",
                parent.display(),
                e
            )
        })?;
    }

    let serialized = if let Ok(key) = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY") {
        let payload = encrypt_json_payload(content, &key)?;
        serde_json::to_string_pretty(&payload)
            .map_err(|e| format!("Failed to serialize encrypted payload: {}", e))?
    } else {
        content.to_string()
    };

    fs::write(path, serialized)
        .map_err(|e| format!("Failed to write state file {}: {}", path.display(), e))
}

fn with_exclusive_lock<T, F>(lock_path: &Path, action: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String>,
{
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create lock directory {}: {}",
                parent.display(),
                e
            )
        })?;
    }

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| format!("Failed to open lock file {}: {}", lock_path.display(), e))?;
    let _guard = FileLockGuard::acquire(&file, lock_path)?;
    action()
}

struct FileLockGuard<'a> {
    #[cfg(unix)]
    file: &'a std::fs::File,
}

impl<'a> FileLockGuard<'a> {
    fn acquire(file: &'a std::fs::File, path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(format!(
                    "Failed to lock tab assignments file {}: {}",
                    path.display(),
                    std::io::Error::last_os_error()
                ));
            }
            Ok(Self { file })
        }

        #[cfg(not(unix))]
        {
            let _ = (file, path);
            Ok(Self {})
        }
    }
}

impl Drop for FileLockGuard<'_> {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn encrypt_json_payload(plaintext: &str, key_str: &str) -> Result<JsonEncryptedPayload, String> {
    let key_bytes = parse_hex_key(key_str)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("Invalid key: {}", e))?;

    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| format!("Failed to generate nonce: {}", e))?;
    let ciphertext = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|e| format!("Encryption failed: {}", e))?;

    if ciphertext.len() < 16 {
        return Err("Ciphertext too short".to_string());
    }

    let split_at = ciphertext.len() - 16;
    let (data, auth_tag) = ciphertext.split_at(split_at);
    Ok(JsonEncryptedPayload {
        version: 1,
        encrypted: true,
        iv: STANDARD.encode(nonce),
        auth_tag: STANDARD.encode(auth_tag),
        data: STANDARD.encode(data),
    })
}

fn decrypt_json_payload(payload: &JsonEncryptedPayload, key_str: &str) -> Result<String, String> {
    let key_bytes = parse_hex_key(key_str)?;
    let cipher =
        Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("Invalid key: {}", e))?;

    let nonce = STANDARD
        .decode(&payload.iv)
        .map_err(|e| format!("Invalid IV encoding: {}", e))?;
    let auth_tag = STANDARD
        .decode(&payload.auth_tag)
        .map_err(|e| format!("Invalid authTag encoding: {}", e))?;
    let data = STANDARD
        .decode(&payload.data)
        .map_err(|e| format!("Invalid encrypted data encoding: {}", e))?;

    if nonce.len() != 12 {
        return Err("Invalid IV length".to_string());
    }

    let mut combined = data;
    combined.extend_from_slice(&auth_tag);
    let decrypted = cipher
        .decrypt(aes_gcm::Nonce::from_slice(&nonce), combined.as_ref())
        .map_err(|e| format!("Decryption failed: {}", e))?;
    String::from_utf8(decrypted).map_err(|e| format!("Decrypted state is not valid UTF-8: {}", e))
}

fn parse_hex_key(key_str: &str) -> Result<Vec<u8>, String> {
    let trimmed = key_str.trim();
    if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("AGENT_BROWSER_ENCRYPTION_KEY must be a 64-character hex string".to_string());
    }

    let mut bytes = Vec::with_capacity(32);
    for chunk in trimmed.as_bytes().chunks(2) {
        let pair = std::str::from_utf8(chunk).map_err(|e| format!("Invalid hex key: {}", e))?;
        let byte = u8::from_str_radix(pair, 16).map_err(|e| format!("Invalid hex key: {}", e))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("agent-browser-tab-assignments-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn test_read_missing_returns_default_file() {
        let root = temp_root();
        let file = read_tab_assignments_in(&root, "default", Some("named")).unwrap();
        assert_eq!(file.version, TAB_ASSIGNMENTS_SCHEMA_VERSION);
        assert_eq!(file.revision, 0);
        assert_eq!(file.session_name, "named");
        assert!(file.assignments.is_empty());
        assert!(file.tabs.is_none());
    }

    #[test]
    fn test_write_then_read_roundtrip() {
        let root = temp_root();
        let mut file = read_tab_assignments_in(&root, "default", Some("named")).unwrap();
        file.updated_at = current_timestamp();
        file.assignments.insert(
            "default".to_string(),
            TabAssignment {
                agent_session_id: "default".to_string(),
                tab_id: Some(1),
                target_id: Some("target-1".to_string()),
                window_id: Some(1),
                status: TabAssignmentStatus::Assigned,
                lease_version: Some(1),
                connection_id: Some("conn-1".to_string()),
                assigned_at: current_timestamp(),
                updated_at: current_timestamp(),
                last_known_url: Some("https://example.com".to_string()),
                last_known_title: Some("Example".to_string()),
                fallback_index: Some(0),
                context_ordinal: Some(0),
            },
        );

        let written = write_tab_assignments_in(&root, "default", Some("named"), &file).unwrap();
        assert_eq!(written.revision, 1);

        let loaded = read_tab_assignments_in(&root, "default", Some("named")).unwrap();
        assert_eq!(loaded.revision, 1);
        assert_eq!(loaded.assignments.len(), 1);
        assert_eq!(
            loaded.assignments["default"].status,
            TabAssignmentStatus::Assigned
        );
    }

    #[test]
    fn test_write_rejects_stale_revision() {
        let root = temp_root();
        let file = read_tab_assignments_in(&root, "default", Some("named")).unwrap();
        let _written = write_tab_assignments_in(&root, "default", Some("named"), &file).unwrap();

        let mut stale = file.clone();
        stale.assignments.insert(
            "other".to_string(),
            TabAssignment {
                agent_session_id: "other".to_string(),
                tab_id: Some(2),
                target_id: Some("target-2".to_string()),
                window_id: None,
                status: TabAssignmentStatus::Detached,
                lease_version: None,
                connection_id: None,
                assigned_at: current_timestamp(),
                updated_at: current_timestamp(),
                last_known_url: None,
                last_known_title: None,
                fallback_index: Some(1),
                context_ordinal: Some(0),
            },
        );

        let err = write_tab_assignments_in(&root, "default", Some("named"), &stale).unwrap_err();
        assert!(err.contains("revision mismatch"));
    }

    #[test]
    fn test_invalid_session_name_is_rejected() {
        let root = temp_root();
        let err = get_tab_assignments_path_in(&root, "default", Some("../evil")).unwrap_err();
        assert!(err.contains("Invalid session name"));
    }

    #[test]
    fn test_temp_file_uses_dotfile_pattern() {
        let path = PathBuf::from("/tmp/tab-assignments.json");
        let temp_path = get_temp_path(&path);
        let file_name = temp_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap();
        assert!(file_name.starts_with(".tab-assignments.json.tmp-"));
    }

    #[test]
    fn test_status_roundtrip() {
        let json = serde_json::to_string(&TabAssignmentStatus::Orphaned).unwrap();
        assert_eq!(json, "\"orphaned\"");
        let parsed: TabAssignmentStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, TabAssignmentStatus::Orphaned);
    }
}
