use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use super::cdp::client::CdpClient;
use super::cdp::types::{
    AttachToTargetParams, AttachToTargetResult, CloseTargetParams, CreateTargetParams,
    CreateTargetResult, EvaluateParams,
};
use super::cookies::{self, Cookie};
use crate::validation::{is_valid_session_name, sanitize_session_component, session_name_error};

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

fn collect_frame_origins(tree: &Value, origins: &mut HashSet<String>) {
    if let Some(frame) = tree.get("frame") {
        if let Some(url_str) = frame.get("url").and_then(|v| v.as_str()) {
            if let Ok(parsed) = url::Url::parse(url_str) {
                let origin = parsed.origin().ascii_serialization();
                if origin != "null" && !origin.is_empty() {
                    origins.insert(origin);
                }
            }
        }
    }
    if let Some(children) = tree.get("childFrames").and_then(|v| v.as_array()) {
        for child in children {
            collect_frame_origins(child, origins);
        }
    }
}

/// Parse the JS-evaluated origin storage data into an OriginStorage struct.
fn parse_origin_storage(data: &Value) -> Option<OriginStorage> {
    if !data.is_object() {
        return None;
    }
    let origin = data
        .get("origin")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if origin.is_empty() || origin == "null" {
        return None;
    }
    let local_storage: Vec<StorageEntry> = data
        .get("localStorage")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    let session_storage: Vec<StorageEntry> = data
        .get("sessionStorage")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Some(OriginStorage {
        origin,
        local_storage,
        session_storage,
    })
}

/// Evaluate the storage-collection JS snippet and parse the result.
async fn eval_origin_storage(
    client: &CdpClient,
    session_id: &str,
    origin_js: &str,
) -> Option<OriginStorage> {
    let result = client
        .send_command_typed::<_, super::cdp::types::EvaluateResult>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: origin_js.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
        .ok()?;
    let data = result.result.value.unwrap_or(Value::Null);
    parse_origin_storage(&data)
}

/// Create a temporary CDP target, navigate it to each origin to collect localStorage,
/// then close it. Uses Fetch interception to serve blank HTML instead of making real
/// network requests.
async fn collect_storage_via_temp_target(
    client: &CdpClient,
    origins: &[String],
    origin_js: &str,
) -> Result<Vec<OriginStorage>, String> {
    let create_result: CreateTargetResult = client
        .send_command_typed(
            "Target.createTarget",
            &CreateTargetParams {
                url: "about:blank".to_string(),
            },
            None,
        )
        .await?;

    let target_id = create_result.target_id;

    // Ensure the target is closed even if attach or later steps fail
    let result = collect_storage_in_target(client, &target_id, origins, origin_js).await;

    let _ = client
        .send_command_typed::<_, Value>(
            "Target.closeTarget",
            &CloseTargetParams { target_id },
            None,
        )
        .await;

    result
}

