use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<String>,
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

const ENCRYPTION_KEY_ENV: &str = "AGENT_BROWSER_ENCRYPTION_KEY";
const KEY_FILE_NAME: &str = ".encryption-key";

fn get_agent_browser_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser")
    } else {
        std::env::temp_dir().join("agent-browser")
    }
}

fn get_key_file_path() -> PathBuf {
    get_agent_browser_dir().join(KEY_FILE_NAME)
}

fn parse_key_hex(hex_str: &str) -> Option<Vec<u8>> {
    let hex_str = hex_str.trim();
    if hex_str.len() != 64 || !hex_str.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let bytes: Vec<u8> = (0..32)
        .map(|i| u8::from_str_radix(&hex_str[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    Some(bytes)
}

/// Read the encryption key from AGENT_BROWSER_ENCRYPTION_KEY env var or
/// ~/.agent-browser/.encryption-key file (matching the Node.js implementation).
fn get_encryption_key() -> Result<Vec<u8>, String> {
    if let Ok(key_hex) = std::env::var(ENCRYPTION_KEY_ENV) {
        return parse_key_hex(&key_hex).ok_or_else(|| {
            format!(
                "{} should be a 64-character hex string (256 bits). Generate one with: openssl rand -hex 32",
                ENCRYPTION_KEY_ENV
            )
        });
    }

    let key_file = get_key_file_path();
    if key_file.exists() {
        let hex = fs::read_to_string(&key_file)
            .map_err(|e| format!("Failed to read encryption key file: {}", e))?;
        return parse_key_hex(&hex).ok_or_else(|| {
            format!(
                "Invalid encryption key in {}. Expected 64-character hex string.",
                key_file.display()
            )
        });
    }

    Err(format!(
        "Encryption key required. Set {} or ensure {} exists.",
        ENCRYPTION_KEY_ENV,
        key_file.display()
    ))
}

/// Ensure an encryption key exists, auto-generating one if needed.
fn ensure_encryption_key() -> Result<Vec<u8>, String> {
    if let Ok(key) = get_encryption_key() {
        return Ok(key);
    }

    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key).map_err(|e| format!("Failed to generate key: {}", e))?;
    let key_hex = key.iter().map(|b| format!("{:02x}", b)).collect::<String>();

    let dir = get_agent_browser_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create directory: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }

    let key_file = get_key_file_path();
    fs::write(&key_file, format!("{}\n", key_hex))
        .map_err(|e| format!("Failed to write encryption key: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600));
    }

    let _ = writeln!(
        std::io::stderr(),
        "[agent-browser] Auto-generated encryption key at {} -- back up this file or set {}",
        key_file.display(),
        ENCRYPTION_KEY_ENV
    );

    Ok(key.to_vec())
}

/// Encrypt a profile to the JSON+base64 format compatible with Node.js.
fn encrypt_profile(profile: &AuthProfile) -> Result<String, String> {
    let key = ensure_encryption_key()?;
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Encryption key error: {}", e))?;

    let plaintext = serde_json::to_string(profile)
        .map_err(|e| format!("Failed to serialize profile: {}", e))?;

    let mut iv = [0u8; 12];
    getrandom::getrandom(&mut iv).map_err(|e| format!("Failed to generate IV: {}", e))?;

    // aes_gcm appends the 16-byte auth tag to the ciphertext
    let encrypted = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&iv), plaintext.as_bytes())
        .map_err(|e| format!("Encryption failed: {}", e))?;

    let tag_offset = encrypted.len() - 16;
    let ciphertext = &encrypted[..tag_offset];
    let auth_tag = &encrypted[tag_offset..];

    let payload = json!({
        "version": 1,
        "encrypted": true,
        "iv": STANDARD.encode(iv),
        "authTag": STANDARD.encode(auth_tag),
        "data": STANDARD.encode(ciphertext),
    });

    serde_json::to_string_pretty(&payload)
        .map_err(|e| format!("Failed to serialize payload: {}", e))
}

/// JSON envelope written by Node.js encryption (src/encryption.ts).
#[derive(Deserialize)]
struct EncryptedPayload {
    #[allow(dead_code)]
    version: u32,
    #[allow(dead_code)]
    encrypted: bool,
    iv: String,
    #[serde(rename = "authTag")]
    auth_tag: String,
    data: String,
}

fn decrypt_profile(data: &[u8]) -> Result<AuthProfile, String> {
    let text = std::str::from_utf8(data).map_err(|_| {
        "Profile is not valid UTF-8 -- it may use an older incompatible binary format".to_string()
    })?;

    if let Ok(payload) = serde_json::from_str::<EncryptedPayload>(text) {
        let key = get_encryption_key()?;

        let iv = STANDARD
            .decode(&payload.iv)
            .map_err(|e| format!("Invalid base64 iv: {}", e))?;
        let auth_tag = STANDARD
            .decode(&payload.auth_tag)
            .map_err(|e| format!("Invalid base64 authTag: {}", e))?;
        let ciphertext = STANDARD
            .decode(&payload.data)
            .map_err(|e| format!("Invalid base64 data: {}", e))?;

        // aes_gcm expects ciphertext || auth_tag as input to decrypt
        let mut combined = Vec::with_capacity(ciphertext.len() + auth_tag.len());
        combined.extend_from_slice(&ciphertext);
        combined.extend_from_slice(&auth_tag);

        let cipher =
            Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Decryption key error: {}", e))?;
        let plaintext = cipher
            .decrypt(aes_gcm::Nonce::from_slice(&iv), combined.as_slice())
            .map_err(|e| format!("Decryption failed: {}", e))?;

        let json_str = String::from_utf8(plaintext)
            .map_err(|e| format!("Decrypted data is not valid UTF-8: {}", e))?;
        return serde_json::from_str(&json_str).map_err(|e| format!("Invalid profile data: {}", e));
    }

    // Fallback: try as plain unencrypted JSON profile
    serde_json::from_str::<AuthProfile>(text)
        .map_err(|_| "Profile is not a valid encrypted or unencrypted payload".to_string())
}

