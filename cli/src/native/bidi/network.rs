use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{broadcast, RwLock};

use super::actions::session;
use super::client::BidiClient;
use super::forms::require;
use super::session::FirefoxSession;
use super::target::resolve_target;
use super::types::{BytesValue, Header, NetworkData, NetworkEvent, RequestData};
use crate::native::actions::{
    self as chrome, DaemonState, HarContentMode, HarEntry, RouteEntry, TrackedRequest,
};
use crate::native::network::DomainFilter;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NetworkDecision {
    Fail,
    Fulfill {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    },
    Abort,
    Continue {
        headers: Option<Vec<(String, String)>>,
    },
}

fn is_blocked(url: &str, filter: Option<&DomainFilter>) -> bool {
    let Some(filter) = filter else {
        return false;
    };
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https" | "ws" | "wss") {
        return false;
    }
    parsed
        .host_str()
        .is_some_and(|hostname| !filter.is_allowed(hostname))
}

/// Decides how one Firefox request is handled: fail, fulfill, abort or continue.
pub(crate) fn decide_request(
    url: &str,
    resource_type: &str,
    headers: &HashMap<String, String>,
    domain_filter: Option<&DomainFilter>,
    routes: &[RouteEntry],
    origin_headers: &HashMap<String, HashMap<String, String>>,
) -> NetworkDecision {
    if is_blocked(url, domain_filter) {
        return NetworkDecision::Fail;
    }

    for route in routes {
        if !route.matches(url, resource_type) {
            continue;
        }
        if route.abort {
            return NetworkDecision::Abort;
        }
        if let Some(response) = &route.response {
            let mut merged_headers = Vec::new();
            if let Some(content_type) = &response.content_type {
                merged_headers.push(("Content-Type".to_string(), content_type.clone()));
            }
            if let Some(extra) = &response.headers {
                merged_headers.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
            return NetworkDecision::Fulfill {
                status: response.status.unwrap_or(200),
                headers: merged_headers,
                body: response.body.clone().unwrap_or_default(),
            };
        }
    }

    let extra_headers = url::Url::parse(url)
        .ok()
        .map(|u| u.origin().ascii_serialization())
        .and_then(|origin| origin_headers.get(&origin));

    let Some(extra_headers) = extra_headers else {
        return NetworkDecision::Continue { headers: None };
    };

    let mut merged_headers: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| {
            !extra_headers
                .keys()
                .any(|extra| extra.eq_ignore_ascii_case(name))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    merged_headers.extend(extra_headers.iter().map(|(k, v)| (k.clone(), v.clone())));

    NetworkDecision::Continue {
        headers: Some(merged_headers),
    }
}

/// BiDi events the session subscribes to for network, console and download handling.
pub const NETWORK_EVENTS: [&str; 8] = [
    "network.beforeRequestSent",
    "network.responseStarted",
    "network.responseCompleted",
    "network.fetchError",
    "network.authRequired",
    "log.entryAdded",
    "browsingContext.downloadWillBegin",
    "browsingContext.downloadEnd",
];

const IDLE_WINDOW: Duration = Duration::from_millis(500);
const DOWNLOAD_TIMEOUT_MS: u64 = 30_000;

/// Shared state the dispatcher reads for every request; most `Arc`s are the daemon's own.
#[derive(Clone, Default)]
pub struct NetContext {
    pub routes: Arc<RwLock<Vec<RouteEntry>>>,
    pub domain_filter: Arc<RwLock<Option<DomainFilter>>>,
    pub origin_headers: Arc<RwLock<HashMap<String, HashMap<String, String>>>>,
    pub proxy_credentials: Arc<RwLock<Option<(String, String)>>>,
    /// Proxy credential attempts per request id; a request gets two before the challenge is cancelled.
    pub proxy_auth_requests: Arc<RwLock<HashMap<String, u8>>>,
    pub headers: Arc<RwLock<HashMap<String, String>>>,
    /// Contexts whose requests are answered with a blank page instead of reaching the network.
    pub blank_contexts: Arc<RwLock<std::collections::HashSet<String>>>,
}

impl NetContext {
    /// Shares the daemon's routes, domain filter, origin headers and proxy credentials.
    pub fn from_state(state: &DaemonState) -> Self {
        Self {
            routes: state.routes.clone(),
            domain_filter: state.domain_filter.clone(),
            origin_headers: state.origin_headers.clone(),
            proxy_credentials: state.proxy_credentials.clone(),
            ..Self::default()
        }
    }
}

/// A background task that is aborted when dropped.
pub struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn header_list(headers: &[Header]) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|h| (h.name.clone(), h.value.text()))
        .collect()
}

