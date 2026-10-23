//! Firefox cookies, web storage and saved state. Formats must match `cookies.rs` and `state.rs`.

use std::collections::HashSet;
use std::fs;

use aes_gcm::aead::Aead;
use aes_gcm::{aead::KeyInit, Aes256Gcm};
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::actions::session;
use super::session::FirefoxSession;
use crate::native::actions::DaemonState;
use crate::native::cookies::Cookie;
use crate::native::state::{self, OriginStorage, StorageState};
use crate::validation::{is_valid_session_name, session_name_error};

const ORIGIN_STORAGE_JS: &str = r#"() => {
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
}"#;

fn storage_js_name(storage_type: &str) -> &'static str {
    match storage_type {
        "session" => "sessionStorage",
        _ => "localStorage",
    }
}

fn same_site_to_state(value: &Value) -> Option<String> {
    match value.as_str()?.to_ascii_lowercase().as_str() {
        "strict" => Some("Strict".to_string()),
        "lax" => Some("Lax".to_string()),
        "none" => Some("None".to_string()),
        _ => None,
    }
}

fn same_site_to_bidi(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| matches!(s.as_str(), "strict" | "lax" | "none"))
}

/// Wraps a cookie value in the BiDi `BytesValue` shape expected by `storage.setCookie`.
fn bidi_bytes_value(value: &str) -> Value {
    json!({ "type": "string", "value": value })
}

/// Decodes a BiDi `BytesValue` (`string` or `base64`) back to plain text.
fn decode_bytes_value(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    let Some(object) = value.as_object() else {
        return String::new();
    };
    let text = object.get("value").and_then(Value::as_str).unwrap_or("");
    match object.get("type").and_then(Value::as_str) {
        Some("base64") => base64::engine::general_purpose::STANDARD
            .decode(text)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default(),
        _ => text.to_string(),
    }
}

/// Converts a BiDi `storage.getCookies` cookie to the `state.rs` cookie shape.
fn bidi_cookie_to_state(cookie: &Value) -> Cookie {
    let expiry = cookie.get("expiry").and_then(Value::as_f64);
    Cookie {
        name: cookie["name"].as_str().unwrap_or("").to_string(),
        value: decode_bytes_value(&cookie["value"]),
        domain: cookie["domain"].as_str().unwrap_or("").to_string(),
        path: cookie["path"]
            .as_str()
            .filter(|p| !p.is_empty())
            .unwrap_or("/")
            .to_string(),
        expires: expiry.unwrap_or(-1.0),
        size: cookie["size"].as_i64().unwrap_or(0),
        http_only: cookie["httpOnly"].as_bool().unwrap_or(false),
        secure: cookie["secure"].as_bool().unwrap_or(false),
        session: expiry.is_none(),
        same_site: cookie.get("sameSite").and_then(same_site_to_state),
    }
}

/// Converts a `state.rs` cookie to the BiDi `storage.setCookie` cookie shape.
/// Chrome-only fields are absent from the source struct and therefore dropped.
fn state_cookie_to_bidi(cookie: &Cookie) -> Value {
    let mut out = json!({
        "name": cookie.name,
        "value": bidi_bytes_value(&cookie.value),
        "domain": cookie.domain,
        "path": if cookie.path.is_empty() { "/" } else { cookie.path.as_str() },
        "httpOnly": cookie.http_only,
        "secure": cookie.secure,
    });
    if cookie.expires > 0.0 {
        out["expiry"] = json!(cookie.expires as u64);
    }
    if let Some(same_site) = cookie.same_site.as_deref() {
        out["sameSite"] = json!(same_site.to_ascii_lowercase());
    }
    out
}