async fn collect_storage_in_target(
    client: &CdpClient,
    target_id: &str,
    origins: &[String],
    origin_js: &str,
) -> Result<Vec<OriginStorage>, String> {
    let attach_result: AttachToTargetResult = client
        .send_command_typed(
            "Target.attachToTarget",
            &AttachToTargetParams {
                target_id: target_id.to_string(),
                flatten: true,
            },
            None,
        )
        .await?;

    let temp_session = &attach_result.session_id;

    client
        .send_command_no_params("Page.enable", Some(temp_session))
        .await?;
    client
        .send_command_no_params("Runtime.enable", Some(temp_session))
        .await?;

    // Blank HTML response body, pre-encoded to avoid repeated base64 work per request
    let blank_html_b64 = base64::engine::general_purpose::STANDARD.encode("<html></html>");

    let _ = client
        .send_command(
            "Fetch.enable",
            Some(json!({ "patterns": [{ "urlPattern": "*" }] })),
            Some(temp_session),
        )
        .await;

    let mut event_rx = client.subscribe();
    let mut results = Vec::new();

    for target_origin in origins {
        let nav_url = format!("{}/", target_origin.trim_end_matches('/'));
        if client
            .send_command(
                "Page.navigate",
                Some(json!({ "url": nav_url })),
                Some(temp_session),
            )
            .await
            .is_err()
        {
            continue;
        }

        // Fulfill intercepted requests with blank HTML until the page loads
        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        let mut page_loaded = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(tokio::time::Duration::from_secs(2), event_rx.recv()).await {
                Ok(Ok(evt)) if evt.session_id.as_deref() == Some(temp_session) => {
                    if evt.method == "Fetch.requestPaused" {
                        if let Some(request_id) =
                            evt.params.get("requestId").and_then(|v| v.as_str())
                        {
                            let _ = client
                                .send_command(
                                    "Fetch.fulfillRequest",
                                    Some(json!({
                                        "requestId": request_id,
                                        "responseCode": 200,
                                        "responseHeaders": [
                                            { "name": "Content-Type", "value": "text/html" }
                                        ],
                                        "body": &blank_html_b64
                                    })),
                                    Some(temp_session),
                                )
                                .await;
                        }
                    } else if evt.method == "Page.loadEventFired" {
                        page_loaded = true;
                        break;
                    }
                }
                Ok(Ok(_)) => continue,  // event for a different session
                Ok(Err(_)) => continue, // lagged or closed — retry within deadline
                Err(_) => break,        // outer timeout elapsed
            }
        }

        if !page_loaded {
            continue;
        }

        if let Some(storage) = eval_origin_storage(client, temp_session, origin_js).await {
            if !storage.local_storage.is_empty() || !storage.session_storage.is_empty() {
                results.push(storage);
            }
        }
    }

    Ok(results)
}

