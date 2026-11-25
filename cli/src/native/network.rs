use serde_json::{json, Value};
use std::collections::HashMap;

use super::cdp::client::CdpClient;

pub async fn set_extra_headers(
    client: &CdpClient,
    session_id: &str,
    headers: &HashMap<String, String>,
) -> Result<(), String> {
    let headers_value: Value = headers
        .iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect::<serde_json::Map<String, Value>>()
        .into();

    client
        .send_command(
            "Network.setExtraHTTPHeaders",
            Some(json!({ "headers": headers_value })),
            Some(session_id),
        )
        .await?;

    Ok(())
}

pub async fn set_offline(
    client: &CdpClient,
    session_id: &str,
    offline: bool,
) -> Result<(), String> {
    client
        .send_command(
            "Network.emulateNetworkConditions",
            Some(json!({
                "offline": offline,
                "latency": 0,
                "downloadThroughput": -1,
                "uploadThroughput": -1,
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

pub async fn set_content(client: &CdpClient, session_id: &str, html: &str) -> Result<(), String> {
    // Get current frame ID
    let tree_result = client
        .send_command_no_params("Page.getFrameTree", Some(session_id))
        .await?;

    let frame_id = tree_result
        .get("frameTree")
        .and_then(|t| t.get("frame"))
        .and_then(|f| f.get("id"))
        .and_then(|id| id.as_str())
        .ok_or("Could not determine frame ID")?;

    client
        .send_command(
            "Page.setDocumentContent",
            Some(json!({
                "frameId": frame_id,
                "html": html,
            })),
            Some(session_id),
        )
        .await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Domain filter
// ---------------------------------------------------------------------------

/// Error shared by every entry point that accepts an allowlist.
pub const EMPTY_ALLOWED_DOMAINS_ERROR: &str =
    "Allowed domains list is empty. Pass at least one domain pattern, or unset \
AGENT_BROWSER_ALLOWED_DOMAINS or remove allowedDomains from the config to run without containment.";

/// Checks the allowlist the daemon inherited from its environment. A variable
/// that is set but lists no domain stops the daemon from starting.
pub fn check_allowed_domains_env() -> Result<(), String> {
    match std::env::var("AGENT_BROWSER_ALLOWED_DOMAINS") {
        Ok(raw) => ensure_allowed_domains_not_empty(&[raw])
            .map_err(|e| format!("AGENT_BROWSER_ALLOWED_DOMAINS: {e}")),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err("AGENT_BROWSER_ALLOWED_DOMAINS: the value is not valid UTF-8".to_string())
        }
        Err(std::env::VarError::NotPresent) => Ok(()),
    }
}

/// Rejects an allowlist that was supplied but holds no domain. An absent
/// allowlist is not passed here; it means no containment was requested.
pub fn ensure_allowed_domains_not_empty<S: AsRef<str>>(domains: &[S]) -> Result<(), String> {
    // Entries may themselves hold comma-separated lists, as DomainFilter reads them.
    if domains
        .iter()
        .flat_map(|d| d.as_ref().split(','))
        .any(|d| !d.trim().is_empty())
    {
        Ok(())
    } else {
        Err(EMPTY_ALLOWED_DOMAINS_ERROR.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct DomainFilter {
    pub allowed_domains: Vec<String>,
}

impl DomainFilter {
    pub fn new(domains: &str) -> Self {
        let allowed = parse_domain_list(domains);
        Self {
            allowed_domains: allowed,
        }
    }

    /// A filter exists only when containment was requested, so an empty list
    /// allows nothing.
    pub fn is_allowed(&self, hostname: &str) -> bool {
        let hostname = hostname.to_lowercase();
        for pattern in &self.allowed_domains {
            if let Some(suffix) = pattern.strip_prefix("*.") {
                if hostname == suffix || hostname.ends_with(&format!(".{}", suffix)) {
                    return true;
                }
            } else if hostname == *pattern {
                return true;
            }
        }
        false
    }

    /// Fails unless `url`'s host is in the list. `about:blank` has no host, so it always passes.
    pub fn check_url(&self, url: &str) -> Result<(), String> {
        if url == "about:blank" {
            return Ok(());
        }
        if self.allowed_domains.is_empty() {
            return Err(EMPTY_ALLOWED_DOMAINS_ERROR.to_string());
        }
        let parsed = url::Url::parse(url).map_err(|_| format!("Invalid URL: {}", url))?;
        let hostname = parsed
            .host_str()
            .ok_or_else(|| format!("No hostname in URL: {}", url))?;
        if self.is_allowed(hostname) {
            Ok(())
        } else {
            Err(format!(
                "Domain '{}' is not in the allowed domains list",
                hostname
            ))
        }
    }
}

/// Rejects a navigation to a URL outside the allowed domains; a missing filter allows everything.
pub(crate) fn check_navigation_allowed(
    filter: Option<&DomainFilter>,
    url: &str,
) -> Result<(), String> {
    match filter {
        Some(filter) => filter.check_url(url),
        None => Ok(()),
    }
}

fn parse_domain_list(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

pub async fn sanitize_existing_pages(
    client: &CdpClient,
    pages: &[super::browser::PageInfo],
    filter: &DomainFilter,
) {
    for page in pages {
        if page.url.is_empty() || page.url == "about:blank" {
            continue;
        }
        if let Ok(parsed) = url::Url::parse(&page.url) {
            if let Some(hostname) = parsed.host_str() {
                if !filter.is_allowed(hostname) {
                    let _ = client
                        .send_command(
                            "Page.navigate",
                            Some(json!({ "url": "about:blank" })),
                            Some(&page.session_id),
                        )
                        .await;
                }
            }
        }
    }
}

pub async fn install_domain_filter_script(
    client: &CdpClient,
    session_id: &str,
    allowed_domains: &[String],
    paused_before_document: bool,
) -> Result<(), String> {
    let script = domain_filter_script(allowed_domains);

    client
        .send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": &script })),
            Some(session_id),
        )
        .await?;

    install_domain_filter_runtime_script(
        client,
        session_id,
        allowed_domains,
        paused_before_document,
    )
    .await?;

    Ok(())
}

async fn install_domain_filter_runtime_script(
    client: &CdpClient,
    session_id: &str,
    allowed_domains: &[String],
    paused_before_document: bool,
) -> Result<(), String> {
    if paused_before_document {
        return Ok(());
    }
    let script = domain_filter_script(allowed_domains);
    let evaluation = client
        .send_command(
            "Runtime.evaluate",
            Some(json!({ "expression": &script })),
            Some(session_id),
        )
        .await?;
    if let Some(details) = evaluation.get("exceptionDetails") {
        let message = details
            .get("exception")
            .and_then(|exception| exception.get("description"))
            .and_then(Value::as_str)
            .or_else(|| details.get("text").and_then(Value::as_str))
            .unwrap_or("unknown JavaScript error");
        return Err(format!(
            "Failed to apply domain filter to the current execution context: {}",
            message
        ));
    }

    Ok(())
}

fn domain_filter_script(allowed_domains: &[String]) -> String {
    let domains_json = serde_json::to_string(allowed_domains).unwrap_or("[]".to_string());
    format!(
        r#"(() => {{
            const _allowed = {};
            function _agentBrowserInstallDomainFilter(_allowed, _baseOverride) {{
            const _global = globalThis;
            function _securityError(message) {{
                if (typeof DOMException === 'function') {{
                    return new DOMException(message, 'SecurityError');
                }}
                const error = new Error(message);
                error.name = 'SecurityError';
                return error;
            }}
            function _isDomainAllowed(hostname) {{
                hostname = hostname.toLowerCase();
                for (const p of _allowed) {{
                    if (p.startsWith('*.')) {{
                        const suffix = p.slice(2);
                        if (hostname === suffix || hostname.endsWith('.' + suffix)) return true;
                    }} else if (hostname === p) return true;
                }}
                return false;
            }}
            const _baseHref = _baseOverride || (_global.location && _global.location.href ? _global.location.href : 'about:blank');
            function _checkedUrl(url, apiName) {{
                const u = new URL(url, _baseHref);
                if (['http:', 'https:', 'ws:', 'wss:'].includes(u.protocol) && !_isDomainAllowed(u.hostname)) {{
                    throw _securityError(apiName + ' blocked: ' + u.hostname);
                }}
                return u.href;
            }}
            function _assertAllowedUrl(url, apiName) {{
                _checkedUrl(url, apiName);
            }}
            function _checkedWebSocketUrl(url, apiName) {{
                const u = new URL(url, _baseHref);
                if (u.protocol === 'http:') u.protocol = 'ws:';
                if (u.protocol === 'https:') u.protocol = 'wss:';
                if (['ws:', 'wss:'].includes(u.protocol) && !_isDomainAllowed(u.hostname)) {{
                    if (typeof _global.__agentBrowserPoppyNet === 'function') {{
                        try {{
                            _global.__agentBrowserPoppyNet(JSON.stringify({{ api: apiName, url: u.href }}));
                        }} catch (_) {{}}
                    }}
                    throw _securityError(apiName + ' blocked: ' + u.hostname);
                }}
                return u.href;
            }}
            function _requestUrl(input) {{
                if (typeof input === 'string') return input;
                if (typeof URL === 'function' && input instanceof URL) return input.href;
                if (input && typeof input.url === 'string') return input.url;
                return String(input);
            }}
            const _workerUrlCache = typeof Map === 'function' ? new Map() : null;
            function _checkedWorkerScriptUrl(scriptURL, apiName) {{
                let absolute;
                try {{
                    const u = new URL(scriptURL, _baseHref);
                    absolute = u.href;
                    if (u.protocol === 'blob:') {{
                        try {{
                            const inner = new URL(u.pathname);
                            if (inner.hostname && !_isDomainAllowed(inner.hostname)) {{
                                throw _securityError(apiName + ' blocked: ' + inner.hostname);
                            }}
                        }} catch(e) {{ if (e && e.name === 'SecurityError') throw e; }}
                    }} else if (u.hostname && !_isDomainAllowed(u.hostname)) {{
                        throw _securityError(apiName + ' blocked: ' + u.hostname);
                    }}
                }} catch(e) {{
                    if (e && e.name === 'SecurityError') throw e;
                    throw e;
                }}
                return absolute;
            }}
            function _workerScriptUrl(scriptURL, options, apiName) {{
                if (!_global.Blob || !_global.URL || typeof _global.URL.createObjectURL !== 'function') {{
                    throw _securityError(apiName + ' blocked: worker bootstrap APIs are unavailable');
                }}
                const absolute = _checkedWorkerScriptUrl(scriptURL, apiName);
                const isModule = options && typeof options === 'object' && options.type === 'module';
                const cacheKey = apiName + '|' + (isModule ? 'module' : 'classic') + '|' + absolute;
                if (_workerUrlCache && _workerUrlCache.has(cacheKey)) return _workerUrlCache.get(cacheKey);
                const installSource = '(' + _agentBrowserInstallDomainFilter.toString() + ')(' + JSON.stringify(_allowed) + ', ' + JSON.stringify(absolute) + ');\n';
                const source = installSource + (isModule
                    ? 'await import(' + JSON.stringify(absolute) + ');\n'
                    : 'importScripts(' + JSON.stringify(absolute) + ');\n');
                const wrapped = _global.URL.createObjectURL(new Blob([source], {{ type: 'application/javascript' }}));
                if (_workerUrlCache) _workerUrlCache.set(cacheKey, wrapped);
                return wrapped;
            }}
            function _constructWorker(OrigCtor, scriptURL, options, apiName) {{
                const checkedUrl = _checkedWorkerScriptUrl(scriptURL, apiName);
                try {{
                    const bootstrapUrl = _workerScriptUrl(checkedUrl, options, apiName);
                    const worker = new OrigCtor(bootstrapUrl, options);
                    return worker;
                }} catch (error) {{
                    // Fail closed if the guarded bootstrap cannot be created.
                    throw error;
                }}
            }}
            const OrigWorker = _global.Worker;
            if (typeof OrigWorker === 'function') {{
                _global.Worker = function(scriptURL, options) {{
                    return _constructWorker(OrigWorker, scriptURL, options, 'Worker');
                }};
                _global.Worker.prototype = OrigWorker.prototype;
            }}
            const OrigSharedWorker = _global.SharedWorker;
            if (typeof OrigSharedWorker === 'function') {{
                _global.SharedWorker = function(scriptURL, options) {{
                    return _constructWorker(OrigSharedWorker, scriptURL, options, 'SharedWorker');
                }};
                _global.SharedWorker.prototype = OrigSharedWorker.prototype;
            }}
            const OrigImportScripts = _global.importScripts;
            if (typeof OrigImportScripts === 'function') {{
                _global.importScripts = function() {{
                    const urls = Array.prototype.slice.call(arguments).map((url) => {{
                        try {{
                            return _checkedUrl(url, 'importScripts');
                        }} catch(e) {{
                            if (e && e.name === 'SecurityError') throw e;
                            return url;
                        }}
                    }});
                    return OrigImportScripts.apply(this, urls);
                }};
            }}
            const OrigFetch = _global.fetch;
            if (typeof OrigFetch === 'function') {{
                _global.fetch = function(input, init) {{
                    try {{
                        if (typeof input === 'string') {{
                            return OrigFetch.call(this, _checkedUrl(input, 'Fetch'), init);
                        }}
                        _assertAllowedUrl(_requestUrl(input), 'Fetch');
                    }} catch(e) {{
                        if (e && e.name === 'SecurityError') return Promise.reject(e);
                    }}
                    return OrigFetch.apply(this, arguments);
                }};
            }}
            const OrigXHR = _global.XMLHttpRequest;
            if (typeof OrigXHR === 'function' && OrigXHR.prototype && OrigXHR.prototype.open) {{
                const origOpen = OrigXHR.prototype.open;
                OrigXHR.prototype.open = function(method, url) {{
                    let checkedUrl = url;
                    try {{
                        checkedUrl = _checkedUrl(url, 'XMLHttpRequest');
                    }} catch(e) {{
                        if (e && e.name === 'SecurityError') throw e;
                    }}
                    const args = Array.prototype.slice.call(arguments);
                    args[1] = checkedUrl;
                    return origOpen.apply(this, args);
                }};
            }}
            const OrigWS = _global.WebSocket;
            if (typeof OrigWS === 'function') {{
                _global.WebSocket = function(url, protocols) {{
                    let checkedUrl = url;
                    try {{
                        checkedUrl = _checkedWebSocketUrl(url, 'WebSocket');
                    }} catch(e) {{ if (e && e.name === 'SecurityError') throw e; }}
                    return new OrigWS(checkedUrl, protocols);
                }};
                _global.WebSocket.prototype = OrigWS.prototype;
            }}
            const OrigES = _global.EventSource;
            if (OrigES) {{
                _global.EventSource = function(url, opts) {{
                    let checkedUrl = url;
                    try {{
                        checkedUrl = _checkedUrl(url, 'EventSource');
                    }} catch(e) {{ if (e && e.name === 'SecurityError') throw e; }}
                    return new OrigES(checkedUrl, opts);
                }};
                _global.EventSource.prototype = OrigES.prototype;
            }}
            const origBeacon = _global.navigator && _global.navigator.sendBeacon;
            if (origBeacon) {{
                _global.navigator.sendBeacon = function(url, data) {{
                    let checkedUrl = url;
                    try {{
                        checkedUrl = _checkedUrl(url, 'Beacon');
                    }} catch(e) {{ return false; }}
                    return origBeacon.call(_global.navigator, checkedUrl, data);
                }};
            }}
            function _blockPeerConnection(name) {{
                if (typeof _global[name] !== 'function') return;
                const BlockedPeerConnection = function() {{
                    throw _securityError('RTCPeerConnection blocked while domain filtering is active');
                }};
                Object.defineProperty(BlockedPeerConnection, 'prototype', {{
                    value: Object.freeze(Object.create(null)),
                    writable: false
                }});
                try {{
                    Object.defineProperty(_global, name, {{
                        value: BlockedPeerConnection,
                        writable: false,
                        configurable: false
                    }});
                }} catch (_) {{
                    _global[name] = BlockedPeerConnection;
                }}
            }}
            _blockPeerConnection('RTCPeerConnection');
            _blockPeerConnection('webkitRTCPeerConnection');
            }}
            _agentBrowserInstallDomainFilter(_allowed);
        }})()"#,
        domains_json,
    )
}

/// Enable Fetch-based network interception for domain filtering.
/// This intercepts all requests and checks them against the allowed domains list.
/// The actual handling of `Fetch.requestPaused` events happens in
/// `resolve_fetch_paused` in the actions module.
pub async fn install_domain_filter_fetch(
    client: &CdpClient,
    session_id: &str,
    handle_auth_requests: bool,
) -> Result<(), String> {
    let mut params = json!({
        "patterns": [{ "urlPattern": "*" }]
    });
    if handle_auth_requests {
        params["handleAuthRequests"] = json!(true);
    }
    client
        .send_command("Fetch.enable", Some(params), Some(session_id))
        .await?;
    Ok(())
}

/// Install both layers of domain filtering on a session:
/// 1. Fetch-based network interception
/// 2. JS patching for APIs outside Fetch interception, including workers,
///    WebSocket, EventSource, sendBeacon, and RTCPeerConnection.
pub async fn install_domain_filter(
    client: &CdpClient,
    session_id: &str,
    allowed_domains: &[String],
    handle_auth_requests: bool,
    paused_before_document: bool,
) -> Result<(), String> {
    install_domain_filter_fetch(client, session_id, handle_auth_requests).await?;
    install_domain_filter_script(client, session_id, allowed_domains, paused_before_document)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Console arg formatting (CDP RemoteObject → human-readable string)
// ---------------------------------------------------------------------------

/// Format a single CDP RemoteObject arg into a human-readable string.
/// Priority: value → preview → description.
pub fn format_console_arg(arg: &Value) -> Option<String> {
    let obj_type = arg.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let subtype = arg.get("subtype").and_then(|v| v.as_str());

    if obj_type == "undefined" {
        return Some("undefined".to_string());
    }

    if subtype == Some("null") {
        return Some("null".to_string());
    }

    // Primitive value
    if let Some(v) = arg.get("value") {
        return Some(match v {
            Value::String(s) => s.clone(),
            Value::Null => "null".to_string(),
            other => other.to_string(),
        });
    }

    // Skip preview for Map/Set — their description ("Map(1)", "Set(3)") is more useful
    // than their preview properties (which only show "size")
    if let Some(preview) = arg.get("preview") {
        let preview_subtype = preview.get("subtype").and_then(|v| v.as_str());
        if matches!(preview_subtype, Some("map" | "set" | "weakmap" | "weakset")) {
            return arg
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
        }
        let is_array = subtype == Some("array") || preview_subtype == Some("array");
        if let Some(props) = preview.get("properties").and_then(|v| v.as_array()) {
            let overflow = preview
                .get("overflow")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let formatted_props: Vec<String> = props
                .iter()
                .filter_map(|p| {
                    let value_str = p.get("value").and_then(|v| v.as_str())?;
                    let prop_type = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    let formatted_value = if prop_type == "string" {
                        format!("\"{}\"", value_str)
                    } else {
                        value_str.to_string()
                    };
                    if is_array {
                        Some(formatted_value)
                    } else {
                        let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        Some(format!("{}: {}", name, formatted_value))
                    }
                })
                .collect();

            let inner = if overflow {
                format!("{}, ...", formatted_props.join(", "))
            } else {
                formatted_props.join(", ")
            };

            return if is_array {
                Some(format!("[{}]", inner))
            } else {
                Some(format!("{{{}}}", inner))
            };
        }
    }

    // Fallback to description
    arg.get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Format an array of CDP RemoteObject args into a single space-separated string.
pub fn format_console_args(args: &[Value]) -> String {
    args.iter()
        .filter_map(format_console_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Console and error tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
    pub args: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ErrorEntry {
    pub text: String,
    pub url: Option<String>,
    pub line: Option<i64>,
    pub column: Option<i64>,
}

/// Most console messages and most page errors the tracker keeps, each.
const MAX_TRACKED_ENTRIES: usize = 1000;
/// Most text one console message or page error keeps; the rest is cut off.
const MAX_ENTRY_TEXT_BYTES: usize = 64 * 1024;
/// Most memory the arguments of one console message may hold; past it they are dropped.
const MAX_ENTRY_ARGS_BYTES: usize = 64 * 1024;
const ARGS_OMITTED: &str = "[arguments omitted: too large]";
/// Longest source URL a page error keeps.
const MAX_ENTRY_URL_BYTES: usize = 8 * 1024;
/// Most bytes the console messages, and separately the page errors, may hold together.
const MAX_RETAINED_BYTES: usize = 8 * 1024 * 1024;

/// Most tracked network requests kept in memory at once.
pub(crate) const MAX_TRACKED_REQUESTS: usize = 1000;
/// Most aggregate bytes tracked network requests may hold together.
pub(crate) const MAX_TRACKED_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// Longest URL or redirect target kept per recorded request or HAR entry.
pub(crate) const MAX_REQUEST_URL_BYTES: usize = 8 * 1024;
/// Most aggregate bytes one request or response header map may keep.
pub(crate) const MAX_REQUEST_HEADERS_BYTES: usize = 16 * 1024;
/// Most request-body bytes kept on one tracked request.
pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;
/// Longest short metadata label (method, MIME type, status text, resource type, request ID).
pub(crate) const MAX_REQUEST_LABEL_BYTES: usize = 256;
/// Most entries retained in one HAR recording session.
pub(crate) const HAR_MAX_ENTRIES: usize = 5000;

/// A copy of `text` of at most `limit` bytes, cut on a character boundary and marked as cut.
pub(crate) fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated {} bytes]", &text[..end], text.len() - end)
}

/// Clips a string to at most `limit` UTF-8 bytes without adding a suffix.
fn clip_raw(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// Copies header `(name, value)` pairs up to `limit` aggregate bytes, stopping before
/// allocating past the budget.
pub(crate) fn clip_header_pairs<'a>(
    pairs: impl IntoIterator<Item = (&'a str, &'a str)>,
    limit: usize,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut left = limit;
    for (name, value) in pairs {
        if left == 0 {
            break;
        }
        let overhead = std::mem::size_of::<(String, String)>();
        if left <= overhead {
            break;
        }
        left -= overhead;
        let clipped_name = clip_raw(name, left.min(MAX_REQUEST_LABEL_BYTES));
        left = left.saturating_sub(clipped_name.len());
        let clipped_value = if value.len() <= left {
            value.to_string()
        } else {
            let v = clip_raw(value, left);
            left = 0;
            v
        };
        left = left.saturating_sub(clipped_value.len());
        out.push((clipped_name, clipped_value));
    }
    out
}

/// Extracts a bounded `(name, value)` list from a BiDi headers array.
pub(crate) fn clip_bidi_headers(headers: &Value) -> Vec<(String, String)> {
    let pairs = headers.as_array().into_iter().flatten().filter_map(|h| {
        Some((
            h["name"].as_str()?,
            h["value"]["value"].as_str().unwrap_or(""),
        ))
    });
    clip_header_pairs(pairs, MAX_REQUEST_HEADERS_BYTES)
}

/// Extracts a bounded JSON object map from a BiDi headers array.
pub(crate) fn clip_bidi_header_object(headers: &Value) -> Value {
    pairs_to_header_object(clip_bidi_headers(headers))
}

/// Extracts a bounded `(name, value)` list from a CDP headers JSON object.
pub(crate) fn clip_cdp_headers(headers: Option<&Value>) -> Vec<(String, String)> {
    let Some(map) = headers.and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut left = MAX_REQUEST_HEADERS_BYTES;
    for (name, raw_val) in map {
        let overhead = std::mem::size_of::<(String, String)>();
        if left <= overhead {
            break;
        }
        left -= overhead;
        let clipped_name = clip_raw(name, left.min(MAX_REQUEST_LABEL_BYTES));
        left = left.saturating_sub(clipped_name.len());
        let owned_val;
        let val_str = match raw_val.as_str() {
            Some(s) => s,
            None => {
                owned_val = raw_val.to_string();
                owned_val.as_str()
            }
        };
        let clipped_value = if val_str.len() <= left {
            val_str.to_string()
        } else {
            let v = clip_raw(val_str, left);
            left = 0;
            v
        };
        left = left.saturating_sub(clipped_value.len());
        out.push((clipped_name, clipped_value));
    }
    out
}

/// Extracts a bounded JSON object map from a CDP headers JSON object.
pub(crate) fn clip_cdp_header_object(headers: Option<&Value>) -> Value {
    pairs_to_header_object(clip_cdp_headers(headers))
}

fn pairs_to_header_object(pairs: Vec<(String, String)>) -> Value {
    Value::Object(
        pairs
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
    )
}

/// Decodes a BiDi `network.getData` payload within `max_bytes` before allocating the full
/// body. When `truncate` is false, payloads exceeding `max_bytes` return `None`; when true,
/// only the bounded prefix is decoded and marked as truncated.
pub(crate) fn decode_bidi_body_bounded(
    raw: &str,
    is_base64: bool,
    max_bytes: usize,
    truncate: bool,
) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    if !is_base64 {
        if !truncate && raw.len() > max_bytes {
            return None;
        }
        return Some(clip(raw, max_bytes));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let estimated_decoded = (trimmed.len() / 4).saturating_mul(3);
    if !truncate && estimated_decoded > max_bytes {
        return None;
    }
    use base64::Engine;
    let engine = &base64::engine::general_purpose::STANDARD;
    let max_encoded_prefix = ((max_bytes / 3) + 1).saturating_mul(4);
    let was_sliced = truncate && trimmed.len() > max_encoded_prefix;
    let slice = if was_sliced {
        &trimmed[..max_encoded_prefix]
    } else {
        trimmed
    };
    let mut bytes = engine.decode(slice).ok()?;
    if bytes.is_empty() {
        return None;
    }
    if !truncate && bytes.len() > max_bytes {
        return None;
    }
    if was_sliced {
        let mut valid_len = bytes.len().min(max_bytes);
        while valid_len > 0 && std::str::from_utf8(&bytes[..valid_len]).is_err() {
            if bytes.len().min(max_bytes) - valid_len >= 3 {
                return None;
            }
            valid_len -= 1;
        }
        bytes.truncate(valid_len);
        let prefix = String::from_utf8(bytes).ok()?;
        let omitted = estimated_decoded.saturating_sub(prefix.len());
        return Some(format!("{prefix}...[truncated {omitted} bytes]"));
    }
    let text = String::from_utf8(bytes).ok()?;
    if text.is_empty() {
        return None;
    }
    Some(if truncate {
        clip(&text, max_bytes)
    } else {
        text
    })
}

/// One network request recorded for `network requests` and `network request`.
#[derive(Clone, serde::Serialize)]
pub struct TrackedRequest {
    pub url: String,
    pub method: String,
    pub headers: Value,
    pub timestamp: u64,
    #[serde(rename = "resourceType")]
    pub resource_type: String,
    #[serde(rename = "requestId")]
    pub request_id: String,
    #[serde(rename = "postData", skip_serializing_if = "Option::is_none")]
    pub post_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i64>,
    #[serde(rename = "responseHeaders", skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Value>,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

fn header_value_bytes(value: &Value) -> usize {
    match value {
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| {
                std::mem::size_of::<String>()
                    + std::mem::size_of::<Value>()
                    + k.len()
                    + v.as_str().map_or(0, str::len)
            })
            .sum(),
        _ => 0,
    }
}

fn tracked_request_weight(entry: &TrackedRequest) -> usize {
    std::mem::size_of::<TrackedRequest>()
        + entry.url.len()
        + entry.method.len()
        + entry.resource_type.len()
        + entry.request_id.len()
        + entry.post_data.as_ref().map_or(0, String::len)
        + entry.mime_type.as_ref().map_or(0, String::len)
        + header_value_bytes(&entry.headers)
        + entry
            .response_headers
            .as_ref()
            .map_or(0, header_value_bytes)
}

/// Network request history kept in arrival order under fixed entry-count and aggregate byte
/// caps. Every insert and response/body update recalculates retained bytes and evicts the
/// oldest entries first.
pub struct RequestTracker {
    entries: Vec<TrackedRequest>,
    weights: Vec<usize>,
    total: usize,
}

impl Default for RequestTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestTracker {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            weights: Vec::new(),
            total: 0,
        }
    }

    pub fn entries(&self) -> &[TrackedRequest] {
        &self.entries
    }

    pub fn iter(&self) -> std::slice::Iter<'_, TrackedRequest> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[cfg(test)]
    pub fn retained_bytes(&self) -> usize {
        self.total
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.weights.clear();
        self.total = 0;
    }

    /// Records a new network request after clipping URL, labels, headers, and optional POST data.
    #[allow(clippy::too_many_arguments)]
    pub fn record_request(
        &mut self,
        request_id: &str,
        url: &str,
        method: &str,
        headers: Value,
        timestamp: u64,
        resource_type: &str,
        post_data: Option<&str>,
    ) {
        let entry = TrackedRequest {
            url: clip(url, MAX_REQUEST_URL_BYTES),
            method: clip_raw(method, MAX_REQUEST_LABEL_BYTES),
            headers: clip_cdp_header_object(Some(&headers)),
            timestamp,
            resource_type: clip_raw(resource_type, MAX_REQUEST_LABEL_BYTES),
            request_id: clip_raw(request_id, MAX_REQUEST_LABEL_BYTES),
            post_data: post_data
                .filter(|body| !body.is_empty())
                .map(|body| clip(body, MAX_REQUEST_BODY_BYTES)),
            status: None,
            response_headers: None,
            mime_type: None,
        };
        let weight = tracked_request_weight(&entry);
        self.entries.push(entry);
        self.weights.push(weight);
        self.total += weight;
        self.evict(Some(self.entries.len() - 1));
    }

    /// Attaches response metadata and bounded response headers to the latest entry for `request_id`.
    pub fn attach_response(
        &mut self,
        request_id: &str,
        status: Option<i64>,
        mime_type: Option<&str>,
        response_headers: Option<Value>,
    ) {
        let id = clip_raw(request_id, MAX_REQUEST_LABEL_BYTES);
        let Some(idx) = self.entries.iter().rposition(|e| e.request_id == id) else {
            return;
        };
        let entry = &mut self.entries[idx];
        entry.status = status;
        entry.mime_type = mime_type.map(|m| clip_raw(m, MAX_REQUEST_LABEL_BYTES));
        entry.response_headers = response_headers.map(|h| clip_cdp_header_object(Some(&h)));
        self.reweigh(idx);
    }

    /// Returns whether the latest entry for `request_id` still has no recorded request body.
    pub fn needs_post_data(&self, request_id: &str) -> bool {
        let id = clip_raw(request_id, MAX_REQUEST_LABEL_BYTES);
        self.entries
            .iter()
            .rev()
            .find(|e| e.request_id == id)
            .is_some_and(|e| e.post_data.is_none())
    }

    /// Attaches a clipped request body to the latest entry for `request_id` and enforces memory caps.
    pub fn attach_post_data(&mut self, request_id: &str, post_data: &str) {
        if post_data.is_empty() {
            return;
        }
        let id = clip_raw(request_id, MAX_REQUEST_LABEL_BYTES);
        let Some(idx) = self.entries.iter().rposition(|e| e.request_id == id) else {
            return;
        };
        self.entries[idx].post_data = Some(clip(post_data, MAX_REQUEST_BODY_BYTES));
        self.reweigh(idx);
    }

    fn reweigh(&mut self, idx: usize) {
        let next_weight = tracked_request_weight(&self.entries[idx]);
        let prev_weight = self.weights[idx];
        self.weights[idx] = next_weight;
        self.total = self.total.saturating_sub(prev_weight) + next_weight;
        self.evict(Some(idx));
    }

    fn evict(&mut self, mut keep_idx: Option<usize>) {
        while self.entries.len() > MAX_TRACKED_REQUESTS
            || (self.total > MAX_TRACKED_REQUEST_BYTES && self.entries.len() > 1)
        {
            if keep_idx == Some(0) {
                break;
            }
            self.entries.remove(0);
            self.total = self.total.saturating_sub(self.weights.remove(0));
            keep_idx = keep_idx.map(|i| i.saturating_sub(1));
        }
    }
}

/// Takes `bytes` from `left`, or `None` when that much is no longer available.
fn spend(left: &mut usize, bytes: usize) -> Option<()> {
    *left = left.checked_sub(bytes)?;
    Some(())
}

fn weigh_value(value: &Value, left: &mut usize) -> Option<()> {
    spend(left, std::mem::size_of::<Value>())?;
    match value {
        Value::String(text) => spend(left, text.len()),
        Value::Array(items) => items.iter().try_for_each(|item| weigh_value(item, left)),
        Value::Object(fields) => fields.iter().try_for_each(|(key, item)| {
            spend(left, std::mem::size_of::<String>() + key.len())?;
            weigh_value(item, left)
        }),
        _ => Some(()),
    }
}

/// The memory `args` hold, counting every value and string in them, or `None` as soon as that is
/// known to pass `limit`, so a huge value is never walked to the end.
fn args_bytes_within(args: &[Value], limit: usize) -> Option<usize> {
    let mut left = limit;
    args.iter()
        .try_for_each(|arg| weigh_value(arg, &mut left))?;
    Some(limit - left)
}

/// Entries kept in arrival order, oldest dropped first once either the entry count or the total
/// weight of what is kept would pass its limit. It is the only place that decides what stays.
struct BoundedLog<T> {
    entries: Vec<T>,
    weights: Vec<usize>,
    total: usize,
}

impl<T> BoundedLog<T> {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            weights: Vec::new(),
            total: 0,
        }
    }

    fn push(&mut self, entry: T, weight: usize) {
        while !self.entries.is_empty()
            && (self.entries.len() >= MAX_TRACKED_ENTRIES
                || self.total + weight > MAX_RETAINED_BYTES)
        {
            self.entries.remove(0);
            self.total -= self.weights.remove(0);
        }
        self.entries.push(entry);
        self.weights.push(weight);
        self.total += weight;
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.weights.clear();
        self.total = 0;
    }
}

/// Console messages and page errors the page produced, kept within fixed memory bounds: every
/// entry is cut to a size limit before it is copied in, and the oldest go once the entries
/// together would hold more than their share.
pub struct EventTracker {
    console: BoundedLog<ConsoleEntry>,
    errors: BoundedLog<ErrorEntry>,
}

impl EventTracker {
    pub fn new() -> Self {
        Self {
            console: BoundedLog::new(),
            errors: BoundedLog::new(),
        }
    }

    pub fn console_entries(&self) -> &[ConsoleEntry] {
        &self.console.entries
    }

    pub fn error_entries(&self) -> &[ErrorEntry] {
        &self.errors.entries
    }

    pub fn add_console(&mut self, level: &str, text: &str, args: &[Value]) {
        let text = clip(text, MAX_ENTRY_TEXT_BYTES);
        let (args, args_bytes) = match args_bytes_within(args, MAX_ENTRY_ARGS_BYTES) {
            Some(bytes) => (args.to_vec(), bytes),
            None => (
                vec![json!({ "type": "string", "value": ARGS_OMITTED })],
                std::mem::size_of::<Value>() + ARGS_OMITTED.len(),
            ),
        };
        let weight = std::mem::size_of::<ConsoleEntry>() + level.len() + text.len() + args_bytes;
        self.console.push(
            ConsoleEntry {
                level: level.to_string(),
                text,
                args,
            },
            weight,
        );
    }

    pub fn add_error(
        &mut self,
        text: &str,
        url: Option<&str>,
        line: Option<i64>,
        col: Option<i64>,
    ) {
        let text = clip(text, MAX_ENTRY_TEXT_BYTES);
        let url = url.map(|url| clip(url, MAX_ENTRY_URL_BYTES));
        let weight =
            std::mem::size_of::<ErrorEntry>() + text.len() + url.as_ref().map_or(0, String::len);
        self.errors.push(
            ErrorEntry {
                text,
                url,
                line,
                column: col,
            },
            weight,
        );
    }

    pub fn clear_console(&mut self) {
        self.console.clear();
    }

    /// Removes all captured page errors.
    pub fn clear_errors(&mut self) {
        self.errors.clear();
    }

    pub fn get_console_json(&self) -> Value {
        let messages: Vec<Value> = self
            .console_entries()
            .iter()
            .map(|e| {
                let mut msg = json!({ "type": e.level, "text": e.text });
                if !e.args.is_empty() {
                    msg.as_object_mut()
                        .unwrap()
                        .insert("args".to_string(), Value::Array(e.args.clone()));
                }
                msg
            })
            .collect();
        json!({ "messages": messages })
    }

    pub fn get_errors_json(&self) -> Value {
        let entries: Vec<Value> = self
            .error_entries()
            .iter()
            .map(|e| {
                json!({
                    "text": e.text,
                    "url": e.url,
                    "line": e.line,
                    "column": e.column,
                })
            })
            .collect();
        json!({ "errors": entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn install_script_with_runtime_error(
        message: &'static str,
        paused_before_document: bool,
    ) -> Result<(), String> {
        install_script_recording_methods(message, paused_before_document)
            .await
            .0
    }

    async fn install_script_recording_methods(
        message: &'static str,
        paused_before_document: bool,
    ) -> (Result<(), String>, Vec<String>) {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let methods = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = methods.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Message::Text(text))) = ws.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                let method = request["method"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push(method.clone());
                let mut response = json!({ "id": request["id"] });
                if method == "Runtime.evaluate" {
                    response["error"] = json!({ "code": -32000, "message": message });
                } else {
                    response["result"] = json!({ "identifier": "1" });
                }
                ws.send(Message::Text(response.to_string())).await.unwrap();
            }
        });

        let client = CdpClient::connect(&url).await.unwrap();
        let result = install_domain_filter_script(
            &client,
            "session",
            &["example.com".to_string()],
            paused_before_document,
        )
        .await;
        drop(client);
        server.abort();
        let methods = methods.lock().unwrap().clone();
        (result, methods)
    }

    #[tokio::test]
    async fn domain_filter_script_does_not_evaluate_in_a_paused_target() {
        let (result, methods) =
            install_script_recording_methods("Execution context was destroyed.", true).await;
        assert_eq!(result, Ok(()));
        assert_eq!(methods, ["Page.addScriptToEvaluateOnNewDocument"]);
    }

    #[tokio::test]
    async fn domain_filter_script_fails_closed_without_a_context_on_live_targets() {
        let (result, methods) =
            install_script_recording_methods("Cannot find default execution context", false).await;
        let error = result.unwrap_err();
        assert!(
            error.contains("Cannot find default execution context"),
            "{}",
            error
        );
        assert_eq!(
            methods,
            vec![
                "Page.addScriptToEvaluateOnNewDocument".to_string(),
                "Runtime.evaluate".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn domain_filter_script_fails_closed_on_other_runtime_errors() {
        let error = install_script_with_runtime_error("Execution context was destroyed.", false)
            .await
            .unwrap_err();
        assert!(
            error.contains("Execution context was destroyed"),
            "{}",
            error
        );
    }

    #[test]
    fn test_domain_filter_exact() {
        let filter = DomainFilter::new("example.com");
        assert!(filter.is_allowed("example.com"));
        assert!(!filter.is_allowed("other.com"));
    }

    #[test]
    fn test_domain_filter_wildcard() {
        let filter = DomainFilter::new("*.example.com");
        assert!(filter.is_allowed("example.com"));
        assert!(filter.is_allowed("api.example.com"));
        assert!(filter.is_allowed("sub.api.example.com"));
        assert!(!filter.is_allowed("other.com"));
    }

    #[test]
    fn test_domain_filter_empty_allows_nothing() {
        for list in ["", ",", " , "] {
            let filter = DomainFilter::new(list);
            assert!(!filter.is_allowed("anything.com"), "{list:?}");
            assert_eq!(
                filter.check_url("http://anything.com/").unwrap_err(),
                EMPTY_ALLOWED_DOMAINS_ERROR
            );
        }
    }

    #[test]
    fn test_ensure_allowed_domains_not_empty() {
        let empty: Vec<String> = Vec::new();
        assert!(ensure_allowed_domains_not_empty(&empty).is_err());
        assert!(ensure_allowed_domains_not_empty(&["", "  "]).is_err());
        assert!(ensure_allowed_domains_not_empty(&[","]).is_err());
        assert!(ensure_allowed_domains_not_empty(&[" , "]).is_err());
        assert!(ensure_allowed_domains_not_empty(&["a.example,"]).is_ok());
        assert!(ensure_allowed_domains_not_empty(&["", "example.com"]).is_ok());
    }

    #[test]
    fn navigation_allows_about_blank_and_blocks_every_other_unlisted_url() {
        let filter = DomainFilter::new("example.com");
        assert!(filter.check_url("about:blank").is_ok());
        assert!(filter.check_url("https://example.com/page").is_ok());
        assert_eq!(
            filter.check_url("https://evil.example/").unwrap_err(),
            "Domain 'evil.example' is not in the allowed domains list"
        );
        assert!(filter.check_url("data:text/html,hi").is_err());
        assert!(check_navigation_allowed(None, "https://evil.example/").is_ok());
    }

    #[test]
    fn test_domain_filter_multiple() {
        let filter = DomainFilter::new("example.com, *.api.io");
        assert!(filter.is_allowed("example.com"));
        assert!(filter.is_allowed("api.io"));
        assert!(filter.is_allowed("v1.api.io"));
        assert!(!filter.is_allowed("other.com"));
    }

    #[test]
    fn test_parse_domain_list() {
        let domains = parse_domain_list("A.com, B.com , *.C.com");
        assert_eq!(domains, vec!["a.com", "b.com", "*.c.com"]);
    }

    #[test]
    fn test_domain_filter_script_blocks_peer_connection_constructors() {
        let script = domain_filter_script(&["example.com".to_string()]);
        assert!(script.contains("_blockPeerConnection('RTCPeerConnection')"));
        assert!(script.contains("_blockPeerConnection('webkitRTCPeerConnection')"));
        assert!(script.contains("RTCPeerConnection blocked while domain filtering is active"));
        assert!(script.contains("configurable: false"));
    }

    #[test]
    fn test_domain_filter_script_fails_closed_when_worker_blob_is_csp_blocked() {
        let script = domain_filter_script(&["example.com".to_string()]);
        assert!(script.contains("createObjectURL(new Blob"));
        assert!(script.contains("'await import(' + JSON.stringify(absolute)"));
        assert!(script.contains("const worker = new OrigCtor(bootstrapUrl, options)"));
        assert!(script.contains("return worker"));
        assert!(!script.contains("_wrapWorkerWithCspFallback"));
        assert!(!script.contains("return new OrigCtor(checkedUrl, options)"));
    }

    #[test]
    fn test_event_tracker() {
        let mut tracker = EventTracker::new();
        tracker.add_console("log", "hello", &[]);
        tracker.add_error("oops", Some("test.js"), Some(1), Some(5));

        assert_eq!(tracker.console_entries().len(), 1);
        assert_eq!(tracker.error_entries().len(), 1);
    }

    #[test]
    fn test_a_long_console_text_is_cut_on_a_character_boundary_and_marked() {
        let mut tracker = EventTracker::new();
        let text = "\u{20ac}".repeat(MAX_ENTRY_TEXT_BYTES);
        tracker.add_console("log", &text, &[]);

        let kept = &tracker.console_entries()[0].text;
        let (head, marker) = kept.split_once("...[truncated ").unwrap();
        assert!(head.len() <= MAX_ENTRY_TEXT_BYTES);
        assert!(text.starts_with(head));
        assert_eq!(
            marker,
            format!("{} bytes]", text.len() - head.len()),
            "the marker reports exactly what was cut"
        );
    }

    #[test]
    fn test_console_args_over_the_limit_are_replaced_by_a_note() {
        let mut tracker = EventTracker::new();
        let huge = vec![json!({ "type": "string", "value": "A".repeat(MAX_ENTRY_ARGS_BYTES) })];
        tracker.add_console("log", "short", &huge);
        let small = vec![json!({ "type": "string", "value": "ok" })];
        tracker.add_console("log", "short", &small);

        let entries = tracker.console_entries();
        assert_eq!(entries[0].text, "short");
        assert_eq!(
            entries[0].args,
            vec![json!({ "type": "string", "value": ARGS_OMITTED })]
        );
        assert_eq!(entries[1].args, small);
    }

    #[test]
    fn test_retained_console_memory_stays_bounded_and_drops_the_oldest_first() {
        let mut tracker = EventTracker::new();
        let big = "x".repeat(MAX_ENTRY_TEXT_BYTES);
        let fill = |tracker: &mut EventTracker| {
            for n in 0..400 {
                tracker.add_console("log", &format!("{n}:{big}"), &[]);
                assert!(tracker.console.total <= MAX_RETAINED_BYTES);
            }
        };
        fill(&mut tracker);

        let kept = tracker.console_entries().len();
        assert!(kept < 400, "the byte limit evicts before the count limit");
        assert!(tracker.console_entries()[kept - 1].text.starts_with("399:"));
        assert!(tracker.console_entries()[0]
            .text
            .starts_with(&format!("{}:", 400 - kept)));

        tracker.clear_console();
        assert_eq!(tracker.console.total, 0);
        fill(&mut tracker);
        assert_eq!(tracker.console_entries().len(), kept);
    }

    #[test]
    fn test_page_errors_are_bounded_in_text_url_and_total() {
        let mut tracker = EventTracker::new();
        let big = "e".repeat(MAX_ENTRY_TEXT_BYTES * 2);
        let url = format!(
            "https://example.test/{}",
            "u".repeat(MAX_ENTRY_URL_BYTES * 4)
        );
        for _ in 0..400 {
            tracker.add_error(&big, Some(&url), Some(1), Some(1));
            assert!(tracker.errors.total <= MAX_RETAINED_BYTES);
        }

        let entry = &tracker.error_entries()[0];
        assert!(entry.text.len() < MAX_ENTRY_TEXT_BYTES + 64);
        assert!(entry.url.as_ref().unwrap().len() < MAX_ENTRY_URL_BYTES + 64);
        assert!(tracker.error_entries().len() < 400);
        assert_eq!((entry.line, entry.column), (Some(1), Some(1)));
    }

    #[test]
    fn test_console_json_includes_args() {
        let mut tracker = EventTracker::new();
        let raw_args = vec![
            json!({"type": "string", "value": "hello"}),
            json!({"type": "number", "value": 42}),
        ];
        tracker.add_console("log", "hello 42", &raw_args);

        let result = tracker.get_console_json();
        let messages = result.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].get("text").unwrap(), "hello 42");
        let args = messages[0].get("args").unwrap().as_array().unwrap();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], json!({"type": "string", "value": "hello"}));
        assert_eq!(args[1], json!({"type": "number", "value": 42}));
    }

    #[test]
    fn test_console_json_empty_args_omits_field() {
        let mut tracker = EventTracker::new();
        tracker.add_console("log", "text only", &[]);

        let result = tracker.get_console_json();
        let messages = result.get("messages").unwrap().as_array().unwrap();
        assert!(messages[0].get("args").is_none());
    }

    // -- format_console_arg: primitives --

    #[test]
    fn test_format_arg_string() {
        let arg = json!({"type": "string", "value": "hello"});
        assert_eq!(format_console_arg(&arg), Some("hello".to_string()));
    }

    #[test]
    fn request_tracker_clips_fields_and_evicts_oldest_under_count_and_byte_caps() {
        let mut tracker = RequestTracker::new();
        let huge_url = format!(
            "https://example.test/{}",
            "a".repeat(MAX_REQUEST_URL_BYTES * 3)
        );
        let huge_headers = json!({
            "X-Big": "h".repeat(MAX_REQUEST_HEADERS_BYTES * 3),
            "X-After": "ignored"
        });
        let huge_body = "b".repeat(MAX_REQUEST_BODY_BYTES * 3);

        tracker.record_request(
            "r0",
            &huge_url,
            "POST",
            huge_headers,
            1,
            "Fetch",
            Some(&huge_body),
        );
        let first = &tracker.entries()[0];
        assert!(first.url.contains("...[truncated "), "{}", first.url);
        assert!(first.url.len() <= MAX_REQUEST_URL_BYTES + 64);
        assert!(
            header_value_bytes(&first.headers) <= MAX_REQUEST_HEADERS_BYTES + 256,
            "{}",
            header_value_bytes(&first.headers)
        );
        let body = first.post_data.as_deref().unwrap();
        assert!(body.contains("...[truncated "));
        assert!(body.len() <= MAX_REQUEST_BODY_BYTES + 64);

        for i in 1..=200 {
            let id = format!("big-{i}");
            tracker.record_request(
                &id,
                "https://example.test/",
                "POST",
                json!({}),
                i as u64,
                "Fetch",
                None,
            );
            tracker.attach_post_data(&id, &huge_body);
            assert!(
                tracker.retained_bytes() <= MAX_TRACKED_REQUEST_BYTES,
                "retained {} > {}",
                tracker.retained_bytes(),
                MAX_TRACKED_REQUEST_BYTES
            );
        }
        assert!(tracker.len() < 200);
        assert_eq!(tracker.entries().last().unwrap().request_id, "big-200");

        tracker.clear();
        for i in 0..(MAX_TRACKED_REQUESTS + 25) {
            tracker.record_request(
                &format!("c-{i}"),
                "https://example.test/small",
                "GET",
                json!({}),
                i as u64,
                "XHR",
                None,
            );
        }
        assert_eq!(tracker.len(), MAX_TRACKED_REQUESTS);
        assert_eq!(tracker.entries()[0].request_id, "c-25");
        assert_eq!(
            tracker.entries().last().unwrap().request_id,
            format!("c-{}", MAX_TRACKED_REQUESTS + 24)
        );
    }

    #[test]
    fn decode_bidi_body_bounded_handles_plain_and_base64_limits() {
        use base64::Engine;
        let plain = "x".repeat(200);
        assert_eq!(decode_bidi_body_bounded(&plain, false, 64, false), None);
        let clipped = decode_bidi_body_bounded(&plain, false, 64, true).unwrap();
        assert!(clipped.starts_with(&"x".repeat(64)));
        assert!(clipped.contains("...[truncated 136 bytes]"));

        let encoded = base64::engine::general_purpose::STANDARD.encode("y".repeat(300));
        assert_eq!(decode_bidi_body_bounded(&encoded, true, 64, false), None);
        let clipped_b64 = decode_bidi_body_bounded(&encoded, true, 64, true).unwrap();
        assert!(clipped_b64.starts_with(&"y".repeat(64)));
        assert!(clipped_b64.contains("...[truncated "));

        let binary = base64::engine::general_purpose::STANDARD.encode([0xff_u8, 0xfe, 0x00, 0x80]);
        assert_eq!(decode_bidi_body_bounded(&binary, true, 64, true), None);
    }

    #[test]
    fn test_format_arg_number() {
        let arg = json!({"type": "number", "value": 42});
        assert_eq!(format_console_arg(&arg), Some("42".to_string()));
    }

    #[test]
    fn test_format_arg_null() {
        let arg = json!({"type": "object", "subtype": "null", "value": null});
        assert_eq!(format_console_arg(&arg), Some("null".to_string()));
    }

    #[test]
    fn test_format_arg_undefined() {
        let arg = json!({"type": "undefined"});
        assert_eq!(format_console_arg(&arg), Some("undefined".to_string()));
    }

    // -- format_console_arg: objects with preview --

    #[test]
    fn test_format_arg_object_preview() {
        let arg = json!({
            "type": "object",
            "preview": {
                "properties": [
                    {"name": "userId", "type": "string", "value": "abc123"},
                    {"name": "count", "type": "number", "value": "42"}
                ],
                "overflow": false
            }
        });
        assert_eq!(
            format_console_arg(&arg),
            Some("{userId: \"abc123\", count: 42}".to_string())
        );
    }

    #[test]
    fn test_format_arg_object_preview_overflow() {
        let arg = json!({
            "type": "object",
            "preview": {
                "properties": [
                    {"name": "a", "type": "number", "value": "1"}
                ],
                "overflow": true
            }
        });
        assert_eq!(format_console_arg(&arg), Some("{a: 1, ...}".to_string()));
    }

    // -- format_console_arg: arrays with preview --

    #[test]
    fn test_format_arg_array_preview() {
        let arg = json!({
            "type": "object",
            "subtype": "array",
            "preview": {
                "subtype": "array",
                "properties": [
                    {"name": "0", "type": "number", "value": "1"},
                    {"name": "1", "type": "number", "value": "2"},
                    {"name": "2", "type": "number", "value": "3"}
                ],
                "overflow": false
            }
        });
        assert_eq!(format_console_arg(&arg), Some("[1, 2, 3]".to_string()));
    }

    // -- format_console_arg: map/set use description --

    #[test]
    fn test_format_arg_map_uses_description() {
        let arg = json!({
            "type": "object",
            "subtype": "map",
            "description": "Map(1)",
            "preview": {
                "subtype": "map",
                "properties": [{"name": "size", "type": "number", "value": "1"}]
            }
        });
        assert_eq!(format_console_arg(&arg), Some("Map(1)".to_string()));
    }

    // -- format_console_arg: fallback --

    #[test]
    fn test_format_arg_description_fallback() {
        let arg = json!({"type": "object", "description": "RegExp"});
        assert_eq!(format_console_arg(&arg), Some("RegExp".to_string()));
    }

    #[test]
    fn test_format_arg_no_value_no_preview_no_description() {
        let arg = json!({"type": "object"});
        assert_eq!(format_console_arg(&arg), None);
    }

    // -- format_console_args --

    #[test]
    fn test_format_console_args_join() {
        let args = vec![
            json!({"type": "string", "value": "user"}),
            json!({
                "type": "object",
                "preview": {
                    "properties": [{"name": "id", "type": "number", "value": "1"}],
                    "overflow": false
                }
            }),
        ];
        assert_eq!(format_console_args(&args), "user {id: 1}");
    }

    #[test]
    fn test_format_console_args_filters_none() {
        // An arg that returns None should be skipped, not produce empty string
        let args = vec![
            json!({"type": "string", "value": "before"}),
            json!({"type": "object"}), // no value, preview, or description → None
            json!({"type": "string", "value": "after"}),
        ];
        assert_eq!(format_console_args(&args), "before after");
    }
}
