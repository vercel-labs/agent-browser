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

// ---------------------------------------------------------------------------
// Network conditions (offline and throttling)
// ---------------------------------------------------------------------------

/// Throttling presets matching Chrome DevTools (`front_end/core/sdk/NetworkManager.ts`).
/// DevTools renamed "Fast 3G" to "Slow 4G" in 2024 without changing the
/// numbers, so `fast-3g` and `slow-4g` are the same profile.
pub const THROTTLE_PRESETS: &[&str] = &["slow-3g", "fast-3g", "slow-4g", "fast-4g"];

/// Throughput in bytes per second; `-1` disables throttling in that direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThrottleProfile {
    pub latency_ms: f64,
    pub download_bytes_per_sec: f64,
    pub upload_bytes_per_sec: f64,
}

impl ThrottleProfile {
    pub const NONE: Self = Self {
        latency_ms: 0.0,
        download_bytes_per_sec: -1.0,
        upload_bytes_per_sec: -1.0,
    };

    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "slow-3g" => Some(Self {
                latency_ms: 400.0 * 5.0,
                download_bytes_per_sec: 500.0 * 1000.0 / 8.0 * 0.8,
                upload_bytes_per_sec: 500.0 * 1000.0 / 8.0 * 0.8,
            }),
            "fast-3g" | "slow-4g" => Some(Self {
                latency_ms: 150.0 * 3.75,
                download_bytes_per_sec: 1.6 * 1000.0 * 1000.0 / 8.0 * 0.9,
                upload_bytes_per_sec: 750.0 * 1000.0 / 8.0 * 0.9,
            }),
            "fast-4g" => Some(Self {
                latency_ms: 60.0 * 2.75,
                download_bytes_per_sec: 9.0 * 1000.0 * 1000.0 / 8.0 * 0.9,
                upload_bytes_per_sec: 1.5 * 1000.0 * 1000.0 / 8.0 * 0.9,
            }),
            _ => None,
        }
    }
}

pub fn kbps_to_bytes_per_sec(kbps: f64) -> f64 {
    kbps * 1000.0 / 8.0
}

fn bytes_per_sec_to_kbps(bytes: f64) -> Value {
    if bytes < 0.0 {
        Value::Null
    } else {
        json!(bytes * 8.0 / 1000.0)
    }
}

/// Turn a `--match` value into an absolute URLPattern for
/// `Network.emulateNetworkConditionsByRule`. Values containing `://` are
/// passed through as URLPattern strings. Anything else is treated like a
/// `network route` glob (`**/api/*`, `*.png`) and anchored to any scheme,
/// host, and port. The explicit `:*` matters: a URLPattern that omits the
/// port only matches the default port, so `*://*/api/*` would silently miss
/// `localhost:3000`.
pub fn normalize_throttle_pattern(pattern: &str) -> String {
    if pattern.contains("://") {
        return pattern.to_string();
    }
    let path = pattern.trim_start_matches('*');
    let mut out = String::from("*://*:*");
    if !path.starts_with('/') {
        out.push_str("/*");
    }
    let mut last_was_star = out.ends_with('*');
    for ch in path.chars() {
        if ch == '*' && last_was_star {
            continue;
        }
        last_was_star = ch == '*';
        out.push(ch);
    }
    out
}

/// Network conditions for a tab: the offline flag plus throttling rules.
/// URL-scoped rules come first in the order they were added, followed by the
/// global rule (empty pattern), because Chrome applies the first rule that
/// matches a request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetworkConditions {
    pub offline: bool,
    rules: Vec<(String, ThrottleProfile)>,
}

impl NetworkConditions {
    /// True when nothing is emulated, which is how a fresh target starts.
    pub fn is_default(&self) -> bool {
        !self.offline && self.rules.is_empty()
    }

    /// Add or replace the rule for `pattern`; an empty pattern is the global rule.
    pub fn set_rule(&mut self, pattern: String, profile: ThrottleProfile) {
        if let Some(existing) = self.rules.iter_mut().find(|(p, _)| *p == pattern) {
            existing.1 = profile;
        } else if pattern.is_empty() {
            self.rules.push((pattern, profile));
        } else {
            let at = self
                .rules
                .iter()
                .position(|(p, _)| p.is_empty())
                .unwrap_or(self.rules.len());
            self.rules.insert(at, (pattern, profile));
        }
    }

    /// Remove the rule for `pattern`, returning whether one existed.
    pub fn remove_rule(&mut self, pattern: &str) -> bool {
        let before = self.rules.len();
        self.rules.retain(|(p, _)| p != pattern);
        self.rules.len() != before
    }

    fn global(&self) -> ThrottleProfile {
        self.rules
            .iter()
            .find(|(p, _)| p.is_empty())
            .map(|(_, profile)| *profile)
            .unwrap_or(ThrottleProfile::NONE)
    }

    fn has_scoped_rules(&self) -> bool {
        self.rules.iter().any(|(p, _)| !p.is_empty())
    }