pub async fn save_state(
    client: &CdpClient,
    session_id: &str,
    path: Option<&str>,
    session_name: Option<&str>,
    session_id_str: &str,
    visited_origins: &HashSet<String>,
) -> Result<String, String> {
    let cookies = cookies::get_all_cookies(client, session_id).await?;

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

    // Merge visited origins with current frame tree origins
    let mut all_origins = visited_origins.clone();
    if let Ok(tree_result) = client
        .send_command_no_params("Page.getFrameTree", Some(session_id))
        .await
    {
        if let Some(tree) = tree_result.get("frameTree") {
            collect_frame_origins(tree, &mut all_origins);
        }
    }

    // 1. Collect localStorage from the current page
    let mut origins = Vec::new();
    let mut current_origin = String::new();

    if let Some(storage) = eval_origin_storage(client, session_id, origin_js).await {
        current_origin = storage.origin.clone();
        if !storage.local_storage.is_empty() || !storage.session_storage.is_empty() {
            origins.push(storage);
        }
    }

    // 2. Collect localStorage from remaining origins via a disposable temp target
    all_origins.remove(&current_origin);
    if !all_origins.is_empty() {
        let remaining: Vec<String> = all_origins.into_iter().collect();
        if let Ok(temp_origins) =
            collect_storage_via_temp_target(client, &remaining, origin_js).await
        {
            origins.extend(temp_origins);
        }
    }

    let state = StorageState { cookies, origins };
    let json_str = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("Failed to serialize state: {}", e))?;

    let mut save_path = match path {
        Some(p) => p.to_string(),
        None => {
            let dir = get_sessions_dir();
            let _ = fs::create_dir_all(&dir);
            let name = session_name.unwrap_or("default");
            dir.join(format!("{}.json", state_base_name(name, session_id_str)?))
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

pub async fn save_auto_state_transactional(
    client: &CdpClient,
    session_id: &str,
    session_name: &str,
    session_id_str: &str,
    visited_origins: &HashSet<String>,
) -> Result<String, String> {
    let base_name = state_base_name(session_name, session_id_str)?;

    let dir = get_sessions_dir();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create state directory {}: {}", dir.display(), e))?;

    let tmp_dir = dir.join(".tmp");
    fs::create_dir_all(&tmp_dir).map_err(|e| {
        format!(
            "Failed to create temporary state directory {}: {}",
            tmp_dir.display(),
            e
        )
    })?;

    let final_json_path = dir.join(format!("{}.json", base_name));
    let final_path = if std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").is_ok() {
        PathBuf::from(format!("{}.enc", final_json_path.to_string_lossy()))
    } else {
        final_json_path
    };
    let candidate_json_path = tmp_dir.join(format!(
        "{}-candidate-{}.json",
        base_name,
        std::process::id()
    ));
    let candidate_arg = candidate_json_path.to_string_lossy().to_string();

    let candidate_path = save_state(
        client,
        session_id,
        Some(&candidate_arg),
        Some(session_name),
        session_id_str,
        visited_origins,
    )
    .await?;

    if let Err(err) = validate_state_file(&candidate_path) {
        let _ = fs::remove_file(&candidate_path);
        return Err(err);
    }

    let previous_path = PathBuf::from(format!("{}.previous", final_path.to_string_lossy()));
    if final_path.exists() {
        let _ = fs::remove_file(&previous_path);
        fs::rename(&final_path, &previous_path).map_err(|e| {
            format!(
                "Failed to rotate previous state {} to {}: {}",
                final_path.display(),
                previous_path.display(),
                e
            )
        })?;
    }

    let candidate = PathBuf::from(&candidate_path);
    if let Err(err) = fs::rename(&candidate, &final_path) {
        if previous_path.exists() && !final_path.exists() {
            let _ = fs::rename(&previous_path, &final_path);
        }
        return Err(format!(
            "Failed to promote state {} to {}: {}",
            candidate.display(),
            final_path.display(),
            err
        ));
    }
    if previous_path.exists() {
        let _ = fs::remove_file(&previous_path);
    }

    Ok(final_path.to_string_lossy().to_string())
}

fn read_state_json(path: &str) -> Result<String, String> {
    if is_encrypted_state(std::path::Path::new(path)) {
        let key = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").map_err(|_| {
            "Encrypted state file requires AGENT_BROWSER_ENCRYPTION_KEY".to_string()
        })?;
        let data =
            fs::read(path).map_err(|e| format!("Failed to read state from {}: {}", path, e))?;
        let decrypted = decrypt_data(&data, &key)?;
        Ok(String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted state is not valid UTF-8: {}", e))?)
    } else {
        match fs::read_to_string(path) {
            Ok(s) => Ok(s),
            Err(e) => {
                if let Ok(key) = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY") {
                    let enc_path = format!("{}.enc", path);
                    if let Ok(data) = fs::read(&enc_path) {
                        let decrypted = decrypt_data(&data, &key)?;
                        Ok(String::from_utf8(decrypted)
                            .map_err(|de| format!("Decrypted state is not valid UTF-8: {}", de))?)
                    } else {
                        Err(format!("Failed to read state from {}: {}", path, e))
                    }
                } else {
                    Err(format!("Failed to read state from {}: {}", path, e))
                }
            }
        }
    }
}

pub fn validate_state_file(path: &str) -> Result<(), String> {
    let json_str = read_state_json(path)?;
    let _: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {}", e))?;
    Ok(())
}

/// Restore cookies and per-origin storage from a state file.
///
/// Storage is written through CDP DOMStorage, so no page script sees the values and Chrome only accepts the write when a frame is at the saved origin. Origins that redirect elsewhere or fail to load are skipped and returned.
pub async fn load_state(
    client: &CdpClient,
    session_id: &str,
    path: &str,
) -> Result<Vec<String>, String> {
    let json_str = read_state_json(path)?;

    let state: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {}", e))?;

    if !state.cookies.is_empty() {
        let cookie_values: Vec<Value> = state
            .cookies
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
            .collect();
        cookies::set_cookies(client, session_id, cookie_values, None).await?;
    }

    let mut skipped = Vec::new();
    for origin in &state.origins {
        if origin.local_storage.is_empty() && origin.session_storage.is_empty() {
            continue;
        }
        if restore_origin_storage(client, session_id, origin)
            .await
            .is_err()
        {
            skipped.push(origin.origin.clone());
        }
    }

    Ok(skipped)
}

