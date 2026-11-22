//! Document loading for the active tab: the one place that navigates, races prompts,
//! resolves the landed URL and records the visit.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{json, Value};

use super::bidi::{remote_to_json, Events, SANDBOX};
use super::daemon;
use super::session::FirefoxSession;
use crate::native::actions::DaemonState;
use crate::native::browser::{load_state_timeout_error, WaitUntil};

/// Firefox net-error codes and `about:neterror` names, mapped to the Chromium code Chrome reports
/// for the same failure.
const NET_ERRORS: &[(&str, &str)] = &[
    ("NS_ERROR_ABORT", "net::ERR_FAILED"),
    ("NS_ERROR_UNKNOWN_HOST", "net::ERR_NAME_NOT_RESOLVED"),
    ("NS_ERROR_CONNECTION_REFUSED", "net::ERR_CONNECTION_REFUSED"),
    ("NS_ERROR_NET_TIMEOUT", "net::ERR_TIMED_OUT"),
    ("NS_ERROR_NET_TIMEOUT_EXTERNAL", "net::ERR_TIMED_OUT"),
    ("NS_ERROR_OFFLINE", "net::ERR_INTERNET_DISCONNECTED"),
    (
        "NS_ERROR_UNKNOWN_PROXY_HOST",
        "net::ERR_PROXY_CONNECTION_FAILED",
    ),
    ("dnsNotFound", "net::ERR_NAME_NOT_RESOLVED"),
    ("connectionFailure", "net::ERR_CONNECTION_REFUSED"),
    ("netTimeout", "net::ERR_TIMED_OUT"),
    ("offline", "net::ERR_INTERNET_DISCONNECTED"),
    ("proxyConnectFailure", "net::ERR_PROXY_CONNECTION_FAILED"),
];

/// The Chromium code for an exact Firefox net-error name or Gecko error code.
fn chromium_net_error(code: &str) -> Option<&'static str> {
    NET_ERRORS
        .iter()
        .find(|(gecko, _)| *gecko == code)
        .map(|(_, chromium)| *chromium)
}

/// Finds the first known net-error token in free-form Firefox text such as `Error: NS_ERROR_ABORT`.
/// Firefox omits the `e` name for failures it has no neterror page for, which is how a route abort
/// arrives, and a BiDi command error carries the Gecko code in its message instead of a URL.
fn net_error_token(text: &str) -> Option<&'static str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find_map(chromium_net_error)
}

/// Rewrites the Gecko net-error token in a Firefox error message to the Chromium code Chrome
/// reports for the same failure. Messages without a known code are returned unchanged.
pub(super) fn normalize_net_error_text(text: &str) -> String {
    net_error_token(text).map_or_else(|| text.to_string(), str::to_string)
}

/// How long a load should wait for the document before reporting back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wait {
    None,
    Interactive,
    Complete,
    NetworkIdle { timeout_ms: u64 },
}

impl Wait {
    /// Maps a command's `waitUntil` value to a load wait, keeping the command's timeout for the
    /// idle wait.
    pub fn from_wait_until(wait_until: Option<&str>, timeout_ms: u64) -> Self {
        match wait_until {
            Some("networkidle") => Wait::NetworkIdle { timeout_ms },
            Some("domcontentloaded") => Wait::Interactive,
            Some("none") => Wait::None,
            _ => Wait::Complete,
        }
    }

    fn as_bidi(self) -> &'static str {
        match self {
            Wait::None => "none",
            Wait::Interactive => "interactive",
            Wait::Complete | Wait::NetworkIdle { .. } => "complete",
        }
    }

    fn idle_timeout(self) -> Option<u64> {
        match self {
            Wait::NetworkIdle { timeout_ms } => Some(timeout_ms),
            _ => None,
        }
    }
}

/// What to load in the active tab.
pub enum Load<'a> {
    Url { url: &'a str, wait: Wait },
    Reload,
    History { delta: i64 },
}

/// Where the active tab ended up.
#[derive(Debug)]
pub struct Landing {
    /// The active context, which a load never changes.
    pub context: String,
    pub url: String,
    /// True when a prompt stopped the load, so the tab may still show the previous page.
    pub prompt: bool,
}

