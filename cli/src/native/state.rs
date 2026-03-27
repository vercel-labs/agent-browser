use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

use super::cdp::client::CdpClient;
use super::cdp::types::EvaluateParams;
use super::cookies::{self, Cookie};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageState {
    pub cookies: Vec<Cookie>,
    pub origins: Vec<OriginStorage>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OriginStorage {
    pub origin: String,
    pub local_storage: Vec<StorageEntry>,
    #[serde(default)]
    pub session_storage: Vec<StorageEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageEntry {
    pub name: String,
    pub value: String,
}

pub async fn save_state(
    client: &CdpClient,
    session_id: &str,
    path: Option<&str>,
    session_name: Option<&str>,
    session_id_str: &str,
) -> Result<String, String> {
    let cookies = cookies::get_cookies(client, session_id, None).await?;

    // Get current origin's storage
    let origin_js = r#"(() => {
        const result = { origin: location.origin, localStorage: [], sessionStorage: [] };
        try {
            for (let i = 0; i < localStorage.length; i++) {
                const key = localStorage.key(i);
                result.localStorage.push({ name: key, value: localStorage.getItem(key) });
            }
        } catch(e) {}
        try {
            for (let i = 0; i < sessionStorage.length; i++) {
                const key = sessionStorage.key(i);
                result.sessionStorage.push({ name: key, value: sessionStorage.getItem(key) });
            }
        } catch(e) {}
        return result;
    })()"#;

    let origin_result: super::cdp::types::EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: origin_js.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    let origin_data = origin_result.result.value.unwrap_or(Value::Null);
    let origins = if origin_data.is_object() {
        let origin = origin_data
            .get("origin")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let local_storage: Vec<StorageEntry> = origin_data
            .get("localStorage")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let session_storage: Vec<StorageEntry> = origin_data
            .get("sessionStorage")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();

        if !origin.is_empty() && origin != "null" {
            vec![OriginStorage {
                origin,
                local_storage,
                session_storage,
            }]
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    let state = StorageState { cookies, origins };
    let json_str = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("Failed to serialize state: {}", e))?;

    let mut save_path = match path {
        Some(p) => p.to_string(),
        None => {
            let dir = get_sessions_dir();
            let _ = fs::create_dir_all(&dir);
            let name = session_name.unwrap_or("default");
            dir.join(format!("{}-{}.json", name, session_id_str))
                .to_string_lossy()
                .to_string()
        }
    };

    if let Ok(key) = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY") {
        let encrypted = encrypt_data(json_str.as_bytes(), &key)?;
        save_path.push_str(".enc");
        fs::write(&save_path, &encrypted)
            .map_err(|e| format!("Failed to write state to {}: {}", save_path, e))?;
    } else {
        fs::write(&save_path, &json_str)
            .map_err(|e| format!("Failed to write state to {}: {}", save_path, e))?;
    }

    Ok(save_path)
}