/// Converts a `cookies set` payload to BiDi `storage.setCookie` params. Fields Chrome knows
/// but BiDi does not (`priority`, `sourceScheme`, `sameParty`, `partitionKey`) are ignored.
fn command_cookie_to_bidi(cookie: &Value, current_url: Option<&str>) -> Result<Value, String> {
    let name = cookie
        .get("name")
        .and_then(Value::as_str)
        .ok_or("Missing cookie 'name'")?;
    let value = cookie.get("value").and_then(Value::as_str).unwrap_or("");
    let explicit_domain = cookie
        .get("domain")
        .and_then(Value::as_str)
        .filter(|d| !d.is_empty());
    let explicit_path = cookie
        .get("path")
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty());
    let source_url = cookie
        .get("url")
        .and_then(Value::as_str)
        .or(if explicit_domain.is_none() {
            current_url
        } else {
            None
        });
    let parsed = source_url.and_then(|u| url::Url::parse(u).ok());
    let domain = explicit_domain
        .map(String::from)
        .or_else(|| parsed.as_ref().and_then(|u| u.host_str().map(String::from)))
        .ok_or("Cookie requires a 'domain' or a current page URL")?;
    let path = explicit_path.unwrap_or("/").to_string();

    let mut out = json!({
        "name": name,
        "value": bidi_bytes_value(value),
        "domain": domain,
        "path": path,
        "httpOnly": cookie.get("httpOnly").and_then(Value::as_bool).unwrap_or(false),
        "secure": cookie.get("secure").and_then(Value::as_bool).unwrap_or_else(|| {
            parsed.as_ref().is_some_and(|u| u.scheme() == "https")
        }),
    });
    if let Some(expires) = cookie.get("expires").and_then(Value::as_f64) {
        if expires > 0.0 {
            out["expiry"] = json!(expires as u64);
        }
    }
    if let Some(same_site) = same_site_to_bidi(&cookie["sameSite"]) {
        out["sameSite"] = json!(same_site);
    }
    Ok(out)
}

fn domain_matches(cookie: &Cookie, url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let domain = cookie.domain.trim_start_matches('.');
    host == domain || host.ends_with(&format!(".{domain}"))
}

async fn get_all_cookies(ff: &FirefoxSession) -> Result<Vec<Cookie>, String> {
    let result = ff.bidi.send("storage.getCookies", json!({})).await?;
    Ok(result["cookies"]
        .as_array()
        .into_iter()
        .flatten()
        .map(bidi_cookie_to_state)
        .collect())
}

/// Returns `{cookies: [...]}` like Chrome's `handle_cookies_get` (via `storage.getCookies`).
pub async fn cookies_get(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let mut cookies = get_all_cookies(ff).await?;
    if let Some(urls) = cmd.get("urls").and_then(Value::as_array) {
        let urls: Vec<url::Url> = urls
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|u| url::Url::parse(u).ok())
            .collect();
        if !urls.is_empty() {
            cookies.retain(|cookie| urls.iter().any(|url| domain_matches(cookie, url)));
        }
    }
    Ok(json!({ "cookies": cookies }))
}

/// Sets cookies like Chrome's `handle_cookies_set` (via `storage.setCookie`).
pub async fn cookies_set(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let current_url = ff.url().await.ok();
    let cookie_values = if let Some(arr) = cmd.get("cookies").and_then(Value::as_array) {
        arr.clone()
    } else {
        let mut cookie = serde_json::Map::new();
        for key in &[
            "name", "value", "domain", "path", "expires", "httpOnly", "secure", "sameSite", "url",
        ] {
            if let Some(v) = cmd.get(*key) {
                if !v.is_null() {
                    cookie.insert(key.to_string(), v.clone());
                }
            }
        }
        vec![Value::Object(cookie)]
    };

    for cookie in &cookie_values {
        let params = command_cookie_to_bidi(cookie, current_url.as_deref())?;
        ff.bidi
            .send("storage.setCookie", json!({ "cookie": params }))
            .await?;
    }
    Ok(json!({ "set": true }))
}

/// Clears cookies like Chrome's `handle_cookies_clear` (via `storage.deleteCookies`).
pub async fn cookies_clear(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    ff.bidi
        .send("storage.deleteCookies", json!({ "filter": {} }))
        .await?;
    Ok(json!({ "cleared": true }))
}

/// Reads local or session storage through evaluate, like Chrome's `handle_storage_get`.
pub async fn storage_get(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let storage_type = cmd.get("type").and_then(Value::as_str).unwrap_or("local");
    let st = storage_js_name(storage_type);
    if let Some(key) = cmd.get("key").and_then(Value::as_str) {
        let js = format!(
            "{}.getItem({})",
            st,
            serde_json::to_string(key).unwrap_or_default()
        );
        let value = ff.eval(&js).await?;
        Ok(json!({ "key": key, "value": value }))
    } else {
        let js = format!(
            r#"(() => {{
                const s = {};
                const data = {{}};
                for (let i = 0; i < s.length; i++) {{
                    const key = s.key(i);
                    data[key] = s.getItem(key);
                }}
                return data;
            }})()"#,
            st
        );
        Ok(json!({ "data": ff.eval(&js).await? }))
    }
}

