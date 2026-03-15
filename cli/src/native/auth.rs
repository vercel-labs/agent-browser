use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthProfile {
    pub name: String,
    pub url: String,
    pub username: String,
    /// Optional 1Password item specifier for resolving login credentials at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_password_item: Option<String>,
    /// Optional vault hint used together with `one_password_item`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_password_vault: Option<String>,
    /// Optional 1Password secret reference for resolving the username at login time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username_op_ref: Option<String>,
    pub password: String,
    /// Optional 1Password secret reference for resolving the password at login time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_op_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username_selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password_selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit_selector: Option<String>,
    /// Optional 1Password secret reference for resolving a one-time password code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otp_op_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otp_selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otp_submit_selector: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_login_at: Option<String>,
}

// Keep legacy Credential alias for backward compatibility
pub type Credential = AuthProfile;

#[derive(Debug, Clone, Copy, Default)]
pub struct AuthSaveOptions<'a> {
    pub one_password_item: Option<&'a str>,
    pub one_password_vault: Option<&'a str>,
    pub username: Option<&'a str>,
    pub username_op_ref: Option<&'a str>,
    pub password: Option<&'a str>,
    pub password_op_ref: Option<&'a str>,
    pub username_selector: Option<&'a str>,
    pub password_selector: Option<&'a str>,
    pub submit_selector: Option<&'a str>,
    pub otp_op_ref: Option<&'a str>,
    pub otp_selector: Option<&'a str>,
    pub otp_submit_selector: Option<&'a str>,
}

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

fn validate_1password_reference(reference: &str) -> Result<(), String> {
    if reference.trim().starts_with("op://") {
        Ok(())
    } else {
        Err(format!(
            "Invalid 1Password secret reference '{}'. Expected a value starting with op://",
            reference
        ))
    }
}

fn validate_1password_item_specifier(item: &str) -> Result<(), String> {
    if item.trim().is_empty() {
        Err("1Password item specifier cannot be empty".to_string())
    } else {
        Ok(())
    }
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

fn run_op_command(op_bin: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new(op_bin)
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "1Password CLI ('{}') was not found. Install the 'op' CLI and sign in before using 1Password-backed auth profiles.",
                    op_bin
                )
            } else {
                format!("Failed to execute 1Password CLI: {}", e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("op exited with status {}", output.status)
        };
        return Err(detail);
    }

    Ok(output.stdout)
}

