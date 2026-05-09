use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use super::browser::{BrowserManager, ContextInfo};
use super::cdp::client::CdpClient;
use super::cdp::types::{
    AttachToTargetParams, AttachToTargetResult, CloseTargetParams, CreateTargetParams,
    CreateTargetResult, EvaluateParams, GetCookiesParams, SetCookiesParams,
};
use super::cookies::{self, Cookie};

/// Snapshot of cookies and localStorage/sessionStorage for a single BrowserContext.
///
/// The `ref_id` field is informational only — after a `load_state` call the
/// context is re-created by Chrome and receives a new CDP id, so the `c<N>`
/// counter restarts. The `label` is preserved and used to re-create the context
/// with the same human-readable name.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    /// Stable ref at save time (`"c1"`, `"c2"`, …). Informational — not
    /// restored on load; the new context gets the next available `c<N>`.
    pub ref_id: String,
    /// Human-readable label assigned at `context new --label <name>`, if any.
    pub label: Option<String>,
    /// Cookies scoped to this BrowserContext.
    pub cookies: Vec<Cookie>,
    /// Per-origin localStorage/sessionStorage for this context.
    pub origins: Vec<OriginStorage>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageState {
    pub cookies: Vec<Cookie>,
    pub origins: Vec<OriginStorage>,
    /// Per-context snapshots. Absent in old state files — `#[serde(default)]`
    /// ensures backward compatibility: old files deserialise with an empty vec.
    #[serde(default)]
    pub contexts: Vec<ContextSnapshot>,
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
///
/// `browser_context_id` scopes the target to a specific BrowserContext so that
/// localStorage collected belongs to that context. Pass `None` for the default context.
async fn collect_storage_via_temp_target(
    client: &CdpClient,
    origins: &[String],
    origin_js: &str,
    browser_context_id: Option<&str>,
) -> Result<Vec<OriginStorage>, String> {
    let create_result: CreateTargetResult = client
        .send_command_typed(
            "Target.createTarget",
            &CreateTargetParams {
                url: "about:blank".to_string(),
                browser_context_id: browser_context_id.map(|s| s.to_string()),
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

/// Collect cookies and localStorage/sessionStorage for a single BrowserContext.
///
/// Cookies are fetched via `Network.getCookies` with `browserContextId` scoping.
/// Storage is collected by opening a temporary target inside the context and
/// navigating it to each origin with active data (all origins visited during the
/// session that have not yet been deduplicated — we use a blank-HTML approach so
/// no real network requests are made).
async fn collect_context_snapshot(
    client: &CdpClient,
    ctx: &ContextInfo,
    origin_js: &str,
) -> Result<ContextSnapshot, String> {
    // Recolher cookies do contexto usando browserContextId
    let result = client
        .send_command_typed::<_, Value>(
            "Network.getCookies",
            &GetCookiesParams {
                urls: None,
                browser_context_id: Some(ctx.browser_context_id.clone()),
            },
            None,
        )
        .await
        .unwrap_or(Value::Null);

    let cookies: Vec<Cookie> = result
        .get("cookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Collect origins that belong to this context by inspecting pages in the
    // context. We open a temp target scoped to the context and iterate the
    // unique origins from the cookie domains.
    let cookie_origins: Vec<String> = {
        let mut seen = HashSet::new();
        for c in &cookies {
            let domain = c.domain.trim_start_matches('.');
            if !domain.is_empty() {
                let origin = if c.secure {
                    format!("https://{}", domain)
                } else {
                    format!("http://{}", domain)
                };
                seen.insert(origin);
            }
        }
        seen.into_iter().collect()
    };

    let origins = if !cookie_origins.is_empty() {
        collect_storage_via_temp_target(
            client,
            &cookie_origins,
            origin_js,
            Some(&ctx.browser_context_id),
        )
        .await
        .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(ContextSnapshot {
        ref_id: ctx.ref_id.clone(),
        label: ctx.label.clone(),
        cookies,
        origins,
    })
}

pub async fn save_state(
    client: &CdpClient,
    session_id: &str,
    path: Option<&str>,
    session_name: Option<&str>,
    session_id_str: &str,
    visited_origins: &HashSet<String>,
    contexts: &[ContextInfo],
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
            collect_storage_via_temp_target(client, &remaining, origin_js, None).await
        {
            origins.extend(temp_origins);
        }
    }

    // 3. Collect per-context state (cookies + localStorage)
    let mut context_snapshots: Vec<ContextSnapshot> = Vec::new();
    for ctx in contexts {
        match collect_context_snapshot(client, ctx, origin_js).await {
            Ok(snapshot) => context_snapshots.push(snapshot),
            Err(e) => {
                eprintln!(
                    "Warning: failed to collect state for context {} ({}): {}",
                    ctx.ref_id,
                    ctx.label.as_deref().unwrap_or("no label"),
                    e
                );
            }
        }
    }

    let state = StorageState {
        cookies,
        origins,
        contexts: context_snapshots,
    };
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
    let json_str = read_state_file(path)?;

    let state: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {}", e))?;

    // Carregar cookies globais
    if !state.cookies.is_empty() {
        let cookie_values: Vec<Value> = state
            .cookies
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
            .collect();
        cookies::set_cookies(client, session_id, cookie_values, None).await?;
    }

    // Carregar localStorage/sessionStorage globais
    restore_origins_into_session(client, session_id, &state.origins).await;

    Ok(())
}

/// Load state from a file AND restore BrowserContext snapshots into `mgr`.
///
/// Global cookies and storage are applied to the current active session (same
/// as [`load_state`]). For each [`ContextSnapshot`] in the file a new
/// BrowserContext is created via `Target.createBrowserContext`, its cookies are
/// set with `Network.setCookies` scoped to the new context, and its
/// localStorage is written by opening a temporary target inside the context.
///
/// If a context snapshot fails (e.g. CDP error), a warning is printed and the
/// next snapshot is still attempted — one failure does not abort the whole restore.
pub async fn load_state_into_manager(mgr: &mut BrowserManager, path: &str) -> Result<(), String> {
    let session_id = mgr.active_session_id()?.to_string();
    let client = mgr.client.clone();

    // --- Decrypt / read file (same logic as load_state) ---
    let json_str = read_state_file(path)?;

    let state: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {}", e))?;

    // --- Restore global cookies ---
    if !state.cookies.is_empty() {
        let cookie_values: Vec<Value> = state
            .cookies
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
            .collect();
        cookies::set_cookies(&client, &session_id, cookie_values, None).await?;
    }

    // --- Restore global localStorage/sessionStorage ---
    restore_origins_into_session(&client, &session_id, &state.origins).await;

    // --- Restore per-context snapshots ---
    for snapshot in &state.contexts {
        match restore_context_snapshot(mgr, snapshot).await {
            Ok(_) => {}
            Err(e) => {
                eprintln!(
                    "Warning: failed to restore context snapshot {} ({}): {}",
                    snapshot.ref_id,
                    snapshot.label.as_deref().unwrap_or("no label"),
                    e
                );
            }
        }
    }

    Ok(())
}

/// Restore a single [`ContextSnapshot`]: create a new BrowserContext, set
/// cookies scoped to it, then write localStorage via a temp target.
async fn restore_context_snapshot(
    mgr: &mut BrowserManager,
    snapshot: &ContextSnapshot,
) -> Result<(), String> {
    // Criar novo context (obtém novo ref_id e cdp_id)
    let result = mgr.context_new(snapshot.label.as_deref()).await?;
    let cdp_id = result
        .get("browserContextId")
        .and_then(|v| v.as_str())
        .ok_or("context_new did not return browserContextId")?
        .to_string();

    let client = mgr.client.clone();

    // Restaurar cookies com browserContextId
    if !snapshot.cookies.is_empty() {
        let cookie_values: Vec<Value> = snapshot
            .cookies
            .iter()
            .map(|c| serde_json::to_value(c).unwrap_or(Value::Null))
            .collect();
        client
            .send_command_typed::<_, Value>(
                "Network.setCookies",
                &SetCookiesParams {
                    cookies: cookie_values,
                    browser_context_id: Some(cdp_id.clone()),
                },
                None,
            )
            .await
            .map_err(|e| format!("Network.setCookies failed: {}", e))?;
    }

    // Restaurar localStorage via temp target dentro do contexto
    if !snapshot.origins.is_empty() {
        restore_origins_via_temp_target(&client, &snapshot.origins, &cdp_id).await;
    }

    Ok(())
}

/// Restore localStorage/sessionStorage entries by navigating an existing
/// active session to each origin.
async fn restore_origins_into_session(
    client: &CdpClient,
    session_id: &str,
    origins: &[OriginStorage],
) {
    for origin in origins {
        if origin.local_storage.is_empty() && origin.session_storage.is_empty() {
            continue;
        }

        let navigate_url = format!("{}/", origin.origin.trim_end_matches('/'));
        if client
            .send_command(
                "Page.navigate",
                Some(json!({ "url": navigate_url })),
                Some(session_id),
            )
            .await
            .is_err()
        {
            continue;
        }

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
}

/// Restore localStorage/sessionStorage for a BrowserContext by opening a
/// temporary target inside the context, navigating to each origin, and writing
/// the stored entries via `localStorage.setItem`.
async fn restore_origins_via_temp_target(
    client: &CdpClient,
    origins: &[OriginStorage],
    browser_context_id: &str,
) {
    let create_result = client
        .send_command_typed::<_, CreateTargetResult>(
            "Target.createTarget",
            &CreateTargetParams {
                url: "about:blank".to_string(),
                browser_context_id: Some(browser_context_id.to_string()),
            },
            None,
        )
        .await;

    let target_id = match create_result {
        Ok(r) => r.target_id,
        Err(e) => {
            eprintln!(
                "Warning: failed to create temp target for context restore: {}",
                e
            );
            return;
        }
    };

    let attach_result = client
        .send_command_typed::<_, AttachToTargetResult>(
            "Target.attachToTarget",
            &AttachToTargetParams {
                target_id: target_id.clone(),
                flatten: true,
            },
            None,
        )
        .await;

    let session = match attach_result {
        Ok(r) => r.session_id,
        Err(e) => {
            eprintln!("Warning: failed to attach to temp context target: {}", e);
            let _ = client
                .send_command_typed::<_, Value>(
                    "Target.closeTarget",
                    &CloseTargetParams { target_id },
                    None,
                )
                .await;
            return;
        }
    };

    let _ = client
        .send_command_no_params("Page.enable", Some(&session))
        .await;
    let _ = client
        .send_command_no_params("Runtime.enable", Some(&session))
        .await;

    for origin in origins {
        if origin.local_storage.is_empty() && origin.session_storage.is_empty() {
            continue;
        }

        let navigate_url = format!("{}/", origin.origin.trim_end_matches('/'));
        let _ = client
            .send_command(
                "Page.navigate",
                Some(json!({ "url": navigate_url })),
                Some(&session),
            )
            .await;

        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

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
                    Some(&session),
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
                    Some(&session),
                )
                .await;
        }
    }

    let _ = client
        .send_command_typed::<_, Value>(
            "Target.closeTarget",
            &CloseTargetParams { target_id },
            None,
        )
        .await;
}

/// Read and decrypt (if needed) a state file, returning the JSON string.
fn read_state_file(path: &str) -> Result<String, String> {
    if path.ends_with(".enc") {
        let key = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").map_err(|_| {
            "Encrypted state file requires AGENT_BROWSER_ENCRYPTION_KEY".to_string()
        })?;
        let data =
            fs::read(path).map_err(|e| format!("Failed to read state from {}: {}", path, e))?;
        let decrypted = decrypt_data(&data, &key)?;
        String::from_utf8(decrypted)
            .map_err(|e| format!("Decrypted state is not valid UTF-8: {}", e))
    } else {
        match fs::read_to_string(path) {
            Ok(s) => Ok(s),
            Err(e) => {
                if let Ok(key) = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY") {
                    let enc_path = format!("{}.enc", path);
                    if let Ok(data) = fs::read(&enc_path) {
                        let decrypted = decrypt_data(&data, &key)?;
                        String::from_utf8(decrypted)
                            .map_err(|de| format!("Decrypted state is not valid UTF-8: {}", de))
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
        "summary": format!(
            "{} cookies, {} origins, {} context(s)",
            state.cookies.len(),
            state.origins.len(),
            state.contexts.len()
        ),
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

/// Return the agent-browser state root (`~/.agent-browser`, falling back to
/// `<tempdir>/agent-browser` when the home directory can't be resolved).
/// This is the parent of `sessions/`, auth storage, and the encryption key.
pub fn get_state_dir() -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        home.join(".agent-browser")
    } else {
        std::env::temp_dir().join("agent-browser")
    }
}

pub fn get_sessions_dir() -> PathBuf {
    get_state_dir().join("sessions")
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
            contexts: vec![],
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: StorageState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.cookies.len(), 1);
        assert_eq!(parsed.cookies[0].name, "session");
        assert_eq!(parsed.origins.len(), 1);
        assert_eq!(parsed.origins[0].local_storage.len(), 1);
        assert!(parsed.contexts.is_empty());
    }

    #[test]
    fn test_storage_state_backward_compat() {
        // State files sem o campo `contexts` (versão antiga) devem carregar OK
        let old_json = r#"{"cookies":[],"origins":[]}"#;
        let parsed: StorageState = serde_json::from_str(old_json).unwrap();
        assert!(
            parsed.contexts.is_empty(),
            "contexts deve ser vazio para state files antigos"
        );
    }

    #[test]
    fn test_context_snapshot_serialization() {
        let snapshot = ContextSnapshot {
            ref_id: "c1".to_string(),
            label: Some("staging".to_string()),
            cookies: vec![],
            origins: vec![],
        };
        let json = serde_json::to_string(&snapshot).unwrap();
        let parsed: ContextSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ref_id, "c1");
        assert_eq!(parsed.label.as_deref(), Some("staging"));
        assert!(parsed.cookies.is_empty());
        assert!(parsed.origins.is_empty());
    }

    #[test]
    fn test_storage_state_with_contexts() {
        let state = StorageState {
            cookies: vec![],
            origins: vec![],
            contexts: vec![
                ContextSnapshot {
                    ref_id: "c1".to_string(),
                    label: Some("tenant-a".to_string()),
                    cookies: vec![],
                    origins: vec![],
                },
                ContextSnapshot {
                    ref_id: "c2".to_string(),
                    label: None,
                    cookies: vec![],
                    origins: vec![],
                },
            ],
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: StorageState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.contexts.len(), 2);
        assert_eq!(parsed.contexts[0].ref_id, "c1");
        assert_eq!(parsed.contexts[0].label.as_deref(), Some("tenant-a"));
        assert_eq!(parsed.contexts[1].ref_id, "c2");
        assert!(parsed.contexts[1].label.is_none());
    }

    #[test]
    fn test_storage_state_empty() {
        let state = StorageState {
            cookies: vec![],
            origins: vec![],
            contexts: vec![],
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
