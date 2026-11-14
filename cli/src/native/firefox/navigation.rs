//! Document loading for the active tab: the one place that navigates, races prompts,
//! resolves the landed URL and records the visit.

use serde_json::{json, Value};

use super::daemon;
use crate::native::actions::DaemonState;

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

impl Landing {
    /// The message Firefox shows when the tab landed on a network error page.
    pub fn failure(&self) -> Option<String> {
        neterror_message(&self.url)
    }
}

/// Loads the active tab, dropping refs and frame scope first, and records the origin it reached.
/// Fails with the load's name when Firefox rejects it.
pub async fn load(state: &mut DaemonState, load: Load<'_>) -> Result<Landing, String> {
    if let Load::Url { url, .. } = &load {
        state.ensure_navigation_allowed(url).await?;
    }
    let (method, params, failure) = match &load {
        Load::Url { url, wait } => (
            "browsingContext.navigate",
            json!({ "url": url, "wait": wait.as_bidi() }),
            "Navigation",
        ),
        Load::Reload => (
            "browsingContext.reload",
            json!({ "wait": "complete" }),
            "Reload",
        ),
        Load::History { delta } => (
            "browsingContext.traverseHistory",
            json!({ "delta": delta }),
            "History navigation",
        ),
    };
    let requested = match &load {
        Load::Url { url, .. } => (*url).to_string(),
        Load::Reload | Load::History { .. } => String::new(),
    };
    let idle_timeout = match &load {
        Load::Url { wait, .. } => wait.idle_timeout(),
        Load::Reload | Load::History { .. } => None,
    };
    daemon::reset_page(state);
    let ff = daemon::session(state)?;
    let context = ff.contexts.active_tab().to_string();
    let mut params = params;
    params["context"] = json!(context);
    let landed = ff
        .race_prompt(&context, ff.bidi.send(method, params))
        .await
        .map_err(|error| format!("{failure} failed: {}", normalize_net_error_text(&error)))?;
    let (url, prompt) = match landed {
        Some(result) => (landed_url(&result, ff.url().await, &requested), false),
        None => (ff.url().await.unwrap_or(requested), true),
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

/// The URL a BiDi load reports, falling back to the tab's own URL and then to the request.
fn landed_url(result: &Value, current: Result<String, String>, requested: &str) -> String {
    if let Some(url) = result["url"].as_str().filter(|url| !url.is_empty()) {
        return url.to_string();
    }
    current
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| requested.to_string())
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
            .insert(("ctx-active".into(), "node".into()), 7);
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
        assert!(landing.failure().is_none());
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

    #[tokio::test]
    async fn reload_and_history_report_the_url_of_the_tab() {
        let tree = json!({"contexts": [{"context": "ctx-active", "url": "https://tab.example/"}]});
        let (ff, _events, calls) =
            crate::native::firefox::session::tests::fake_session_with_trees(vec![tree]).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);

        let history = load(&mut state, Load::History { delta: -1 }).await.unwrap();
        assert_eq!(history.url, "https://tab.example/");
        assert!(!history.prompt);
        let reload = load(&mut state, Load::Reload).await.unwrap();
        assert_eq!(reload.url, "https://tab.example/");

        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls[0], "browsingContext.traverseHistory");
        assert_eq!(calls[1], "browsingContext.getTree");
        assert_eq!(calls[2], "browsingContext.reload");
        assert_eq!(calls[3], "browsingContext.getTree");
    }
}
