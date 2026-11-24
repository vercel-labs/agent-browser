//! Page-level Firefox commands: navigation, history, reading page state, evaluate and wait.

use std::time::Duration;

use serde_json::{json, Value};

use super::daemon::session;
use super::navigation::{self, Load, Wait};
use super::session::FirefoxSession;
use crate::native::actions::{
    eval_data, route_url_matches, selector_state_expression, DaemonState,
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Navigates the active tab and returns `{url, title, targetId}` like `BrowserManager::navigate`.
/// `waitUntil` maps to the load wait.
pub async fn navigate(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let url = cmd
        .get("url")
        .and_then(Value::as_str)
        .ok_or("Missing 'url' parameter")?;
    if cmd
        .get("headers")
        .and_then(Value::as_object)
        .is_some_and(|headers| !headers.is_empty())
    {
        return Err("--headers is not supported with --engine firefox".into());
    }
    let wait = Wait::from_wait_until(cmd.get("waitUntil").and_then(Value::as_str))?;
    let landing = navigation::load(state, Load::Url { url, wait }).await?;
    if landing.prompt {
        return Ok(json!({
            "url": landing.url,
            "targetId": landing.context,
            "dialogOpened": true,
        }));
    }
    if let Some(message) = navigation::neterror_message(&landing.url) {
        return Err(format!("Navigation failed: {message}"));
    }
    let title = session(state)?
        .eval("document.title")
        .await
        .unwrap_or(Value::Null);
    Ok(json!({
        "url": landing.url,
        "title": title,
        "targetId": landing.context,
    }))
}

/// The `{url}` answer a load reports, flagging a prompt that stopped it the way `navigate` does.
fn landing_json(landing: navigation::Landing) -> Value {
    let mut result = json!({ "url": landing.url });
    if landing.prompt {
        result["dialogOpened"] = json!(true);
    }
    result
}

/// Moves `delta` entries through the active tab's history and returns `{url}` like `BrowserManager`.
async fn traverse_history(delta: i64, state: &mut DaemonState) -> Result<Value, String> {
    let landing = navigation::load(state, Load::History { delta }).await?;
    Ok(landing_json(landing))
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
    let landing = navigation::load(state, Load::Reload).await?;
    Ok(landing_json(landing))
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
    let script = cmd
        .get("script")
        .and_then(Value::as_str)
        .ok_or("Missing 'script' parameter")?;
    let ff = session(state)?;
    let (result, unserializable) = ff.eval_result(script, false).await?;
    let origin = ff.url().await.unwrap_or_default();
    Ok(eval_data(result, origin, unserializable))
}

/// The expression Chrome polls for a `--load` state, or `None` when the state is `none`.
fn load_state_check(load_state: &str) -> Result<Option<&'static str>, String> {
    Ok(match load_state {
        "networkidle" => return Err("networkidle is not supported with --engine firefox".into()),
        "none" => None,
        "domcontentloaded" => Some("document.readyState !== 'loading'"),
        _ => Some("document.readyState === 'complete'"),
    })
}

/// Polls `check` until it is true, failing with `timeout_error` when the deadline passes.
async fn poll_until<F, Fut>(
    timeout_ms: u64,
    timeout_error: &str,
    mut check: F,
) -> Result<(), String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if check().await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(timeout_error.to_string());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Polls `expression` until truthy, failing with `timeout_error` when the deadline passes.
async fn poll_expression(
    ff: &FirefoxSession,
    expression: &str,
    timeout_ms: u64,
    timeout_error: &str,
) -> Result<(), String> {
    poll_until(timeout_ms, timeout_error, || async {
        Ok(ff.eval_lenient(expression).await?.as_bool() == Some(true))
    })
    .await
}

/// Polls `expression` in the active context until it is truthy or the timeout expires.
pub(super) async fn poll_until_true(
    ff: &FirefoxSession,
    expression: &str,
    timeout_ms: u64,
) -> Result<(), String> {
    let error = format!("Wait timed out after {timeout_ms}ms");
    poll_expression(ff, expression, timeout_ms, &error).await
}

/// Polls the active URL until it matches a glob, with Chrome's `route_url_matches` semantics.
async fn poll_url(ff: &FirefoxSession, pattern: &str, timeout_ms: u64) -> Result<(), String> {
    let error = format!("Wait timed out after {timeout_ms}ms");
    poll_until(timeout_ms, &error, || async {
        Ok(route_url_matches(pattern, &ff.url().await?))
    })
    .await
}