fn header_object(headers: &[Header]) -> Value {
    Value::Object(
        header_list(headers)
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
    )
}

fn bidi_headers(headers: Vec<(String, String)>) -> Vec<Header> {
    headers
        .into_iter()
        .map(|(name, value)| Header::string(name, value))
        .collect()
}

/// Maps a BiDi request to the resource type names Chrome reports.
fn resource_type(request: &RequestData) -> &'static str {
    match (
        request.initiator_type.as_deref(),
        request.destination.as_deref(),
    ) {
        (Some("fetch"), _) => "Fetch",
        (Some("xmlhttprequest"), _) => "XHR",
        (Some("beacon"), _) => "Ping",
        (_, Some("document" | "iframe" | "frame")) => "Document",
        (_, Some("script")) => "Script",
        (_, Some("style")) => "Stylesheet",
        (_, Some("image")) => "Image",
        (_, Some("font")) => "Font",
        (_, Some("audio" | "video" | "track")) => "Media",
        _ => "Other",
    }
}

fn is_websocket(url: &str) -> bool {
    url.starts_with("ws:") || url.starts_with("wss:")
}

/// Installs the single global intercept the first time it is needed.
pub async fn ensure_intercept(ff: &mut FirefoxSession) -> Result<(), String> {
    if !ff.intercepting {
        ff.bidi
            .send(
                "network.addIntercept",
                json!({"phases": ["beforeRequestSent", "authRequired"]}),
            )
            .await?;
        ff.intercepting = true;
    }
    Ok(())
}

/// Starts the task that resolves every intercepted request exactly once.
pub fn spawn_dispatcher(bidi: Arc<BidiClient>, net: NetContext) -> AbortOnDrop {
    let mut rx = bidi.subscribe();
    AbortOnDrop(tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(e) if e["params"]["isBlocked"] == true => {
                    tokio::spawn(resolve(bidi.clone(), net.clone(), e));
                }
                Ok(e)
                    if matches!(
                        e["method"].as_str(),
                        Some("network.responseCompleted" | "network.fetchError")
                    ) =>
                {
                    if let Some(id) = e["params"]["request"]["request"].as_str() {
                        net.proxy_auth_requests.write().await.remove(id);
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    }))
}

async fn resolve(bidi: Arc<BidiClient>, net: NetContext, event: Value) {
    let ev: NetworkEvent = serde_json::from_value(event["params"].clone()).unwrap_or_default();
    let id = ev.request.request.clone();
    let (result, fallback, fallback_params) = match event["method"].as_str() {
        Some("network.beforeRequestSent") => (
            before_request(&bidi, &net, &ev.request, ev.context.as_deref()).await,
            "network.failRequest",
            json!({"request": id}),
        ),
        Some("network.authRequired") => (
            auth_required(&bidi, &net, &id, &event["params"]).await,
            "network.continueWithAuth",
            json!({"request": id, "action": "cancel"}),
        ),
        _ => return,
    };
    if result.is_err() {
        let _ = bidi.send(fallback, fallback_params).await;
    }
}