fn save_profile(profile: &AuthProfile) -> Result<(), String> {
    let dir = get_auth_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create auth dir: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }

    let encrypted_json = encrypt_profile(profile)?;
    let path = get_profile_path(&profile.name);
    fs::write(&path, &encrypted_json).map_err(|e| format!("Failed to write profile: {}", e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
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
        created_at: None,
        last_login_at: None,
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
        created_at: None,
        last_login_at: None,
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
pub(crate) static AUTH_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    fn with_test_key<F: FnOnce()>(f: F) {
        let _lock = AUTH_TEST_MUTEX.lock().unwrap();
        let original = std::env::var(ENCRYPTION_KEY_ENV).ok();
        let test_key = "a".repeat(64);
        // SAFETY: TEST_MUTEX serializes all test access so no concurrent mutation.
        unsafe { std::env::set_var(ENCRYPTION_KEY_ENV, &test_key) };
        f();
        // SAFETY: TEST_MUTEX serializes all test access so no concurrent mutation.
        match original {
            Some(val) => unsafe { std::env::set_var(ENCRYPTION_KEY_ENV, val) },
            None => unsafe { std::env::remove_var(ENCRYPTION_KEY_ENV) },
        }
    }

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
            created_at: None,
            last_login_at: None,
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
        with_test_key(|| {
            let profile = AuthProfile {
                name: "roundtrip".to_string(),
                url: "https://example.com".to_string(),
                username: "user".to_string(),
                password: "s3cret!".to_string(),
                username_selector: None,
                password_selector: None,
                submit_selector: None,
                created_at: None,
                last_login_at: None,
            };
            let encrypted_json = encrypt_profile(&profile).unwrap();
            let decrypted = decrypt_profile(encrypted_json.as_bytes()).unwrap();
            assert_eq!(decrypted.name, "roundtrip");
            assert_eq!(decrypted.password, "s3cret!");
        });
    }

    #[test]
    fn test_get_encryption_key_from_env() {
        with_test_key(|| {
            let key = get_encryption_key().unwrap();
            assert_eq!(key.len(), 32);
            assert!(key.iter().all(|&b| b == 0xaa));
        });
    }

    #[test]
    fn test_parse_key_hex_valid() {
        let hex = "ab".repeat(32);
        let key = parse_key_hex(&hex).unwrap();
        assert_eq!(key.len(), 32);
        assert!(key.iter().all(|&b| b == 0xab));
    }

    #[test]
    fn test_parse_key_hex_invalid() {
        assert!(parse_key_hex("too_short").is_none());
        assert!(parse_key_hex(&"g".repeat(64)).is_none());
        assert!(parse_key_hex("").is_none());
    }

    #[test]
    fn test_decrypt_json_payload_format() {
        with_test_key(|| {
            let key = get_encryption_key().unwrap();
            let profile = AuthProfile {
                name: "json-test".to_string(),
                url: "https://example.com/login".to_string(),
                username: "admin".to_string(),
                password: "hunter2".to_string(),
                username_selector: Some("#email".to_string()),
                password_selector: None,
                submit_selector: None,
                created_at: None,
                last_login_at: None,
            };

            // Encrypt with aes_gcm, then manually build the JSON payload
            // to simulate what Node.js would produce
            let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
            let mut iv = [0u8; 12];
            getrandom::getrandom(&mut iv).unwrap();
            let plaintext = serde_json::to_string(&profile).unwrap();
            let encrypted = cipher
                .encrypt(aes_gcm::Nonce::from_slice(&iv), plaintext.as_bytes())
                .unwrap();

            let tag_offset = encrypted.len() - 16;
            let ciphertext = &encrypted[..tag_offset];
            let auth_tag = &encrypted[tag_offset..];

            let payload = format!(
                r#"{{"version":1,"encrypted":true,"iv":"{}","authTag":"{}","data":"{}"}}"#,
                STANDARD.encode(iv),
                STANDARD.encode(auth_tag),
                STANDARD.encode(ciphertext),
            );

            let decrypted = decrypt_profile(payload.as_bytes()).unwrap();
            assert_eq!(decrypted.name, "json-test");
            assert_eq!(decrypted.password, "hunter2");
            assert_eq!(decrypted.username_selector, Some("#email".to_string()));
        });
    }

    #[test]
    fn test_encrypted_output_is_json_format() {
        with_test_key(|| {
            let profile = AuthProfile {
                name: "format-check".to_string(),
                url: "https://example.com".to_string(),
                username: "user".to_string(),
                password: "pass".to_string(),
                username_selector: None,
                password_selector: None,
                submit_selector: None,
                created_at: None,
                last_login_at: None,
            };
            let encrypted = encrypt_profile(&profile).unwrap();
            let parsed: Value = serde_json::from_str(&encrypted).unwrap();
            assert_eq!(parsed["version"], 1);
            assert_eq!(parsed["encrypted"], true);
            assert!(parsed["iv"].is_string());
            assert!(parsed["authTag"].is_string());
            assert!(parsed["data"].is_string());
        });
    }
}
