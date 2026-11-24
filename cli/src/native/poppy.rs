//! Daemon side of `poppy join` (experimental).
//!
//! One action covers the whole join: discovery, then the one-time code is read
//! from the page, handed to the authorizer, and submitted without leaving the
//! daemon. Discovery runs inside the normal dispatch, so action policy,
//! confirmation and the session's domain allowlist apply before any request.

use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::{timeout_at, Instant};

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use url::Url;

use super::actions::DaemonState;
use super::network::DomainFilter;

/// Marks an error string as `poppy:<code>: <message>`.
pub const ERROR_PREFIX: &str = "poppy:";

/// The endpoint page's URL and the code in its form, read from one document.
const READ_CODE: &str = "(() => ({ href: location.href, \
                      code: document.querySelector('form input[name=code]')?.value ?? null }))()";
const SUBMIT: &str = "(() => { const i = document.querySelector('form input[name=code]'); \
                      if (!i || !i.form) return false; i.form.submit(); return true; })()";

const POPPY_NET_BINDING: &str = "__agentBrowserPoppyNet";

const POPPY_JOIN_GUARD_SCRIPT: &str = r#"(() => {
    function _agentBrowserInstallPoppyGuard(_baseOverride) {
        const _global = globalThis;
        if (_global.__agentBrowserPoppyGuardInstalled) return;
        _global.__agentBrowserPoppyGuardInstalled = true;
        const _baseHref = _baseOverride || (_global.location && _global.location.href ? _global.location.href : 'about:blank');
        function _securityError(message) {
            if (typeof DOMException === 'function') {
                return new DOMException(message, 'SecurityError');
            }
            const err = new Error(message);
            err.name = 'SecurityError';
            return err;
        }
        function _notify(api, url) {
            try {
                if (typeof _global.__agentBrowserPoppyNet === 'function') {
                    _global.__agentBrowserPoppyNet(JSON.stringify({ api, url }));
                }
            } catch (_) {}
        }
        function _resolveUrl(raw, asWebSocket) {
            try {
                const u = new URL(raw, _baseHref);
                if (asWebSocket) {
                    if (u.protocol === 'http:') u.protocol = 'ws:';
                    if (u.protocol === 'https:') u.protocol = 'wss:';
                }
                return u.href;
            } catch (_) {
                return String(raw);
            }
        }
        if (typeof _global.WebSocket === 'function') {
            const OrigWS = _global.WebSocket;
            const BlockedWS = function(url) {
                _notify('WebSocket', _resolveUrl(url, true));
                throw _securityError('WebSocket blocked during join');
            };
            BlockedWS.prototype = OrigWS.prototype;
            BlockedWS.CONNECTING = 0;
            BlockedWS.OPEN = 1;
            BlockedWS.CLOSING = 2;
            BlockedWS.CLOSED = 3;
            _global.WebSocket = BlockedWS;
        }
        if (typeof _global.WebTransport === 'function') {
            const OrigWT = _global.WebTransport;
            const BlockedWT = function(url) {
                _notify('WebTransport', _resolveUrl(url, false));
                throw _securityError('WebTransport blocked during join');
            };
            BlockedWT.prototype = OrigWT.prototype;
            _global.WebTransport = BlockedWT;
        }
        function _blockCtor(name, api) {
            if (typeof _global[name] !== 'function') return;
            const Blocked = function(firstArg) {
                if (api) _notify(api, _resolveUrl(firstArg, false));
                throw _securityError(name + ' blocked during join');
            };
            Object.defineProperty(Blocked, 'prototype', {
                value: Object.freeze(Object.create(null)),
                writable: false
            });
            try {
                Object.defineProperty(_global, name, {
                    value: Blocked,
                    writable: false,
                    configurable: true
                });
            } catch (_) {
                _global[name] = Blocked;
            }
        }
        _blockCtor('RTCPeerConnection', null);
        _blockCtor('webkitRTCPeerConnection', null);
        _blockCtor('SharedWorker', 'SharedWorker');
        const OrigWorker = _global.Worker;
        if (typeof OrigWorker === 'function') {
            _global.Worker = function(scriptURL, options) {
                if (!_global.Blob || !_global.URL || typeof _global.URL.createObjectURL !== 'function') {
                    throw _securityError('Worker blocked during join');
                }
                const absolute = _resolveUrl(scriptURL, false);
                const isModule = options && typeof options === 'object' && options.type === 'module';
                const installSource = '(' + _agentBrowserInstallPoppyGuard.toString() + ')(' + JSON.stringify(absolute) + ');\n';
                const source = installSource + (isModule
                    ? 'await import(' + JSON.stringify(absolute) + ');\n'
                    : 'importScripts(' + JSON.stringify(absolute) + ');\n');
                const wrapped = _global.URL.createObjectURL(new Blob([source], { type: 'application/javascript' }));
                return new OrigWorker(wrapped, options);
            };
            _global.Worker.prototype = OrigWorker.prototype;
        }
    }
    _agentBrowserInstallPoppyGuard();
})()"#;

