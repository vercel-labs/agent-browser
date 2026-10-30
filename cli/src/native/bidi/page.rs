use std::time::Duration;

use serde_json::{json, Value};

use super::actions::session;
use super::forms::require;
use super::network;
use super::session::FirefoxSession;
use crate::native::actions::{route_url_matches, DaemonState};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Firefox net-error codes and `about:neterror` names, mapped to the Chromium code Chrome
/// reports for the same failure.
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

/// Turns an `about:neterror?e=...&d=...` URL into the failure message, using Chrome's
/// `net::ERR_*` text when the Firefox code has a known Chromium equivalent.
fn neterror_message(url: &str) -> Option<String> {
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

/// Drops refs and frame state after a document-level navigation.
fn reset_page_state(state: &mut DaemonState) {
    super::tabs::reset_refs(state);
    state.ref_map.invalidate_all_documents();
}

/// Navigates the active tab and returns `{url, title, targetId}` like `BrowserManager::navigate`.
/// `waitUntil` maps to the BiDi `wait` state.
pub async fn navigate(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let url = require(cmd, "url")?;
    let wait = match cmd.get("waitUntil").and_then(Value::as_str) {
        Some("domcontentloaded") => "interactive",
        Some("none") => "none",
        _ => "complete",
    };
    if let Some(filter) = state.domain_filter.read().await.as_ref() {
        filter.check_url(url)?;
    }
    network::set_origin_headers(cmd, url, state).await?;
    reset_page_state(state);
    let ff = session(state)?;
    let context = ff.top_context.clone();
    let nav = ff
        .race_prompt(
            &context,
            ff.bidi.send(
                "browsingContext.navigate",
                json!({"context": context, "url": url, "wait": wait}),
            ),
        )
        .await
        .map_err(|e| format!("Navigation failed: {}", normalize_net_error_text(&e)))?;
    let Some(nav) = nav else {
        let url = ff.url().await.unwrap_or_default();
        return Ok(json!({ "url": url, "targetId": context, "dialogOpened": true }));
    };
    let landed = nav["url"].as_str().unwrap_or(url);
    if let Some(msg) = neterror_message(landed) {
        return Err(format!("Navigation failed: {msg}"));
    }
    ff.note_visit(landed);
    let title = ff.eval("document.title").await.unwrap_or(Value::Null);
    Ok(json!({ "url": landed, "title": title, "targetId": context }))
}

/// Moves `delta` entries through the active tab's history and returns `{url}` like `BrowserManager`.
async fn traverse_history(delta: i64, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let context = ff.top_context.clone();
    ff.race_prompt(
        &context,
        ff.bidi.send(
            "browsingContext.traverseHistory",
            json!({"context": context, "delta": delta}),
        ),
    )
    .await
    .map_err(|e| {
        format!(
            "History navigation failed: {}",
            normalize_net_error_text(&e)
        )
    })?;
    reset_page_state(state);
    let url = session(state)?.url().await.unwrap_or_default();
    Ok(json!({ "url": url }))
}

/// History back. Must return what Chrome's `handle_back` returns.
pub async fn back(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    traverse_history(-1, state).await
}

/// History forward. Must return what Chrome's `handle_forward` returns.
pub async fn forward(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    traverse_history(1, state).await
}

/// Reload. Must return what Chrome's `handle_reload` returns.
pub async fn reload(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let context = ff.top_context.clone();
    ff.race_prompt(
        &context,
        ff.bidi.send(
            "browsingContext.reload",
            json!({"context": context, "wait": "complete"}),
        ),
    )
    .await
    .map_err(|e| format!("Reload failed: {}", normalize_net_error_text(&e)))?;
    reset_page_state(state);
    let url = session(state)?.url().await.unwrap_or_default();
    Ok(json!({ "url": url }))
}

/// Current URL as `{url}`.
pub async fn url(state: &mut DaemonState) -> Result<Value, String> {
    Ok(json!({ "url": session(state)?.url().await? }))
}

/// Document title as `{title}`.
pub async fn title(state: &mut DaemonState) -> Result<Value, String> {
    Ok(json!({ "title": session(state)?.eval("document.title").await? }))
}

/// Page HTML. Must return `{html, origin}` like Chrome's `handle_content`.
pub async fn content(state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    let html = ff.eval("document.documentElement.outerHTML").await?;
    let origin = ff.url().await.unwrap_or_default();
    Ok(json!({ "html": html.as_str().unwrap_or(""), "origin": origin }))
}

/// Evaluates `script` and returns `{result, origin}` like Chrome's `handle_evaluate`.
pub async fn evaluate(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let script = require(cmd, "script")?;
    let ff = session(state)?;
    let result = ff.eval(script).await?;
    let origin = ff.url().await.unwrap_or_default();
    Ok(json!({ "result": result, "origin": origin }))
}

/// The expression Chrome polls for a `--load` state, or `None` when the state is `none`.
fn load_state_check(load_state: &str) -> Option<&'static str> {
    match load_state {
        "none" => None,
        "domcontentloaded" => Some("document.readyState !== 'loading'"),
        _ => Some("document.readyState === 'complete'"),
    }
}

