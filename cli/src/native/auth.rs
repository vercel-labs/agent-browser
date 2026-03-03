use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfile {
    pub name: String,
    pub url: String,
    pub username: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username_selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit_selector: Option<String>,
}

// Keep legacy Credential alias for backward compatibility
pub type Credential = AuthProfile;

fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "Invalid profile name '{}'. Must match /^[a-zA-Z0-9_-]+$/",
            name
        ));
    }
    Ok(())
}

fn get_auth_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser").join("auth")
    } else {
        std::env::temp_dir().join("agent-browser").join("auth")
    }
}

fn get_profile_path(name: &str) -> PathBuf {
    get_auth_dir().join(format!("{}.json", name))
}

fn derive_encryption_key() -> Vec<u8> {
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| {
            #[cfg(unix)]
            {
                let mut buf = [0u8; 256];
                let len = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut _, buf.len()) };
                if len == 0 {
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    String::from_utf8_lossy(&buf[..end]).to_string()
                } else {
                    "unknown-host".to_string()
                }
            }
            #[cfg(not(unix))]
            {
                "unknown-host".to_string()
            }
        });
    let username = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown-user".to_string());
    let mut hasher = Sha256::new();
    hasher.update(format!("agent-browser:{}:{}", hostname, username).as_bytes());
    hasher.finalize().to_vec()
}

fn encrypt_profile(profile: &AuthProfile) -> Result<Vec<u8>, String> {
    let key = derive_encryption_key();
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Encryption key error: {}", e))?;

    let plaintext = serde_json::to_string(profile)
        .map_err(|e| format!("Failed to serialize profile: {}", e))?;

    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| format!("Failed to generate nonce: {}", e))?;
    let ciphertext = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|e| format!("Encryption failed: {}", e))?;

    let mut result = Vec::with_capacity(12 + ciphertext.len());
    result.extend_from_slice(&nonce);
    result.extend_from_slice(&ciphertext);
    Ok(result)
}

fn decrypt_profile(data: &[u8]) -> Result<AuthProfile, String> {
    if data.len() < 13 {
        return Err("Encrypted data too short".to_string());
    }
    let (nonce_bytes, ciphertext) = data.split_at(12);

    let key = derive_encryption_key();
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Decryption key error: {}", e))?;
    let plaintext = cipher
        .decrypt(aes_gcm::Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|e| format!("Decryption failed: {}", e))?;

    let json_str = String::from_utf8(plaintext)
        .map_err(|e| format!("Decrypted data is not valid UTF-8: {}", e))?;
    serde_json::from_str(&json_str).map_err(|e| format!("Invalid profile data: {}", e))
}

fn save_profile(profile: &AuthProfile) -> Result<(), String> {
    let dir = get_auth_dir();
    let _ = fs::create_dir_all(&dir);

    let encrypted = encrypt_profile(profile)?;
    let path = get_profile_path(&profile.name);
    fs::write(&path, &encrypted).map_err(|e| format!("Failed to write profile: {}", e))
}

fn load_profile(name: &str) -> Result<AuthProfile, String> {
    let path = get_profile_path(name);
    if !path.exists() {
        return Err(format!("Auth profile '{}' not found", name));
    }
    let data = fs::read(&path).map_err(|e| format!("Failed to read profile: {}", e))?;
    decrypt_profile(&data)
}

pub fn credentials_set(
    name: &str,
    username: &str,
    password: &str,
    url: Option<&str>,
) -> Result<Value, String> {
    validate_profile_name(name)?;
    let profile = AuthProfile {
        name: name.to_string(),
        url: url.unwrap_or("").to_string(),
        username: username.to_string(),
        password: password.to_string(),
        username_selector: None,
        password_selector: None,
        submit_selector: None,
    };
    save_profile(&profile)?;
    Ok(json!({ "saved": name }))
}