async fn before_request(
    bidi: &BidiClient,
    net: &NetContext,
    request: &RequestData,
    context: Option<&str>,
) -> Result<(), String> {
    let id = request.request.as_str();
    let url = request.url.as_str();
    let blank = match context {
        Some(c) => net.blank_contexts.read().await.contains(c),
        None => false,
    };
    let mut decision = {
        let mut extra = net.headers.read().await.clone();
        let origin = url::Url::parse(url)
            .ok()
            .map(|u| u.origin().ascii_serialization())
            .unwrap_or_default();
        if let Some(scoped) = net.origin_headers.read().await.get(&origin) {
            extra.retain(|name, _| !scoped.keys().any(|k| k.eq_ignore_ascii_case(name)));
            extra.extend(scoped.clone());
        }
        let origin_headers = if extra.is_empty() {
            HashMap::new()
        } else {
            HashMap::from([(origin, extra)])
        };
        let headers: HashMap<String, String> = header_list(&request.headers).into_iter().collect();
        let filter = net.domain_filter.read().await;
        let routes = net.routes.read().await;
        decide_request(
            url,
            resource_type(request),
            &headers,
            filter.as_ref(),
            &routes,
            &origin_headers,
        )
    };
    if blank && !matches!(decision, NetworkDecision::Fail | NetworkDecision::Abort) {
        decision = NetworkDecision::Fulfill {
            status: 200,
            headers: vec![("Content-Type".to_string(), "text/html".to_string())],
            body: "<html></html>".to_string(),
        };
    }
    match decision {
        NetworkDecision::Fail | NetworkDecision::Abort => {
            bidi.send("network.failRequest", json!({"request": id}))
                .await?;
        }
        NetworkDecision::Fulfill {
            status,
            headers,
            body,
        } => {
            bidi.send(
                "network.provideResponse",
                json!({
                    "request": id,
                    "statusCode": status,
                    "headers": bidi_headers(headers),
                    "body": BytesValue::string(body),
                }),
            )
            .await?;
        }
        NetworkDecision::Continue { headers } => {
            let mut params = json!({"request": id});
            if let Some(headers) = headers {
                params["headers"] = json!(bidi_headers(headers));
            }
            bidi.send("network.continueRequest", params).await?;
        }
    }
    Ok(())
}

/// Answers an auth challenge; proxy credentials go only to proxy (407) challenges, never to origins.
async fn auth_required(
    bidi: &BidiClient,
    net: &NetContext,
    id: &str,
    event: &Value,
) -> Result<(), String> {
    let credentials = if event["response"]["status"] == 407 {
        net.proxy_credentials.read().await.clone()
    } else {
        None
    };
    let params = match credentials {
        Some((username, password))
            if {
                let mut tries = net.proxy_auth_requests.write().await;
                let n = tries.entry(id.to_string()).or_insert(0);
                *n += 1;
                *n <= 2
            } =>
        {
            json!({
                "request": id,
                "action": "provideCredentials",
                "credentials": {"type": "password", "username": username, "password": password},
            })
        }
        _ => json!({"request": id, "action": "cancel"}),
    };
    bidi.send("network.continueWithAuth", params).await?;
    Ok(())
}

/// Drains buffered BiDi events and applies them to the daemon's dialog, network and console state.
pub async fn drain(state: &mut DaemonState) {
    if let Some(ff) = state.firefox.as_mut() {
        let events = ff.drain_events(&mut state.pending_dialog);
        apply_events(state, events).await;
    }
}

async fn apply_events(state: &mut DaemonState, events: Vec<Value>) {
    for e in events {
        let method = e["method"].as_str().unwrap_or("");
        if method == "log.entryAdded" {
            log_entry(state, &e["params"]);
            continue;
        }
        let p: NetworkEvent = serde_json::from_value(e["params"].clone()).unwrap_or_default();
        let id = p.request.request.as_str();
        match method {
            "network.beforeRequestSent" => request_started(state, &p),
            "network.responseStarted" => response_seen(state, &p),
            "network.responseCompleted" => {
                response_seen(state, &p);
                if let Some(ff) = state.firefox.as_mut() {
                    ff.inflight.remove(id);
                }
                har_body(state, id).await;
            }
            "network.fetchError" => {
                if let Some(ff) = state.firefox.as_mut() {
                    ff.inflight.remove(id);
                }
            }
            _ => {}
        }
    }
}