/// The expression Chrome polls for a selector in one of its `wait` states.
fn selector_check(selector: &str, state: &str) -> String {
    let sel = serde_json::to_string(selector).unwrap_or_default();
    match state {
        "attached" => format!("!!document.querySelector({sel})"),
        "detached" => format!("!document.querySelector({sel})"),
        "hidden" => format!(
            r#"(() => {{
                const el = document.querySelector({sel});
                if (!el) return true;
                const s = window.getComputedStyle(el);
                return s.display === 'none' || s.visibility === 'hidden' || parseFloat(s.opacity) === 0;
            }})()"#
        ),
        _ => format!(
            r#"(() => {{
                const el = document.querySelector({sel});
                if (!el) return false;
                const r = el.getBoundingClientRect();
                const s = window.getComputedStyle(el);
                return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none';
            }})()"#
        ),
    }
}

/// Polls `expression` in the active context until it is truthy or the timeout expires.
pub(crate) async fn poll_until_true(
    ff: &FirefoxSession,
    expression: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    poll_until_true_or_timeout(
        ff,
        expression,
        timeout_ms,
        &format!("Wait timed out after {timeout_ms}ms"),
    )
    .await
}

/// Polls `expression` until truthy, failing with `timeout_error` when the deadline passes.
async fn poll_until_true_or_timeout(
    ff: &FirefoxSession,
    expression: &str,
    timeout_ms: u64,
    timeout_error: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if ff.eval_lenient(expression).await?.as_bool() == Some(true) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(timeout_error.to_string());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Polls the active URL until it matches a glob, with Chrome's `route_url_matches` semantics.
async fn poll_url(ff: &FirefoxSession, pattern: &str, timeout_ms: u64) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if route_url_matches(pattern, &ff.url().await?) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("Wait timed out after {timeout_ms}ms"));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Waits for ms, selector, text, load state, function or URL. Must return the same
/// `{waited: ...}` shapes as Chrome's `handle_wait`, honouring `state.timeout_ms(cmd)`.
pub async fn wait(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let timeout_ms = cmd
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(state.default_timeout_ms);
    let ff = session(state)?;

    if let Some(text) = cmd.get("text").and_then(Value::as_str) {
        let check = format!(
            "(document.body.innerText || '').includes({})",
            serde_json::to_string(text).unwrap_or_default()
        );
        poll_until_true(ff, &check, timeout_ms).await?;
        return Ok(json!({ "waited": "text", "text": text }));
    }

    if let Some(selector) = cmd.get("selector").and_then(Value::as_str) {
        let state_str = cmd
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("visible");
        poll_until_true(ff, &selector_check(selector, state_str), timeout_ms).await?;
        return Ok(json!({ "waited": "selector", "selector": selector }));
    }

    if let Some(url_pattern) = cmd.get("url").and_then(Value::as_str) {
        poll_url(ff, url_pattern, timeout_ms).await?;
        return Ok(json!({ "waited": "url", "url": url_pattern }));
    }

    if let Some(fn_str) = cmd.get("function").and_then(Value::as_str) {
        poll_until_true(ff, &format!("!!({fn_str})"), timeout_ms).await?;
        return Ok(json!({ "waited": "function" }));
    }

    if let Some(load_state) = cmd.get("loadState").and_then(Value::as_str) {
        if load_state == "networkidle" {
            network::wait_idle(state, timeout_ms)
                .await
                .map_err(|_| format!("Wait timed out after {timeout_ms}ms"))?;
        } else if let Some(check) = load_state_check(load_state) {
            poll_until_true(ff, check, timeout_ms).await?;
        }
        return Ok(json!({ "waited": "load", "state": load_state }));
    }

    tokio::time::sleep(Duration::from_millis(timeout_ms)).await;
    Ok(json!({ "waited": "timeout", "ms": timeout_ms }))
}

