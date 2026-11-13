//! Firefox cookies, web storage and saved state. Formats must match `cookies.rs` and `state.rs`.

use base64::Engine;
use serde_json::{json, Value};
use std::collections::HashSet;

use super::client::SANDBOX;
use super::daemon::session;
use super::navigation::{self, Load, Wait};
use super::session::FirefoxSession;
use crate::native::actions::DaemonState;
use crate::native::cookies::Cookie;
use crate::native::state::{self, OriginStorage, StorageState};
use crate::native::storage::storage_js_name;
use crate::validation::{is_valid_session_name, session_name_error};

/// Reads one origin's storage through a sandbox and returns `null` unless the page is still on
/// `expectedOrigin`, checked in the same call so a navigation cannot race the read.
const ORIGIN_STORAGE_JS: &str = r#"(expectedOrigin) => {
    if (location.origin !== expectedOrigin) {
        return null;
    }
    const result = { origin: location.origin, localStorage: [], sessionStorage: [] };
    try {
        for (let i = 0; i < localStorage.length; i++) {
            const key = localStorage.key(i);
            result.localStorage.push({ name: key, value: localStorage.getItem(key) });
        }
    } catch(e) { return null; }
    try {
        for (let i = 0; i < sessionStorage.length; i++) {
            const key = sessionStorage.key(i);
            result.sessionStorage.push({ name: key, value: sessionStorage.getItem(key) });
        }
    } catch(e) { return null; }
    return result;
}"#;

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

fn cookie_applies_to_url(cookie: &Cookie, url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    let domain = cookie.domain.to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    let domain_matches = host.as_str() == domain.strip_prefix('.').unwrap_or(&domain)
        || (domain.starts_with('.') && host.ends_with(&domain));
    let path = if cookie.path.is_empty() {
        "/"
    } else {
        cookie.path.as_str()
    };
    let url_path = url.path();
    let path_matches = url_path == path
        || url_path
            .strip_prefix(path)
            .is_some_and(|suffix| path.ends_with('/') || suffix.starts_with('/'));
    let secure_transport = matches!(url.scheme(), "https" | "wss");
    let supported_scheme = secure_transport || matches!(url.scheme(), "http" | "ws");
    domain_matches && path_matches && supported_scheme && (!cookie.secure || secure_transport)
}

fn append_context_urls(contexts: &[Value], urls: &mut Vec<url::Url>) {
    for context in contexts {
        if let Some(value) = context["url"].as_str() {
            if let Ok(url) = url::Url::parse(value) {
                urls.push(url);
            }
        }
        if let Some(children) = context["children"].as_array() {
            append_context_urls(children, urls);
        }
    }
}

async fn cookie_urls(ff: &FirefoxSession) -> Result<Vec<url::Url>, String> {
    let tree = ff
        .bidi
        .send(
            "browsingContext.getTree",
            json!({"root": ff.contexts.active_tab()}),
        )
        .await?;
    let contexts = tree["contexts"]
        .as_array()
        .ok_or("Failed to read active browsing contexts")?;
    let mut urls = Vec::new();
    append_context_urls(contexts, &mut urls);
    Ok(urls)
}

async fn get_all_cookies(ff: &FirefoxSession) -> Result<Vec<Cookie>, String> {
    let mut params = json!({});
    if let Some(partition) = ff.active_partition() {
        params["partition"] = partition;
    }
    let result = ff.bidi.send("storage.getCookies", params).await?;
    Ok(result["cookies"]
        .as_array()
        .into_iter()
        .flatten()
        .map(bidi_cookie_to_state)
        .collect())
}