pub async fn load_state(client: &CdpClient, session_id: &str, path: &str) -> Result<(), String> {
    let json_str = if path.ends_with(".enc") {
        let key = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").map_err(|_| {
            "Encrypted state file requires AGENT_BROWSER_ENCRYPTION_KEY".to_string()
        })?;
        let data =
            fs::read(path).map_err(|e| format!("Failed to read state from {}: {}", path, e))?;
        let decrypted = decrypt_data(&data, &key)?;
        String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted state is not valid UTF-8: {}", e))?
    } else {
        match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                if let Ok(key) = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY") {
                    let enc_path = format!("{}.enc", path);
                    if let Ok(data) = fs::read(&enc_path) {
                        let decrypted = decrypt_data(&data, &key)?;
                        String::from_utf8(decrypted)
                            .map_err(|de| format!("Decrypted state is not valid UTF-8: {}", de))?
                    } else {
                        return Err(format!("Failed to read state from {}: {}", path, e));
                    }
                } else {
                    return Err(format!("Failed to read state from {}: {}", path, e));
                }
            }
        }
    };

    let state: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {}", e))?;

    // Load cookies
    if !state.cookies.is_empty() {
        let cookie_values: Vec<Value> = state
            .cookies
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
            .collect();
        cookies::set_cookies(client, session_id, cookie_values, None).await?;
    }

    // Load storage per origin
    for origin in &state.origins {
        if origin.local_storage.is_empty() && origin.session_storage.is_empty() {
            continue;
        }

        // Navigate to origin to set storage
        let navigate_url = format!("{}/", origin.origin.trim_end_matches('/'));
        client
            .send_command(
                "Page.navigate",
                Some(json!({ "url": navigate_url })),
                Some(session_id),
            )
            .await?;

        // Brief wait for navigation
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        for entry in &origin.local_storage {
            let js = format!(
                "localStorage.setItem({}, {})",
                serde_json::to_string(&entry.name).unwrap_or_default(),
                serde_json::to_string(&entry.value).unwrap_or_default(),
            );
            let _ = client
                .send_command_typed::<_, super::cdp::types::EvaluateResult>(
                    "Runtime.evaluate",
                    &EvaluateParams {
                        expression: js,
                        return_by_value: Some(true),
                        await_promise: Some(false),
                    },
                    Some(session_id),
                )
                .await;
        }

        for entry in &origin.session_storage {
            let js = format!(
                "sessionStorage.setItem({}, {})",
                serde_json::to_string(&entry.name).unwrap_or_default(),
                serde_json::to_string(&entry.value).unwrap_or_default(),
            );
            let _ = client
                .send_command_typed::<_, super::cdp::types::EvaluateResult>(
                    "Runtime.evaluate",
                    &EvaluateParams {
                        expression: js,
                        return_by_value: Some(true),
                        await_promise: Some(false),
                    },
                    Some(session_id),
                )
                .await;
        }
    }

    Ok(())
}

fn is_state_file(path: &std::path::Path) -> bool {
    let fname = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    fname.ends_with(".json") || fname.ends_with(".json.enc")
}

fn is_encrypted_state(path: &std::path::Path) -> bool {
    path.to_string_lossy().ends_with(".json.enc")
}

pub fn state_list() -> Result<Value, String> {
    let dir = get_sessions_dir();
    if !dir.exists() {
        return Ok(json!({ "files": [], "directory": dir.to_string_lossy() }));
    }

    let mut files = Vec::new();

    let entries = fs::read_dir(&dir).map_err(|e| format!("Failed to read sessions dir: {}", e))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if is_state_file(&path) {
            let metadata = fs::metadata(&path).ok();
            let filename = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
            let modified = metadata
                .as_ref()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let encrypted = is_encrypted_state(&path);

            files.push(json!({
                "filename": filename,
                "path": path.to_string_lossy(),
                "size": size,
                "modified": modified,
                "encrypted": encrypted,
            }));
        }
    }

    Ok(json!({ "files": files, "directory": dir.to_string_lossy() }))
}

pub fn state_show(path: &str) -> Result<Value, String> {
    let encrypted = path.ends_with(".enc");
    let json_str = if encrypted {
        let key = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").map_err(|_| {
            "Encrypted state file requires AGENT_BROWSER_ENCRYPTION_KEY".to_string()
        })?;
        let data = fs::read(path).map_err(|e| format!("Failed to read state file: {}", e))?;
        let decrypted = decrypt_data(&data, &key)?;
        String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted state is not valid UTF-8: {}", e))?
    } else {
        fs::read_to_string(path).map_err(|e| format!("Failed to read state file: {}", e))?
    };

    let state: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {}", e))?;

    let metadata = fs::metadata(path).ok();
    let filename = std::path::Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    Ok(json!({
        "filename": filename,
        "path": path,
        "size": metadata.as_ref().map(|m| m.len()).unwrap_or(0),
        "modified": metadata.as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "encrypted": encrypted,
        "summary": format!("{} cookies, {} origins", state.cookies.len(), state.origins.len()),
        "state": state,
    }))
}

pub fn state_clear(path: Option<&str>) -> Result<Value, String> {
    if let Some(p) = path {
        fs::remove_file(p).map_err(|e| format!("Failed to delete state: {}", e))?;
        return Ok(json!({ "deleted": p }));
    }

    let dir = get_sessions_dir();
    if !dir.exists() {
        return Ok(json!({ "deleted": 0 }));
    }

    let mut count = 0;
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_state_file(&path) {
                let _ = fs::remove_file(&path);
                count += 1;
            }
        }
    }

    Ok(json!({ "deleted": count }))
}