#[derive(Debug, Deserialize)]
struct OnePasswordItemField {
    #[serde(default)]
    id: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(rename = "type", default)]
    field_type: Option<String>,
    #[serde(default)]
    value: Option<Value>,
    #[serde(default)]
    reference: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OnePasswordItem {
    #[serde(default)]
    fields: Vec<OnePasswordItemField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOnePasswordItem {
    pub username: String,
    pub password: String,
    pub otp: Option<String>,
}

fn value_as_string(value: &Value) -> Option<String> {
    value.as_str().map(ToString::to_string)
}

fn is_purpose(field: &OnePasswordItemField, purpose: &str) -> bool {
    field
        .purpose
        .as_deref()
        .map(|p| p.eq_ignore_ascii_case(purpose))
        .unwrap_or(false)
}

fn is_field_name(field: &OnePasswordItemField, name: &str) -> bool {
    field.id.eq_ignore_ascii_case(name) || field.label.eq_ignore_ascii_case(name)
}

fn append_query_parameter(reference: &str, query: &str) -> String {
    if reference.contains('?') {
        format!("{}&{}", reference, query)
    } else {
        format!("{}?{}", reference, query)
    }
}

fn read_1password_item_json_with_op_bin(
    op_bin: &str,
    item: &str,
    vault: Option<&str>,
) -> Result<OnePasswordItem, String> {
    validate_1password_item_specifier(item)?;

    let mut args = vec!["item", "get", item];
    if let Some(vault) = vault {
        args.push("--vault");
        args.push(vault);
    }
    args.push("--reveal");
    args.push("--format");
    args.push("json");

    let stdout = run_op_command(op_bin, &args)
        .map_err(|detail| format!("Failed to retrieve 1Password item details: {}", detail))?;

    serde_json::from_slice::<OnePasswordItem>(&stdout)
        .map_err(|e| format!("Failed to parse 1Password item JSON: {}", e))
}

fn resolve_1password_item_with_op_bin(
    op_bin: &str,
    item: &str,
    vault: Option<&str>,
) -> Result<ResolvedOnePasswordItem, String> {
    let item_json = read_1password_item_json_with_op_bin(op_bin, item, vault)?;

    let username = item_json
        .fields
        .iter()
        .find(|field| is_purpose(field, "username") || is_field_name(field, "username"))
        .and_then(|field| field.value.as_ref())
        .and_then(value_as_string)
        .ok_or("1Password item is missing a username field")?;

    let password = item_json
        .fields
        .iter()
        .find(|field| is_purpose(field, "password") || is_field_name(field, "password"))
        .and_then(|field| field.value.as_ref())
        .and_then(value_as_string)
        .ok_or("1Password item is missing a password field")?;

    let otp = item_json
        .fields
        .iter()
        .find(|field| {
            field
                .field_type
                .as_deref()
                .map(|field_type| field_type.eq_ignore_ascii_case("otp"))
                .unwrap_or(false)
        })
        .and_then(|field| field.reference.as_deref())
        .map(|reference| append_query_parameter(reference, "attribute=otp"))
        .map(|reference| resolve_1password_reference_with_op_bin(op_bin, &reference))
        .transpose()?;

    Ok(ResolvedOnePasswordItem {
        username,
        password,
        otp,
    })
}

fn resolve_1password_reference_with_op_bin(
    op_bin: &str,
    reference: &str,
) -> Result<String, String> {
    validate_1password_reference(reference)?;

    let stdout = run_op_command(op_bin, &["read", "--no-newline", reference])
        .map_err(|detail| format!("Failed to resolve 1Password secret reference: {}", detail))?;

    let value = String::from_utf8(stdout)
        .map_err(|e| format!("1Password CLI returned invalid UTF-8: {}", e))?;

    if value.is_empty() {
        return Err("1Password CLI returned an empty secret value".to_string());
    }

    Ok(value)
}

/// Resolve a 1Password secret reference using the local `op` CLI.
pub fn resolve_1password_reference(reference: &str) -> Result<String, String> {
    resolve_1password_reference_with_op_bin("op", reference)
}

/// Resolve username, password, and optional OTP from a 1Password Login item.
pub fn resolve_1password_item(
    item: &str,
    vault: Option<&str>,
) -> Result<ResolvedOnePasswordItem, String> {
    resolve_1password_item_with_op_bin("op", item, vault)
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
        one_password_item: None,
        one_password_vault: None,
        username_op_ref: None,
        password: password.to_string(),
        password_op_ref: None,
        username_selector: None,
        password_selector: None,
        submit_selector: None,
        otp_op_ref: None,
        otp_selector: None,
        otp_submit_selector: None,
        created_at: None,
        last_login_at: None,
    };
    save_profile(&profile)?;
    Ok(json!({ "saved": true, "name": name }))
}

pub fn auth_save(name: &str, url: &str, options: AuthSaveOptions<'_>) -> Result<Value, String> {
    validate_profile_name(name)?;
    let using_one_password_item = options.one_password_item.is_some();
    if options.one_password_vault.is_some() && !using_one_password_item {
        return Err("--onepassword-vault requires --onepassword-item".to_string());
    }
    if let Some(item) = options.one_password_item {
        validate_1password_item_specifier(item)?;
    }
    if using_one_password_item {
        let has_other_secret_sources = options.username.is_some()
            || options.username_op_ref.is_some()
            || options.password.is_some()
            || options.password_op_ref.is_some()
            || options.otp_op_ref.is_some();
        if has_other_secret_sources {
            return Err(
                "Use either --onepassword-item or individual username/password/otp sources"
                    .to_string(),
            );
        }
    }
    let username_source_count =
        usize::from(options.username.is_some()) + usize::from(options.username_op_ref.is_some());
    if !using_one_password_item && username_source_count == 0 {
        return Err("Auth profile requires either a username or --username-op".to_string());
    }
    if !using_one_password_item && username_source_count > 1 {
        return Err("Auth profile accepts only one username source".to_string());
    }
    let password_source_count =
        usize::from(options.password.is_some()) + usize::from(options.password_op_ref.is_some());
    if !using_one_password_item && password_source_count == 0 {
        return Err("Auth profile requires either a password or --password-op".to_string());
    }
    if !using_one_password_item && password_source_count > 1 {
        return Err("Auth profile accepts only one password source".to_string());
    }
    if let Some(reference) = options.username_op_ref {
        validate_1password_reference(reference)?;
    }
    if let Some(reference) = options.password_op_ref {
        validate_1password_reference(reference)?;
    }
    if let Some(reference) = options.otp_op_ref {
        validate_1password_reference(reference)?;
    }
    if !using_one_password_item
        && options.otp_op_ref.is_none()
        && (options.otp_selector.is_some() || options.otp_submit_selector.is_some())
    {
        return Err("OTP selectors require an --otp-op reference".to_string());
    }
    let profile = AuthProfile {
        name: name.to_string(),
        url: url.to_string(),
        one_password_item: options.one_password_item.map(String::from),
        one_password_vault: options.one_password_vault.map(String::from),
        username: options.username.unwrap_or("").to_string(),
        username_op_ref: options.username_op_ref.map(String::from),
        password: options.password.unwrap_or("").to_string(),
        password_op_ref: options.password_op_ref.map(String::from),
        username_selector: options.username_selector.map(String::from),
        password_selector: options.password_selector.map(String::from),
        submit_selector: options.submit_selector.map(String::from),
        otp_op_ref: options.otp_op_ref.map(String::from),
        otp_selector: options.otp_selector.map(String::from),
        otp_submit_selector: options.otp_submit_selector.map(String::from),
        created_at: None,
        last_login_at: None,
    };
    save_profile(&profile)?;
    Ok(json!({ "saved": true, "name": name }))
}