fn request_started(state: &mut DaemonState, p: &NetworkEvent) {
    let request = &p.request;
    let id = request.request.clone();
    let url = request.url.clone();
    let method = request.method.clone().unwrap_or_else(|| "GET".to_string());
    if !is_websocket(&url) {
        if let Some(ff) = state.firefox.as_mut() {
            ff.inflight.insert(id.clone());
        }
    }
    let millis = p.timestamp;
    let resource_type = resource_type(request).to_string();
    if state.har_recording {
        state.har_entries.push(HarEntry {
            request_id: id.clone(),
            wall_time: millis as f64 / 1000.0,
            method: method.clone(),
            url: url.clone(),
            request_headers: header_list(&request.headers),
            post_data: None,
            request_body_size: request.body_size.unwrap_or(0),
            resource_type: resource_type.clone(),
            status: None,
            status_text: String::new(),
            http_version: "HTTP/1.1".to_string(),
            response_headers: Vec::new(),
            mime_type: String::new(),
            redirect_url: String::new(),
            response_body_size: -1,
            cdp_timing: None,
            loading_finished_timestamp: None,
            response_body: None,
            response_body_base64: false,
        });
    }
    if state.request_tracking {
        state.tracked_requests.push(TrackedRequest {
            url,
            method,
            headers: header_object(&request.headers),
            timestamp: millis,
            resource_type,
            request_id: id,
            post_data: None,
            status: None,
            response_headers: None,
            mime_type: None,
        });
    }
}

fn response_seen(state: &mut DaemonState, p: &NetworkEvent) {
    let id = p.request.request.as_str();
    let response = &p.response;
    let status = response.status;
    let mime = response.mime_type.clone();
    if state.har_recording {
        if let Some(entry) = state
            .har_entries
            .iter_mut()
            .rev()
            .find(|e| e.request_id == id)
        {
            let headers = header_list(&response.headers);
            entry.status = status;
            entry.status_text = response.status_text.clone();
            entry.mime_type = mime.clone();
            entry.http_version = chrome::har_cdp_protocol_to_http_version(&response.protocol);
            entry.redirect_url = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("location"))
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            entry.response_headers = headers;
            entry.response_body_size = response.body_size.or(response.content.size).unwrap_or(-1);
        }
    }
    if state.request_tracking {
        if let Some(entry) = state
            .tracked_requests
            .iter_mut()
            .rev()
            .find(|e| e.request_id == id)
        {
            entry.status = status;
            entry.mime_type = Some(mime);
            entry.response_headers = Some(header_object(&response.headers));
        }
    }
}

/// Reads the body bytes of a collected response as `(text, is_base64)`.
async fn response_data(bidi: &BidiClient, id: &str) -> Result<(String, bool), String> {
    let r: NetworkData = bidi
        .send_typed(
            "network.getData",
            &json!({"dataType": "response", "request": id}),
        )
        .await?;
    Ok((r.bytes.raw().to_string(), r.bytes.is_base64()))
}

async fn har_body(state: &mut DaemonState, id: &str) {
    let wanted = state.har_recording
        && state
            .har_entries
            .iter()
            .rev()
            .find(|e| e.request_id == id)
            .is_some_and(|e| {
                e.response_body.is_none()
                    && match state.har_content_mode {
                        HarContentMode::None => false,
                        HarContentMode::Text => chrome::har_mime_is_text(&e.mime_type),
                        HarContentMode::All => true,
                    }
            });
    let Some(ff) = state.firefox.as_ref().filter(|_| wanted) else {
        return;
    };
    let Ok((body, base64)) = response_data(&ff.bidi, id).await else {
        return;
    };
    if body.is_empty()
        || body.len() > chrome::HAR_MAX_BODY_BYTES
        || state.har_body_total_bytes + body.len() > chrome::HAR_MAX_TOTAL_BODY_BYTES
    {
        return;
    }
    if let Some(entry) = state
        .har_entries
        .iter_mut()
        .rev()
        .find(|e| e.request_id == id)
    {
        state.har_body_total_bytes += body.len();
        entry.response_body = Some(body);
        entry.response_body_base64 = base64;
    }
}

fn log_entry(state: &mut DaemonState, p: &Value) {
    let text = p["text"].as_str().unwrap_or("");
    match p["type"].as_str() {
        Some("console") => {
            let level = match p["method"].as_str().unwrap_or("log") {
                "warn" => "warning",
                other => other,
            };
            let args = p["args"].as_array().cloned().unwrap_or_default();
            state.event_tracker.add_console(level, text, args);
        }
        Some("javascript") => {
            let frame = &p["stackTrace"]["callFrames"][0];
            state.event_tracker.add_error(
                text,
                frame["url"].as_str(),
                frame["lineNumber"].as_i64(),
                frame["columnNumber"].as_i64(),
            );
        }
        _ => {}
    }
}