pub fn state_clean(max_age_days: u64) -> Result<Value, String> {
    let dir = get_sessions_dir();
    if !dir.exists() {
        return Ok(json!({ "cleaned": 0, "keptCount": 0, "days": max_age_days }));
    }

    let now = std::time::SystemTime::now();
    let max_age = std::time::Duration::from_secs(max_age_days * 86400);
    let mut deleted = 0;
    let mut kept = 0;

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_state_file(&path) {
                continue;
            }

            if let Ok(metadata) = fs::metadata(&path) {
                if let Ok(modified) = metadata.modified() {
                    if let Ok(age) = now.duration_since(modified) {
                        if age > max_age {
                            let _ = fs::remove_file(&path);
                            deleted += 1;
                            continue;
                        }
                    }
                }
            }
            kept += 1;
        }
    }

    Ok(json!({ "cleaned": deleted, "keptCount": kept, "days": max_age_days }))
}

pub fn state_rename(old_path: &str, new_name: &str) -> Result<Value, String> {
    let old = PathBuf::from(old_path);
    if !old.exists() {
        return Err(format!("State file not found: {}", old_path));
    }

    let fallback = PathBuf::from(".");
    let dir = old.parent().unwrap_or(&fallback);
    let new_path = dir.join(format!("{}.json", new_name));

    fs::rename(&old, &new_path).map_err(|e| format!("Failed to rename state: {}", e))?;

    Ok(json!({
        "renamed": true,
        "from": old_path,
        "to": new_path.to_string_lossy(),
    }))
}

fn encrypt_data(data: &[u8], key_str: &str) -> Result<Vec<u8>, String> {
    let mut hasher = Sha256::new();
    hasher.update(key_str.as_bytes());
    let key_bytes = hasher.finalize();
    let cipher =
        Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("Invalid key: {}", e))?;

    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| format!("Failed to generate nonce: {}", e))?;
    let ciphertext = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), data)
        .map_err(|e| format!("Encryption failed: {}", e))?;

    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

fn decrypt_data(data: &[u8], key_str: &str) -> Result<Vec<u8>, String> {
    if data.len() < 13 {
        return Err("Ciphertext too short".to_string());
    }
    let (nonce_bytes, ciphertext) = data.split_at(12);

    let mut hasher = Sha256::new();
    hasher.update(key_str.as_bytes());
    let key_bytes = hasher.finalize();
    let cipher =
        Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("Invalid key: {}", e))?;
    let plaintext = cipher
        .decrypt(aes_gcm::Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|e| format!("Decryption failed: {}", e))?;
    Ok(plaintext)
}

pub fn find_auto_state_file(session_name: &str) -> Option<String> {
    let dir = get_sessions_dir();
    if !dir.exists() {
        return None;
    }
    let prefix = format!("{}-", session_name);
    let mut best_path: Option<(String, std::time::SystemTime)> = None;

    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let fname = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let is_match = fname.starts_with(&prefix)
                && (fname.ends_with(".json") || fname.ends_with(".json.enc"));
            if !is_match {
                continue;
            }
            let modified = fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best_path.as_ref().is_none_or(|(_, t)| modified > *t) {
                best_path = Some((path.to_string_lossy().to_string(), modified));
            }
        }
    }
    best_path.map(|(p, _)| p)
}

/// Dispatch a state management command from its JSON payload.
/// Returns `Some(result)` for recognised state_* actions, `None` otherwise.
pub fn dispatch_state_command(cmd: &Value) -> Option<Result<Value, String>> {
    let action = cmd.get("action").and_then(|v| v.as_str())?;
    match action {
        "state_list" => Some(state_list()),
        "state_show" => Some(
            cmd.get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing 'path' parameter".to_string())
                .and_then(state_show),
        ),
        "state_clear" => {
            let path = cmd.get("path").and_then(|v| v.as_str());
            Some(state_clear(path))
        }
        "state_clean" => {
            let days = cmd.get("days").and_then(|v| v.as_u64()).unwrap_or(30);
            Some(state_clean(days))
        }
        "state_rename" => Some(
            cmd.get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing 'path' parameter".to_string())
                .and_then(|path| {
                    cmd.get("name")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| "Missing 'name' parameter".to_string())
                        .and_then(|name| state_rename(path, name))
                }),
        ),
        _ => None,
    }
}