/// Writes local or session storage like Chrome's `handle_storage_set`.
pub async fn storage_set(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let storage_type = cmd.get("type").and_then(Value::as_str).unwrap_or("local");
    let key = cmd
        .get("key")
        .and_then(Value::as_str)
        .ok_or("Missing 'key' parameter")?;
    let value = cmd
        .get("value")
        .and_then(Value::as_str)
        .ok_or("Missing 'value' parameter")?;
    let js = format!(
        "{}.setItem({}, {})",
        storage_js_name(storage_type),
        serde_json::to_string(key).unwrap_or_default(),
        serde_json::to_string(value).unwrap_or_default(),
    );
    ff.eval(&js).await?;
    Ok(json!({ "set": true }))
}

/// Clears local or session storage like Chrome's `handle_storage_clear`.
pub async fn storage_clear(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let storage_type = cmd.get("type").and_then(Value::as_str).unwrap_or("local");
    ff.eval(&format!("{}.clear()", storage_js_name(storage_type)))
        .await?;
    Ok(json!({ "cleared": true }))
}

async fn collect_origins(ff: &FirefoxSession) -> Vec<OriginStorage> {
    let Ok(tree) = ff
        .bidi
        .send("browsingContext.getTree", json!({"maxDepth": 0}))
        .await
    else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut origins = Vec::new();
    for context in tree["contexts"].as_array().into_iter().flatten() {
        let Some(id) = context["context"].as_str() else {
            continue;
        };
        let Ok(data) = ff
            .bidi
            .call_function(id, ORIGIN_STORAGE_JS, None, Vec::new())
            .await
        else {
            continue;
        };
        let Ok(origin) = serde_json::from_value::<OriginStorage>(data) else {
            continue;
        };
        let has_entries = !origin.local_storage.is_empty() || !origin.session_storage.is_empty();
        if has_entries && origin.origin != "null" && seen.insert(origin.origin.clone()) {
            origins.push(origin);
        }
    }
    origins
}

async fn collect_state(ff: &FirefoxSession) -> Result<StorageState, String> {
    let cookies = get_all_cookies(ff).await?;
    let origins = collect_origins(ff).await;
    Ok(StorageState { cookies, origins })
}

fn encryption_key() -> Option<String> {
    std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").ok()
}

fn is_encrypted(path: &str) -> bool {
    path.ends_with(".json.enc") || path.ends_with(".json.enc.previous")
}

fn encrypt_data(data: &[u8], key_str: &str) -> Result<Vec<u8>, String> {
    let mut hasher = Sha256::new();
    hasher.update(key_str.as_bytes());
    let key_bytes = hasher.finalize();
    let cipher = Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("Invalid key: {e}"))?;

    let mut nonce = [0u8; 12];
    getrandom::getrandom(&mut nonce).map_err(|e| format!("Failed to generate nonce: {e}"))?;
    let ciphertext = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), data)
        .map_err(|e| format!("Encryption failed: {e}"))?;

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
    let cipher = Aes256Gcm::new_from_slice(&key_bytes).map_err(|e| format!("Invalid key: {e}"))?;
    cipher
        .decrypt(aes_gcm::Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|e| format!("Decryption failed: {e}"))
}

fn read_state_json(path: &str) -> Result<String, String> {
    if is_encrypted(path) {
        let key = encryption_key().ok_or_else(|| {
            "Encrypted state file requires AGENT_BROWSER_ENCRYPTION_KEY".to_string()
        })?;
        let data = fs::read(path).map_err(|e| format!("Failed to read state from {path}: {e}"))?;
        let plaintext = decrypt_data(&data, &key)?;
        return String::from_utf8(plaintext)
            .map_err(|e| format!("Decrypted state is not valid UTF-8: {e}"));
    }
    match fs::read_to_string(path) {
        Ok(json) => Ok(json),
        Err(err) => match encryption_key() {
            Some(key) => {
                let encrypted = format!("{path}.enc");
                let data = fs::read(&encrypted)
                    .map_err(|_| format!("Failed to read state from {path}: {err}"))?;
                let plaintext = decrypt_data(&data, &key)?;
                String::from_utf8(plaintext)
                    .map_err(|e| format!("Decrypted state is not valid UTF-8: {e}"))
            }
            None => Err(format!("Failed to read state from {path}: {err}")),
        },
    }
}

fn serialize_state(state: &StorageState) -> Result<String, String> {
    serde_json::to_string_pretty(state).map_err(|e| format!("Failed to serialize state: {e}"))
}

fn write_state_json(path: &str, json_str: &str) -> Result<String, String> {
    match encryption_key() {
        Some(key) => {
            let encrypted_path = format!("{path}.enc");
            let encrypted = encrypt_data(json_str.as_bytes(), &key)?;
            state::write_private(&encrypted_path, &encrypted)
                .map_err(|e| format!("Failed to write state to {encrypted_path}: {e}"))?;
            Ok(encrypted_path)
        }
        None => {
            state::write_private(path, json_str.as_bytes())
                .map_err(|e| format!("Failed to write state to {path}: {e}"))?;
            Ok(path.to_string())
        }
    }
}