/// Waits until no tracked request has been in flight for 500ms; `Err` on timeout.
pub async fn wait_idle(state: &mut DaemonState, timeout_ms: u64) -> Result<(), ()> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut idle_since: Option<tokio::time::Instant> = None;
    loop {
        drain(state).await;
        let busy = state
            .firefox
            .as_ref()
            .is_some_and(|ff| !ff.inflight.is_empty());
        let now = tokio::time::Instant::now();
        if busy {
            idle_since = None;
        } else if now.duration_since(*idle_since.get_or_insert(now)) >= IDLE_WINDOW {
            return Ok(());
        }
        if now >= deadline {
            return Err(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Stores origin-scoped headers from `open --headers` and makes sure requests are intercepted.
pub async fn set_origin_headers(
    cmd: &Value,
    url: &str,
    state: &mut DaemonState,
) -> Result<(), String> {
    let Some(map) = cmd
        .get("headers")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty())
    else {
        return Ok(());
    };
    let Ok(parsed) = url::Url::parse(url) else {
        return Ok(());
    };
    let headers = map
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
        .collect();
    let ff = session(state)?;
    ff.net
        .origin_headers
        .write()
        .await
        .insert(parsed.origin().ascii_serialization(), headers);
    ensure_intercept(ff).await
}

/// `route`: adds a route to the shared list and installs the intercept if needed.
pub async fn route(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    session(state)?;
    let url = chrome::add_route(cmd, state).await?;
    ensure_intercept(session(state)?).await?;
    Ok(json!({ "routed": url }))
}

/// `unroute`: removes one route or all of them.
pub async fn unroute(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    session(state)?;
    Ok(json!({ "unrouted": chrome::remove_routes(cmd, state).await }))
}

async fn set_headers(
    state: &mut DaemonState,
    headers: HashMap<String, String>,
) -> Result<Value, String> {
    let ff = session(state)?;
    let any = !headers.is_empty();
    *ff.net.headers.write().await = headers;
    if any {
        ensure_intercept(ff).await?;
    }
    Ok(json!({ "set": true }))
}

/// `headers`: replaces the extra headers sent with every request.
pub async fn headers(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let headers = cmd
        .get("headers")
        .ok_or("Missing 'headers' parameter")?
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();
    set_headers(state, headers).await
}

/// `credentials`: sends HTTP basic auth with every request, as Chrome does.
pub async fn credentials(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let field = |key: &str| {
        cmd.get(key)
            .and_then(Value::as_str)
            .ok_or(format!("Missing '{key}' parameter"))
    };
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{}:{}", field("username")?, field("password")?),
    );
    set_headers(
        state,
        HashMap::from([("Authorization".to_string(), format!("Basic {encoded}"))]),
    )
    .await
}

/// `request_detail`: the tracked request plus its response body when one was collected.
pub async fn request_detail(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mut result = chrome::handle_request_detail(cmd, state).await?;
    if let (Some(ff), Some(id)) = (
        state.firefox.as_ref(),
        cmd.get("requestId").and_then(Value::as_str),
    ) {
        if let Ok((body, base64)) = response_data(&ff.bidi, id).await {
            result["responseBody"] = if base64 {
                json!(format!("[base64, {} chars]", body.len()))
            } else {
                json!(body)
            };
        }
    }
    Ok(result)
}

/// True when `context` is the active tab's top-level context or one of its frames.
async fn in_active_tab(ff: &FirefoxSession, context: &str) -> bool {
    fn contains(node: &Value, context: &str) -> bool {
        node["context"] == context
            || node["children"]
                .as_array()
                .is_some_and(|c| c.iter().any(|n| contains(n, context)))
    }
    if context == ff.top_context {
        return true;
    }
    ff.bidi
        .send("browsingContext.getTree", json!({"root": ff.top_context}))
        .await
        .is_ok_and(|tree| {
            tree["contexts"]
                .as_array()
                .is_some_and(|c| c.iter().any(|n| contains(n, context)))
        })
}

/// `responsebody`: waits for the next response whose URL contains the pattern and returns its body.
pub async fn responsebody(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let pattern = require(cmd, "url")?;
    let timeout_ms = cmd
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(state.default_timeout_ms);
    let ff = session(state)?;
    let mut rx = ff.bidi.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let event = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Err("Event stream closed".to_string()),
            Err(_) => return Err(format!("Timeout waiting for response matching '{pattern}'")),
        };
        let p: NetworkEvent = serde_json::from_value(event["params"].clone()).unwrap_or_default();
        if event["method"] == "network.responseCompleted"
            && p.response.url.contains(pattern)
            && in_active_tab(ff, event["params"]["context"].as_str().unwrap_or("")).await
        {
            let (body, _) = response_data(&ff.bidi, &p.request.request).await?;
            return Ok(json!({
                "body": body,
                "status": p.response.status.unwrap_or(0),
                "headers": header_object(&p.response.headers),
            }));
        }
    }
}