    /// The `{ offline, rules }` shape returned by `network throttle`.
    pub fn to_json(&self) -> Value {
        let rules: Vec<Value> = self
            .rules
            .iter()
            .map(|(pattern, profile)| {
                json!({
                    "match": (!pattern.is_empty()).then_some(pattern),
                    "latencyMs": profile.latency_ms,
                    "downloadKbps": bytes_per_sec_to_kbps(profile.download_bytes_per_sec),
                    "uploadKbps": bytes_per_sec_to_kbps(profile.upload_bytes_per_sec),
                })
            })
            .collect();
        json!({ "offline": self.offline, "rules": rules })
    }

    fn by_rule_params(&self) -> Value {
        let rules: Vec<Value> = self
            .rules
            .iter()
            .map(|(pattern, profile)| {
                json!({
                    "urlPattern": pattern,
                    "latency": profile.latency_ms,
                    "downloadThroughput": profile.download_bytes_per_sec,
                    "uploadThroughput": profile.upload_bytes_per_sec,
                })
            })
            .collect();
        json!({ "offline": self.offline, "matchedNetworkConditions": rules })
    }

    /// Params for `Network.overrideNetworkState` or the deprecated
    /// `Network.emulateNetworkConditions`, which share this shape.
    fn global_params(&self) -> Value {
        let global = self.global();
        json!({
            "offline": self.offline,
            "latency": global.latency_ms,
            "downloadThroughput": global.download_bytes_per_sec,
            "uploadThroughput": global.upload_bytes_per_sec,
        })
    }
}