type AddressSpaces = HashMap<String, Vec<(i64, bool)>>;

pub(super) fn fetch_enable_params(handle_auth_requests: bool) -> Value {
    let mut params = json!({
        "patterns": [
            { "urlPattern": "*" },
            { "urlPattern": "*", "requestStage": "Response" },
        ]
    });
    if handle_auth_requests {
        params["handleAuthRequests"] = json!(true);
    }
    params
}

const RESPONSE_WAIT: Duration = Duration::from_secs(1);

fn fail(code: &str, message: impl std::fmt::Display) -> String {
    format!("{}{}: {}", ERROR_PREFIX, code, message)
}

/// Holds every request of the join tab to the endpoint's address rules while
/// the join runs. The fetch handler asks it before a request, a redirect hop
/// included, is sent, and fails a refused one, so nothing reaches the refused
/// address. The first refusal fails the join.
#[derive(Clone)]
pub struct JoinGuard {
    session_id: String,
    sessions: Arc<Mutex<HashSet<String>>>,
    guard_scripts: Arc<Mutex<Vec<(String, String)>>>,
    local: bool,
    deadline: Instant,
    blocked: Arc<Mutex<Option<String>>>,
    spaces: Arc<Mutex<AddressSpaces>>,
    recorded: Arc<tokio::sync::Notify>,
    code: Arc<Mutex<Option<String>>>,
}

impl JoinGuard {
    fn new(session_id: &str, local: bool, deadline: Instant) -> Self {
        Self {
            session_id: session_id.to_string(),
            sessions: Arc::new(Mutex::new(HashSet::from([session_id.to_string()]))),
            guard_scripts: Arc::default(),
            local,
            deadline,
            blocked: Arc::default(),
            spaces: Arc::default(),
            recorded: Arc::default(),
            code: Arc::default(),
        }
    }

    pub(super) async fn install_session_guard(
        &self,
        client: &super::cdp::client::CdpClient,
        session_id: &str,
        worker_like: bool,
    ) -> Result<(), String> {
        let block_ws = client
            .send_command(
                "Network.setBlockedURLs",
                Some(json!({ "urls": ["ws://*", "wss://*"] })),
                Some(session_id),
            )
            .await;
        if !worker_like {
            block_ws?;
            client
                .send_command(
                    "Runtime.addBinding",
                    Some(json!({ "name": POPPY_NET_BINDING })),
                    Some(session_id),
                )
                .await?;
            let added = client
                .send_command(
                    "Page.addScriptToEvaluateOnNewDocument",
                    Some(json!({ "source": POPPY_JOIN_GUARD_SCRIPT })),
                    Some(session_id),
                )
                .await?;
            if let Some(id) = added.get("identifier").and_then(Value::as_str) {
                self.guard_scripts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((session_id.to_string(), id.to_string()));
            }
        }
        Ok(())
    }