/// Loads the active tab, dropping refs and frame scope first, and records the origin it reached.
/// Fails with the load's name when Firefox rejects it, and with Chrome's load timeout when the
/// load outlasts the session's default timeout.
pub async fn load(state: &mut DaemonState, load: Load<'_>) -> Result<Landing, String> {
    if let Load::Url { url, .. } = &load {
        state.ensure_navigation_allowed(url).await?;
    }
    let (requested, waited, idle_timeout) = match &load {
        Load::Url { url, wait } => (
            *url,
            match wait {
                Wait::None => WaitUntil::None,
                Wait::Interactive => WaitUntil::DomContentLoaded,
                Wait::Complete | Wait::NetworkIdle { .. } => WaitUntil::Load,
            },
            wait.idle_timeout(),
        ),
        Load::Reload | Load::History { .. } => ("", WaitUntil::Load, None),
    };
    let budget = Duration::from_millis(state.default_timeout_ms);
    daemon::reset_page(state);
    let ff = daemon::session(state)?;
    let context = ff.active_tab().to_string();
    let operation = async {
        match load {
            Load::Url { url, wait } => {
                wire_load(
                    ff,
                    &context,
                    "browsingContext.navigate",
                    json!({ "url": url, "wait": wait.as_bidi() }),
                    "Navigation",
                    requested,
                )
                .await
            }
            Load::Reload => {
                wire_load(
                    ff,
                    &context,
                    "browsingContext.reload",
                    json!({ "wait": "complete" }),
                    "Reload",
                    requested,
                )
                .await
            }
            Load::History { delta } => traverse_history(ff, &context, delta).await,
        }
    };
    let landed = tokio::time::timeout(budget, ff.race_prompt(&context, operation))
        .await
        .map_err(|_| load_state_timeout_error(waited, budget))??;
    let (url, prompt) = match landed {
        Some(url) => (url, false),
        None => (current_url(ff, &context, requested).await, true),
    };
    ff.note_visit(&url);
    if !prompt {
        if let Some(timeout_ms) = idle_timeout {
            super::network::wait_idle(state, timeout_ms)
                .await
                .map_err(|_| "Timeout waiting for networkidle".to_string())?;
        }
    }
    Ok(Landing {
        context,
        url,
        prompt,
    })
}

async fn wire_load(
    ff: &FirefoxSession,
    context: &str,
    method: &str,
    mut params: Value,
    failure: &str,
    requested: &str,
) -> Result<String, String> {
    params["context"] = json!(context);
    let result = ff
        .bidi
        .send_without_deadline(method, params)
        .await
        .map_err(|error| format!("{failure} failed: {}", normalize_net_error_text(&error)))?;
    match result["url"].as_str().filter(|url| !url.is_empty()) {
        Some(url) => Ok(url.to_string()),
        None => Ok(current_url(ff, context, requested).await),
    }
}

async fn current_url(ff: &FirefoxSession, context: &str, requested: &str) -> String {
    ff.url_in_context(context)
        .await
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| requested.to_string())
}

struct Document {
    entry_key: Option<String>,
    realm: String,
    url: String,
    complete: bool,
}

impl Document {
    async fn read(ff: &FirefoxSession, context: &str) -> Option<Self> {
        let result = ff.bidi.send_without_deadline("script.evaluate", json!({
            "expression": "({ entryKey: window.navigation?.currentEntry?.key ?? null, url: location.href, readyState: document.readyState })",
            "target": { "context": context, "sandbox": SANDBOX },
            "awaitPromise": false,
            "resultOwnership": "none",
        })).await.ok()?;
        let value = remote_to_json(&result["result"]);
        Some(Self {
            entry_key: value["entryKey"]
                .as_str()
                .filter(|key| !key.is_empty())
                .map(str::to_string),
            realm: result["realm"].as_str()?.to_string(),
            url: value["url"].as_str()?.to_string(),
            complete: value["readyState"] == "complete",
        })
    }

    fn landed_from(&self, before: &Self) -> bool {
        if self.realm != before.realm {
            self.complete
        } else {
            self.entry_key
                .as_ref()
                .zip(before.entry_key.as_ref())
                .is_some_and(|(after, before)| after != before)
        }
    }
}

fn history_event(event: &Value) -> bool {
    matches!(
        event["method"].as_str(),
        Some(
            "browsingContext.contextCreated"
                | "browsingContext.contextDestroyed"
                | "browsingContext.navigationStarted"
                | "browsingContext.domContentLoaded"
                | "browsingContext.load"
                | "browsingContext.fragmentNavigated"
        )
    )
}