async fn restore_origin_storage(
    client: &CdpClient,
    session_id: &str,
    origin: &OriginStorage,
) -> Result<(), String> {
    let security_origin = url::Url::parse(&origin.origin)
        .ok()
        .map(|u| u.origin())
        .filter(|o| o.is_tuple())
        .map(|o| o.ascii_serialization())
        .unwrap_or_else(|| origin.origin.trim_end_matches('/').to_string());
    client
        .send_command(
            "Page.navigate",
            Some(json!({ "url": format!("{}/", security_origin) })),
            Some(session_id),
        )
        .await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let entries = origin
        .local_storage
        .iter()
        .map(|e| (true, e))
        .chain(origin.session_storage.iter().map(|e| (false, e)));
    for (is_local_storage, entry) in entries {
        client
            .send_command(
                "DOMStorage.setDOMStorageItem",
                Some(json!({
                    "storageId": {
                        "securityOrigin": security_origin,
                        "isLocalStorage": is_local_storage,
                    },
                    "key": entry.name,
                    "value": entry.value,
                })),
                Some(session_id),
            )
            .await?;
    }
    Ok(())
}

fn is_state_file(path: &std::path::Path) -> bool {
    let fname = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    fname.ends_with(".json")
        || fname.ends_with(".json.enc")
        || fname.ends_with(".json.previous")
        || fname.ends_with(".json.enc.previous")
}

fn is_encrypted_state(path: &std::path::Path) -> bool {
    let path = path.to_string_lossy();
    path.ends_with(".json.enc") || path.ends_with(".json.enc.previous")
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
    let encrypted = is_encrypted_state(std::path::Path::new(path));
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

/// Deletes the state file at `path`, or every state file saved under `name`, or all of them when `all` is set; a name wins over `all`.
pub fn state_clear(path: Option<&str>, name: Option<&str>, all: bool) -> Result<Value, String> {
    if let Some(p) = path {
        fs::remove_file(p).map_err(|e| format!("Failed to delete state: {}", e))?;
        return Ok(json!({ "deleted": p }));
    }
    if name.is_none() && !all {
        return Err("state clear needs a session name or --all".to_string());
    }

    let files = matching_state_files(&get_sessions_dir(), name)?;
    for path in &files {
        fs::remove_file(path)
            .map_err(|e| format!("Failed to delete state {}: {}", path.display(), e))?;
    }

    Ok(json!({ "deleted": files.len() }))
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

fn state_base_name(session_name: &str, session_id: &str) -> Result<String, String> {
    if !is_valid_session_name(session_name) {
        return Err(session_name_error(session_name));
    }
    if !is_valid_session_name(session_id) {
        return Err(format!("Invalid state session ID: {}", session_id));
    }
    Ok(format!("v2@.{}.{}", session_name, session_id))
}

fn state_file_owner(fname: &str, name: &str) -> Option<bool> {
    let stem = [".json.enc.previous", ".json.previous", ".json.enc", ".json"]
        .iter()
        .find_map(|suffix| fname.strip_suffix(suffix))?;

    if let Some((owner, id)) = stem
        .strip_prefix("v2@.")
        .and_then(|rest| rest.split_once('.'))
    {
        if is_valid_session_name(owner) && is_valid_session_name(id) {
            return (owner == name).then_some(true);
        }
    }

    let mut owners = HashSet::new();
    for (index, _) in stem.match_indices('-') {
        let (owner, rest) = stem.split_at(index);
        if is_valid_session_name(owner) && rest.len() > 1 {
            owners.insert(owner);
        }
    }
    if let Some((owner, id)) = stem.split_once('.') {
        if is_valid_session_name(owner) && !id.is_empty() {
            owners.insert(owner);
        }
    }

    owners.contains(name).then_some(owners.len() == 1)
}

fn matching_state_files(dir: &std::path::Path, name: Option<&str>) -> Result<Vec<PathBuf>, String> {
    if let Some(name) = name {
        if !is_valid_session_name(name) {
            return Err(session_name_error(name));
        }
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!(
                "Failed to read sessions dir {}: {}",
                dir.display(),
                e
            ))
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("Failed to read sessions dir {}: {}", dir.display(), e))?;
        let path = entry.path();
        if !is_state_file(&path) {
            continue;
        }
        if let Some(name) = name {
            match state_file_owner(&entry.file_name().to_string_lossy(), name) {
                Some(true) => {}
                Some(false) => {
                    return Err(format!(
                        "Ambiguous ownership of saved state file {}. Preserve it and any backups securely outside the sessions directory, then confirm ownership before loading with `state load`. If ownership is uncertain, do not load it. Load only into the intended session and namespace; see https://agent-browser.dev/sessions. No files were changed.",
                        path.display()
                    ));
                }
                None => continue,
            }
        }
        files.push(path);
    }
    Ok(files)
}