    async fn remove_session_guards(&self, client: &super::cdp::client::CdpClient) {
        let scripts = {
            let mut guard = self.guard_scripts.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *guard)
        };
        for (sid, identifier) in scripts {
            let _ = client
                .send_command(
                    "Page.removeScriptToEvaluateOnNewDocument",
                    Some(json!({ "identifier": identifier })),
                    Some(&sid),
                )
                .await;
        }
        let mut sessions = self.adopted();
        sessions.push(self.session_id.clone());
        for sid in sessions {
            let _ = client
                .send_command(
                    "Network.setBlockedURLs",
                    Some(json!({ "urls": [] })),
                    Some(&sid),
                )
                .await;
            let _ = client
                .send_command(
                    "Runtime.removeBinding",
                    Some(json!({ "name": POPPY_NET_BINDING })),
                    Some(&sid),
                )
                .await;
        }
    }

    fn redact(&self, message: String) -> String {
        match self
            .code
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
        {
            Some(code) => message.replace(code, "[redacted]"),
            None => message,
        }
    }

    fn record_address_space(&self, params: &Value) {
        let Some(network_id) = params.get("requestId").and_then(Value::as_str) else {
            return;
        };
        let status = params
            .get("statusCode")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let space = params
            .get("resourceIPAddressSpace")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let admitted = match crate::poppy::check_address_space(space, self.local) {
            Ok(()) => true,
            Err(e) => {
                self.block(e.message);
                false
            }
        };
        self.spaces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(network_id.to_string())
            .or_default()
            .push((status, admitted));
        self.recorded.notify_waiters();
    }

    fn response_verdict(&self, network_id: &str, status: i64) -> Option<bool> {
        let spaces = self.spaces.lock().unwrap_or_else(|e| e.into_inner());
        let hops = spaces.get(network_id)?;
        if hops.iter().any(|(_, admitted)| !admitted) {
            return Some(false);
        }
        hops.iter().any(|(s, _)| *s == status).then_some(true)
    }

    pub(super) async fn admits_response(&self, network_id: &str, status: Option<i64>) -> bool {
        let Some(status) = status.filter(|_| !self.local) else {
            return true;
        };
        let wait_until = self.deadline.min(Instant::now() + RESPONSE_WAIT);
        loop {
            let recorded = self.recorded.notified();
            tokio::pin!(recorded);
            recorded.as_mut().enable();
            if let Some(verdict) = self.response_verdict(network_id, status) {
                return verdict;
            }
            if timeout_at(wait_until, recorded).await.is_err() {
                return self.response_verdict(network_id, status) != Some(false);
            }
        }
    }

    pub(super) fn guards(&self, session_id: &str) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(session_id)
    }

    pub(super) fn adopt(&self, session_id: &str) {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.to_string());
    }

    fn adopted(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|s| **s != self.session_id)
            .cloned()
            .collect()
    }

    /// Whether the tab may request `url`; a refusal is recorded for the join.
    pub(super) async fn admits(&self, url: &str) -> bool {
        match crate::poppy::check_request(url, self.local, self.deadline).await {
            Ok(()) => true,
            Err(e) => {
                self.block(e.message);
                false
            }
        }
    }

    fn block(&self, message: String) {
        let mut blocked = self.blocked.lock().unwrap_or_else(|e| e.into_inner());
        if blocked.is_none() {
            *blocked = Some(message);
        }
    }

    fn blocked(&self) -> Option<String> {
        self.blocked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    async fn check_socket_attempt(&self, api: &str, url: &str, filter: Option<&DomainFilter>) {
        let check = if api == "WebSocket" {
            crate::poppy::check_websocket_request(url, self.local, self.deadline).await
        } else {
            crate::poppy::check_request(url, self.local, self.deadline).await
        };
        if let Err(e) = check {
            self.block(e.message);
            return;
        }
        if let Some(filter) = filter {
            if let Err(message) = filter.check_url(url) {
                self.block(message);
            }
        }
    }

    /// Fails the join if a request was refused, a blocked socket/transport
    /// attempt violated the rules, or a response came from a refused address,
    /// so the join stops before its next step.
    async fn checkpoint(
        &self,
        events: &mut tokio::sync::mpsc::UnboundedReceiver<super::cdp::types::CdpEvent>,
        filter: Option<&DomainFilter>,
    ) -> Result<(), String> {
        while let Ok(event) = events.try_recv() {
            if !event.session_id.as_deref().is_some_and(|s| self.guards(s)) {
                continue;
            }
            match event.method.as_str() {
                "Network.responseReceived" => {
                    let response = event.params.get("response");
                    let url = response
                        .and_then(|r| r.get("url"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let remote = response
                        .and_then(|r| r.get("remoteIPAddress"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if let Err(e) = crate::poppy::check_connected_address(url, remote) {
                        self.block(e.message);
                    }
                }
                "Runtime.bindingCalled" => {
                    if event.params.get("name").and_then(Value::as_str) != Some(POPPY_NET_BINDING) {
                        continue;
                    }
                    let Some(payload) = event.params.get("payload").and_then(Value::as_str) else {
                        continue;
                    };
                    let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                        continue;
                    };
                    let api = parsed
                        .get("api")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let url = parsed
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.check_socket_attempt(api, url, filter).await;
                }
                "Network.webSocketCreated" => {
                    let url = event
                        .params
                        .get("url")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    self.check_socket_attempt("WebSocket", url, filter).await;
                }
                _ => {}
            }
        }
        match self.blocked() {
            Some(message) => Err(fail("request_blocked", message)),
            None => Ok(()),
        }
    }
}

fn same_resource(a: &str, b: &str) -> bool {
    match (Url::parse(a), Url::parse(b)) {
        (Ok(mut a), Ok(mut b)) => {
            a.set_fragment(None);
            b.set_fragment(None);
            a == b
        }
        _ => false,
    }
}

fn check_domain(filter: &Option<DomainFilter>, url: &str) -> Result<(), crate::poppy::JoinError> {
    match filter {
        Some(filter) => filter
            .check_url(url)
            .map_err(|message| crate::poppy::JoinError {
                code: "domain_not_allowed",
                message,
            }),
        None => Ok(()),
    }
}

/// Reads the Company's `poppy.json`. The session's domain allowlist is checked
/// on the discovery URL and on every redirect hop before it is requested, and
/// on the endpoint the document names before the browser navigates to it.
async fn discover(
    cmd: &Value,
    state: &DaemonState,
    deadline: Instant,
) -> Result<crate::poppy::Discovery, String> {
    let url = cmd
        .get("url")
        .and_then(Value::as_str)
        .ok_or("Missing 'url' parameter")?;
    let trust = match cmd.get("tls") {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| format!("Invalid TLS options: {e}"))?,
        None => crate::tls::TrustOptions::default(),
    };
    let to_error = |e: crate::poppy::JoinError| fail(e.code, e.message);
    let filter = state.domain_filter.read().await.clone();
    let target = crate::poppy::target(url).map_err(to_error)?;
    let discovery = crate::poppy::fetch(target, deadline, &trust, |hop| {
        check_domain(&filter, hop.as_str())
    })
    .await
    .map_err(to_error)?;
    check_domain(&filter, &discovery.endpoint).map_err(to_error)?;
    Ok(discovery)
}

/// Discovery, then steps 1 to 3 of the browser join, with the authorizer between them.
pub(super) async fn handle_join(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let field = |name: &str| {
        cmd.get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("Missing '{}' parameter", name))
    };
    let authorizer = field("authorizer")?;
    let cwd = cmd.get("cwd").and_then(Value::as_str);
    let timeout_ms = state.timeout_ms(cmd);

    state
        .refuse_auto_connect()
        .map_err(|e| fail("auto_connect_refused", e))?;
    // One deadline covers discovery, navigation, the authorizer and the submit.
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    let discovery = discover(cmd, state, deadline).await?;
    let session_id = state
        .browser
        .as_ref()
        .ok_or("Browser not launched")?
        .active_session_id()?
        .to_string();

    super::actions::ensure_browser_auto_attach(state)
        .await
        .map_err(|e| fail("join_failed", e))?;
    let guard = JoinGuard::new(&session_id, discovery.local, deadline);
    let client = state
        .browser
        .as_ref()
        .ok_or("Browser not launched")?
        .client
        .clone();
    let mut infos = client.subscribe_methods(&["Network.responseReceivedExtraInfo"]);
    let mut events = client.subscribe_methods(&[
        "Network.responseReceived",
        "Runtime.bindingCalled",
        "Network.webSocketCreated",
    ]);
    let filter = state.domain_filter.read().await.clone();
    let recorder = {
        let guard = guard.clone();
        tokio::spawn(async move {
            while let Some(event) = infos.recv().await {
                if event.session_id.as_deref().is_some_and(|s| guard.guards(s)) {
                    guard.record_address_space(&event.params);
                }
            }
        })
    };
    *state.poppy_guard.write().await = Some(guard.clone());
    let setup_result = match super::actions::sync_fetch_interception(state, &session_id).await {
        Err(e) => Err(fail("join_failed", e)),
        Ok(()) => guard
            .install_session_guard(&client, &session_id, false)
            .await
            .map_err(|e| fail("join_failed", e)),
    };
    let result = match setup_result {
        Err(e) => Err(e),
        Ok(()) => {
            let join = run_join(
                state,
                &guard,
                &mut events,
                filter.as_ref(),
                &discovery.endpoint,
                &authorizer,
                &discovery.origin,
                cwd,
                deadline,
            );
            timeout_at(deadline, join).await.unwrap_or_else(|_| {
                Err(fail(
                    "join_failed",
                    format!(
                        "Timed out after {}ms waiting for the join to finish",
                        timeout_ms
                    ),
                ))
            })
        }
    };
    let _ = guard.checkpoint(&mut events, filter.as_ref()).await;
    let result = match guard.blocked() {
        Some(message) => Err(fail("request_blocked", message)),
        None => result,
    };
    let result = result.map_err(|e| guard.redact(e));
    if result.is_err() {
        leave_join_page(state, &session_id).await;
    }
    *state.poppy_guard.write().await = None;
    guard.remove_session_guards(&client).await;
    let _ = super::actions::sync_fetch_interception(state, &session_id).await;
    for adopted in guard.adopted() {
        let _ = super::actions::sync_fetch_interception(state, &adopted).await;
    }
    recorder.abort();
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_join(
    state: &mut DaemonState,
    guard: &JoinGuard,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<super::cdp::types::CdpEvent>,
    filter: Option<&DomainFilter>,
    endpoint: &str,
    authorizer: &str,
    origin: &str,
    cwd: Option<&str>,
    deadline: Instant,
) -> Result<Value, String> {
    let navigated = super::actions::handle_navigate(&json!({ "url": endpoint }), state).await;
    let mgr = state.browser.as_ref().ok_or("Browser not launched")?;
    let page = mgr.evaluate(READ_CODE, None).await;
    *guard.code.lock().unwrap_or_else(|e| e.into_inner()) = page
        .as_ref()
        .ok()
        .and_then(|p| p.get("code"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    guard.checkpoint(events, filter).await?;
    navigated.map_err(|e| fail("join_failed", e))?;
    let page = page.map_err(|e| fail("join_failed", e))?;
    let href = page.get("href").and_then(Value::as_str).unwrap_or_default();
    // The code is read only from the endpoint the Company published.
    if !same_resource(href, endpoint) {
        return Err(fail(
            "join_failed",
            "The browser session page is not the endpoint the Company published",
        ));
    }
    let code = page
        .get("code")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .ok_or_else(|| fail("join_failed", "The browser session page has no code form"))?;
    guard.checkpoint(events, filter).await?;

    let input = json!({
        "code": code,
        "authorize_url": format!("{}/authorize", endpoint.trim_end_matches('/')),
        "origin": origin,
        "poppy_version": "1.0",
    });
    let signed_in = run_authorizer(authorizer, cwd, &input, &code, deadline).await?;
    guard.checkpoint(events, filter).await?;

    // Subscribe before submitting so the navigation cannot be missed.
    let session_id = mgr.active_session_id()?.to_string();
    let mut nav_events = mgr.client.subscribe();
    let submitted = mgr
        .evaluate(SUBMIT, None)
        .await
        .map_err(|e| fail("join_failed", e))?;
    if submitted != json!(true) {
        return Err(fail(
            "join_failed",
            "The page no longer shows the join form",
        ));
    }
    let wait = async {
        loop {
            match nav_events.recv().await {
                Ok(event)
                    if event.session_id.as_deref() == Some(session_id.as_str())
                        && event.method == "Page.frameNavigated"
                        && event.params.pointer("/frame/parentId").is_none() =>
                {
                    return Ok(event
                        .params
                        .pointer("/frame/url")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string());
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(fail("join_failed", "CDP event stream closed"))
                }
            }
        }
    };
    let url = timeout_at(deadline, wait)
        .await
        .map_err(|_| fail("join_failed", "Timed out waiting for the join to finish"))??;
    guard.checkpoint(events, filter).await?;
    // A Company's error page stays on the endpoint; success leaves it.
    if same_resource(&url, endpoint) {
        return Err(fail(
            "join_failed",
            "The Company did not accept the browser session form",
        ));
    }
    // A target that fails to load commits a browser error page, not a site page.
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(fail(
            "join_failed",
            "The page the join redirected to did not load",
        ));
    }
    // A redirect the domain filter blocked commits a synthetic error page.
    if let Some(filter) = state.domain_filter.read().await.as_ref() {
        if filter.check_url(&url).is_err() {
            return Err(fail(
                "join_failed",
                "The join redirected to a host outside the allowed domains",
            ));
        }
    }
    let url = url.replace(&code, "[redacted]");
    state.ref_map.invalidate_page(&session_id);
    Ok(json!({ "experimental": true, "joined": true, "signed_in": signed_in, "url": url }))
}

/// Returns the tab to `about:blank` after a failed join.
async fn leave_join_page(state: &mut DaemonState, session_id: &str) {
    let Some(mgr) = state
        .browser
        .as_ref()
        .filter(|m| m.has_page_session(session_id))
    else {
        return;
    };
    let _ = mgr
        .client
        .send_command(
            "Page.navigate",
            Some(json!({ "url": "about:blank" })),
            Some(session_id),
        )
        .await;
    state.ref_map.invalidate_page(session_id);
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    }
    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    }
}

/// Runs the authorizer with `input` as one JSON line on stdin and returns
/// whether it signed the browser in.
async fn run_authorizer(
    command: &str,
    cwd: Option<&str>,
    input: &Value,
    code: &str,
    deadline: Instant,
) -> Result<bool, String> {
    let failed = |message: String| fail("authorizer_failed", message);
    let mut cmd = shell_command(command);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| failed(format!("Could not start authorizer: {}", e)))?;
    let line = format!("{}\n", input);
    let mut stdin = child.stdin.take();
    let run = async move {
        if let Some(pipe) = stdin.as_mut() {
            // A broken pipe means the authorizer exited unread; its status explains why.
            let _ = pipe.write_all(line.as_bytes()).await;
        }
        drop(stdin);
        child.wait_with_output().await
    };
    let output = timeout_at(deadline, run)
        .await
        .map_err(|_| failed("Authorizer did not finish before the join timed out".to_string()))?
        .map_err(|e| failed(format!("Could not wait for authorizer: {}", e)))?;
    interpret_output(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        code,
    )
}