async fn write_state_to(ff: &FirefoxSession, path: &str) -> Result<String, String> {
    let state = collect_state(ff).await?;
    write_state_json(path, &serialize_state(&state)?)
}

/// Saves cookies and origin storage in the `state.rs` JSON format, like Chrome's `handle_state_save`.
pub async fn state_save(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let explicit_path = cmd.get("path").and_then(Value::as_str);
    let session_name = state.session_name.clone();
    let session_id = state.session_id.clone();
    let ff = session(state)?;
    let path = match explicit_path {
        Some(path) => path.to_string(),
        None => {
            let name = session_name.as_deref().unwrap_or("default");
            if !is_valid_session_name(name) {
                return Err(session_name_error(name));
            }
            let dir = state::get_sessions_dir();
            let _ = state::create_private_dir(&dir);
            dir.join(format!("{name}-{session_id}.json"))
                .to_string_lossy()
                .to_string()
        }
    };
    let saved_path = write_state_to(ff, &path).await?;
    Ok(json!({ "saved": true, "path": saved_path }))
}

/// Saves the `--session-name` state file used by restore-on-close/autosave.
pub async fn save_auto_state(
    ff: &FirefoxSession,
    session_name: &str,
    session_id: &str,
) -> Result<String, String> {
    if !is_valid_session_name(session_name) {
        return Err(session_name_error(session_name));
    }
    let dir = state::get_sessions_dir();
    state::create_private_dir(&dir)
        .map_err(|e| format!("Failed to create state directory {}: {e}", dir.display()))?;
    let path = dir
        .join(format!("{session_name}-{session_id}.json"))
        .to_string_lossy()
        .to_string();
    write_state_to(ff, &path).await
}

/// Run in a BiDi sandbox so page scripts never see the payload; the origin is checked in the same call so a redirect cannot race it.
const RESTORE_STORAGE_JS: &str = r#"(expectedOrigin, payload) => {
    if (location.origin !== expectedOrigin) {
        throw new Error('landed on ' + location.origin + ' instead of ' + expectedOrigin);
    }
    const p = JSON.parse(payload);
    for (const e of p.localStorage) localStorage.setItem(e.name, e.value);
    for (const e of p.sessionStorage) sessionStorage.setItem(e.name, e.value);
}"#;

/// Loads the cookies and origin storage in a `state.rs` JSON file, dropping fields BiDi
/// does not support, like Chrome's `handle_state_load`.
pub async fn load_state_path(ff: &mut FirefoxSession, path: &str) -> Result<(), String> {
    let json_str = read_state_json(path)?;
    let state: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {e}"))?;

    for cookie in &state.cookies {
        ff.bidi
            .send(
                "storage.setCookie",
                json!({ "cookie": state_cookie_to_bidi(cookie) }),
            )
            .await?;
    }

    let context = ff.context.clone();
    for origin in &state.origins {
        if origin.local_storage.is_empty() && origin.session_storage.is_empty() {
            continue;
        }
        let nav_url = format!("{}/", origin.origin.trim_end_matches('/'));
        ff.bidi
            .send(
                "browsingContext.navigate",
                json!({ "context": context, "url": nav_url, "wait": "complete" }),
            )
            .await?;
        ff.refs.clear();

        let payload = serde_json::to_string(origin).map_err(|e| e.to_string())?;
        ff.bidi
            .call_function_in_sandbox(
                &context,
                "agent-browser",
                RESTORE_STORAGE_JS,
                vec![json!(origin.origin), json!(payload)],
            )
            .await
            .map_err(|e| format!("Failed to restore storage for {}: {e}", origin.origin))?;
    }

    Ok(())
}

