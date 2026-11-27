use aes_gcm::{aead::Aead, aead::KeyInit, Aes256Gcm};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::cdp::client::CdpClient;
use super::cdp::types::{
    AttachToTargetParams, AttachToTargetResult, CloseTargetParams, CreateTargetParams,
    CreateTargetResult, EvaluateParams, GetTargetsResult, TargetInfo,
};
use super::cookies::{self, Cookie};
use crate::validation::{is_valid_session_name, sanitize_session_component, session_name_error};

/// Writes a state file with owner-only permissions on Unix.
pub(crate) fn write_private(path: impl AsRef<std::path::Path>, bytes: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.set_len(0)?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        fs::write(path, bytes)
    }
}

/// Creates a state directory that only the owner can enter on Unix, tightening it if it already exists.
pub(crate) fn create_private_dir(path: &std::path::Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}

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
pub(crate) fn parse_origin_storage(data: &Value) -> Option<OriginStorage> {
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
) -> Result<Option<OriginStorage>, String> {
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
        .await?;
    if let Some(details) = result.exception_details {
        return Err(format!(
            "Failed to read page storage: {}",
            details
                .exception
                .and_then(|e| e.description)
                .unwrap_or(details.text)
        ));
    }
    let data = result.result.value.unwrap_or(Value::Null);
    Ok(parse_origin_storage(&data))
}

/// Temporary targets opened to read other origins' storage, from creation
/// until `Target.closeTarget` has answered.
static STORAGE_TARGETS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Explicit CDP sessions attached to temporary storage targets. The global
/// `Fetch.requestPaused` handler skips these so the collector answers its own
/// paused requests without hitting the network.
static STORAGE_SESSIONS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct StorageTargetGuard(String);

impl StorageTargetGuard {
    fn register(target_id: String) -> Self {
        STORAGE_TARGETS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(target_id.clone());
        Self(target_id)
    }
}

impl Drop for StorageTargetGuard {
    fn drop(&mut self) {
        let mut targets = STORAGE_TARGETS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = targets.iter().position(|id| id == &self.0) {
            targets.swap_remove(pos);
        }
    }
}

struct StorageSessionGuard(String);

impl StorageSessionGuard {
    fn register(session_id: String) -> Self {
        STORAGE_SESSIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(session_id.clone());
        Self(session_id)
    }
}

impl Drop for StorageSessionGuard {
    fn drop(&mut self) {
        let mut sessions = STORAGE_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = sessions.iter().position(|id| id == &self.0) {
            sessions.swap_remove(pos);
        }
    }
}

pub(crate) fn owns_storage_attachment(target_id: &str, waiting_for_debugger: bool) -> bool {
    let targets = STORAGE_TARGETS.lock().unwrap_or_else(|e| e.into_inner());
    is_own_storage_attachment(&targets, target_id, waiting_for_debugger)
}

fn is_own_storage_attachment(
    storage_targets: &[String],
    target_id: &str,
    waiting_for_debugger: bool,
) -> bool {
    !waiting_for_debugger && storage_targets.iter().any(|id| id == target_id)
}

pub(crate) fn owns_storage_session(session_id: &str) -> bool {
    let sessions = STORAGE_SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    sessions.iter().any(|id| id == session_id)
}

fn is_cdp_error(error: &str) -> bool {
    error.starts_with("CDP error (")
}

/// Create the scratch page used to read storage from other origins without
/// foregrounding Chrome, stepping down when a browser rejects `hidden` or
/// `background`.
async fn create_scratch_target(client: &CdpClient) -> Result<CreateTargetResult, String> {
    let [hidden, background_only, plain] =
        [(Some(true), Some(true)), (Some(true), None), (None, None)].map(|(background, hidden)| {
            CreateTargetParams {
                url: "about:blank".to_string(),
                background,
                hidden,
            }
        });
    for params in [&hidden, &background_only] {
        match client
            .send_command_typed("Target.createTarget", params, None)
            .await
        {
            Err(error) if is_cdp_error(&error) => continue,
            result => return result,
        }
    }
    client
        .send_command_typed("Target.createTarget", &plain, None)
        .await
}

/// Scope controlling which open page targets from `Target.getTargets` may
/// contribute origins during `save_state`.
#[derive(Debug, Clone, Default)]
pub struct TargetOriginScope {
    /// Target IDs tracked/owned by this session (`BrowserManager::pages`).
    pub tracked_target_ids: HashSet<String>,
    /// Allow unpinned browser targets opened by attached sessions when this
    /// session launched the browser and is not pinned to a single tab.
    pub include_unpinned_browser_targets: bool,
}