/// Find the newest active state file, or report ambiguous ownership and read errors.
pub fn find_auto_state_file(session_name: &str) -> Result<Option<String>, String> {
    let files = matching_state_files(&get_sessions_dir(), Some(session_name))?;
    let mut best_path: Option<(PathBuf, std::time::SystemTime)> = None;
    for path in files {
        if path.to_string_lossy().ends_with(".previous") {
            continue;
        }
        let modified = fs::metadata(&path)
            .map_err(|e| format!("Failed to read state metadata {}: {}", path.display(), e))?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        if best_path.as_ref().is_none_or(|(_, m)| modified > *m) {
            best_path = Some((path, modified));
        }
    }
    Ok(best_path.map(|(p, _)| p.to_string_lossy().to_string()))
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
            let name = cmd.get("sessionName").and_then(|v| v.as_str());
            let all = cmd.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(state_clear(path, name, all))
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

/// Return the agent-browser state root (`~/.agent-browser`, falling back to
/// `<tempdir>/agent-browser` when the home directory can't be resolved).
/// This is the parent of `sessions/`, auth storage, and the encryption key.
pub fn get_state_dir() -> PathBuf {
    let base = if let Some(home) = std::env::home_dir() {
        home.join(".agent-browser")
    } else {
        std::env::temp_dir().join("agent-browser")
    };

    if let Ok(namespace) = std::env::var("AGENT_BROWSER_NAMESPACE") {
        let namespace = sanitize_session_component(&namespace);
        if !namespace.is_empty() {
            return base.join("namespaces").join(namespace).join("state");
        }
    }

    base
}

pub fn get_sessions_dir() -> PathBuf {
    get_state_dir().join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    fn write_state_fixture(path: &std::path::Path) {
        let state = StorageState {
            cookies: vec![Cookie {
                name: "session".to_string(),
                value: "fixture".to_string(),
                domain: ".example.com".to_string(),
                path: "/".to_string(),
                expires: 0.0,
                size: 0,
                http_only: true,
                secure: true,
                session: true,
                same_site: Some("Lax".to_string()),
            }],
            origins: vec![OriginStorage {
                origin: "https://example.com".to_string(),
                local_storage: vec![StorageEntry {
                    name: "token".to_string(),
                    value: "fixture".to_string(),
                }],
                session_storage: vec![],
            }],
        };
        let data = serde_json::to_vec(&state).unwrap();
        let data = if is_encrypted_state(path) {
            encrypt_data(&data, "fixture-key").unwrap()
        } else {
            data
        };
        fs::write(path, data).unwrap();
    }

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
        let result = state_clear(
            Some("/tmp/nonexistent-agent-browser-state-file.json"),
            None,
            false,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_state_file_matcher_includes_transactional_backups() {
        assert!(is_state_file(std::path::Path::new("auth.json")));
        assert!(is_state_file(std::path::Path::new("auth.json.enc")));
        assert!(is_state_file(std::path::Path::new("auth.json.previous")));
        assert!(is_state_file(std::path::Path::new(
            "auth.json.enc.previous"
        )));
        assert!(is_encrypted_state(std::path::Path::new(
            "auth.json.enc.previous"
        )));
    }

    #[cfg(not(windows))]
    #[test]
    fn test_state_clear_removes_transactional_backups() {
        let guard = crate::test_utils::EnvGuard::new(&["HOME", "AGENT_BROWSER_NAMESPACE"]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("HOME", dir.path().to_str().unwrap());
        guard.remove("AGENT_BROWSER_NAMESPACE");

        let sessions = get_sessions_dir();
        fs::create_dir_all(&sessions).unwrap();
        for file in [
            "auth-test-one.json",
            "auth-test-one.json.previous",
            "auth-test-one.json.enc.previous",
        ] {
            write_state_fixture(&sessions.join(file));
        }

        let result = state_clear(None, None, true).unwrap();

        assert_eq!(result["deleted"], 3);
        assert!(!sessions.join("auth-test-one.json").exists());
        assert!(!sessions.join("auth-test-one.json.previous").exists());
        assert!(!sessions.join("auth-test-one.json.enc.previous").exists());
    }

    #[test]
    fn test_state_base_name_uses_validated_v2_components() {
        assert_eq!(
            state_base_name("auth-admin", "browser-one").unwrap(),
            "v2@.auth-admin.browser-one"
        );
        for id in ["", "browser.one", "../browser"] {
            assert!(state_base_name("auth", id).is_err());
        }
        assert!(state_base_name("auth.admin", "browser").is_err());
    }

    #[test]
    fn test_state_file_owner_distinguishes_formats_and_collisions() {
        for (file, key, expected) in [
            ("auth-browser.json", "auth", Some(true)),
            ("auth-browser.json", "auth-browser", None),
            ("auth-browser-one.json", "auth", Some(false)),
            ("auth-browser-one.json", "auth-browser", Some(false)),
            ("auth-browser-one-two.json", "auth-browser-one", Some(false)),
            ("auth-browser.one.json", "auth", Some(false)),
            ("auth-browser.one.json", "auth-browser", Some(false)),
            ("auth-browser.json.json", "auth", Some(false)),
            ("auth-browser.json.json", "auth-browser", Some(false)),
            ("auth-browser.release.v1.json", "auth", Some(false)),
            ("auth-browser.release.v1.json", "auth-browser", Some(false)),
            ("auth.browser-one.json", "auth", Some(true)),
            ("auth.browser-one.json", "auth-browser", None),
            ("auth.browser.one.json", "auth", Some(true)),
            ("auth..json", "auth", None),
            ("auth...json", "auth", Some(true)),
            ("auth.browser@one.json", "auth", Some(true)),
            ("v2.auth.browser.json", "v2", Some(true)),
            ("v2.auth.browser.json", "auth", None),
            ("v2@.auth.browser.json", "auth", Some(true)),
            ("v2@.auth.browser.json", "v2", None),
            (
                "v2@.auth-browser.session-one.json",
                "auth-browser",
                Some(true),
            ),
            ("v2@.auth-browser.session-one.json", "auth", None),
            ("v2@.auth-browser.session-one.json", "v2", None),
            ("v2.browser-one.json", "v2", Some(true)),
            ("v2.browser-one.json", "browser-one", None),
        ] {
            assert_eq!(state_file_owner(file, key), expected, "{file}, {key}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn test_state_restore_and_clear_unambiguous_files() {
        let guard = crate::test_utils::EnvGuard::new(&[
            "HOME",
            "AGENT_BROWSER_NAMESPACE",
            "AGENT_BROWSER_ENCRYPTION_KEY",
        ]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("HOME", dir.path().to_str().unwrap());
        guard.remove("AGENT_BROWSER_NAMESPACE");
        guard.set("AGENT_BROWSER_ENCRYPTION_KEY", "fixture-key");

        let sessions = get_sessions_dir();
        fs::create_dir_all(&sessions).unwrap();
        assert!(find_auto_state_file("auth").unwrap().is_none());
        let now = std::time::SystemTime::now();
        for (i, file) in [
            "auth-old.json",
            "auth.browser.one.json.enc",
            "auth.browser-old.json.enc",
            "v2@.auth.browser-current.json",
            "v2@.auth.browser-current-encrypted.json.enc",
        ]
        .iter()
        .enumerate()
        {
            let path = sessions.join(file);
            write_state_fixture(&path);
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(now + std::time::Duration::from_secs(i as u64 + 1))
                .unwrap();
            let found = find_auto_state_file("auth").unwrap().unwrap();
            assert_eq!(PathBuf::from(&found), path);
            assert!(validate_state_file(&found).is_ok());
        }

        for file in [
            "v2@.auth.backup.json.previous",
            "v2@.auth.backup.json.enc.previous",
            "v2@.auth-admin.browser.json",
        ] {
            write_state_fixture(&sessions.join(file));
        }
        assert!(find_auto_state_file("auth")
            .unwrap()
            .unwrap()
            .ends_with("v2@.auth.browser-current-encrypted.json.enc"));
        assert!(find_auto_state_file("auth-admin")
            .unwrap()
            .unwrap()
            .ends_with("v2@.auth-admin.browser.json"));
        assert!(find_auto_state_file("auth-ad").unwrap().is_none());
        assert!(dispatch_state_command(&json!({"action":"state_clear"}))
            .unwrap()
            .is_err());
        let cleared = dispatch_state_command(&json!({"action":"state_clear","sessionName":"auth"}))
            .unwrap()
            .unwrap();
        assert_eq!(cleared["deleted"], 7);
        assert!(sessions.join("v2@.auth-admin.browser.json").exists());
        assert!(find_auto_state_file("auth").unwrap().is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn test_old_v2_key_does_not_collide_with_new_marker() {
        let guard = crate::test_utils::EnvGuard::new(&[
            "HOME",
            "AGENT_BROWSER_NAMESPACE",
            "AGENT_BROWSER_ENCRYPTION_KEY",
        ]);
        guard.remove("AGENT_BROWSER_NAMESPACE");
        guard.set("AGENT_BROWSER_ENCRYPTION_KEY", "fixture-key");

        for suffix in [".json", ".json.enc", ".json.previous", ".json.enc.previous"] {
            for first in ["auth", "v2"] {
                let dir = tempfile::tempdir().unwrap();
                guard.set("HOME", dir.path().to_str().unwrap());
                let sessions = get_sessions_dir();
                fs::create_dir_all(&sessions).unwrap();
                let legacy = sessions.join(format!("v2.auth.browser{suffix}"));
                let current = sessions.join(format!("v2@.auth.browser{suffix}"));
                for path in [&legacy, &current] {
                    write_state_fixture(path);
                    assert!(validate_state_file(path.to_str().unwrap()).is_ok());
                }

                for (key, path) in [("v2", &legacy), ("auth", &current)] {
                    let found = find_auto_state_file(key).unwrap();
                    if suffix.ends_with(".previous") {
                        assert!(found.is_none());
                    } else {
                        assert_eq!(PathBuf::from(found.unwrap()), *path);
                    }
                }

                let (removed, kept, second) = if first == "auth" {
                    (&current, &legacy, "v2")
                } else {
                    (&legacy, &current, "auth")
                };
                assert_eq!(state_clear(None, Some(first), false).unwrap()["deleted"], 1);
                assert!(!removed.exists());
                assert!(kept.exists());
                assert_eq!(
                    state_clear(None, Some(second), false).unwrap()["deleted"],
                    1
                );
                assert!(!kept.exists());
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn test_ambiguous_active_and_backup_files_block_restore_and_named_clear() {
        let guard = crate::test_utils::EnvGuard::new(&[
            "HOME",
            "AGENT_BROWSER_NAMESPACE",
            "AGENT_BROWSER_ENCRYPTION_KEY",
        ]);
        guard.set("AGENT_BROWSER_NAMESPACE", "team-one");
        guard.set("AGENT_BROWSER_ENCRYPTION_KEY", "fixture-key");

        for stem in [
            "auth-worker-one",
            "auth-worker.one",
            "auth-worker.json",
            "auth-worker.release.v1",
        ] {
            for suffix in [".json", ".json.enc", ".json.previous", ".json.enc.previous"] {
                let dir = tempfile::tempdir().unwrap();
                guard.set("HOME", dir.path().to_str().unwrap());
                let sessions = get_sessions_dir();
                fs::create_dir_all(&sessions).unwrap();
                let definite = sessions.join("v2@.auth.browser.json");
                let definite_prefix = sessions.join("v2@.auth-worker.browser.json");
                let ambiguous = sessions.join(format!("{stem}{suffix}"));
                let other = sessions.join("v2@.other.browser.json");
                for path in [&definite, &definite_prefix, &ambiguous, &other] {
                    write_state_fixture(path);
                    assert!(validate_state_file(path.to_str().unwrap()).is_ok());
                }

                for key in ["auth", "auth-worker"] {
                    let restore_error = find_auto_state_file(key).unwrap_err();
                    assert!(restore_error.contains(ambiguous.to_str().unwrap()));
                    assert!(restore_error.contains("Preserve it and any backups"));
                    assert!(restore_error.contains("confirm ownership before loading"));
                    assert!(restore_error.contains("If ownership is uncertain, do not load it"));
                    assert!(restore_error.contains("intended session and namespace"));
                    assert!(restore_error.contains("https://agent-browser.dev/sessions"));
                    let clear_error = state_clear(None, Some(key), false).unwrap_err();
                    assert_eq!(clear_error, restore_error);
                    for path in [&definite, &definite_prefix, &ambiguous, &other] {
                        assert!(path.exists());
                    }
                }
            }
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn test_state_lookup_distinguishes_missing_from_errors() {
        let guard = crate::test_utils::EnvGuard::new(&["HOME", "AGENT_BROWSER_NAMESPACE"]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("HOME", dir.path().to_str().unwrap());
        guard.remove("AGENT_BROWSER_NAMESPACE");
        assert!(find_auto_state_file("auth").unwrap().is_none());
        assert!(find_auto_state_file("invalid.name").is_err());

        let sessions = get_sessions_dir();
        fs::create_dir_all(sessions.parent().unwrap()).unwrap();
        fs::write(&sessions, "not a directory").unwrap();
        assert!(find_auto_state_file("auth").is_err());
        assert!(state_clear(None, Some("auth"), false).is_err());
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
    fn test_get_state_dir_namespace_scopes_sessions() {
        let _guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_NAMESPACE"]);
        _guard.set("AGENT_BROWSER_NAMESPACE", "Worktree: One");

        let dir = get_state_dir();
        let expected_state_suffix = PathBuf::from(".agent-browser")
            .join("namespaces")
            .join("worktree-one")
            .join("state");
        let expected_sessions_suffix = expected_state_suffix.join("sessions");

        assert!(dir.ends_with(expected_state_suffix));
        assert!(get_sessions_dir().ends_with(expected_sessions_suffix));
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
