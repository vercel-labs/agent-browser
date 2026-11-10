//! Document loading for the active tab: the one place that navigates, races prompts,
//! resolves the landed URL and records the visit.

use serde_json::{json, Value};

use super::daemon;
use crate::native::actions::DaemonState;

/// How long a load should wait for the document before reporting back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Wait {
    None,
    Interactive,
    Complete,
}

impl Wait {
    /// Maps a command's `waitUntil` value to a load wait.
    pub fn from_wait_until(wait_until: Option<&str>) -> Result<Self, String> {
        match wait_until {
            Some("networkidle") => Err("networkidle is not supported with --engine firefox".into()),
            Some("domcontentloaded") => Ok(Wait::Interactive),
            Some("none") => Ok(Wait::None),
            _ => Ok(Wait::Complete),
        }
    }

    fn as_bidi(self) -> &'static str {
        match self {
            Wait::None => "none",
            Wait::Interactive => "interactive",
            Wait::Complete => "complete",
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
    if state.domain_filter.read().await.is_some() {
        return Err(
            "--allowed-domains is not supported with --engine firefox, so no page may load; \
             clear it with `--allowed-domains \"\"`"
                .to_string(),
        );
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
    daemon::reset_page(state);
    let ff = daemon::session(state)?;
    let context = ff.contexts.active_tab().to_string();
    let mut params = params;
    params["context"] = json!(context);
    let landed = ff
        .race_prompt(&context, ff.bidi.send(method, params))
        .await
        .map_err(|error| format!("{failure} failed: {error}"))?;
    let (url, prompt) = match landed {
        Some(result) => (landed_url(&result, ff.url().await, &requested), false),
        None => (ff.url().await.unwrap_or(requested), true),
    };
    ff.note_visit(&url);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::firefox::session::tests::fake_session;

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

    /// Firefox cannot enforce the allowlist, so an installed filter must stop every load rather
    /// than let a page through unfiltered.
    #[tokio::test]
    async fn a_foreign_allowlist_refuses_every_load() {
        let (ff, _events, calls) = fake_session().await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        *state.domain_filter.write().await =
            Some(crate::native::network::DomainFilter::new("example.com"));
        let loads = [
            Load::Url {
                url: "https://example.com/",
                wait: Wait::Complete,
            },
            Load::Reload,
            Load::History { delta: -1 },
        ];
        for load in loads {
            let error = super::load(&mut state, load).await.unwrap_err();
            assert!(error.contains("--allowed-domains"), "{error}");
        }
        assert!(calls.lock().unwrap().is_empty());
    }

    #[test]
    fn wait_until_values_map_to_load_waits() {
        assert_eq!(
            Wait::from_wait_until(Some("domcontentloaded")).unwrap(),
            Wait::Interactive
        );
        assert_eq!(Wait::from_wait_until(Some("none")).unwrap(), Wait::None);
        assert_eq!(Wait::from_wait_until(None).unwrap(), Wait::Complete);
        assert_eq!(Wait::from_wait_until(Some("load")).unwrap(), Wait::Complete);
        assert_eq!(
            Wait::from_wait_until(Some("networkidle")).unwrap_err(),
            "networkidle is not supported with --engine firefox"
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