pub fn credentials_get(name: &str) -> Result<Value, String> {
    let profile = load_profile(name)?;
    Ok(json!({
        "name": profile.name,
        "username": profile.username,
        "url": profile.url,
        "hasPassword": !profile.password.is_empty()
            || profile.password_op_ref.is_some()
            || profile.one_password_item.is_some(),
        "hasUsername": !profile.username.is_empty()
            || profile.username_op_ref.is_some()
            || profile.one_password_item.is_some(),
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
    Ok(json!({ "deleted": true, "name": name }))
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
    let username_source = if profile.one_password_item.is_some() {
        "1password-item"
    } else if profile.username_op_ref.is_some() {
        "1password"
    } else if !profile.username.is_empty() {
        "stored"
    } else {
        "missing"
    };
    let password_source = if profile.one_password_item.is_some() {
        "1password-item"
    } else if profile.password_op_ref.is_some() {
        "1password"
    } else if !profile.password.is_empty() {
        "stored"
    } else {
        "missing"
    };
    Ok(json!({
        "profile": {
            "name": profile.name,
            "url": profile.url,
            "username": profile.username,
            "onePasswordItem": profile.one_password_item,
            "onePasswordVault": profile.one_password_vault,
            "usernameSource": username_source,
            "usernameSelector": profile.username_selector,
            "passwordSelector": profile.password_selector,
            "submitSelector": profile.submit_selector,
            "passwordSource": password_source,
            "otpEnabled": profile.otp_op_ref.is_some() || profile.one_password_item.is_some(),
            "otpSelector": profile.otp_selector,
            "otpSubmitSelector": profile.otp_submit_selector,
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
            one_password_item: None,
            one_password_vault: None,
            username: "".to_string(),
            username_op_ref: Some("op://vault/item/username".to_string()),
            password: "pass".to_string(),
            password_op_ref: Some("op://vault/item/password".to_string()),
            username_selector: None,
            password_selector: None,
            submit_selector: Some("button[type=submit]".to_string()),
            otp_op_ref: Some("op://vault/item/otp".to_string()),
            otp_selector: Some("#otp".to_string()),
            otp_submit_selector: Some("button.verify".to_string()),
            created_at: None,
            last_login_at: None,
        };
        let json = serde_json::to_string(&profile).unwrap();
        let parsed: AuthProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "test");
        assert_eq!(
            parsed.username_op_ref,
            Some("op://vault/item/username".to_string())
        );
        assert_eq!(
            parsed.password_op_ref,
            Some("op://vault/item/password".to_string())
        );
        assert_eq!(
            parsed.submit_selector,
            Some("button[type=submit]".to_string())
        );
        assert_eq!(parsed.otp_selector, Some("#otp".to_string()));
        assert!(parsed.username_selector.is_none());
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        with_test_key(|| {
            let profile = AuthProfile {
                name: "roundtrip".to_string(),
                url: "https://example.com".to_string(),
                one_password_item: None,
                one_password_vault: None,
                username: "user".to_string(),
                username_op_ref: None,
                password: "s3cret!".to_string(),
                password_op_ref: None,
                username_selector: None,
                password_selector: None,
                submit_selector: None,
                otp_op_ref: None,
                otp_selector: None,
                otp_submit_selector: None,
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
                one_password_item: None,
                one_password_vault: None,
                username: "admin".to_string(),
                username_op_ref: None,
                password: "hunter2".to_string(),
                password_op_ref: None,
                username_selector: Some("#email".to_string()),
                password_selector: None,
                submit_selector: None,
                otp_op_ref: None,
                otp_selector: None,
                otp_submit_selector: None,
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
                one_password_item: None,
                one_password_vault: None,
                username: "user".to_string(),
                username_op_ref: None,
                password: "pass".to_string(),
                password_op_ref: None,
                username_selector: None,
                password_selector: None,
                submit_selector: None,
                otp_op_ref: None,
                otp_selector: None,
                otp_submit_selector: None,
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

    #[test]
    fn test_validate_1password_reference() {
        assert!(validate_1password_reference("op://work/github/password").is_ok());
        assert!(validate_1password_reference("https://example.com").is_err());
    }

    #[test]
    fn test_auth_save_rejects_multiple_password_sources() {
        let result = auth_save(
            "github",
            "https://github.com/login",
            AuthSaveOptions {
                username: Some("user"),
                password: Some("pass"),
                password_op_ref: Some("op://work/github/password"),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("only one password source"));
    }

    #[test]
    fn test_auth_save_rejects_onepassword_vault_without_item() {
        let result = auth_save(
            "github",
            "https://github.com/login",
            AuthSaveOptions {
                one_password_vault: Some("Work"),
                username: Some("user"),
                password: Some("pass"),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("--onepassword-vault requires"));
    }

    #[test]
    fn test_auth_save_rejects_mixed_onepassword_item_and_manual_sources() {
        let result = auth_save(
            "github",
            "https://github.com/login",
            AuthSaveOptions {
                one_password_item: Some("GitHub"),
                username: Some("user"),
                password: Some("pass"),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("either --onepassword-item"));
    }

    #[test]
    fn test_auth_save_rejects_otp_selectors_without_reference() {
        let result = auth_save(
            "github",
            "https://github.com/login",
            AuthSaveOptions {
                username: Some("user"),
                password: Some("pass"),
                otp_selector: Some("#otp"),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("OTP selectors require"));
    }

    #[test]
    fn test_auth_save_rejects_multiple_username_sources() {
        let result = auth_save(
            "github",
            "https://github.com/login",
            AuthSaveOptions {
                username: Some("user"),
                username_op_ref: Some("op://work/github/username"),
                password: Some("pass"),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("only one username source"));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_1password_reference_with_stub_binary() {
        use std::os::unix::fs::PermissionsExt;

        let script_path = std::env::temp_dir().join(format!(
            "agent-browser-op-stub-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        fs::write(
            &script_path,
            "#!/bin/sh\nif [ \"$1\" = \"read\" ] && [ \"$2\" = \"--no-newline\" ]; then\n  printf '654321'\nelse\n  echo 'unexpected args' >&2\n  exit 1\nfi\n",
        )
        .unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let result = resolve_1password_reference_with_op_bin(
            script_path.to_str().unwrap(),
            "op://work/github/one-time password?attribute=otp",
        )
        .unwrap();

        assert_eq!(result, "654321");

        let _ = fs::remove_file(&script_path);
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_1password_item_with_stub_binary() {
        use std::os::unix::fs::PermissionsExt;

        let script_path = std::env::temp_dir().join(format!(
            "agent-browser-op-item-stub-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        fs::write(
            &script_path,
            r##"#!/bin/sh
if [ "$1" = "item" ] && [ "$2" = "get" ]; then
  printf '%s' '{"fields":[{"id":"username","label":"username","purpose":"USERNAME","type":"STRING","value":"octocat"},{"id":"password","label":"password","purpose":"PASSWORD","type":"CONCEALED","value":"s3cret"},{"id":"otp","label":"one-time password","type":"OTP","reference":"op://work/github/one-time password"}]}'
elif [ "$1" = "read" ] && [ "$2" = "--no-newline" ] && [ "$3" = "op://work/github/one-time password?attribute=otp" ]; then
  printf '654321'
else
  echo 'unexpected args' >&2
  exit 1
fi
"##,
        )
        .unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();

        let result = resolve_1password_item_with_op_bin(
            script_path.to_str().unwrap(),
            "GitHub",
            Some("Work"),
        )
        .unwrap();

        assert_eq!(
            result,
            ResolvedOnePasswordItem {
                username: "octocat".to_string(),
                password: "s3cret".to_string(),
                otp: Some("654321".to_string()),
            }
        );

        let _ = fs::remove_file(&script_path);
    }
}