pub fn get_sessions_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser").join("sessions")
    } else {
        std::env::temp_dir().join("agent-browser").join("sessions")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_state_serialization() {
        let state = StorageState {
            cookies: vec![Cookie {
                name: "session".to_string(),
                value: "abc123".to_string(),
                domain: ".example.com".to_string(),
                path: "/".to_string(),
                expires: 0.0,
                size: 0,
                http_only: true,
                secure: false,
                session: true,
                same_site: Some("Lax".to_string()),
            }],
            origins: vec![OriginStorage {
                origin: "https://example.com".to_string(),
                local_storage: vec![StorageEntry {
                    name: "key".to_string(),
                    value: "val".to_string(),
                }],
                session_storage: vec![],
            }],
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: StorageState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.cookies.len(), 1);
        assert_eq!(parsed.cookies[0].name, "session");
        assert_eq!(parsed.origins.len(), 1);
        assert_eq!(parsed.origins[0].local_storage.len(), 1);
    }

    #[test]
    fn test_storage_state_empty() {
        let state = StorageState {
            cookies: vec![],
            origins: vec![],
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: StorageState = serde_json::from_str(&json).unwrap();
        assert!(parsed.cookies.is_empty());
        assert!(parsed.origins.is_empty());
    }

    #[test]
    fn test_state_show_nonexistent_file() {
        let result = state_show("/tmp/nonexistent-agent-browser-state-file.json");
        assert!(result.is_err());
    }

    #[test]
    fn test_state_clear_nonexistent_file() {
        let result = state_clear(Some("/tmp/nonexistent-agent-browser-state-file.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_state_rename_nonexistent() {
        let result = state_rename("/tmp/nonexistent-agent-browser-state-file.json", "new-name");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn test_state_list_returns_json() {
        let result = state_list().unwrap();
        assert!(result.get("files").is_some());
        assert!(result.get("directory").is_some());
    }

    #[test]
    fn test_sessions_dir_path() {
        let dir = get_sessions_dir();
        assert!(dir.to_string_lossy().contains("sessions"));
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let plain = b"hello world";
        let key = "test-secret-key";
        let encrypted = encrypt_data(plain, key).unwrap();
        assert!(encrypted.len() > 12);
        assert_ne!(&encrypted[12..], plain);
        let decrypted = decrypt_data(&encrypted, key).unwrap();
        assert_eq!(decrypted, plain);
    }

    #[test]
    fn test_decrypt_wrong_key_fails() {
        let plain = b"secret data";
        let encrypted = encrypt_data(plain, "key1").unwrap();
        let result = decrypt_data(&encrypted, "key2");
        assert!(result.is_err());
    }

    #[test]
    fn test_cookie_serde_roundtrip() {
        let cookie = Cookie {
            name: "test".to_string(),
            value: "123".to_string(),
            domain: ".test.com".to_string(),
            path: "/api".to_string(),
            expires: 1700000000.0,
            size: 7,
            http_only: false,
            secure: true,
            session: false,
            same_site: Some("Strict".to_string()),
        };

        let json = serde_json::to_value(&cookie).unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["httpOnly"], false);
        assert_eq!(json["secure"], true);
        assert_eq!(json["sameSite"], "Strict");
    }

    #[test]
    fn test_dispatch_state_command_routes_state_list() {
        let cmd = serde_json::json!({ "action": "state_list" });
        let result = dispatch_state_command(&cmd);
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn test_dispatch_state_command_returns_none_for_unknown() {
        let cmd = serde_json::json!({ "action": "navigate" });
        assert!(dispatch_state_command(&cmd).is_none());
    }

    #[test]
    fn test_dispatch_state_command_returns_none_for_missing_action() {
        let cmd = serde_json::json!({});
        assert!(dispatch_state_command(&cmd).is_none());
    }

    #[test]
    fn test_dispatch_state_show_missing_path() {
        let cmd = serde_json::json!({ "action": "state_show" });
        let result = dispatch_state_command(&cmd).unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Missing 'path' parameter");
    }

    #[test]
    fn test_dispatch_state_rename_missing_params() {
        let cmd = serde_json::json!({ "action": "state_rename" });
        let result = dispatch_state_command(&cmd).unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Missing 'path' parameter");

        let cmd = serde_json::json!({ "action": "state_rename", "path": "/tmp/test.json" });
        let result = dispatch_state_command(&cmd).unwrap();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Missing 'name' parameter");
    }
}