/// Waits for the active URL to match `url` and returns `{url}` like Chrome's
/// `handle_waitforurl`.
pub async fn waitforurl(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let url_pattern = require(cmd, "url")?;
    let timeout_ms = cmd
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(state.default_timeout_ms);
    let ff = session(state)?;
    poll_url(ff, url_pattern, timeout_ms).await?;
    let url = ff.url().await.unwrap_or_default();
    Ok(json!({ "url": url }))
}

/// Waits for a load state and returns `{state}` like Chrome's
/// `handle_waitforloadstate`.
pub async fn waitforloadstate(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let load_state = cmd.get("state").and_then(Value::as_str).unwrap_or("load");
    let timeout_ms = cmd
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(state.default_timeout_ms);
    if load_state == "networkidle" {
        network::wait_idle(state, timeout_ms)
            .await
            .map_err(|_| "Timeout waiting for networkidle".to_string())?;
    } else if let Some(check) = load_state_check(load_state) {
        let ff = session(state)?;
        poll_until_true_or_timeout(
            ff,
            check,
            timeout_ms,
            &format!("Timeout waiting for load state: {load_state}"),
        )
        .await?;
    }
    Ok(json!({ "state": load_state }))
}

/// Waits for `expression` to be truthy and returns `{result}` like Chrome's
/// `handle_waitforfunction`.
pub async fn waitforfunction(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let expression = require(cmd, "expression")?;
    let timeout_ms = cmd
        .get("timeout")
        .and_then(Value::as_u64)
        .unwrap_or(state.default_timeout_ms);
    let ff = session(state)?;
    poll_until_true(ff, &format!("!!({expression})"), timeout_ms).await?;
    let result = ff.eval(&format!("({expression})")).await?;
    Ok(json!({ "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::bidi::session::tests::{
        fake_session, fake_session_with, fake_session_with_replies, FakeReply,
    };

    #[test]
    fn neterror_codes_map_to_chromium_net_errors() {
        let cases = [
            (
                "about:neterror?d=Error%3A%20NS_ERROR_ABORT",
                "net::ERR_FAILED",
            ),
            (
                "about:neterror?e=NS_ERROR_UNKNOWN_HOST",
                "net::ERR_NAME_NOT_RESOLVED",
            ),
            (
                "about:neterror?e=NS_ERROR_CONNECTION_REFUSED",
                "net::ERR_CONNECTION_REFUSED",
            ),
            (
                "about:neterror?e=NS_ERROR_NET_TIMEOUT",
                "net::ERR_TIMED_OUT",
            ),
            (
                "about:neterror?d=Error%3A%20NS_ERROR_NET_TIMEOUT_EXTERNAL",
                "net::ERR_TIMED_OUT",
            ),
            (
                "about:neterror?e=NS_ERROR_OFFLINE",
                "net::ERR_INTERNET_DISCONNECTED",
            ),
            (
                "about:neterror?d=Error%3A%20NS_ERROR_UNKNOWN_PROXY_HOST",
                "net::ERR_PROXY_CONNECTION_FAILED",
            ),
            (
                "about:neterror?e=dnsNotFound&u=https%3A//nope.invalid/&d=We%20can%27t%20connect",
                "net::ERR_NAME_NOT_RESOLVED",
            ),
            (
                "about:neterror?e=connectionFailure",
                "net::ERR_CONNECTION_REFUSED",
            ),
        ];
        for (url, expected) in cases {
            assert_eq!(neterror_message(url).unwrap(), expected, "{url}");
        }
    }

    #[test]
    fn unknown_neterror_codes_keep_the_firefox_text() {
        assert_eq!(
            neterror_message("about:neterror?e=corruptedContentErrorv2&d=Bad%20content").unwrap(),
            "corruptedContentErrorv2: Bad content"
        );
        assert_eq!(
            neterror_message("about:neterror?d=Error%3A%20NS_ERROR_NOT_IMPLEMENTED").unwrap(),
            "unknown error: Error: NS_ERROR_NOT_IMPLEMENTED"
        );
        assert!(neterror_message("https://example.com").is_none());
    }

    #[tokio::test]
    async fn navigate_rejects_about_blank_under_an_allowlist_like_chrome() {
        let mut state = DaemonState::new();
        *state.domain_filter.write().await =
            Some(crate::native::network::DomainFilter::new("allowed.example"));
        let error = navigate(&json!({ "url": "about:blank" }), &mut state)
            .await
            .unwrap_err();
        assert_eq!(error, "No hostname in URL: about:blank");
    }

    #[tokio::test]
    async fn aborted_navigation_reports_chromes_net_error() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session_with(|cmd: &Value| {
            if cmd["method"] == "browsingContext.navigate" {
                json!({"navigation": null, "url": "about:neterror?d=Error%3A%20NS_ERROR_ABORT"})
            } else {
                json!({})
            }
        })
        .await;
        state.firefox = Some(ff);
        let error = navigate(&json!({"url": "http://blocked.test/"}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(error, "Navigation failed: net::ERR_FAILED");
    }

    #[tokio::test]
    async fn aborted_navigation_bidi_error_reports_chromes_net_error() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session_with_replies(|cmd: &Value| {
            if cmd["method"] == "browsingContext.navigate" {
                FakeReply::Error {
                    error: "unknown error",
                    message: "Error: NS_ERROR_ABORT",
                }
            } else {
                FakeReply::Result(json!({}))
            }
        })
        .await;
        state.firefox = Some(ff);
        let error = navigate(
            &json!({"url": "http://blocked.test/blocked.html"}),
            &mut state,
        )
        .await
        .unwrap_err();
        assert_eq!(error, "Navigation failed: net::ERR_FAILED");
    }

    #[test]
    fn error_text_maps_gecko_codes_and_keeps_unknown_ones() {
        assert_eq!(
            normalize_net_error_text("unknown error: Error: NS_ERROR_ABORT"),
            "net::ERR_FAILED"
        );
        assert_eq!(
            normalize_net_error_text("unknown error: Error: NS_ERROR_UNKNOWN_HOST"),
            "net::ERR_NAME_NOT_RESOLVED"
        );
        assert_eq!(
            normalize_net_error_text("unknown error: Error: dnsNotFound"),
            "net::ERR_NAME_NOT_RESOLVED"
        );
        assert_eq!(
            normalize_net_error_text("unknown error: Error: NS_ERROR_NOT_IMPLEMENTED"),
            "unknown error: Error: NS_ERROR_NOT_IMPLEMENTED"
        );
        assert_eq!(
            normalize_net_error_text("no such frame: gone"),
            "no such frame: gone"
        );
    }

    #[tokio::test]
    async fn bare_wait_sleeps_and_returns_the_timeout_shape() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let result = wait(&json!({"timeout": 5}), &mut state).await.unwrap();
        assert_eq!(result, json!({ "waited": "timeout", "ms": 5 }));
    }

    #[tokio::test]
    async fn text_wait_polls_then_reports_the_timeout() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let err = wait(&json!({"text": "Missing", "timeout": 50}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(err, "Wait timed out after 50ms");
    }

    #[tokio::test]
    async fn waitforurl_requires_a_url() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        let err = waitforurl(&json!({}), &mut state).await.unwrap_err();
        assert_eq!(err, "Missing 'url' parameter");
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn waitforloadstate_defaults_to_load_and_reports_chrome_timeout_wording() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let err = waitforloadstate(&json!({"timeout": 30}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(err, "Timeout waiting for load state: load");
    }

    #[tokio::test]
    async fn waitforfunction_requires_an_expression() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let err = waitforfunction(&json!({}), &mut state).await.unwrap_err();
        assert_eq!(err, "Missing 'expression' parameter");
    }
}