/// `har_start`: starts recording requests, with bodies per the `content` mode.
pub async fn har_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let mode = match cmd.get("content").and_then(Value::as_str) {
        Some(s) => HarContentMode::parse(s)?,
        None => HarContentMode::default(),
    };
    session(state)?;
    state.har_recording = true;
    state.har_entries.clear();
    state.har_content_mode = mode;
    state.har_body_total_bytes = 0;
    Ok(json!({ "started": true }))
}

/// Waits for the download that starts in one of `contexts` (any context when empty) and returns its file path when Firefox reports one.
async fn wait_download(
    rx: &mut broadcast::Receiver<Value>,
    timeout_ms: u64,
    dir: Option<&std::path::Path>,
    contexts: &[&str],
    timeout_error: &str,
) -> Result<Option<String>, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    let mut tracked: Option<(Value, Option<String>)> = None;
    loop {
        let event = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(_)) => return Err("Event stream closed".to_string()),
            Err(_) => return Err(timeout_error.to_string()),
        };
        let p = &event["params"];
        match (event["method"].as_str(), &tracked) {
            (Some("browsingContext.downloadWillBegin"), None)
                if contexts.is_empty() || contexts.iter().any(|c| p["context"] == *c) =>
            {
                tracked = Some((
                    p["navigation"].clone(),
                    p["suggestedFilename"].as_str().map(String::from),
                ));
            }
            (Some("browsingContext.downloadEnd"), Some((id, suggested)))
                if p["navigation"] == *id =>
            {
                if p["status"] == "canceled" {
                    return Err("Download was canceled".to_string());
                }
                return Ok(p["filepath"].as_str().map(String::from).or_else(|| {
                    Some(
                        dir?.join(suggested.as_deref()?)
                            .to_string_lossy()
                            .into_owned(),
                    )
                }));
            }
            _ => {}
        }
    }
}

/// `download`: clicks the ref or selector and saves the resulting file at `path`.
pub async fn download(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let selector = require(cmd, "selector")?;
    let path = require(cmd, "path")?;
    let (dir, dest) = chrome::resolve_download_dest(path)?;
    let target = resolve_target(state, selector).await?;
    let ff = session(state)?;
    let mut rx = ff.bidi.subscribe();
    ff.bidi
        .send(
            "browser.setDownloadBehavior",
            json!({"downloadBehavior": {"type": "allowed", "destinationFolder": dir}}),
        )
        .await?;
    let this = json!({"sharedId": target.shared_id});
    let clicked = ff
        .race_prompt(
            &target.context,
            ff.bidi.call_function(
                &target.context,
                "function() { this.click(); return true; }",
                Some(this),
                vec![],
            ),
        )
        .await?;
    if clicked.is_none() {
        return Ok(json!({ "clicked": selector, "dialogOpened": true }));
    }
    let saved = wait_download(
        &mut rx,
        DOWNLOAD_TIMEOUT_MS,
        Some(&dir),
        &[target.context.as_str(), ff.top_context.as_str()],
        "Timeout waiting for download to complete",
    )
    .await?;
    match saved {
        Some(file) if std::path::Path::new(&file) != dest => std::fs::rename(&file, &dest)
            .map_err(|e| format!("Failed to rename downloaded file: {e}"))?,
        Some(_) => {}
        None => {
            return Err(
                "Download completed but could not determine the downloaded file name".to_string(),
            )
        }
    }
    Ok(json!({ "path": dest.to_string_lossy() }))
}