/// `{"signed_in": bool}` with exit 0, or `{"error": "<code>"}` with a nonzero exit.
fn interpret_output(success: bool, stdout: &str, code: &str) -> Result<bool, String> {
    let parsed: Option<Value> = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| serde_json::from_str(line.trim()).ok());
    if !success {
        let error = parsed
            .as_ref()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .filter(|e| !e.is_empty() && !e.contains(code));
        return Err(match error {
            Some(error) => fail(error, "The authorizer reported an error"),
            None => fail(
                "authorizer_failed",
                "The authorizer failed without an error code",
            ),
        });
    }
    parsed
        .as_ref()
        .and_then(|v| v.get("signed_in"))
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            fail(
                "authorizer_failed",
                "The authorizer exited 0 without a JSON line containing signed_in",
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_ms(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn authorizer_output_is_interpreted() {
        assert_eq!(
            interpret_output(true, "{\"signed_in\":true}\n", "bc_1"),
            Ok(true)
        );
        assert_eq!(
            interpret_output(true, "noise\n{\"signed_in\":false}", "bc_1"),
            Ok(false)
        );
        assert_eq!(
            interpret_output(false, "{\"error\":\"insufficient_scope\"}", "bc_1").unwrap_err(),
            "poppy:insufficient_scope: The authorizer reported an error"
        );
    }

    #[test]
    fn malformed_authorizer_output_is_authorizer_failed() {
        for (success, stdout) in [
            (true, ""),
            (true, "{\"signed_in\":\"yes\"}"),
            (false, ""),
            (false, "not json"),
            (false, "{\"error\":\"\"}"),
            (false, "{\"error\":\"leak_bc_1\"}"),
        ] {
            let err = interpret_output(success, stdout, "bc_1").unwrap_err();
            assert!(
                err.starts_with("poppy:authorizer_failed: "),
                "{stdout}: {err}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_authorizer_gets_one_json_line_in_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("marker"), "").unwrap();
        let script = "read line; case \"$line\" in *bc_1*) [ -f marker ] && \
                      echo '{\"signed_in\":true}' || echo '{\"signed_in\":false}';; esac";
        let cwd = dir.path().to_str().unwrap();
        let input = json!({ "code": "bc_1" });
        assert_eq!(
            run_authorizer(script, Some(cwd), &input, "bc_1", in_ms(5_000)).await,
            Ok(true)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_slow_authorizer_is_stopped_at_the_timeout() {
        let started = std::time::Instant::now();
        let err = run_authorizer("sleep 5", None, &json!({}), "bc_1", in_ms(100))
            .await
            .unwrap_err();
        assert!(
            err.contains("did not finish before the join timed out"),
            "{err}"
        );
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn same_resource_ignores_fragments() {
        assert!(same_resource("http://h/a#x", "http://h/a"));
        assert!(!same_resource("http://h/", "http://h/a"));
    }
}
