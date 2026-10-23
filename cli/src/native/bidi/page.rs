use std::time::Duration;

use serde_json::{json, Value};

use super::actions::session;
use super::forms::require;
use super::network;
use super::session::FirefoxSession;
use crate::native::actions::{route_url_matches as url_matches, DaemonState};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Turns an `about:neterror?e=...&d=...` URL into the message Firefox shows for the failure.
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
    Some(match pair("d") {
        Some(detail) => format!("{code}: {detail}"),
        None => code,
    })
}

/// Drops refs and frame state after a document-level navigation.
fn reset_page_state(state: &mut DaemonState) {
    state.ref_map.invalidate_all_documents();
    state.active_frame_id = None;
    if let Some(ff) = state.firefox.as_mut() {
        ff.refs.clear();
    }
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
    state.ref_map.invalidate_all_documents();
    state.active_frame_id = None;
    let ff = session(state)?;
    ff.refs.clear();
    let context = ff.context.clone();
    let nav = ff
        .race_prompt(
            &context,
            ff.bidi.send(
                "browsingContext.navigate",
                json!({"context": context, "url": url, "wait": wait}),
            ),
        )
        .await
        .map_err(|e| format!("Navigation failed: {e}"))?;
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
    let context = ff.context.clone();
    ff.race_prompt(
        &context,
        ff.bidi.send(
            "browsingContext.traverseHistory",
            json!({"context": context, "delta": delta}),
        ),
    )
    .await
    .map_err(|e| format!("History navigation failed: {e}"))?;
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
    let context = ff.context.clone();
    ff.race_prompt(
        &context,
        ff.bidi.send(
            "browsingContext.reload",
            json!({"context": context, "wait": "complete"}),
        ),
    )
    .await
    .map_err(|e| format!("Reload failed: {e}"))?;
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
        if url_matches(pattern, &ff.url().await?) {
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
    use crate::native::bidi::session::tests::fake_session;

    #[test]
    fn neterror_url_becomes_a_message() {
        let url = "about:neterror?e=dnsNotFound&u=https%3A//nope.invalid/&d=We%20can%27t%20connect";
        assert_eq!(
            neterror_message(url).unwrap(),
            "dnsNotFound: We can't connect"
        );
        assert_eq!(
            neterror_message("about:neterror?e=connectionFailure").unwrap(),
            "connectionFailure"
        );
        assert!(neterror_message("https://example.com").is_none());
    }

    #[test]
    fn url_globs_match_the_way_chrome_does() {
        assert!(url_matches("*", "https://example.com/any"));
        assert!(url_matches(
            "**/dashboard",
            "https://example.com/app/dashboard"
        ));
        assert!(url_matches(
            "https://example.com/*",
            "https://example.com/a/b?c=1"
        ));
        assert!(url_matches("example.com", "https://example.com/a"));
        assert!(url_matches("*dashboard", "https://example.com/dashboard"));
        assert!(!url_matches(
            "https://example.com/a",
            "https://example.com/b"
        ));
        assert!(!url_matches("https://one.test/*", "https://two.test/a"));
        assert!(!url_matches("**/dashboard", "https://example.com/settings"));
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