pub fn auth_save(
    name: &str,
    url: &str,
    username: &str,
    password: &str,
    username_selector: Option<&str>,
    password_selector: Option<&str>,
    submit_selector: Option<&str>,
) -> Result<Value, String> {
    validate_profile_name(name)?;
    let profile = AuthProfile {
        name: name.to_string(),
        url: url.to_string(),
        username: username.to_string(),
        password: password.to_string(),
        username_selector: username_selector.map(String::from),
        password_selector: password_selector.map(String::from),
        submit_selector: submit_selector.map(String::from),
    };
    save_profile(&profile)?;
    Ok(json!({ "saved": name }))
}

pub fn credentials_get(name: &str) -> Result<Value, String> {
    let profile = load_profile(name)?;
    Ok(json!({
        "name": profile.name,
        "username": profile.username,
        "url": profile.url,
        "hasPassword": true,
    }))
}

pub fn credentials_get_full(name: &str) -> Result<AuthProfile, String> {
    load_profile(name)
}

pub fn credentials_delete(name: &str) -> Result<Value, String> {
    validate_profile_name(name)?;
    let path = get_profile_path(name);
    if !path.exists() {
        return Err(format!("Auth profile '{}' not found", name));
    }
    fs::remove_file(&path).map_err(|e| format!("Failed to delete profile: {}", e))?;
    Ok(json!({ "deleted": name }))
}

pub fn credentials_list() -> Result<Value, String> {
    let dir = get_auth_dir();
    if !dir.exists() {
        return Ok(json!({ "profiles": [] }));
    }

    let mut profiles = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            match load_profile(&name) {
                Ok(profile) => {
                    profiles.push(json!({
                        "name": profile.name,
                        "username": profile.username,
                        "url": profile.url,
                    }));
                }
                Err(_) => {
                    profiles.push(json!({
                        "name": name,
                        "error": "Failed to decrypt",
                    }));
                }
            }
        }
    }
    Ok(json!({ "profiles": profiles }))
}

pub fn auth_show(name: &str) -> Result<Value, String> {
    validate_profile_name(name)?;
    let profile = load_profile(name)?;
    Ok(json!({
        "profile": {
            "name": profile.name,
            "url": profile.url,
            "username": profile.username,
            "usernameSelector": profile.username_selector,
            "passwordSelector": profile.password_selector,
            "submitSelector": profile.submit_selector,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_profile_name() {
        assert!(validate_profile_name("github").is_ok());
        assert!(validate_profile_name("my-app").is_ok());
        assert!(validate_profile_name("test_123").is_ok());
        assert!(validate_profile_name("").is_err());
        assert!(validate_profile_name("has space").is_err());
        assert!(validate_profile_name("../evil").is_err());
        assert!(validate_profile_name("foo/bar").is_err());
    }

    #[test]
    fn test_auth_profile_serialization() {
        let profile = AuthProfile {
            name: "test".to_string(),
            url: "https://example.com".to_string(),
            username: "user".to_string(),
            password: "pass".to_string(),
            username_selector: None,
            password_selector: None,
            submit_selector: Some("button[type=submit]".to_string()),
        };
        let json = serde_json::to_string(&profile).unwrap();
        let parsed: AuthProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(
            parsed.submit_selector,
            Some("button[type=submit]".to_string())
        );
        assert!(parsed.username_selector.is_none());
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let profile = AuthProfile {
            name: "roundtrip".to_string(),
            url: "https://example.com".to_string(),
            username: "user".to_string(),
            password: "s3cret!".to_string(),
            username_selector: None,
            password_selector: None,
            submit_selector: None,
        };
        let encrypted = encrypt_profile(&profile).unwrap();
        let decrypted = decrypt_profile(&encrypted).unwrap();
        assert_eq!(decrypted.name, "roundtrip");
        assert_eq!(decrypted.password, "s3cret!");
    }

    #[test]
    fn test_derive_encryption_key_is_stable() {
        let k1 = derive_encryption_key();
        let k2 = derive_encryption_key();
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 32);
    }
}