/// `waitfordownload`: waits for the next finished download.
pub async fn waitfordownload(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let timeout_ms = cmd
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(state.default_timeout_ms);
    let mut rx = session(state)?.bidi.subscribe();
    let saved = wait_download(
        &mut rx,
        timeout_ms,
        None,
        &[],
        "Timeout waiting for download",
    )
    .await?;
    let path = cmd
        .get("path")
        .and_then(Value::as_str)
        .map(String::from)
        .or(saved)
        .unwrap_or_else(|| "download".to_string());
    Ok(json!({ "path": path }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::actions::RouteResponse;

    fn mock_route(pattern: &str, body: &str, status: u16) -> RouteEntry {
        RouteEntry {
            url_pattern: pattern.to_string(),
            response: Some(RouteResponse {
                status: Some(status),
                body: Some(body.to_string()),
                content_type: None,
                headers: None,
            }),
            abort: false,
            resource_types: Vec::new(),
        }
    }

    #[tokio::test]
    async fn wait_download_ignores_other_downloads() {
        let (tx, mut rx) = broadcast::channel(16);
        let send = |method: &str, params: Value| {
            tx.send(json!({"method": method, "params": params}))
                .unwrap();
        };
        send(
            "browsingContext.downloadWillBegin",
            json!({"context": "other", "navigation": "n1", "suggestedFilename": "a.txt"}),
        );
        send(
            "browsingContext.downloadWillBegin",
            json!({"context": "mine", "navigation": "n2", "suggestedFilename": "b.txt"}),
        );
        send(
            "browsingContext.downloadEnd",
            json!({"navigation": "n1", "status": "canceled"}),
        );
        send(
            "browsingContext.downloadEnd",
            json!({"navigation": "n2", "status": "complete", "filepath": "/tmp/b.txt"}),
        );
        let saved = wait_download(&mut rx, 1000, None, &["mine"], "timeout")
            .await
            .unwrap();
        assert_eq!(saved.as_deref(), Some("/tmp/b.txt"));
    }

    #[test]
    fn blocked_domain_fails_even_when_route_matches() {
        let filter = DomainFilter::new("allowed.example");
        let routes = vec![mock_route("*", "mocked", 200)];

        let decision = decide_request(
            "https://blocked.example/page",
            "document",
            &HashMap::new(),
            Some(&filter),
            &routes,
            &HashMap::new(),
        );

        assert_eq!(decision, NetworkDecision::Fail);
    }

    #[test]
    fn first_matching_route_wins() {
        let routes = vec![
            mock_route("*", "first", 200),
            mock_route("*", "second", 200),
        ];

        let decision = decide_request(
            "https://example.com/page",
            "document",
            &HashMap::new(),
            None,
            &routes,
            &HashMap::new(),
        );

        match decision {
            NetworkDecision::Fulfill { body, .. } => assert_eq!(body, "first"),
            other => panic!("expected fulfill, got {:?}", other),
        }
    }

    #[test]
    fn mock_route_returns_status_body_and_headers() {
        let routes = vec![RouteEntry {
            url_pattern: "*".to_string(),
            response: Some(RouteResponse {
                status: Some(201),
                body: Some("hello".to_string()),
                content_type: Some("text/plain".to_string()),
                headers: Some(HashMap::from([("X-Test".to_string(), "yes".to_string())])),
            }),
            abort: false,
            resource_types: Vec::new(),
        }];

        let decision = decide_request(
            "https://example.com/page",
            "document",
            &HashMap::new(),
            None,
            &routes,
            &HashMap::new(),
        );

        assert_eq!(
            decision,
            NetworkDecision::Fulfill {
                status: 201,
                headers: vec![
                    ("Content-Type".to_string(), "text/plain".to_string()),
                    ("X-Test".to_string(), "yes".to_string()),
                ],
                body: "hello".to_string(),
            }
        );
    }

    #[test]
    fn extra_headers_are_merged_on_continue() {
        let origin_headers = HashMap::from([(
            "https://example.com".to_string(),
            HashMap::from([("X-Extra".to_string(), "new".to_string())]),
        )]);
        let headers = HashMap::from([
            ("Accept".to_string(), "text/html".to_string()),
            ("x-extra".to_string(), "old".to_string()),
        ]);

        let decision = decide_request(
            "https://example.com/page",
            "document",
            &headers,
            None,
            &[],
            &origin_headers,
        );

        assert_eq!(
            decision,
            NetworkDecision::Continue {
                headers: Some(vec![
                    ("Accept".to_string(), "text/html".to_string()),
                    ("X-Extra".to_string(), "new".to_string()),
                ]),
            }
        );
    }

    #[test]
    fn non_matching_allowed_request_continues_unchanged() {
        let filter = DomainFilter::new("allowed.example");
        let routes = vec![mock_route("*.json", "mocked", 200)];

        let decision = decide_request(
            "https://allowed.example/page",
            "document",
            &HashMap::new(),
            Some(&filter),
            &routes,
            &HashMap::new(),
        );

        assert_eq!(decision, NetworkDecision::Continue { headers: None });
    }

    use crate::native::bidi::session::tests::fake_session;

    fn request_event(method: &str, id: &str, url: &str, blocked: bool) -> Value {
        json!({
            "type": "event",
            "method": method,
            "params": {
                "isBlocked": blocked,
                "timestamp": 1000,
                "request": {"request": id, "url": url, "method": "GET", "headers": []},
                "response": {"status": 200, "headers": []},
            },
        })
    }

    #[tokio::test]
    async fn dispatcher_resolves_each_blocked_request_once() {
        let (ff, tx, calls) = fake_session().await;
        let net = NetContext::default();
        *net.domain_filter.write().await = Some(DomainFilter::new("allowed.example"));
        let _dispatcher = spawn_dispatcher(ff.bidi.clone(), net);
        let blocked = "network.beforeRequestSent";
        tx.send(request_event(
            blocked,
            "r1",
            "https://blocked.example/x",
            true,
        ))
        .unwrap();
        tx.send(request_event(
            blocked,
            "r2",
            "https://allowed.example/x",
            true,
        ))
        .unwrap();
        tx.send(request_event(
            blocked,
            "r3",
            "https://blocked.example/y",
            false,
        ))
        .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut seen = calls.lock().unwrap().clone();
        seen.sort();
        assert_eq!(seen, ["network.continueRequest", "network.failRequest"]);
    }

    #[tokio::test]
    async fn in_flight_set_ignores_websockets_and_clears_on_completion() {
        let (ff, _tx, _) = fake_session().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        let started = "network.beforeRequestSent";
        apply_events(
            &mut state,
            vec![
                request_event(started, "a", "http://example.com/", false),
                request_event(started, "b", "ws://example.com/socket", false),
            ],
        )
        .await;
        assert_eq!(state.firefox.as_ref().unwrap().inflight.len(), 1);
        apply_events(
            &mut state,
            vec![request_event(
                "network.responseCompleted",
                "a",
                "http://example.com/",
                false,
            )],
        )
        .await;
        assert!(state.firefox.as_ref().unwrap().inflight.is_empty());
    }

    #[test]
    fn log_entries_fill_console_and_errors() {
        let mut state = DaemonState::new();
        log_entry(
            &mut state,
            &json!({"type": "console", "method": "warn", "text": "careful", "args": [{"type": "string", "value": "careful"}]}),
        );
        log_entry(
            &mut state,
            &json!({"type": "javascript", "text": "Error: boom", "stackTrace": {"callFrames": [{"url": "http://x/", "lineNumber": 3, "columnNumber": 7}]}}),
        );
        let console = state.event_tracker.get_console_json();
        assert_eq!(console["messages"][0]["type"], "warning");
        assert_eq!(console["messages"][0]["args"][0]["value"], "careful");
        let errors = state.event_tracker.get_errors_json();
        assert_eq!(errors["errors"][0]["text"], "Error: boom");
        assert_eq!(errors["errors"][0]["line"], 3);
    }
}