/// Waits for ms, selector, text, load state, function or URL. Must return the same
/// `{waited: ...}` shapes as Chrome's `handle_wait`, honouring `state.timeout_ms(cmd)`.
pub async fn wait(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let timeout_ms = state.timeout_ms(cmd);
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
        poll_until_true(
            ff,
            &selector_state_expression(selector, state_str),
            timeout_ms,
        )
        .await?;
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
        if let Some(check) = load_state_check(load_state)? {
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
    let url_pattern = cmd
        .get("url")
        .and_then(Value::as_str)
        .ok_or("Missing 'url' parameter")?;
    let timeout_ms = state.timeout_ms(cmd);
    let ff = session(state)?;
    poll_url(ff, url_pattern, timeout_ms).await?;
    let url = ff.url().await.unwrap_or_default();
    Ok(json!({ "url": url }))
}

/// Waits for a load state and returns `{state}` like Chrome's
/// `handle_waitforloadstate`.
pub async fn waitforloadstate(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let load_state = cmd.get("state").and_then(Value::as_str).unwrap_or("load");
    let timeout_ms = state.timeout_ms(cmd);
    if let Some(check) = load_state_check(load_state)? {
        let ff = session(state)?;
        poll_expression(
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
    let expression = cmd
        .get("expression")
        .and_then(Value::as_str)
        .ok_or("Missing 'expression' parameter")?;
    let timeout_ms = state.timeout_ms(cmd);
    let ff = session(state)?;
    poll_until_true(ff, &format!("!!({expression})"), timeout_ms).await?;
    let result = ff.eval(&format!("({expression})")).await?;
    Ok(json!({ "result": result }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::firefox::session::tests::fake_session;

    #[tokio::test]
    async fn bare_wait_sleeps_and_returns_the_timeout_shape() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let result = wait(&json!({"timeout": 5}), &mut state).await.unwrap();
        assert_eq!(result, json!({ "waited": "timeout", "ms": 5 }));
    }

    #[tokio::test]
    async fn wait_variants_poll_then_report_the_timeout() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        let err = wait(&json!({"text": "Missing", "timeout": 50}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(err, "Wait timed out after 50ms");
        let err = wait(
            &json!({"selector": "#missing", "state": "hidden", "timeout": 50}),
            &mut state,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Wait timed out after 50ms");
        let call_count = calls.lock().unwrap().len();
        let err = waitforfunction(&json!({"expression": "true", "timeout": 30}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(err, "Wait timed out after 30ms");
        assert!(calls.lock().unwrap()[call_count..]
            .iter()
            .any(|call| call == "script.evaluate"));
    }

    #[tokio::test]
    async fn url_wait_resolves_when_the_current_url_already_matches() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let result = wait(&json!({"url": "*", "timeout": 50}), &mut state)
            .await
            .unwrap();
        assert_eq!(result, json!({ "waited": "url", "url": "*" }));
    }

    #[tokio::test]
    async fn load_state_none_returns_without_polling() {
        let mut state = DaemonState::new();
        let (ff, _tx, _) = fake_session().await;
        state.firefox = Some(ff);
        let result = wait(&json!({"loadState": "none", "timeout": 50}), &mut state)
            .await
            .unwrap();
        assert_eq!(result, json!({ "waited": "load", "state": "none" }));
    }

    #[tokio::test]
    async fn waitforurl_resolves_when_the_current_url_already_matches() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        let result = waitforurl(&json!({"url": "*", "timeout": 50}), &mut state)
            .await
            .unwrap();
        assert_eq!(result, json!({ "url": "" }));
        assert_eq!(
            calls.lock().unwrap().clone(),
            vec!["browsingContext.getTree", "browsingContext.getTree"]
        );
    }

    #[tokio::test]
    async fn waitforloadstate_none_returns_without_touching_the_page() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        let result = waitforloadstate(&json!({"state": "none", "timeout": 50}), &mut state)
            .await
            .unwrap();
        assert_eq!(result, json!({ "state": "none" }));
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
    async fn networkidle_is_rejected_for_navigation_and_waits() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        let error = "networkidle is not supported with --engine firefox";
        assert_eq!(
            navigate(
                &json!({"url": "https://example.com", "waitUntil": "networkidle"}),
                &mut state
            )
            .await
            .unwrap_err(),
            error
        );
        assert_eq!(
            wait(&json!({"loadState": "networkidle"}), &mut state)
                .await
                .unwrap_err(),
            error
        );
        assert_eq!(
            waitforloadstate(&json!({"state": "networkidle"}), &mut state)
                .await
                .unwrap_err(),
            error
        );
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn navigation_headers_are_rejected_before_navigation() {
        let mut state = DaemonState::new();
        let (ff, _tx, calls) = fake_session().await;
        state.firefox = Some(ff);
        let err = navigate(
            &json!({
                "url": "https://example.com",
                "headers": {"Authorization": "Bearer token"}
            }),
            &mut state,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "--headers is not supported with --engine firefox");
        assert!(calls.lock().unwrap().is_empty());
    }
}
