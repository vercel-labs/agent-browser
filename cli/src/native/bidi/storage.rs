//! Firefox cookies, web storage and saved state. Formats must match `cookies.rs` and `state.rs`.

use serde_json::{json, Value};
use std::collections::HashSet;

use super::actions::session;
use super::forms::require;
use super::page::normalize_net_error_text;
use super::session::FirefoxSession;
use super::types::{BytesValue, Cookie as BidiCookie, GetCookies};
use crate::native::actions::{self as chrome, DaemonState};
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
    } catch(e) { return null; }
    try {
        for (let i = 0; i < sessionStorage.length; i++) {
            const key = sessionStorage.key(i);
            result.sessionStorage.push({ name: key, value: sessionStorage.getItem(key) });
        }
    } catch(e) { return null; }
    return result;
}"#;

fn storage_js_name(storage_type: &str) -> &'static str {
    match storage_type {
        "session" => "sessionStorage",
        _ => "localStorage",
    }
}

fn same_site_to_state(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
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

/// Converts a BiDi `storage.getCookies` cookie to the `state.rs` cookie shape.
fn bidi_cookie_to_state(cookie: &BidiCookie) -> Cookie {
    Cookie {
        name: cookie.name.clone(),
        value: cookie.value.text(),
        domain: cookie.domain.clone(),
        path: if cookie.path.is_empty() {
            "/".to_string()
        } else {
            cookie.path.clone()
        },
        expires: cookie.expiry.map_or(-1.0, |e| e as f64),
        size: cookie.size.unwrap_or(0),
        http_only: cookie.http_only,
        secure: cookie.secure,
        session: cookie.expiry.is_none(),
        same_site: cookie.same_site.as_deref().and_then(same_site_to_state),
    }
}

/// Converts a `state.rs` cookie to the BiDi `storage.setCookie` cookie shape.
/// Chrome-only fields are absent from the source struct and therefore dropped.
fn state_cookie_to_bidi(cookie: &Cookie) -> BidiCookie {
    BidiCookie {
        name: cookie.name.clone(),
        value: BytesValue::string(&cookie.value),
        domain: cookie.domain.clone(),
        path: if cookie.path.is_empty() {
            "/".to_string()
        } else {
            cookie.path.clone()
        },
        size: None,
        http_only: cookie.http_only,
        secure: cookie.secure,
        same_site: cookie.same_site.as_deref().map(str::to_ascii_lowercase),
        expiry: (cookie.expires > 0.0).then_some(cookie.expires as u64),
    }
}

/// Converts a `cookies set` payload to BiDi `storage.setCookie` params. Fields Chrome knows
/// but BiDi does not (`priority`, `sourceScheme`, `sameParty`, `partitionKey`) are ignored.
fn command_cookie_to_bidi(cookie: &Value, current_url: Option<&str>) -> Result<BidiCookie, String> {
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

    Ok(BidiCookie {
        name: name.to_string(),
        value: BytesValue::string(value),
        domain,
        path,
        size: None,
        http_only: cookie
            .get("httpOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        secure: cookie
            .get("secure")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| parsed.as_ref().is_some_and(|u| u.scheme() == "https")),
        same_site: same_site_to_bidi(&cookie["sameSite"]),
        expiry: cookie
            .get("expires")
            .and_then(Value::as_f64)
            .filter(|e| *e > 0.0)
            .map(|e| e as u64),
    })
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
        .send("browsingContext.getTree", json!({"root": ff.context}))
        .await?;
    let contexts = tree["contexts"]
        .as_array()
        .ok_or("Failed to read active browsing contexts")?;
    let mut urls = Vec::new();
    append_context_urls(contexts, &mut urls);
    Ok(urls)
}

async fn get_all_cookies(ff: &FirefoxSession) -> Result<Vec<Cookie>, String> {
    let result: GetCookies = ff.bidi.send_typed("storage.getCookies", &json!({})).await?;
    Ok(result.cookies.iter().map(bidi_cookie_to_state).collect())
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
    let key = require(cmd, "key")?;
    let value = require(cmd, "value")?;
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

fn collect_context_ids(contexts: &[Value], out: &mut Vec<(String, String)>) {
    for context in contexts {
        let url = context["url"].as_str().unwrap_or("");
        if let Some(id) = context["context"]
            .as_str()
            .filter(|_| !url.starts_with("about:"))
        {
            out.push((id.to_string(), url.to_string()));
        }
        if let Some(children) = context["children"].as_array() {
            collect_context_ids(children, out);
        }
    }
}

async fn read_origin(ff: &FirefoxSession, context: &str) -> Result<Option<OriginStorage>, String> {
    let data = ff
        .bidi
        .call_function(context, ORIGIN_STORAGE_JS, None, Vec::new())
        .await
        .map_err(|e| format!("Failed to read storage: {e}"))?;
    Ok(state::parse_origin_storage(&data))
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
    let created = ff
        .bidi
        .send(
            "browsingContext.create",
            json!({"type": "tab", "background": true}),
        )
        .await?;
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
        if let Some(filter) = ff.net.domain_filter.read().await.as_ref() {
            if filter.check_url(origin).is_err() {
                continue;
            }
        }
        ff.bidi
            .send(
                "browsingContext.navigate",
                json!({"context": context, "url": format!("{origin}/"), "wait": "complete"}),
            )
            .await
            .map_err(|e| format!("Failed to read storage for {origin}: {e}"))?;
        if let Some(data) = read_origin(ff, context).await? {
            if data.origin == *origin {
                found.push(data);
            }
        }
    }
    Ok(found)
}

/// Collects open and previously visited origins, returning which origins were readable, including empty ones.
/// Protocol failures abort the save; unavailable storage is omitted so autosave can keep its previous value.
async fn collect_origins(
    ff: &mut FirefoxSession,
) -> Result<(Vec<OriginStorage>, HashSet<String>), String> {
    let tree = ff.bidi.send("browsingContext.getTree", json!({})).await?;
    let contexts = tree["contexts"]
        .as_array()
        .ok_or("Failed to read active browsing contexts")?;
    let mut ids = Vec::new();
    collect_context_ids(contexts, &mut ids);
    let mut open = HashSet::new();
    let mut read = HashSet::new();
    let mut seen = HashSet::new();
    let mut origins = Vec::new();
    for (id, url) in ids {
        if let Ok(url) = url::Url::parse(&url) {
            let origin = url.origin().ascii_serialization();
            if origin != "null" {
                ff.visited_origins.insert(origin.clone());
                open.insert(origin);
            }
        }
        let Some(origin) = read_origin(ff, &id).await? else {
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
        .filter(|o| o.starts_with("http") && !open.contains(*o))
        .cloned()
        .collect();
    if !remaining.is_empty() {
        for origin in collect_via_temp_context(ff, &remaining).await? {
            read.insert(origin.origin.clone());
            if has_entries(&origin) && seen.insert(origin.origin.clone()) {
                origins.push(origin);
            }
        }
    }
    Ok((origins, read))
}

async fn write_state_to(
    ff: &mut FirefoxSession,
    path: &str,
    previous_path: Option<&str>,
) -> Result<String, String> {
    let cookies = get_all_cookies(ff).await?;
    let (origins, mut seen) = collect_origins(ff).await?;
    let mut state = StorageState { cookies, origins };
    if let Some(previous_path) = previous_path.filter(|path| {
        std::path::Path::new(path).exists() || std::path::Path::new(&format!("{path}.enc")).exists()
    }) {
        let json = state::read_state_json(previous_path)?;
        let previous: StorageState =
            serde_json::from_str(&json).map_err(|e| format!("Invalid state file: {e}"))?;
        state.origins.extend(
            previous
                .origins
                .into_iter()
                .filter(|origin| seen.insert(origin.origin.clone())),
        );
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

/// Saves the `--session-name` state file used by restore-on-close/autosave. A complete candidate
/// is validated and then swapped in, so a failed save never damages the last good file.
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
/// does not support, like Chrome's `handle_state_load`. Origins that redirect elsewhere are skipped and returned.
pub async fn load_state_path(ff: &mut FirefoxSession, path: &str) -> Result<Vec<String>, String> {
    let json_str = state::read_state_json(path)?;
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
    let mut skipped = Vec::new();
    for origin in &state.origins {
        if !has_entries(origin) {
            continue;
        }
        let nav_url = format!("{}/", origin.origin.trim_end_matches('/'));
        ff.bidi
            .send(
                "browsingContext.navigate",
                json!({ "context": context, "url": nav_url, "wait": "complete" }),
            )
            .await
            .map_err(|e| normalize_net_error_text(&e))?;
        ff.refs.clear();
        ff.note_visit(&nav_url);

        let payload = serde_json::to_string(origin).map_err(|e| e.to_string())?;
        let restored = ff
            .bidi
            .call_function_in_sandbox(
                &context,
                "agent-browser",
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
    let path = require(cmd, "path")?;
    chrome::ensure_state_replay_supported_by_active_domain_filter(state, "state load").await?;
    let ff = session(state)?;
    let skipped = load_state_path(ff, path).await?;
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

        let (origins, seen) = collect_origins(&mut ff).await.unwrap();

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

        let (origins, seen) = collect_origins(&mut ff).await.unwrap();

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
        let params = serde_json::to_value(state_cookie_to_bidi(&parsed.cookies[0])).unwrap();
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
        let params = serde_json::to_value(
            command_cookie_to_bidi(
                &json!({"name": "demo", "value": "42"}),
                Some("https://example.com"),
            )
            .unwrap(),
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
        let text = bidi_cookie_to_state(
            &serde_json::from_value(json!({
                "name": "demo",
                "value": {"type": "string", "value": "42"},
                "domain": "example.com",
                "path": "/",
            }))
            .unwrap(),
        );
        assert_eq!(text.value, "42");

        let encoded = bidi_cookie_to_state(
            &serde_json::from_value(json!({
                "name": "demo",
                "value": {"type": "base64", "value": "NDI="},
                "domain": "example.com",
                "path": "/",
            }))
            .unwrap(),
        );
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
        let params = serde_json::to_value(state_cookie_to_bidi(&cookie)).unwrap();
        assert!(params.get("expiry").is_none());
        assert!(params.get("sameSite").is_none());
    }

    #[test]
    fn bidi_cookies_convert_to_the_state_cookie_format() {
        let persistent = bidi_cookie_to_state(
            &serde_json::from_value(json!({
                "name": "sid",
                "value": "abc",
                "domain": "example.com",
                "path": "/",
                "size": 3,
                "httpOnly": true,
                "secure": true,
                "sameSite": "strict",
                "expiry": 1700000000u64,
            }))
            .unwrap(),
        );
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

        let session = bidi_cookie_to_state(
            &serde_json::from_value(json!({
                "name": "temp",
                "value": "v",
                "domain": "example.com",
                "path": "/",
                "size": 1,
                "httpOnly": false,
                "secure": false,
            }))
            .unwrap(),
        );
        assert_eq!(session.expires, -1.0);
        assert!(session.session);
        assert!(session.same_site.is_none());
    }
}