/// Origins of open page targets eligible for `own_session_id`'s state save.
/// Excludes temporary storage targets, internal pages, any tab pinned by
/// another session, and (when `include_unpinned_browser_targets` is false)
/// targets not tracked by this session.
fn origins_from_targets(
    target_infos: &[TargetInfo],
    own_session_id: &str,
    scope: &TargetOriginScope,
) -> HashSet<String> {
    let storage_targets = STORAGE_TARGETS.lock().unwrap_or_else(|e| e.into_inner());
    let mut origins = HashSet::new();
    for target in target_infos {
        if storage_targets.iter().any(|id| id == &target.target_id) {
            continue;
        }
        if !super::browser::should_track_target(target) {
            continue;
        }
        if super::tab_binding::pinned_owner(&target.target_id, own_session_id).is_some() {
            continue;
        }
        if !scope.include_unpinned_browser_targets
            && !scope.tracked_target_ids.contains(&target.target_id)
        {
            continue;
        }
        if let Ok(parsed) = url::Url::parse(&target.url) {
            let origin = parsed.origin().ascii_serialization();
            if origin != "null" && !origin.is_empty() {
                origins.insert(origin);
            }
        }
    }
    origins
}

/// Create a temporary CDP target, navigate it to each origin to collect localStorage,
/// then close it. Uses private Fetch interception and service worker bypass to serve
/// blank HTML instead of making real network requests.
async fn collect_storage_via_temp_target(
    client: &CdpClient,
    origins: &[String],
    origin_js: &str,
) -> Result<Vec<OriginStorage>, String> {
    let create_result = create_scratch_target(client).await?;
    let target_id = create_result.target_id;
    let _target_guard = StorageTargetGuard::register(target_id.clone());

    let attach_result: Result<AttachToTargetResult, String> = client
        .send_command_typed(
            "Target.attachToTarget",
            &AttachToTargetParams {
                target_id: target_id.clone(),
                flatten: true,
            },
            None,
        )
        .await;

    let (result, attached_session) = match attach_result {
        Ok(attach) => {
            let temp_session = attach.session_id;
            let session_guard = StorageSessionGuard::register(temp_session.clone());
            let mut event_rx = client.subscribe_session(&temp_session);
            let res = collect_storage_in_session(
                client,
                &temp_session,
                &mut event_rx,
                origins,
                origin_js,
            )
            .await;
            (res, Some((temp_session, session_guard, event_rx)))
        }
        Err(err) => (Err(err), None),
    };

    // Keep `_session_guard` and the private session subscription alive across
    // `Target.closeTarget` so a late `Fetch.requestPaused` cannot fall through
    // to the shared handler while the target shuts down.
    let _ = client
        .send_command_typed::<_, Value>(
            "Target.closeTarget",
            &CloseTargetParams { target_id },
            None,
        )
        .await;

    if let Some((temp_session, _session_guard, _event_rx)) = attached_session {
        client.unsubscribe_session(&temp_session);
    }

    result
}

/// Answer a paused request on the scratch session. Returns `true` when a
/// `Document` request was fulfilled with the blank HTML stub.
async fn handle_storage_fetch_paused(
    client: &CdpClient,
    temp_session: &str,
    params: &Value,
    blank_html_b64: &str,
) -> bool {
    let Some(request_id) = params.get("requestId").and_then(|v| v.as_str()) else {
        return false;
    };
    let resource_type = params
        .get("resourceType")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if resource_type == "Document" {
        let ok = client
            .send_command(
                "Fetch.fulfillRequest",
                Some(json!({
                    "requestId": request_id,
                    "responseCode": 200,
                    "responseHeaders": [
                        { "name": "Content-Type", "value": "text/html" }
                    ],
                    "body": blank_html_b64
                })),
                Some(temp_session),
            )
            .await
            .is_ok();
        ok
    } else {
        let _ = client
            .send_command(
                "Fetch.failRequest",
                Some(json!({
                    "requestId": request_id,
                    "errorReason": "Aborted"
                })),
                Some(temp_session),
            )
            .await;
        false
    }
}