async fn traverse_history(
    ff: &FirefoxSession,
    context: &str,
    delta: i64,
) -> Result<String, String> {
    let before = Document::read(ff, context).await;
    let events = ff.bidi.subscribe_matching(history_event);
    match ff
        .bidi
        .send_without_deadline_raw(
            "browsingContext.traverseHistory",
            json!({ "context": context, "delta": delta }),
        )
        .await
    {
        Ok(_) => {}
        Err(error) if error.code.as_deref() == Some("no such history entry") => {
            return ff.url_in_context(context).await;
        }
        Err(error) => {
            return Err(format!(
                "History navigation failed: {}",
                normalize_net_error_text(&error.to_string())
            ));
        }
    }
    tokio::select! {
        biased;
        landed = wait_history_events(ff, context, before.as_ref(), events) => landed,
        landed = wait_history_document(ff, context, before.as_ref()) => Ok(landed),
    }
}

async fn wait_history_document(
    ff: &FirefoxSession,
    context: &str,
    before: Option<&Document>,
) -> String {
    let Some(before) = before else {
        return std::future::pending().await;
    };
    loop {
        if let Some(document) = Document::read(ff, context).await {
            if document.landed_from(before) {
                return document.url;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_history_events(
    ff: &FirefoxSession,
    context: &str,
    before: Option<&Document>,
    mut events: Events,
) -> Result<String, String> {
    let mut descendants = HashSet::new();
    let mut navigations = HashMap::new();
    loop {
        let event = events.recv().await;
        if events.has_lost_events() {
            return Err("History navigation failed: Firefox BiDi event queue overflowed".into());
        }
        let event = event.ok_or("History navigation failed: BiDi connection closed")?;
        let params = &event["params"];
        let Some(owner) = params["context"].as_str() else {
            continue;
        };
        if event["method"] == "browsingContext.contextCreated"
            && params["parent"].as_str().is_some_and(|id| {
                ff.contexts.tab_of(id) == Some(context) || descendants.contains(id)
            })
        {
            descendants.insert(owner.to_string());
        }
        if owner != context
            && ff.contexts.tab_of(owner) != Some(context)
            && !descendants.contains(owner)
        {
            continue;
        }
        match event["method"].as_str() {
            Some("browsingContext.contextDestroyed") if owner == context => {
                return Err("History navigation failed: Firefox closed the tab".into());
            }
            Some("browsingContext.contextDestroyed") => {
                descendants.remove(owner);
                navigations.remove(owner);
            }
            Some("browsingContext.navigationStarted") => {
                navigations.insert(owner.to_string(), params["navigation"].clone());
            }
            Some(
                method @ ("browsingContext.load"
                | "browsingContext.domContentLoaded"
                | "browsingContext.fragmentNavigated"),
            ) => {
                let fragment = method == "browsingContext.fragmentNavigated";
                let started_here = navigations
                    .get(owner)
                    .is_some_and(|id| !id.is_null() && *id == params["navigation"]);
                if owner == context {
                    if let Some(url) = params["url"].as_str().filter(|url| !url.is_empty()) {
                        if fragment || started_here || before.is_some_and(|doc| doc.url != url) {
                            return Ok(url.to_string());
                        }
                    }
                } else if !navigations.contains_key(context) && (fragment || started_here) {
                    return ff.url_in_context(context).await;
                }
            }
            _ => {}
        }
    }
}

/// Turns an `about:neterror?e=...&d=...` URL into the message Firefox shows for the failure.

pub(super) fn neterror_message(url: &str) -> Option<String> {
    if !url.starts_with("about:neterror") {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    let pair = |key: &str| {
        parsed
            .query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .filter(|v| !v.is_empty())
    };
    let code = pair("e").unwrap_or_else(|| "unknown error".to_string());
    let detail = pair("d");
    if let Some(chromium) =
        chromium_net_error(&code).or_else(|| detail.as_deref().and_then(net_error_token))
    {
        return Some(chromium.to_string());
    }
    Some(match detail {
        Some(detail) => format!("{code}: {detail}"),
        None => code,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::firefox::session::tests::fake_session;

    #[test]
    fn neterror_url_becomes_chromes_message() {
        let url = "about:neterror?e=dnsNotFound&u=https%3A//nope.invalid/&d=We%20can%27t%20connect";
        assert_eq!(neterror_message(url).unwrap(), "net::ERR_NAME_NOT_RESOLVED");
        assert_eq!(
            neterror_message("about:neterror?e=connectionFailure").unwrap(),
            "net::ERR_CONNECTION_REFUSED"
        );
        assert_eq!(
            neterror_message("about:neterror?e=corruptedContentErrorv2&d=Bad%20content").unwrap(),
            "corruptedContentErrorv2: Bad content"
        );
        assert!(neterror_message("https://example.com").is_none());
    }

    #[test]
    fn error_text_maps_gecko_codes_and_keeps_unknown_ones() {
        assert_eq!(
            normalize_net_error_text("unknown error: Error: NS_ERROR_ABORT"),
            "net::ERR_FAILED"
        );
        assert_eq!(
            normalize_net_error_text("no such frame: gone"),
            "no such frame: gone"
        );
    }

    /// The allowlist is checked here for every URL a caller loads, so no caller repeats the rule.
    #[tokio::test]
    async fn a_foreign_allowlist_refuses_urls_it_does_not_allow() {
        let (ff, _events, calls) = fake_session().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        *state.domain_filter.write().await =
            Some(crate::native::network::DomainFilter::new("example.com"));
        let error = load(
            &mut state,
            Load::Url {
                url: "https://evil.example/",
                wait: Wait::Complete,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            error,
            "Domain 'evil.example' is not in the allowed domains list"
        );
        assert!(calls.lock().unwrap().is_empty());
        for load in [
            Load::Url {
                url: "about:blank",
                wait: Wait::Complete,
            },
            Load::Reload,
            Load::History { delta: -1 },
        ] {
            assert!(super::load(&mut state, load).await.is_ok());
        }
    }

    #[test]
    fn wait_until_values_map_to_load_waits() {
        assert_eq!(
            Wait::from_wait_until(Some("domcontentloaded"), 0),
            Wait::Interactive
        );
        assert_eq!(Wait::from_wait_until(Some("none"), 0), Wait::None);
        assert_eq!(Wait::from_wait_until(None, 0), Wait::Complete);
        assert_eq!(Wait::from_wait_until(Some("load"), 0), Wait::Complete);
        assert_eq!(
            Wait::from_wait_until(Some("networkidle"), 500),
            Wait::NetworkIdle { timeout_ms: 500 }
        );
    }

    #[tokio::test]
    async fn a_load_drops_refs_and_reports_the_landed_url() {
        let (ff, _events, calls) = fake_session().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .firefox
            .as_mut()
            .unwrap()
            .refs
            .insert(("ctx-active".into(), "node".into(), "doc".into()), 7);
        state.active_frame_id = Some("frame".into());

        let landing = load(
            &mut state,
            Load::Url {
                url: "https://landed.example/page",
                wait: Wait::Complete,
            },
        )
        .await
        .unwrap();

        assert_eq!(landing.context, "ctx-active");
        assert_eq!(landing.url, "https://landed.example/page");
        assert!(!landing.prompt);
        assert!(neterror_message(&landing.url).is_none());
        assert!(state.active_frame_id.is_none());
        assert!(state.firefox.as_ref().unwrap().refs.is_empty());
        assert!(state
            .firefox
            .as_ref()
            .unwrap()
            .visited_origins
            .contains("https://landed.example"));
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls[0], "browsingContext.navigate");
    }

    /// A session timeout longer than the BiDi client's default must still decide when a load
    /// gives up, and the failure must name the load timeout rather than the client's own.
    #[tokio::test(start_paused = true)]
    async fn a_load_past_the_client_default_reports_the_load_timeout() {
        use crate::native::firefox::session::tests::FakeReply;
        let (ff, _events, _calls) =
            crate::native::firefox::session::tests::fake_session_with_replies(|cmd: &Value| {
                if cmd["method"] == "browsingContext.navigate" {
                    FakeReply::Silent
                } else {
                    FakeReply::Result(json!({}))
                }
            })
            .await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.default_timeout_ms = 60_000;

        let error = load(
            &mut state,
            Load::Url {
                url: "https://slow.example/",
                wait: Wait::Complete,
            },
        )
        .await
        .unwrap_err();

        assert_eq!(
            error,
            "Timed out after 60000ms waiting for load state 'load'"
        );
    }

    type Socket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn mock_history_socket() -> (DaemonState, Socket) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let client = super::super::bidi::BidiClient::connect(&url);
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap()
        };
        let (bidi, socket) = tokio::join!(client, server);
        let (mut ff, _events, _) = fake_session().await;
        ff.bidi = bidi.unwrap().into();
        let mut state = DaemonState::new();
        state.default_timeout_ms = 2_000;
        state.firefox = Some(ff);
        (state, socket)
    }

    async fn read_method(socket: &mut Socket, method: &str) -> Value {
        use futures_util::StreamExt;
        let message = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let request: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(request["method"], method);
        request
    }

    async fn send_packet(socket: &mut Socket, packet: Value) {
        use futures_util::SinkExt;
        use tokio_tungstenite::tungstenite::Message;
        socket
            .send(Message::Text(packet.to_string()))
            .await
            .unwrap();
    }

    async fn reply(socket: &mut Socket, method: &str, result: Value) -> Value {
        let request = read_method(socket, method).await;
        send_packet(
            socket,
            json!({ "type": "success", "id": request["id"], "result": result }),
        )
        .await;
        request
    }

    fn document(realm: &str, key: &str, url: &str, ready_state: &str) -> Value {
        json!({ "type": "success", "realm": realm, "result": {
            "type": "object", "value": [
                ["entryKey", { "type": "string", "value": key }],
                ["url", { "type": "string", "value": url }],
                ["readyState", { "type": "string", "value": ready_state }]
            ]
        } })
    }

    const START_URL: &str = "https://start.example/";
    const END_URL: &str = "https://end.example/";

    fn start_tree() -> Value {
        json!({ "contexts": [{ "context": "ctx-active", "url": START_URL }] })
    }

    #[tokio::test]
    async fn only_the_history_boundary_code_is_a_no_op() {
        for (delta, code) in [
            (-1, "no such history entry"),
            (1, "no such history entry"),
            (1, "unknown error"),
        ] {
            let (mut state, mut socket) = mock_history_socket().await;
            let browser = async {
                reply(
                    &mut socket,
                    "script.evaluate",
                    document("before", "a", START_URL, "complete"),
                )
                .await;
                let request = read_method(&mut socket, "browsingContext.traverseHistory").await;
                assert_eq!(
                    request["params"],
                    json!({ "context": "ctx-active", "delta": delta })
                );
                send_packet(&mut socket, json!({ "type": "error", "id": request["id"], "error": code, "message": "no such history entry" })).await;
                if code == "no such history entry" {
                    reply(&mut socket, "browsingContext.getTree", start_tree()).await;
                }
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(load(&mut state, Load::History { delta }), browser)
            })
            .await
            .unwrap();
            if code == "no such history entry" {
                let landing = result.unwrap();
                assert_eq!(landing.url, START_URL);
                assert!(!landing.prompt);
            } else {
                assert_eq!(
                    result.unwrap_err(),
                    "History navigation failed: unknown error: no such history entry"
                );
            }
        }
    }

    #[tokio::test]
    async fn history_acknowledgment_does_not_end_the_landing_or_prompt_wait() {
        for prompt in [false, true] {
            let (mut state, mut socket) = mock_history_socket().await;
            let browser = async {
                let snapshot = reply(
                    &mut socket,
                    "script.evaluate",
                    document("before", "a", START_URL, "complete"),
                )
                .await;
                assert_eq!(
                    snapshot["params"]["target"],
                    json!({ "context": "ctx-active", "sandbox": SANDBOX })
                );
                reply(&mut socket, "browsingContext.traverseHistory", json!({})).await;
                send_packet(&mut socket, json!({ "type": "event", "method": "browsingContext.load", "params": { "context": "other-tab", "url": "https://unrelated.example/" } })).await;
                reply(
                    &mut socket,
                    "script.evaluate",
                    document("before", "a", START_URL, "complete"),
                )
                .await;
                reply(
                    &mut socket,
                    "script.evaluate",
                    document("after", "b", END_URL, "interactive"),
                )
                .await;
                if prompt {
                    send_packet(&mut socket, json!({ "type": "event", "method": "browsingContext.userPromptOpened", "params": { "context": "ctx-active", "type": "confirm", "handler": "ignore" } })).await;
                    while let Some(message) = futures_util::StreamExt::next(&mut socket).await {
                        let request: Value =
                            serde_json::from_str(&message.unwrap().into_text().unwrap()).unwrap();
                        if request["method"] == "browsingContext.getTree" {
                            send_packet(&mut socket, json!({ "type": "success", "id": request["id"], "result": start_tree() })).await;
                            break;
                        }
                        assert_eq!(request["method"], "script.evaluate");
                    }
                } else {
                    reply(
                        &mut socket,
                        "script.evaluate",
                        document("after", "b", END_URL, "complete"),
                    )
                    .await;
                }
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(load(&mut state, Load::History { delta: -1 }), browser)
            })
            .await
            .unwrap();
            let landing = result.unwrap();
            assert_eq!(landing.prompt, prompt);
            assert_eq!(landing.url, if prompt { START_URL } else { END_URL });
        }
    }

    #[tokio::test]
    async fn reload_uses_the_wire_url_without_querying_the_context_tree() {
        let (mut state, mut socket) = mock_history_socket().await;
        let browser = async {
            reply(
                &mut socket,
                "browsingContext.reload",
                json!({"url": END_URL}),
            )
            .await;
        };
        let (landing, ()) = tokio::join!(load(&mut state, Load::Reload), browser);
        assert_eq!(landing.unwrap().url, END_URL);
    }
}