/// Returns cookies for requested URLs or active page and frames when omitted.
pub async fn cookies_get(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let urls = match cmd.get("urls") {
        Some(value) => value
            .as_array()
            .ok_or("Invalid 'urls' parameter: expected an array")?
            .iter()
            .map(|value| {
                let value = value
                    .as_str()
                    .ok_or("Invalid URL in 'urls': expected a string")?;
                url::Url::parse(value).map_err(|error| format!("Invalid URL '{value}': {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        None => cookie_urls(ff).await?,
    };
    let mut cookies = get_all_cookies(ff).await?;
    cookies.retain(|cookie| urls.iter().any(|url| cookie_applies_to_url(cookie, url)));
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

    let mut command = json!({});
    if let Some(partition) = ff.active_partition() {
        command["partition"] = partition;
    }
    for cookie in &cookie_values {
        command["cookie"] = command_cookie_to_bidi(cookie, current_url.as_deref())?;
        ff.bidi.send("storage.setCookie", command.clone()).await?;
    }
    Ok(json!({ "set": true }))
}

/// Clears cookies like Chrome's `handle_cookies_clear` (via `storage.deleteCookies`).
pub async fn cookies_clear(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let mut params = json!({ "filter": {} });
    if let Some(partition) = ff.active_partition() {
        params["partition"] = partition;
    }
    ff.bidi.send("storage.deleteCookies", params).await?;
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

fn collect_context_ids(contexts: &[Value], out: &mut Vec<(String, String, bool)>, top_level: bool) {
    for context in contexts {
        let url = context["url"].as_str().unwrap_or("");
        if let Some(id) = context["context"]
            .as_str()
            .filter(|_| !url.starts_with("about:"))
        {
            out.push((id.to_string(), url.to_string(), top_level));
        }
        if let Some(children) = context["children"].as_array() {
            collect_context_ids(children, out, false);
        }
    }
}

/// Reads the storage the browser attributes to `expected_origin` in a sandbox realm, so page
/// scripts cannot alter the result and a reply claiming another origin is dropped.
async fn read_origin(
    ff: &FirefoxSession,
    context: &str,
    expected_origin: &str,
) -> Result<Option<OriginStorage>, String> {
    let data = ff
        .bidi
        .call_function_in_sandbox(
            context,
            SANDBOX,
            ORIGIN_STORAGE_JS,
            vec![json!(expected_origin)],
        )
        .await
        .map_err(|e| format!("Failed to read storage: {e}"))?;
    Ok(state::parse_origin_storage(&data).filter(|storage| storage.origin == expected_origin))
}

fn has_entries(origin: &OriginStorage) -> bool {
    !origin.local_storage.is_empty() || !origin.session_storage.is_empty()
}

/// Reads each origin's storage from a throwaway background tab whose requests are answered
/// with a blank page, so the active page never moves and no origin's server is contacted.
async fn collect_via_temp_context(
    ff: &mut FirefoxSession,
    origins: &[String],
) -> Result<Vec<OriginStorage>, String> {
    super::network::ensure_intercept(ff).await?;
    let mut params = json!({"type": "tab", "background": true});
    if let Some(user_context) = ff.contexts.user_context(ff.contexts.active()) {
        params["userContext"] = json!(user_context);
    }
    let created = ff.bidi.send("browsingContext.create", params).await?;
    let context = created["context"]
        .as_str()
        .ok_or("Firefox did not return a context id for the storage tab")?
        .to_string();
    ff.net.blank_contexts.write().await.insert(context.clone());
    let result = read_origins_in(ff, &context, origins).await;
    ff.net.blank_contexts.write().await.remove(&context);
    let _ = ff
        .bidi
        .send("browsingContext.close", json!({"context": context}))
        .await;
    result
}

async fn read_origins_in(
    ff: &FirefoxSession,
    context: &str,
    origins: &[String],
) -> Result<Vec<OriginStorage>, String> {
    let mut found = Vec::new();
    for origin in origins {
        let allowed = {
            let filter = ff.net.domain_filter.read().await;
            crate::native::network::check_navigation_allowed(filter.as_ref(), origin).is_ok()
        };
        if !allowed {
            continue;
        }
        ff.bidi
            .send(
                "browsingContext.navigate",
                json!({"context": context, "url": format!("{origin}/"), "wait": "complete"}),
            )
            .await
            .map_err(|e| format!("Failed to read storage for {origin}: {e}"))?;
        if let Some(data) = read_origin(ff, context, origin).await? {
            found.push(data);
        }
    }
    Ok(found)
}

/// Collects open and previously visited origins, returning which origins were readable, including empty ones.
/// Protocol failures abort the save; unavailable storage is omitted so autosave can keep its previous value.
async fn collect_origins(
    ff: &mut FirefoxSession,
) -> Result<(Vec<OriginStorage>, HashSet<String>, HashSet<String>), String> {
    let tree = ff.bidi.send("browsingContext.getTree", json!({})).await?;
    let contexts = tree["contexts"]
        .as_array()
        .ok_or("Failed to read active browsing contexts")?;
    let mut ids = Vec::new();
    for info in contexts
        .iter()
        .filter(|info| ff.contexts.in_active_jar(info))
    {
        collect_context_ids(std::slice::from_ref(info), &mut ids, true);
    }
    let mut open = HashSet::new();
    let mut read = HashSet::new();
    let mut seen = HashSet::new();
    let mut temporary = HashSet::new();
    let mut origins = Vec::new();
    for (id, url, top_level) in ids {
        let Some(expected_origin) = url::Url::parse(&url)
            .ok()
            .map(|url| url.origin())
            .filter(url::Origin::is_tuple)
            .map(|origin| origin.ascii_serialization())
        else {
            continue;
        };
        ff.visited_origins.insert(expected_origin.clone());
        open.insert(expected_origin.clone());
        let origin = match read_origin(ff, &id, &expected_origin).await {
            Ok(origin) => origin,
            Err(error) if !top_level => {
                eprintln!("Skipping unreadable Firefox child context {id}: {error}");
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some(origin) = origin else {
            continue;
        };
        ff.visited_origins.insert(origin.origin.clone());
        open.insert(origin.origin.clone());
        read.insert(origin.origin.clone());
        if has_entries(&origin) && seen.insert(origin.origin.clone()) {
            origins.push(origin);
        }
    }
    let remaining: Vec<String> = ff
        .visited_origins
        .iter()
        .filter(|origin| {
            (origin.starts_with("http://") || origin.starts_with("https://"))
                && !open.contains(*origin)
        })
        .cloned()
        .collect();
    if !remaining.is_empty() {
        for mut origin in collect_via_temp_context(ff, &remaining).await? {
            read.insert(origin.origin.clone());
            temporary.insert(origin.origin.clone());
            origin.session_storage.clear();
            if has_entries(&origin) && seen.insert(origin.origin.clone()) {
                origins.push(origin);
            }
        }
    }
    Ok((origins, read, temporary))
}

async fn write_state_to(
    ff: &mut FirefoxSession,
    path: &str,
    previous_path: Option<&str>,
) -> Result<String, String> {
    let cookies = get_all_cookies(ff).await?;
    let (origins, mut seen, temporary) = collect_origins(ff).await?;
    let mut state = StorageState { cookies, origins };
    if let Some(previous_path) = previous_path.filter(|path| {
        std::path::Path::new(path).exists() || std::path::Path::new(&format!("{path}.enc")).exists()
    }) {
        let json = state::read_state_json(previous_path)?;
        let previous: StorageState =
            serde_json::from_str(&json).map_err(|e| format!("Invalid state file: {e}"))?;
        for origin in previous.origins {
            if temporary.contains(&origin.origin) {
                if let Some(current) = state
                    .origins
                    .iter_mut()
                    .find(|current| current.origin == origin.origin)
                {
                    current.session_storage = origin.session_storage;
                } else if !origin.session_storage.is_empty() {
                    state.origins.push(OriginStorage {
                        origin: origin.origin,
                        local_storage: Vec::new(),
                        session_storage: origin.session_storage,
                    });
                }
            } else if seen.insert(origin.origin.clone()) {
                state.origins.push(origin);
            }
        }
    }
    let json = serde_json::to_string_pretty(&state)
        .map_err(|e| format!("Failed to serialize state: {e}"))?;
    state::write_state_json(path, &json)
}

pub async fn validate_restore(
    ff: &FirefoxSession,
    url_pattern: Option<&str>,
    text: Option<&str>,
    expression: Option<&str>,
    timeout_ms: u64,
) -> Result<(), String> {
    if let Some(pattern) = url_pattern {
        let url = ff.url().await?;
        if !crate::native::actions::route_url_matches(pattern, &url) {
            return Err(format!(
                "restore URL validation failed: '{}' did not match '{}'",
                url, pattern
            ));
        }
    }
    if let Some(text) = text {
        let check = format!(
            "(document.body.innerText || '').includes({})",
            serde_json::to_string(text).unwrap_or_default()
        );
        super::page::poll_until_true(ff, &check, timeout_ms).await?;
    }
    if let Some(expression) = expression {
        super::page::poll_until_true(ff, &format!("!!({expression})"), timeout_ms).await?;
    }
    Ok(())
}

/// Saves cookies and origin storage in the `state.rs` JSON format, like Chrome's `handle_state_save`.
pub async fn state_save(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    super::network::drain(state).await;
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
    let saved_path = write_state_to(ff, &path, None).await?;
    Ok(json!({ "saved": true, "path": saved_path }))
}

/// Saves the `--session-name` state file used by restore-on-close/autosave.
pub async fn save_auto_state(
    ff: &mut FirefoxSession,
    session_name: &str,
    session_id: &str,
) -> Result<String, String> {
    let (candidate_path, final_path) = state::auto_state_paths(session_name, session_id)?;
    let candidate_path = candidate_path.to_string_lossy().to_string();
    let final_path = final_path.to_string_lossy().to_string();
    let candidate_path = write_state_to(ff, &candidate_path, Some(&final_path)).await?;
    state::promote_auto_state_candidate(&candidate_path, std::path::Path::new(&final_path))
}

/// Run in a BiDi sandbox so page scripts never see the payload. The origin is checked in the same call so a redirect cannot race it, and the call returns false when the page landed elsewhere.
const RESTORE_STORAGE_JS: &str = r#"(expectedOrigin, payload) => {
    if (location.origin !== expectedOrigin) {
        return false;
    }
    const p = JSON.parse(payload);
    for (const e of p.localStorage) localStorage.setItem(e.name, e.value);
    for (const e of p.sessionStorage) sessionStorage.setItem(e.name, e.value);
    return true;
}"#;

/// Loads the cookies and origin storage in a `state.rs` JSON file, dropping fields BiDi
/// does not support, like Chrome's `handle_state_load`. Origins that cannot be loaded, or that
/// land somewhere else, are skipped and returned.
pub async fn load_state_path(state: &mut DaemonState, path: &str) -> Result<Vec<String>, String> {
    let json_str = state::read_state_json(path)?;
    let saved: StorageState =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid state file: {e}"))?;

    let mut command = json!({});
    if let Some(partition) = session(state)?.active_partition() {
        command["partition"] = partition;
    }
    for cookie in &saved.cookies {
        command["cookie"] = state_cookie_to_bidi(cookie);
        session(state)?
            .bidi
            .send("storage.setCookie", command.clone())
            .await?;
    }

    let mut skipped = Vec::new();
    for origin in &saved.origins {
        if origin.local_storage.is_empty() && origin.session_storage.is_empty() {
            continue;
        }
        let nav_url = format!("{}/", origin.origin.trim_end_matches('/'));
        let landing = match navigation::load(
            state,
            Load::Url {
                url: &nav_url,
                wait: Wait::Complete,
            },
        )
        .await
        {
            Ok(landing) if !landing.prompt => landing,
            _ => {
                skipped.push(origin.origin.clone());
                continue;
            }
        };

        let payload = serde_json::to_string(origin).map_err(|e| e.to_string())?;
        let restored = session(state)?
            .bidi
            .call_function_in_sandbox(
                &landing.context,
                SANDBOX,
                RESTORE_STORAGE_JS,
                vec![json!(origin.origin), json!(payload)],
            )
            .await
            .map_err(|e| format!("Failed to restore storage for {}: {e}", origin.origin))?;
        if restored != json!(true) {
            skipped.push(origin.origin.clone());
        }
    }

    Ok(skipped)
}

/// Loads a `state.rs` JSON file, dropping Chrome-only cookie fields, like Chrome's
/// `handle_state_load`. `--session-name` restore reuses this through `load_state_path`.
pub async fn state_load(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let path = cmd
        .get("path")
        .and_then(Value::as_str)
        .ok_or("Missing 'path' parameter")?;
    crate::native::actions::ensure_state_replay_supported_by_active_domain_filter(
        state,
        "state load",
    )
    .await?;
    let skipped = load_state_path(state, path).await?;
    crate::native::actions::mark_explicit_storage_state_loaded(state, path);
    Ok(json!({ "loaded": true, "path": path, "skippedOrigins": skipped }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::session::tests::fake_session_with;
    use crate::test_utils::EnvGuard;
    use std::fs;

    fn origin_result(origin: &str) -> Value {
        json!({
            "type": "success",
            "result": {
                "type": "object",
                "value": [
                    ["origin", {"type": "string", "value": origin}],
                    ["localStorage", {"type": "array", "value": []}],
                    ["sessionStorage", {"type": "array", "value": []}]
                ]
            }
        })
    }

    #[tokio::test]
    async fn collect_origins_marks_every_read_origin_as_visited() {
        let (mut ff, tx, _calls) = fake_session_with(|req| match req["method"].as_str() {
            Some("browsingContext.getTree") => json!({
                "contexts": [
                    {"context": "ctx-1", "url": "https://one.test/"},
                    {"context": "ctx-2", "url": "https://two.test/"}
                ]
            }),
            Some("script.callFunction") => {
                let context = req["params"]["target"]["context"].as_str().unwrap_or("");
                let origin = if context == "ctx-1" {
                    "https://one.test"
                } else {
                    "https://two.test"
                };
                origin_result(origin)
            }
            _ => json!({}),
        })
        .await;
        std::mem::forget(tx);

        let (origins, seen, _) = collect_origins(&mut ff).await.unwrap();

        assert!(origins.is_empty());
        assert_eq!(seen, ff.visited_origins);
        assert_eq!(
            ff.visited_origins,
            HashSet::from([
                "https://one.test".to_string(),
                "https://two.test".to_string()
            ])
        );
    }

    #[tokio::test]
    async fn blocked_open_storage_is_unread_and_is_not_retried_in_a_blank_tab() {
        let (mut ff, tx, calls) = fake_session_with(|req| match req["method"].as_str() {
            Some("browsingContext.getTree") => json!({
                "contexts": [{"context": "ctx-1", "url": "https://one.test/"}]
            }),
            Some("script.callFunction") => json!({
                "type": "success", "result": {"type": "null"}
            }),
            _ => json!({}),
        })
        .await;
        std::mem::forget(tx);

        let (origins, seen, _) = collect_origins(&mut ff).await.unwrap();

        assert!(origins.is_empty());
        assert!(seen.is_empty());
        assert!(ff.visited_origins.contains("https://one.test"));
        assert!(!calls
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "browsingContext.create"));
    }

    #[tokio::test]
    async fn unreadable_child_context_does_not_abort_state_collection() {
        let (mut ff, tx, _calls) = fake_session_with(|req| match req["method"].as_str() {
            Some("browsingContext.getTree") => json!({
                "contexts": [{
                    "context": "ctx-page",
                    "url": "https://one.test/",
                    "children": [{"context": "ctx-frame", "url": "https://frame.test/"}]
                }]
            }),
            Some("script.callFunction") if req["params"]["target"]["context"] == "ctx-frame" => {
                json!({
                    "type": "exception",
                    "exceptionDetails": {"text": "no such frame"}
                })
            }
            Some("script.callFunction") => origin_result("https://one.test"),
            _ => json!({}),
        })
        .await;
        std::mem::forget(tx);

        let (_, seen, _) = collect_origins(&mut ff).await.unwrap();

        assert_eq!(seen, HashSet::from(["https://one.test".to_string()]));
    }

    #[tokio::test]
    async fn failed_collection_keeps_the_previous_auto_state_file() {
        let guard = EnvGuard::new(&["HOME", "AGENT_BROWSER_NAMESPACE"]);
        let home = tempfile::tempdir().unwrap();
        guard.set("HOME", home.path().to_str().unwrap());
        guard.remove("AGENT_BROWSER_NAMESPACE");
        let (_, final_path) = state::auto_state_paths("saved", "id").unwrap();
        fs::create_dir_all(final_path.parent().unwrap()).unwrap();
        fs::write(&final_path, "previous").unwrap();

        let (mut ff, tx, _calls) = fake_session_with(|req| match req["method"].as_str() {
            Some("browsingContext.getTree") => json!({
                "contexts": [{"context": "ctx-1", "url": "https://one.test/"}]
            }),
            Some("script.callFunction") => json!({
                "type": "exception",
                "exceptionDetails": {"text": "storage unavailable"}
            }),
            _ => json!({}),
        })
        .await;
        std::mem::forget(tx);

        let error = save_auto_state(&mut ff, "saved", "id").await.unwrap_err();

        assert!(error.contains("storage unavailable"), "{error}");
        assert_eq!(fs::read_to_string(&final_path).unwrap(), "previous");
    }

    type Requests = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;

    /// A session whose BiDi socket reports `tree` for `browsingContext.getTree` and
    /// `script_result` for `script.callFunction`, recording every request it received.
    async fn fake_storage_session(tree: Value, script_result: Value) -> (FirefoxSession, Requests) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let requests = Requests::default();
        let log = requests.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                let id = request["id"].clone();
                let result = match request["method"].as_str().unwrap_or_default() {
                    "browsingContext.getTree" => tree.clone(),
                    "script.callFunction" => {
                        json!({"type": "success", "result": script_result.clone()})
                    }
                    _ => json!({}),
                };
                log.lock().unwrap().push(request);
                let reply = json!({"type": "success", "id": id, "result": result});
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
        });
        let (mut ff, _tx, _) = super::super::session::tests::fake_session().await;
        ff.bidi = super::super::client::BidiClient::connect(&url)
            .await
            .unwrap()
            .into();
        (ff, requests)
    }

    fn storage_reply(origin: &str) -> Value {
        json!({"type": "object", "value": [
            ["origin", {"type": "string", "value": origin}],
            ["localStorage", {"type": "array", "value": [
                {"type": "object", "value": [
                    ["name", {"type": "string", "value": "token"}],
                    ["value", {"type": "string", "value": "secret"}]
                ]}
            ]}],
            ["sessionStorage", {"type": "array", "value": []}]
        ]})
    }

    #[tokio::test]
    async fn collect_origins_drops_a_reply_claiming_another_origin() {
        let (mut ff, requests) = fake_storage_session(
            json!({"contexts": [{"context": "ctx-active", "url": "https://real.example/page"}]}),
            storage_reply("https://bank.example"),
        )
        .await;

        let (origins, seen, _) = collect_origins(&mut ff).await.unwrap();

        assert!(origins.is_empty());
        assert!(!seen.contains("https://bank.example"));
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .find(|request| request["method"] == "script.callFunction")
                .unwrap()["params"]["target"],
            json!({"context": "ctx-active", "sandbox": "agent-browser"})
        );
    }

    #[tokio::test]
    async fn collect_origins_reads_in_a_sandbox_with_the_browser_origin() {
        let (mut ff, requests) = fake_storage_session(
            json!({"contexts": [{"context": "ctx-active", "url": "https://real.example/page"}]}),
            storage_reply("https://real.example"),
        )
        .await;

        let (origins, _, _) = collect_origins(&mut ff).await.unwrap();

        assert_eq!(origins.len(), 1);
        assert_eq!(origins[0].origin, "https://real.example");
        assert_eq!(origins[0].local_storage[0].value, "secret");
        let requests = requests.lock().unwrap();
        let call = requests
            .iter()
            .find(|request| request["method"] == "script.callFunction")
            .unwrap();
        assert_eq!(call["params"]["target"]["sandbox"], "agent-browser");
        assert_eq!(
            call["params"]["arguments"],
            json!([{"type": "string", "value": "https://real.example"}])
        );
    }

    #[tokio::test]
    async fn cookie_urls_use_the_top_page_and_its_frames() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let (mut ff, _tx, _) = super::super::session::tests::fake_session_with(move |request| {
            seen.lock().unwrap().push(request.clone());
            json!({"contexts": [{
                "context": "ctx-page",
                "url": "https://page.example/",
                "children": [{"context": "ctx-frame", "url": "https://frame.example/"}]
            }]})
        })
        .await;
        ff.contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        ff.contexts.activate("ctx-frame");

        let urls = cookie_urls(&ff).await.unwrap();

        assert_eq!(
            urls.iter().map(|url| url.as_str()).collect::<Vec<_>>(),
            ["https://page.example/", "https://frame.example/"]
        );
        assert_eq!(requests.lock().unwrap()[0]["params"]["root"], "ctx-active");
    }

    #[tokio::test]
    async fn explicit_state_load_clears_failed_restore() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"cookies":[],"origins":[]}"#).unwrap();
        let mut state = DaemonState::new();
        let (ff, _tx, _) = super::super::session::tests::fake_session().await;
        state.firefox = Some(ff);
        state.session_name = Some("test".into());
        state.restore_load_failed = true;
        state.restore_status = "failed".into();
        state_load(&json!({"path": path}), &mut state)
            .await
            .unwrap();
        assert!(!state.restore_load_failed);
        assert_eq!(state.restore_status, "loaded");
    }

    #[tokio::test]
    async fn unreachable_origin_is_skipped_and_restore_continues() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                let Message::Text(text) = message else {
                    continue;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                let id = request["id"].clone();
                let reply = if request["method"] == "browsingContext.navigate"
                    && request["params"]["url"] == "https://unreachable.example/"
                {
                    json!({"type":"error","id":id,"error":"unknown error","message":"unreachable"})
                } else if request["method"] == "script.callFunction" {
                    json!({
                        "type": "success",
                        "id": id,
                        "result": {
                            "type": "success",
                            "result": {"type": "boolean", "value": true}
                        }
                    })
                } else {
                    json!({"type":"success","id":id,"result":{}})
                };
                ws.send(Message::Text(reply.to_string())).await.unwrap();
            }
        });
        let (mut ff, _tx, _) = super::super::session::tests::fake_session().await;
        ff.bidi = std::sync::Arc::new(
            super::super::client::BidiClient::connect(&url)
                .await
                .unwrap(),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "cookies": [],
                "origins": [
                    {
                        "origin": "https://unreachable.example",
                        "localStorage": [{"name": "a", "value": "1"}],
                        "sessionStorage": []
                    },
                    {
                        "origin": "https://reachable.example",
                        "localStorage": [{"name": "b", "value": "2"}],
                        "sessionStorage": []
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        let skipped = load_state_path(&mut state, path.to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(skipped, vec!["https://unreachable.example".to_string()]);
        assert!(state
            .firefox
            .as_ref()
            .unwrap()
            .visited_origins
            .contains("https://reachable.example"));
        server.abort();
    }

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