async fn collect_storage_in_session(
    client: &CdpClient,
    temp_session: &str,
    event_rx: &mut tokio::sync::mpsc::Receiver<super::cdp::types::CdpEvent>,
    origins: &[String],
    origin_js: &str,
) -> Result<Vec<OriginStorage>, String> {
    client
        .send_command_no_params("Page.enable", Some(temp_session))
        .await?;
    client
        .send_command_no_params("Runtime.enable", Some(temp_session))
        .await?;
    client
        .send_command_no_params("Network.enable", Some(temp_session))
        .await?;
    client
        .send_command(
            "Network.setBypassServiceWorker",
            Some(json!({ "bypass": true })),
            Some(temp_session),
        )
        .await?;

    let blank_html_b64 = base64::engine::general_purpose::STANDARD.encode("<html></html>");

    client
        .send_command(
            "Fetch.enable",
            Some(json!({ "patterns": [{ "urlPattern": "*" }] })),
            Some(temp_session),
        )
        .await?;

    let mut results = Vec::new();

    for target_origin in origins {
        // Drain any stale events (e.g. delayed loadEventFired from about:blank
        // or the previous origin) before starting the next navigation.
        while event_rx.try_recv().is_ok() {}

        let nav_url = format!("{}/", target_origin.trim_end_matches('/'));
        let nav_fut = client.send_command(
            "Page.navigate",
            Some(json!({ "url": nav_url })),
            Some(temp_session),
        );
        tokio::pin!(nav_fut);

        let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
        let mut nav_done = false;
        let mut nav_error: Option<String> = None;
        let mut document_fulfilled = false;
        let mut page_loaded = false;

        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            if !nav_done {
                tokio::select! {
                    res = &mut nav_fut => {
                        nav_done = true;
                        if let Err(err) = res {
                            nav_error = Some(err);
                            break;
                        }
                        if page_loaded {
                            break;
                        }
                    }
                    evt = tokio::time::timeout(remaining, event_rx.recv()) => {
                        match evt {
                            Ok(Some(evt)) if evt.method == "Fetch.requestPaused" => {
                                if handle_storage_fetch_paused(client, temp_session, &evt.params, &blank_html_b64).await {
                                    document_fulfilled = true;
                                }
                            }
                            Ok(Some(evt)) if evt.method == "Page.loadEventFired" => {
                                if document_fulfilled {
                                    page_loaded = true;
                                }
                            }
                            Ok(Some(_)) => {}
                            Ok(None) | Err(_) => break,
                        }
                    }
                }
            } else {
                match tokio::time::timeout(remaining, event_rx.recv()).await {
                    Ok(Some(evt)) if evt.method == "Fetch.requestPaused" => {
                        if handle_storage_fetch_paused(
                            client,
                            temp_session,
                            &evt.params,
                            &blank_html_b64,
                        )
                        .await
                        {
                            document_fulfilled = true;
                        }
                    }
                    Ok(Some(evt)) if evt.method == "Page.loadEventFired" => {
                        if document_fulfilled {
                            page_loaded = true;
                            break;
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }

        if let Some(err) = nav_error {
            return Err(format!(
                "Failed to navigate temporary storage target to {}: {}",
                target_origin, err
            ));
        }
        if !page_loaded {
            return Err(format!(
                "Timed out loading {} in temporary storage target",
                target_origin
            ));
        }

        if let Some(storage) = eval_origin_storage(client, temp_session, origin_js).await? {
            if !storage.local_storage.is_empty() || !storage.session_storage.is_empty() {
                results.push(storage);
            }
        }
    }

    Ok(results)
}

/// Page function that collects the current origin's localStorage and sessionStorage.
pub(crate) const ORIGIN_STORAGE_JS: &str = r#"() => {
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

pub async fn save_state(
    client: &CdpClient,
    session_id: &str,
    path: Option<&str>,
    session_name: Option<&str>,
    session_id_str: &str,
    visited_origins: &HashSet<String>,
    target_scope: &TargetOriginScope,
) -> Result<String, String> {
    let cookies = cookies::get_all_cookies(client, session_id).await?;

    let origin_js = format!("({ORIGIN_STORAGE_JS})()");

    // Merge visited origins with current frame tree origins and open page targets
    let mut all_origins = visited_origins.clone();
    if let Ok(tree_result) = client
        .send_command_no_params("Page.getFrameTree", Some(session_id))
        .await
    {
        if let Some(tree) = tree_result.get("frameTree") {
            collect_frame_origins(tree, &mut all_origins);
        }
    }
    if let Ok(targets) = client
        .send_command_typed::<_, GetTargetsResult>("Target.getTargets", &json!({}), None)
        .await
    {
        all_origins.extend(origins_from_targets(
            &targets.target_infos,
            session_id_str,
            target_scope,
        ));
    }

    // 1. Collect localStorage from the current page
    let mut origins = Vec::new();
    let mut current_origin = String::new();

    if let Some(storage) = eval_origin_storage(client, session_id, &origin_js).await? {
        current_origin = storage.origin.clone();
        if !storage.local_storage.is_empty() || !storage.session_storage.is_empty() {
            origins.push(storage);
        }
    }

    // 2. Collect localStorage from remaining origins via a disposable temp target
    all_origins.remove(&current_origin);
    if !all_origins.is_empty() {
        let remaining: Vec<String> = all_origins.into_iter().collect();
        let temp_origins = collect_storage_via_temp_target(client, &remaining, &origin_js).await?;
        origins.extend(temp_origins);
    }

    let state = StorageState { cookies, origins };
    let json_str = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("Failed to serialize state: {}", e))?;

    let save_path = match path {
        Some(p) => p.to_string(),
        None => {
            let dir = get_sessions_dir();
            let _ = create_private_dir(&dir);
            let name = session_name.unwrap_or("default");
            if !is_valid_session_name(name) {
                return Err(session_name_error(name));
            }
            dir.join(format!("{}-{}.json", name, session_id_str))
                .to_string_lossy()
                .to_string()
        }
    };

    write_state_json(&save_path, &json_str)
}

pub(crate) fn write_state_json(path: &str, json_str: &str) -> Result<String, String> {
    let mut save_path = path.to_string();
    if let Ok(key) = std::env::var("AGENT_BROWSER_ENCRYPTION_KEY") {
        let encrypted = encrypt_data(json_str.as_bytes(), &key)?;
        save_path.push_str(".enc");
        write_private(&save_path, &encrypted)
            .map_err(|e| format!("Failed to write state to {}: {}", save_path, e))?;
    } else {
        write_private(&save_path, json_str.as_bytes())
            .map_err(|e| format!("Failed to write state to {}: {}", save_path, e))?;
    }
    Ok(save_path)
}

pub(crate) fn auto_state_paths(
    session_name: &str,
    session_id: &str,
) -> Result<(PathBuf, PathBuf), String> {
    if !is_valid_session_name(session_name) {
        return Err(session_name_error(session_name));
    }

    let dir = get_sessions_dir();
    create_private_dir(&dir)
        .map_err(|e| format!("Failed to create state directory {}: {}", dir.display(), e))?;

    let tmp_dir = dir.join(".tmp");
    create_private_dir(&tmp_dir).map_err(|e| {
        format!(
            "Failed to create temporary state directory {}: {}",
            tmp_dir.display(),
            e
        )
    })?;

    let base_name = format!("{}-{}", session_name, session_id);
    let final_json_path = dir.join(format!("{}.json", base_name));
    let final_path = if std::env::var("AGENT_BROWSER_ENCRYPTION_KEY").is_ok() {
        PathBuf::from(format!("{}.enc", final_json_path.to_string_lossy()))
    } else {
        final_json_path
    };
    let candidate_path = tmp_dir.join(format!(
        "{}-candidate-{}.json",
        base_name,
        std::process::id()
    ));

    Ok((candidate_path, final_path))
}

/// Validates a fully written candidate state file, then swaps it in for the last good file, rolling back on failure.
pub(crate) fn promote_auto_state_candidate(
    candidate_path: &str,
    final_path: &Path,
) -> Result<String, String> {
    if let Err(err) = validate_state_file(candidate_path) {
        let _ = fs::remove_file(candidate_path);
        return Err(err);
    }

    let previous_path = PathBuf::from(format!("{}.previous", final_path.to_string_lossy()));
    if final_path.exists() {
        let _ = fs::remove_file(&previous_path);
        fs::rename(final_path, &previous_path).map_err(|e| {
            format!(
                "Failed to rotate previous state {} to {}: {}",
                final_path.display(),
                previous_path.display(),
                e
            )
        })?;
    }

    let candidate = PathBuf::from(candidate_path);
    if let Err(err) = fs::rename(&candidate, final_path) {
        if previous_path.exists() && !final_path.exists() {
            let _ = fs::rename(&previous_path, final_path);
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

pub async fn save_auto_state_transactional(
    client: &CdpClient,
    session_id: &str,
    session_name: &str,
    session_id_str: &str,
    visited_origins: &HashSet<String>,
    target_scope: &TargetOriginScope,
) -> Result<String, String> {
    let (candidate_json_path, final_path) = auto_state_paths(session_name, session_id_str)?;
    let candidate_arg = candidate_json_path.to_string_lossy().to_string();

    let candidate_path = save_state(
        client,
        session_id,
        Some(&candidate_arg),
        Some(session_name),
        session_id_str,
        visited_origins,
        target_scope,
    )
    .await?;

    promote_auto_state_candidate(&candidate_path, &final_path)
}

pub(crate) fn read_state_json(path: &str) -> Result<String, String> {
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

#[derive(Debug, Serialize)]
pub struct StorageRestoreFailure {
    pub origin: String,
    pub reason: &'static str,
}

/// Restore cookies and per-origin storage from a state file.
///
/// Write storage through CDP DOMStorage rather than page scripts. Report failed
/// writes by origin while continuing to restore the rest of the file.
pub async fn load_state(
    client: &CdpClient,
    session_id: &str,
    path: &str,
) -> Result<Vec<StorageRestoreFailure>, String> {
    let (skipped, _) = load_state_with_origins(client, session_id, path).await?;
    Ok(skipped)
}

pub async fn load_state_with_origins(
    client: &CdpClient,
    session_id: &str,
    path: &str,
) -> Result<(Vec<StorageRestoreFailure>, Vec<String>), String> {
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
    let mut restored = Vec::new();
    for origin in &state.origins {
        if origin.local_storage.is_empty() && origin.session_storage.is_empty() {
            continue;
        }
        if let Err(reason) = restore_origin_storage(client, session_id, origin).await {
            skipped.push(StorageRestoreFailure {
                origin: origin.origin.clone(),
                reason,
            });
        } else {
            restored.push(origin.origin.clone());
        }
    }

    Ok((skipped, restored))
}

/// Returns the serialized tuple origin for a saved origin, or `None` when it has none.
pub(crate) fn canonical_origin(origin: &str) -> Option<String> {
    url::Url::parse(origin)
        .ok()
        .map(|u| u.origin())
        .filter(|o| o.is_tuple())
        .map(|o| o.ascii_serialization())
}

async fn restore_origin_storage(
    client: &CdpClient,
    session_id: &str,
    origin: &OriginStorage,
) -> Result<(), &'static str> {
    let security_origin = canonical_origin(&origin.origin)
        .unwrap_or_else(|| origin.origin.trim_end_matches('/').to_string());
    let navigation = client
        .send_command(
            "Page.navigate",
            Some(json!({ "url": format!("{}/", security_origin) })),
            Some(session_id),
        )
        .await
        .map_err(|_| "Could not navigate to the saved origin")?;
    if navigation
        .get("errorText")
        .and_then(Value::as_str)
        .is_some()
    {
        return Err("Could not navigate to the saved origin");
    }
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
            .await
            .map_err(|error| {
                // Report the failure without forwarding browser-supplied text,
                // which may include storage values.
                if error.ends_with(" 'DOMStorage.setDOMStorageItem' wasn't found") {
                    "Browser does not support CDP DOMStorage.setDOMStorageItem"
                } else if error.contains("Frame not found for the given") {
                    "No frame at the saved origin"
                } else {
                    "Could not write storage through CDP DOMStorage.setDOMStorageItem"
                }
            })?;
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
        "cookieCount": state.cookies.len(),
        "originCount": state.origins.len(),
        "state": state,
    }))
}

pub fn state_clear(path: Option<&str>) -> Result<Value, String> {
    if let Some(p) = path {
        fs::remove_file(p).map_err(|e| format!("Failed to delete state: {}", e))?;
        return Ok(json!({ "deleted": p }));
    }

    clear_state_files(&get_sessions_dir())
}

fn clear_state_files(dir: &Path) -> Result<Value, String> {
    if !dir.exists() {
        return Ok(json!({ "deleted": 0 }));
    }

    let mut count = 0;
    if let Ok(entries) = fs::read_dir(dir) {
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

pub(crate) fn encrypt_data(data: &[u8], key_str: &str) -> Result<Vec<u8>, String> {
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
    if !is_valid_session_name(session_name) {
        return None;
    }

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

    #[cfg(unix)]
    #[test]
    fn write_private_overwrites_and_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing.json");
        fs::write(&existing, "old").unwrap();
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&existing, b"new").unwrap();
        assert_eq!(fs::read_to_string(&existing).unwrap(), "new");
        assert_eq!(
            fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let fresh = dir.path().join("fresh.json");
        write_private(&fresh, b"fresh").unwrap();
        assert_eq!(fs::read_to_string(&fresh).unwrap(), "fresh");
        assert_eq!(
            fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn test_state_clear_removes_transactional_backups() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(sessions.join("auth-test.json"), "{}").unwrap();
        fs::write(sessions.join("auth-test.json.previous"), "{}").unwrap();
        fs::write(sessions.join("auth-test.json.enc.previous"), "encrypted").unwrap();

        let result = clear_state_files(&sessions).unwrap();

        assert_eq!(result["deleted"], 3);
        assert!(!sessions.join("auth-test.json").exists());
        assert!(!sessions.join("auth-test.json.previous").exists());
        assert!(!sessions.join("auth-test.json.enc.previous").exists());
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
    fn canonical_origin_normalizes_saved_origins() {
        assert_eq!(
            canonical_origin("https://Example.COM:443/app/").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            canonical_origin("http://example.com:80").as_deref(),
            Some("http://example.com")
        );
        assert_eq!(
            canonical_origin("https://example.com/").as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn canonical_origin_rejects_non_tuple_origins() {
        assert_eq!(canonical_origin("null"), None);
        assert_eq!(canonical_origin("file:///tmp/page/"), None);
        assert_eq!(canonical_origin("not a url/"), None);
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

    #[test]
    fn test_storage_attachment_and_session_guards() {
        assert!(is_own_storage_attachment(&["t1".to_string()], "t1", false));
        assert!(!is_own_storage_attachment(&["t1".to_string()], "t1", true));
        assert!(!is_own_storage_attachment(&["t1".to_string()], "t2", false));

        {
            let _tg = StorageTargetGuard::register("guard-t1".to_string());
            let _sg = StorageSessionGuard::register("guard-s1".to_string());
            assert!(owns_storage_attachment("guard-t1", false));
            assert!(!owns_storage_attachment("guard-t1", true));
            assert!(owns_storage_session("guard-s1"));
            assert!(!owns_storage_session("other-s"));
        }
        assert!(!owns_storage_attachment("guard-t1", false));
        assert!(!owns_storage_session("guard-s1"));
    }

    #[test]
    fn test_origins_from_targets_filters_internal_and_storage_targets() {
        let _tg = StorageTargetGuard::register("scratch-1".to_string());
        let mk = |id: &str, ttype: &str, url: &str| TargetInfo {
            target_id: id.to_string(),
            target_type: ttype.to_string(),
            title: String::new(),
            url: url.to_string(),
            attached: Some(false),
            browser_context_id: None,
        };
        let targets = vec![
            mk("p1", "page", "http://127.0.0.1:8001/a"),
            mk("p2", "page", "https://example.test/path?q=1"),
            mk("scratch-1", "page", "http://127.0.0.1:8002/"),
            mk("sw1", "service_worker", "https://sw.test/sw.js"),
            mk("int1", "page", "chrome://settings"),
            mk("blank", "page", "about:blank"),
        ];
        let host_scope = TargetOriginScope {
            tracked_target_ids: HashSet::from(["p1".to_string()]),
            include_unpinned_browser_targets: true,
        };
        let origins = origins_from_targets(&targets, "session-a", &host_scope);
        assert!(origins.contains("http://127.0.0.1:8001"));
        assert!(origins.contains("https://example.test"));
        assert!(!origins.contains("http://127.0.0.1:8002"));
        assert!(!origins.contains("https://sw.test"));
        assert_eq!(origins.len(), 2);

        let attached_scope = TargetOriginScope {
            tracked_target_ids: HashSet::from(["p1".to_string()]),
            include_unpinned_browser_targets: false,
        };
        let attached_origins = origins_from_targets(&targets, "session-a", &attached_scope);
        assert!(attached_origins.contains("http://127.0.0.1:8001"));
        assert!(!attached_origins.contains("https://example.test"));
        assert_eq!(attached_origins.len(), 1);
    }

    #[test]
    fn test_is_cdp_error() {
        assert!(is_cdp_error(
            "CDP error (Target.createTarget): {\"code\":-32602,\"message\":\"Invalid parameters\"}"
        ));
        assert!(!is_cdp_error("CDP command timed out: Target.createTarget"));
        assert!(!is_cdp_error("CDP response channel closed"));
    }

    #[tokio::test]
    async fn test_create_scratch_target_steps_down_and_preserves_autosave_on_capture_error() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // 1. Verify create_scratch_target steps down from hidden+background -> background -> plain.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut seen = Vec::new();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let cmd: Value = serde_json::from_str(&text).unwrap();
                let id = cmd["id"].clone();
                let method = cmd["method"].as_str().unwrap_or("");
                let reply = if method == "Target.createTarget" {
                    seen.push(cmd["params"].clone());
                    if cmd["params"].get("hidden").is_some()
                        || cmd["params"].get("background").is_some()
                    {
                        json!({ "id": id, "error": { "code": -32602, "message": "Invalid parameters" } })
                    } else {
                        json!({ "id": id, "result": { "targetId": "t-plain" } })
                    }
                } else {
                    json!({ "id": id, "result": {} })
                };
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
            seen
        });

        let client = CdpClient::connect(&ws_url).await.unwrap();
        let created = create_scratch_target(&client).await.unwrap();
        assert_eq!(created.target_id, "t-plain");
        drop(client);
        let seen = server.await.unwrap();
        assert_eq!(
            seen,
            vec![
                json!({ "url": "about:blank", "background": true, "hidden": true }),
                json!({ "url": "about:blank", "background": true }),
                json!({ "url": "about:blank" }),
            ]
        );

        // 2. Verify save_auto_state_transactional preserves an existing file when active capture fails.
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url2 = format!("ws://{}", listener2.local_addr().unwrap());
        let server2 = tokio::spawn(async move {
            let (stream, _) = listener2.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let cmd: Value = serde_json::from_str(&text).unwrap();
                let id = cmd["id"].clone();
                let method = cmd["method"].as_str().unwrap_or("");
                let reply = match method {
                    "Network.getAllCookies" => json!({ "id": id, "result": { "cookies": [] } }),
                    "Runtime.evaluate" => json!({
                        "id": id,
                        "error": { "code": -32000, "message": "Execution context was destroyed" }
                    }),
                    _ => json!({ "id": id, "result": {} }),
                };
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
        });

        let tmp_home = tempfile::tempdir().unwrap();
        let sessions_dir = tmp_home.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        let state_file = sessions_dir.join("preserve-test-s1.json");
        let original = r#"{"cookies":[],"origins":[{"origin":"https://keep.test","localStorage":[{"name":"tok","value":"ok"}]}]}"#;
        fs::write(&state_file, original).unwrap();

        let client2 = CdpClient::connect(&ws_url2).await.unwrap();
        let save_res = save_state(
            &client2,
            "s1",
            Some(state_file.to_str().unwrap()),
            Some("preserve-test"),
            "s1",
            &HashSet::new(),
            &TargetOriginScope::default(),
        )
        .await;
        assert!(save_res.is_err(), "capture error should propagate");
        assert_eq!(fs::read_to_string(&state_file).unwrap(), original);
        drop(client2);
        server2.abort();
    }

    #[test]
    fn test_state_show_returns_structured_summary_counts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("demo-default.json");
        let fixture = serde_json::json!({
            "cookies": [
                {
                    "name": "a",
                    "value": "1",
                    "domain": "example.com",
                    "path": "/",
                    "expires": -1.0,
                    "httpOnly": false,
                    "secure": false,
                    "sameSite": "Lax"
                },
                {
                    "name": "b",
                    "value": "2",
                    "domain": "example.com",
                    "path": "/",
                    "expires": -1.0,
                    "httpOnly": false,
                    "secure": false,
                    "sameSite": "Lax"
                }
            ],
            "origins": [
                {
                    "origin": "https://example.com",
                    "localStorage": [{ "name": "k", "value": "v" }],
                    "sessionStorage": []
                }
            ]
        });
        fs::write(&path, serde_json::to_string(&fixture).unwrap()).unwrap();

        let shown = state_show(path.to_str().unwrap()).expect("state_show");
        assert_eq!(shown["summary"], "2 cookies, 1 origins");
        assert_eq!(shown["cookieCount"], 2);
        assert_eq!(shown["originCount"], 1);
        assert_eq!(shown["filename"], "demo-default.json");
    }

    #[tokio::test]
    async fn test_collect_storage_bypasses_sw_ignores_stale_load_keeps_guard_and_propagates_errors()
    {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        // 1. Stale Page.loadEventFired before Fetch.requestPaused is ignored,
        //    Network.setBypassServiceWorker is enabled, and STORAGE_SESSIONS
        //    still holds the scratch session during Target.closeTarget.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url = format!("ws://{}", listener.local_addr().unwrap());
        let observed =
            std::sync::Arc::new(std::sync::Mutex::new((Vec::<String>::new(), false, false)));
        let obs_srv = std::sync::Arc::clone(&observed);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let cmd: Value = serde_json::from_str(&text).unwrap();
                let id = cmd["id"].clone();
                let method = cmd["method"].as_str().unwrap_or("").to_string();
                obs_srv.lock().unwrap().0.push(method.clone());
                match method.as_str() {
                    "Target.createTarget" => {
                        ws.send(Message::Text(
                            json!({ "id": id, "result": { "targetId": "T-SW" } }).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                    "Target.attachToTarget" => {
                        ws.send(Message::Text(
                            json!({ "id": id, "result": { "sessionId": "S-SW" } }).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                    "Page.navigate" => {
                        // Send a stale loadEventFired BEFORE Fetch.requestPaused.
                        ws.send(Message::Text(
                            json!({ "method": "Page.loadEventFired", "sessionId": "S-SW", "params": {} }).to_string(),
                        ))
                        .await
                        .unwrap();
                        ws.send(Message::Text(
                            json!({
                                "method": "Fetch.requestPaused",
                                "sessionId": "S-SW",
                                "params": { "requestId": "req-1", "resourceType": "Document" }
                            })
                            .to_string(),
                        ))
                        .await
                        .unwrap();
                        ws.send(Message::Text(
                            json!({ "id": id, "result": { "frameId": "F1" } }).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                    "Fetch.fulfillRequest" => {
                        obs_srv.lock().unwrap().1 = true;
                        ws.send(Message::Text(json!({ "id": id, "result": {} }).to_string()))
                            .await
                            .unwrap();
                        ws.send(Message::Text(
                            json!({ "method": "Page.loadEventFired", "sessionId": "S-SW", "params": {} }).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                    "Runtime.evaluate" => {
                        ws.send(Message::Text(
                            json!({
                                "id": id,
                                "result": {
                                    "result": {
                                        "type": "object",
                                        "value": {
                                            "origin": "http://127.0.0.1:9001",
                                            "localStorage": [{ "name": "k", "value": "v" }],
                                            "sessionStorage": []
                                        }
                                    }
                                }
                            })
                            .to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                    "Target.closeTarget" => {
                        obs_srv.lock().unwrap().2 = owns_storage_session("S-SW");
                        ws.send(Message::Text(
                            json!({ "id": id, "result": { "success": true } }).to_string(),
                        ))
                        .await
                        .unwrap();
                    }
                    _ => {
                        ws.send(Message::Text(json!({ "id": id, "result": {} }).to_string()))
                            .await
                            .unwrap();
                    }
                }
            }
        });

        let client = CdpClient::connect(&ws_url).await.unwrap();
        let collected = collect_storage_via_temp_target(
            &client,
            &["http://127.0.0.1:9001".to_string()],
            "({})",
        )
        .await
        .expect("collection should succeed after ignoring stale loadEventFired");
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].local_storage[0].value, "v");
        let (methods, fulfilled, guard_alive_during_close) = observed.lock().unwrap().clone();
        assert!(
            fulfilled,
            "document must be fulfilled before load completes"
        );
        assert!(
            guard_alive_during_close,
            "session guard must stay registered across Target.closeTarget"
        );
        let sw_idx = methods
            .iter()
            .position(|m| m == "Network.setBypassServiceWorker")
            .expect("Network.setBypassServiceWorker should be called");
        let fetch_idx = methods
            .iter()
            .position(|m| m == "Fetch.enable")
            .expect("Fetch.enable should be called");
        assert!(sw_idx < fetch_idx, "SW bypass must precede Fetch.enable");
        assert!(!owns_storage_session("S-SW"));
        drop(client);
        server.abort();

        // 2. Per-origin navigation failure propagates so an existing state file stays intact.
        let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_url2 = format!("ws://{}", listener2.local_addr().unwrap());
        let server2 = tokio::spawn(async move {
            let (stream, _) = listener2.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let cmd: Value = serde_json::from_str(&text).unwrap();
                let id = cmd["id"].clone();
                let method = cmd["method"].as_str().unwrap_or("");
                let reply = match method {
                    "Network.getAllCookies" => json!({ "id": id, "result": { "cookies": [] } }),
                    "Target.createTarget" => json!({ "id": id, "result": { "targetId": "T-ERR" } }),
                    "Target.attachToTarget" => {
                        json!({ "id": id, "result": { "sessionId": "S-ERR" } })
                    }
                    "Page.navigate" => json!({
                        "id": id,
                        "error": { "code": -32000, "message": "Cannot navigate to invalid URL" }
                    }),
                    "Runtime.evaluate" => json!({
                        "id": id,
                        "result": {
                            "result": {
                                "type": "object",
                                "value": { "origin": "http://127.0.0.1:9000", "localStorage": [], "sessionStorage": [] }
                            }
                        }
                    }),
                    _ => json!({ "id": id, "result": {} }),
                };
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
        });

        let tmp = tempfile::tempdir().unwrap();
        let state_file = tmp.path().join("keep.json");
        let original = r#"{"cookies":[],"origins":[{"origin":"http://127.0.0.1:9002","localStorage":[{"name":"keep","value":"1"}]}]}"#;
        fs::write(&state_file, original).unwrap();

        let client2 = CdpClient::connect(&ws_url2).await.unwrap();
        let visited = HashSet::from(["http://127.0.0.1:9002".to_string()]);
        let err = save_state(
            &client2,
            "S-TOP",
            Some(state_file.to_str().unwrap()),
            Some("keep"),
            "s1",
            &visited,
            &TargetOriginScope::default(),
        )
        .await;
        assert!(
            err.is_err(),
            "per-origin navigation failure must return Err"
        );
        assert_eq!(fs::read_to_string(&state_file).unwrap(), original);
        drop(client2);
        server2.abort();
    }
}