/// Apply `conditions` to one target, replacing whatever it had before.
/// Browsers without `Network.emulateNetworkConditionsByRule` fall back to the
/// deprecated `Network.emulateNetworkConditions`, which can express offline
/// and global throttling but not per-URL rules.
pub async fn apply_network_conditions(
    client: &CdpClient,
    session_id: &str,
    conditions: &NetworkConditions,
) -> Result<(), String> {
    const BY_RULE: &str = "Network.emulateNetworkConditionsByRule";
    match client
        .send_command(BY_RULE, Some(conditions.by_rule_params()), Some(session_id))
        .await
    {
        Ok(_) => {
            client
                .send_command(
                    "Network.overrideNetworkState",
                    Some(conditions.global_params()),
                    Some(session_id),
                )
                .await?;
        }
        Err(e) if super::webmcp::method_is_unsupported(&e, BY_RULE) => {
            if conditions.has_scoped_rules() {
                return Err(format!(
                    "Per-URL throttling (--match) is not supported by this browser: {}",
                    e
                ));
            }
            client
                .send_command(
                    "Network.emulateNetworkConditions",
                    Some(conditions.global_params()),
                    Some(session_id),
                )
                .await?;
        }
        Err(e) => return Err(e),
    }
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

    pub fn is_allowed(&self, hostname: &str) -> bool {
        if self.allowed_domains.is_empty() {
            return true;
        }
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

    pub fn check_url(&self, url: &str) -> Result<(), String> {
        if self.allowed_domains.is_empty() {
            return Ok(());
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
) -> Result<(), String> {
    if allowed_domains.is_empty() {
        return Ok(());
    }

    let script = domain_filter_script(allowed_domains);

    client
        .send_command(
            "Page.addScriptToEvaluateOnNewDocument",
            Some(json!({ "source": &script })),
            Some(session_id),
        )
        .await?;

    install_domain_filter_runtime_script(client, session_id, allowed_domains).await?;

    Ok(())
}

async fn install_domain_filter_runtime_script(
    client: &CdpClient,
    session_id: &str,
    allowed_domains: &[String],
) -> Result<(), String> {
    if allowed_domains.is_empty() {
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
) -> Result<(), String> {
    install_domain_filter_fetch(client, session_id, handle_auth_requests).await?;
    install_domain_filter_script(client, session_id, allowed_domains).await?;
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

pub struct EventTracker {
    pub console_entries: Vec<ConsoleEntry>,
    pub error_entries: Vec<ErrorEntry>,
    pub max_entries: usize,
}

impl EventTracker {
    pub fn new() -> Self {
        Self {
            console_entries: Vec::new(),
            error_entries: Vec::new(),
            max_entries: 1000,
        }
    }

    pub fn add_console(&mut self, level: &str, text: &str, args: Vec<Value>) {
        if self.console_entries.len() >= self.max_entries {
            self.console_entries.remove(0);
        }
        self.console_entries.push(ConsoleEntry {
            level: level.to_string(),
            text: text.to_string(),
            args,
        });
    }

    pub fn add_error(
        &mut self,
        text: &str,
        url: Option<&str>,
        line: Option<i64>,
        col: Option<i64>,
    ) {
        if self.error_entries.len() >= self.max_entries {
            self.error_entries.remove(0);
        }
        self.error_entries.push(ErrorEntry {
            text: text.to_string(),
            url: url.map(String::from),
            line,
            column: col,
        });
    }

    pub fn clear_console(&mut self) {
        self.console_entries.clear();
    }

    pub fn get_console_json(&self) -> Value {
        let messages: Vec<Value> = self
            .console_entries
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
            .error_entries
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

    #[test]
    fn test_throttle_presets_match_devtools() {
        let slow_3g = ThrottleProfile::preset("slow-3g").unwrap();
        assert_eq!(slow_3g.latency_ms, 2000.0);
        assert_eq!(slow_3g.download_bytes_per_sec, 50_000.0);
        assert_eq!(slow_3g.upload_bytes_per_sec, 50_000.0);

        let slow_4g = ThrottleProfile::preset("slow-4g").unwrap();
        assert_eq!(slow_4g.latency_ms, 562.5);
        assert_eq!(slow_4g.download_bytes_per_sec, 180_000.0);
        assert_eq!(slow_4g.upload_bytes_per_sec, 84_375.0);
        assert_eq!(ThrottleProfile::preset("fast-3g"), Some(slow_4g));

        let fast_4g = ThrottleProfile::preset("fast-4g").unwrap();
        assert_eq!(fast_4g.latency_ms, 165.0);
        assert_eq!(fast_4g.download_bytes_per_sec, 1_012_500.0);
        assert_eq!(fast_4g.upload_bytes_per_sec, 168_750.0);

        for name in THROTTLE_PRESETS {
            assert!(ThrottleProfile::preset(name).is_some(), "{name}");
        }
        assert!(ThrottleProfile::preset("3g").is_none());
    }

    #[test]
    fn test_normalize_throttle_pattern() {
        assert_eq!(normalize_throttle_pattern("**/api/*"), "*://*:*/api/*");
        assert_eq!(normalize_throttle_pattern("/api/*"), "*://*:*/api/*");
        assert_eq!(normalize_throttle_pattern("*/api/**"), "*://*:*/api/*");
        assert_eq!(normalize_throttle_pattern("*.png"), "*://*:*/*.png");
        assert_eq!(
            normalize_throttle_pattern("**/api/appearances*"),
            "*://*:*/api/appearances*"
        );
        assert_eq!(
            normalize_throttle_pattern("http://localhost:3000/api/*"),
            "http://localhost:3000/api/*"
        );
    }

    #[test]
    fn test_network_conditions_rule_order_and_replacement() {
        let slow = ThrottleProfile::preset("slow-3g").unwrap();
        let fast = ThrottleProfile::preset("fast-4g").unwrap();
        let mut conditions = NetworkConditions::default();
        assert!(conditions.is_default());

        conditions.set_rule(String::new(), slow);
        conditions.set_rule("*://*:*/api/*".to_string(), fast);
        conditions.set_rule("*://*:*/img/*".to_string(), fast);
        conditions.set_rule("*://*:*/api/*".to_string(), slow);

        let params = conditions.by_rule_params();
        let patterns: Vec<&str> = params["matchedNetworkConditions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["urlPattern"].as_str().unwrap())
            .collect();
        // Scoped rules keep insertion order and stay ahead of the global rule.
        assert_eq!(patterns, vec!["*://*:*/api/*", "*://*:*/img/*", ""]);
        assert_eq!(params["matchedNetworkConditions"][0]["latency"], 2000.0);
        assert_eq!(conditions.global_params()["latency"], 2000.0);

        assert!(conditions.remove_rule(""));
        assert!(!conditions.remove_rule(""));
        assert_eq!(conditions.global_params()["downloadThroughput"], -1.0);
        assert!(conditions.has_scoped_rules());
    }

    #[test]
    fn test_network_conditions_json_reports_kbps() {
        let mut conditions = NetworkConditions {
            offline: true,
            ..NetworkConditions::default()
        };
        conditions.set_rule(
            String::new(),
            ThrottleProfile {
                latency_ms: 100.0,
                download_bytes_per_sec: kbps_to_bytes_per_sec(750.0),
                upload_bytes_per_sec: -1.0,
            },
        );
        assert_eq!(
            conditions.to_json(),
            json!({
                "offline": true,
                "rules": [{
                    "match": null,
                    "latencyMs": 100.0,
                    "downloadKbps": 750.0,
                    "uploadKbps": null,
                }],
            })
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
    fn test_domain_filter_empty() {
        let filter = DomainFilter::new("");
        assert!(filter.is_allowed("anything.com"));
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
        tracker.add_console("log", "hello", vec![]);
        tracker.add_error("oops", Some("test.js"), Some(1), Some(5));

        assert_eq!(tracker.console_entries.len(), 1);
        assert_eq!(tracker.error_entries.len(), 1);
    }

    #[test]
    fn test_console_json_includes_args() {
        let mut tracker = EventTracker::new();
        let raw_args = vec![
            json!({"type": "string", "value": "hello"}),
            json!({"type": "number", "value": 42}),
        ];
        tracker.add_console("log", "hello 42", raw_args);

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
        tracker.add_console("log", "text only", vec![]);

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