/// Loads a `state.rs` JSON file, dropping Chrome-only cookie fields, like Chrome's
/// `handle_state_load`. `--session-name` restore reuses this through `load_state_path`.
pub async fn state_load(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(Value::as_str)
        .ok_or("Missing 'path' parameter")?;
    let ff = session(state)?;
    load_state_path(ff, path).await?;
    Ok(json!({ "loaded": true, "path": path }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_state_cookie_converts_to_bidi_params_without_chrome_only_fields() {
        let json_str = r#"{
            "cookies": [{
                "name": "sid",
                "value": "abc",
                "domain": ".example.com",
                "path": "/app",
                "expires": 1700000000.0,
                "size": 12,
                "httpOnly": true,
                "secure": true,
                "session": false,
                "sameSite": "Lax",
                "priority": "Medium",
                "sourceScheme": "Secure",
                "sourcePort": 443,
                "sameParty": false,
                "partitionKey": {"topLevelSite": "https://example.com"},
                "url": "https://example.com/app"
            }],
            "origins": []
        }"#;
        let parsed: StorageState = serde_json::from_str(json_str).unwrap();
        let params = state_cookie_to_bidi(&parsed.cookies[0]);
        assert_eq!(
            params,
            json!({
                "name": "sid",
                "value": {"type": "string", "value": "abc"},
                "domain": ".example.com",
                "path": "/app",
                "httpOnly": true,
                "secure": true,
                "expiry": 1700000000u64,
                "sameSite": "lax",
            })
        );
        for field in [
            "priority",
            "sourceScheme",
            "sourcePort",
            "sameParty",
            "partitionKey",
            "size",
            "url",
        ] {
            assert!(params.get(field).is_none(), "{field} must be dropped");
        }
    }

    #[test]
    fn cookies_set_sends_the_current_page_host_and_a_string_bytes_value() {
        let params = command_cookie_to_bidi(
            &json!({"name": "demo", "value": "42"}),
            Some("https://example.com"),
        )
        .unwrap();
        assert_eq!(
            params,
            json!({
                "name": "demo",
                "value": {"type": "string", "value": "42"},
                "domain": "example.com",
                "path": "/",
                "httpOnly": false,
                "secure": true,
            })
        );
    }

    #[test]
    fn bidi_cookie_values_decode_from_string_and_base64_bytes_values() {
        let text = bidi_cookie_to_state(&json!({
            "name": "demo",
            "value": {"type": "string", "value": "42"},
            "domain": "example.com",
            "path": "/",
        }));
        assert_eq!(text.value, "42");

        let encoded = bidi_cookie_to_state(&json!({
            "name": "demo",
            "value": {"type": "base64", "value": "NDI="},
            "domain": "example.com",
            "path": "/",
        }));
        assert_eq!(encoded.value, "42");
    }

    #[test]
    fn session_state_cookie_has_no_expiry_field() {
        let cookie = Cookie {
            name: "a".into(),
            value: "b".into(),
            domain: "example.com".into(),
            path: "/".into(),
            expires: -1.0,
            size: 1,
            http_only: false,
            secure: false,
            session: true,
            same_site: None,
        };
        let params = state_cookie_to_bidi(&cookie);
        assert!(params.get("expiry").is_none());
        assert!(params.get("sameSite").is_none());
    }

    #[test]
    fn bidi_cookies_convert_to_the_state_cookie_format() {
        let persistent = bidi_cookie_to_state(&json!({
            "name": "sid",
            "value": "abc",
            "domain": "example.com",
            "path": "/",
            "size": 3,
            "httpOnly": true,
            "secure": true,
            "sameSite": "strict",
            "expiry": 1700000000u64,
        }));
        assert_eq!(
            serde_json::to_value(&persistent).unwrap(),
            json!({
                "name": "sid",
                "value": "abc",
                "domain": "example.com",
                "path": "/",
                "expires": 1700000000.0,
                "size": 3,
                "httpOnly": true,
                "secure": true,
                "session": false,
                "sameSite": "Strict",
            })
        );

        let session = bidi_cookie_to_state(&json!({
            "name": "temp",
            "value": "v",
            "domain": "example.com",
            "path": "/",
            "size": 1,
            "httpOnly": false,
            "secure": false,
        }));
        assert_eq!(session.expires, -1.0);
        assert!(session.session);
        assert!(session.same_site.is_none());
    }

    #[test]
    fn chrome_state_origins_roundtrip_through_the_state_format() {
        let json_str = r#"{
            "cookies": [],
            "origins": [{
                "origin": "https://example.com",
                "localStorage": [{"name": "key", "value": "value"}],
                "sessionStorage": [{"name": "s", "value": "t"}]
            }]
        }"#;
        let parsed: StorageState = serde_json::from_str(json_str).unwrap();
        assert_eq!(parsed.origins[0].local_storage.len(), 1);
        assert_eq!(parsed.origins[0].local_storage[0].name, "key");
        assert_eq!(parsed.origins[0].local_storage[0].value, "value");
        assert_eq!(parsed.origins[0].session_storage[0].name, "s");
        let reparsed: StorageState =
            serde_json::from_str(&serde_json::to_string(&parsed).unwrap()).unwrap();
        assert_eq!(reparsed.origins[0].origin, "https://example.com");
    }
}
