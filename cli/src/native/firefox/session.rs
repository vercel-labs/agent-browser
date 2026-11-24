//! A running Firefox: the owned process, both protocol clients and per-session state.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::Notify;

use super::bidi::{remote_to_json, unserializable, BidiClient, Events};
use super::contexts::{Contexts, Removed};
use super::process::{check_firefox_version, find_firefox, FirefoxProcess};
use crate::native::actions::PendingDialog;
use crate::native::cdp::chrome::LaunchOptions;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const SUBSCRIBED_EVENTS: [&str; 8] = [
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
    "browsingContext.contextCreated",
    "browsingContext.contextDestroyed",
    "browsingContext.domContentLoaded",
    "browsingContext.navigationStarted",
    "browsingContext.load",
    "browsingContext.fragmentNavigated",
];

/** One launched Firefox. Dropping it kills the process tree and deletes the profile. */
pub struct FirefoxSession {
    process: Option<FirefoxProcess>,
    pub headless: bool,
    pub bidi: Arc<BidiClient>,
    pub auto_dialog: Arc<AtomicBool>,
    pub auto_dialog_changed: Arc<Notify>,
    pub contexts: Contexts,
    /// Durable ref ids keyed by `(context, sharedId)`; reset when a document changes.
    pub refs: HashMap<(String, String), i64>,
    pub visited_origins: HashSet<String>,
    events: Events,
}

/// Builds the BiDi `proxy` capability from a `--proxy` value such as `http://host:3128`.
/// The capability cannot ask for TLS to the proxy, so a scheme it cannot honour is rejected.
pub(super) fn proxy_capability(proxy: &str, bypass: Option<&str>) -> Result<Value, String> {
    let (scheme, hostport) = proxy.split_once("://").unwrap_or(("http", proxy));
    let mut cap = match scheme.to_ascii_lowercase().as_str() {
        "http" => json!({"proxyType": "manual", "httpProxy": hostport, "sslProxy": hostport}),
        "socks5" | "socks5h" | "socks" => {
            json!({"proxyType": "manual", "socksProxy": hostport, "socksVersion": 5})
        }
        "socks4" => json!({"proxyType": "manual", "socksProxy": hostport, "socksVersion": 4}),
        other => {
            return Err(format!(
                "{other}:// proxy is not supported with --engine firefox; use http://, socks4://, or socks5://"
            ))
        }
    };
    let no_proxy: Vec<&str> = bypass
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !no_proxy.is_empty() {
        cap["noProxy"] = json!(no_proxy);
    }
    Ok(cap)
}

impl FirefoxSession {
    /// Launches Firefox and opens a BiDi session.
    /// The daemon handles automatic prompts so the setting can change without relaunching Firefox.
    pub async fn launch(options: &LaunchOptions, auto_dialog: bool) -> Result<Self, String> {
        let path = find_firefox(options.executable_path.as_deref())?;
        let (headless, download) = (options.headless, options.download_path.clone());
        let process = tokio::task::spawn_blocking(move || {
            FirefoxProcess::launch(&path, headless, download.as_deref(), START_TIMEOUT)
        })
        .await
        .map_err(|e| format!("Firefox launch task failed: {e}"))??;

        let bidi = tokio::time::timeout(
            START_TIMEOUT,
            BidiClient::connect(&format!("{}/session", process.bidi_url)),
        )
        .await
        .map_err(|_| "Firefox BiDi connection timed out".to_string())??;
        let bidi = Arc::new(bidi);
        let events = bidi.subscribe_matching(lifecycle_event);
        let mut caps = json!({
            "acceptInsecureCerts": options.ignore_https_errors,
            "unhandledPromptBehavior": {"default": "ignore"},
        });
        if let Some(proxy) = options.proxy.as_deref() {
            caps["proxy"] = proxy_capability(proxy, options.proxy_bypass.as_deref())?;
        }
        let created = bidi
            .send(
                "session.new",
                json!({"capabilities": {"alwaysMatch": caps}}),
            )
            .await?;
        check_firefox_version(
            created["capabilities"]["browserVersion"]
                .as_str()
                .unwrap_or(""),
        )?;
        bidi.send("session.subscribe", json!({"events": SUBSCRIBED_EVENTS}))
            .await?;

        let tree = bidi.send("browsingContext.getTree", json!({})).await?;
        let mut session = Self {
            process: Some(process),
            headless: options.headless,
            bidi,
            auto_dialog: Arc::new(AtomicBool::new(auto_dialog)),
            auto_dialog_changed: Arc::new(Notify::new()),
            contexts: Contexts::from_tree(&tree)?,
            refs: HashMap::new(),
            visited_origins: HashSet::new(),
            events,
        };
        if let Some(contexts) = tree["contexts"].as_array() {
            session.record_context_origins(contexts);
        }
        if let Some(viewport) = options.viewport_size {
            session.set_viewport(viewport).await?;
        }
        Ok(session)
    }

    /// Sizes the viewport of the active context.
    pub async fn set_viewport(&self, (width, height): (u32, u32)) -> Result<(), String> {
        self.bidi.send(
            "browsingContext.setViewport",
            json!({"context": self.contexts.active(), "viewport": {"width": width, "height": height}, "devicePixelRatio": 1}),
        ).await?;
        Ok(())
    }

    fn record_context_origins(&mut self, contexts: &[Value]) {
        for context in contexts {
            if let Some(url) = context["url"].as_str() {
                self.note_visit(url);
            }
            if let Some(children) = context["children"].as_array() {
                self.record_context_origins(children);
            }
        }
    }

    pub fn note_visit(&mut self, url: &str) {
        if let Ok(parsed) = url::Url::parse(url) {
            let origin = parsed.origin().ascii_serialization();
            if origin != "null" {
                self.visited_origins.insert(origin);
            }
        }
    }

    /// The BiDi storage partition of the active context's tab, so a storage command only touches
    /// that tab's cookie jar.
    pub fn active_partition(&self) -> Value {
        self.contexts.partition(self.contexts.active())
    }

    /// Reads the context tree when no context is active, which happens when the event that
    /// announced the next tab was lost. Returns true when a tab became active.
    pub async fn recover_active_context(&mut self) -> Result<bool, String> {
        if !self.contexts.active().is_empty() || self.contexts.pinned_gone().is_some() {
            return Ok(false);
        }
        let tree = self.bidi.send("browsingContext.getTree", json!({})).await?;
        self.contexts.sync(&tree);
        Ok(!self.contexts.active().is_empty())
    }

    /// Removes the user contexts this session created once their last tab is gone.
    pub async fn release_unused_user_contexts(&mut self) {
        for user_context in self.contexts.take_unused_user_contexts() {
            let _ = self
                .bidi
                .send(
                    "browser.removeUserContext",
                    json!({"userContext": user_context}),
                )
                .await;
        }
    }

    /// True once the Firefox process has exited or its BiDi connection has closed.
    pub fn has_exited(&mut self) -> bool {
        self.process
            .as_mut()
            .is_some_and(FirefoxProcess::has_exited)
            || self.bidi.reader.is_finished()
    }

    /// Ends the BiDi session, then kills Firefox and deletes its profile.
    pub async fn close(self) {
        let _ = self
            .bidi
            .send_timeout("session.end", json!({}), SHUTDOWN_TIMEOUT)
            .await;
    }

    /// Applies buffered BiDi events and returns whether the active context changed.
    pub fn drain_events(&mut self, dialog: &mut Option<PendingDialog>) -> bool {
        let mut active_changed = false;
        while let Ok(event) = self.events.try_recv() {
            let params = &event["params"];
            match event["method"].as_str() {
                Some("browsingContext.contextCreated") => {
                    active_changed |= self.contexts.record(params);
                    if let Some(url) = params["url"].as_str() {
                        self.note_visit(url);
                    }
                }
                Some("browsingContext.domContentLoaded") => {
                    if let (Some(context), Some(url)) =
                        (params["context"].as_str(), params["url"].as_str())
                    {
                        self.contexts.set_url(context, url);
                    }
                    if let Some(url) = params["url"].as_str() {
                        self.note_visit(url);
                    }
                }
                Some("browsingContext.contextDestroyed") => {
                    if let Some(context) = params["context"].as_str() {
                        active_changed |= self.forget_context(context, dialog);
                    }
                }
                _ => {}
            }
            apply_event(&event, dialog, self.auto_dialog.load(Ordering::Acquire));
        }
        if self.events.has_lost_events() {
            self.events = self.bidi.subscribe_matching(lifecycle_event);
        }
        active_changed
    }

    /// Forgets a browsing context that is gone and everything nested in it: element refs and
    /// any prompt they owned. Returns true when the active context was among them, which the
    /// registry then replaces.
    pub fn forget_context(&mut self, context: &str, dialog: &mut Option<PendingDialog>) -> bool {
        let removed = self.contexts.remove(context);
        self.forget_removed(&removed, dialog)
    }

    /// Drops refs and any prompt owned by contexts that are gone.
    fn forget_removed(&mut self, removed: &Removed, dialog: &mut Option<PendingDialog>) -> bool {
        self.refs
            .retain(|(id, _), _| !removed.contexts.contains(id));
        if dialog
            .as_ref()
            .and_then(|d| d.session_id.as_ref())
            .is_some_and(|owner| removed.contexts.contains(owner))
        {
            *dialog = None;
        }
        if removed.active_changed {
            self.refs.clear();
        }
        removed.active_changed
    }

    /// Re-reads the context tree after lost events so refs and prompts follow what is still there.
    /// Returns true whenever events were lost, which makes the caller reset page state.
    pub async fn reconcile_event_loss(
        &mut self,
        dialog: &mut Option<PendingDialog>,
    ) -> Result<bool, String> {
        if !self.bidi.take_event_loss() {
            return Ok(false);
        }
        if self.events.has_lost_events() {
            self.events = self.bidi.subscribe_matching(lifecycle_event);
        }
        let active = self.contexts.active().to_string();
        let tree = self.bidi.send("browsingContext.getTree", json!({})).await?;
        if let Some(contexts) = tree["contexts"].as_array() {
            self.record_context_origins(contexts);
        }
        let removed = self.contexts.sync(&tree);
        if self.contexts.tab_of(&active).is_some() {
            self.contexts.activate(&active);
        }
        self.forget_removed(&removed, dialog);
        Ok(true)
    }

    /// Accepts the prompts the daemon answers itself, following `auto_dialog` as it changes.
    pub fn spawn_auto_dismiss(&self) -> tokio::task::JoinHandle<()> {
        let client = Arc::clone(&self.bidi);
        let mut events = client.subscribe_matching(prompt_lifecycle_event);
        let enabled = Arc::clone(&self.auto_dialog);
        let changed = Arc::clone(&self.auto_dialog_changed);
        tokio::spawn(async move {
            let mut pending = HashSet::<String>::new();
            let track = |event: &Value, pending: &mut HashSet<String>| {
                let params = &event["params"];
                let Some(context) = params["context"].as_str() else {
                    return;
                };
                match event["method"].as_str() {
                    Some("browsingContext.userPromptOpened") => {
                        pending.remove(context);
                        if params["handler"] != "accept" && auto_handles_prompt(params, true) {
                            pending.insert(context.to_string());
                        }
                    }
                    Some("browsingContext.userPromptClosed") => {
                        pending.remove(context);
                    }
                    _ => {}
                }
            };
            loop {
                let received = tokio::select! {
                    biased;
                    event = events.recv() => Some(event),
                    _ = changed.notified() => None,
                };
                match received {
                    Some(Some(event)) => track(&event, &mut pending),
                    Some(None) if events.has_lost_events() => {
                        pending.clear();
                        events = client.subscribe_matching(prompt_lifecycle_event);
                        continue;
                    }
                    Some(None) => break,
                    None => {}
                }
                while let Ok(event) = events.try_recv() {
                    track(&event, &mut pending);
                }
                if enabled.load(Ordering::Acquire) {
                    if let Some(context) = pending.iter().next().cloned() {
                        pending.remove(&context);
                        if let Err(error) = client
                            .send(
                                "browsingContext.handleUserPrompt",
                                json!({"context": context, "accept": true}),
                            )
                            .await
                        {
                            eprintln!("[auto-dismiss] failed to accept Firefox dialog: {error}");
                        }
                        if !pending.is_empty() {
                            changed.notify_one();
                        }
                    }
                }
            }
        })
    }

    /// Runs `fut` until it finishes or a prompt opens on the active context. Returns `None` for the prompt.
    pub async fn race_prompt<T>(
        &self,
        fut: impl Future<Output = Result<T, String>>,
    ) -> Result<Option<T>, String> {
        let mut events = self.bidi.subscribe_matching(prompt_opened_event);
        tokio::pin!(fut);
        loop {
            let event = tokio::select! {
                r = &mut fut => return r.map(Some),
                event = events.recv() => event,
            };
            if events.has_lost_events() {
                return Err("Firefox event queue overflowed while waiting for a dialog".into());
            }
            match event {
                Some(event)
                    if !auto_handles_prompt(
                        &event["params"],
                        self.auto_dialog.load(Ordering::Acquire),
                    ) && event["params"]["context"] == self.contexts.active() =>
                {
                    return Ok(None);
                }
                Some(_) => {}
                None => {
                    return Err("Firefox BiDi connection closed while waiting for a dialog".into());
                }
            }
        }
    }

    /// Evaluates `expression` in the active context and returns the decoded value.
    pub async fn eval(&self, expression: &str) -> Result<Value, String> {
        self.eval_result(expression, false).await.map(|r| r.0)
    }

    /// Like `eval`, but a script exception yields `null` so callers can keep polling.
    pub async fn eval_lenient(&self, expression: &str) -> Result<Value, String> {
        self.eval_result(expression, true).await.map(|r| r.0)
    }

    /// Evaluates `expression`, also returning the JavaScript token JSON cannot carry.
    pub async fn eval_result(
        &self,
        expression: &str,
        lenient: bool,
    ) -> Result<(Value, Option<String>), String> {
        let r = self
            .bidi
            .send(
                "script.evaluate",
                json!({
                    "expression": expression,
                    "target": {"context": self.contexts.active()},
                    "awaitPromise": true,
                    "resultOwnership": "none",
                }),
            )
            .await?;
        if r["type"] == "exception" {
            if lenient {
                return Ok((Value::Null, None));
            }
            let d = &r["exceptionDetails"];
            let msg = d["exception"]["value"]
                .as_str()
                .or(d["text"].as_str())
                .unwrap_or("script exception");
            return Err(format!("Evaluation error: {msg}"));
        }
        Ok((remote_to_json(&r["result"]), unserializable(&r["result"])))
    }

    /// The tab that holds the active context.
    pub fn active_tab(&self) -> &str {
        self.contexts.active_tab()
    }

    /// Enables or disables strict tab pinning on this Firefox session.
    pub fn set_pin(&mut self, pin: bool) {
        self.contexts.set_pin(pin);
    }

    /// URL of `context`, read from the browsing context tree.
    pub async fn url_in_context(&self, context: &str) -> Result<String, String> {
        let tree = self
            .bidi
            .send("browsingContext.getTree", json!({"root": context}))
            .await?;
        Ok(tree["contexts"][0]["url"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    /// URL of the active tab.
    pub async fn url(&self) -> Result<String, String> {
        self.url_in_context(self.active_tab()).await
    }
}

/// The lifecycle events `drain_events` applies, so unrelated floods cannot crowd them out.
fn lifecycle_event(event: &Value) -> bool {
    matches!(
        event["method"].as_str(),
        Some(
            "browsingContext.userPromptOpened"
                | "browsingContext.userPromptClosed"
                | "browsingContext.contextCreated"
                | "browsingContext.contextDestroyed"
                | "browsingContext.domContentLoaded"
        )
    )
}

/// The prompt events the auto-dismiss task answers.
fn prompt_lifecycle_event(event: &Value) -> bool {
    matches!(
        event["method"].as_str(),
        Some("browsingContext.userPromptOpened" | "browsingContext.userPromptClosed")
    )
}

/// The event that makes a waiting command return so it can report the dialog.
fn prompt_opened_event(event: &Value) -> bool {
    event["method"] == "browsingContext.userPromptOpened"
}

/// Whether Firefox already accepted a prompt or the daemon will automatically accept it.
pub(crate) fn auto_handles_prompt(params: &Value, auto_dialog: bool) -> bool {
    params["handler"] == "accept"
        || (auto_dialog
            && matches!(
                params["type"].as_str(),
                Some("alert" | "beforeunload" | "beforeUnload")
            ))
}

fn apply_event(e: &Value, dialog: &mut Option<PendingDialog>, auto_dialog: bool) {
    let p = &e["params"];
    match e["method"].as_str() {
        Some("browsingContext.userPromptOpened") if !auto_handles_prompt(p, auto_dialog) => {
            *dialog = Some(PendingDialog {
                dialog_type: p["type"].as_str().unwrap_or("alert").to_string(),
                message: p["message"].as_str().unwrap_or("").to_string(),
                url: String::new(),
                default_prompt: p["defaultValue"].as_str().map(String::from),
                session_id: p["context"].as_str().map(String::from),
            });
        }
        Some("browsingContext.userPromptClosed")
            if dialog.as_ref().and_then(|d| d.session_id.as_deref()) == p["context"].as_str() =>
        {
            *dialog = None;
        }
        _ => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    pub(crate) type Calls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A session with no process, a BiDi socket that answers an empty `success`
    /// to every command and an idle DevTools stream. The sender injects BiDi events
    /// and `Calls` records the methods the socket received.
    pub(crate) async fn fake_session() -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        fake_session_with_trees(vec![json!({"contexts": [{"context": "ctx-active"}]})]).await
    }

    pub(crate) async fn fake_session_with_trees(
        trees: Vec<Value>,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        fake_session_with_navigation_prompt(trees, false).await
    }

    pub(crate) async fn fake_session_with_navigation_prompt(
        trees: Vec<Value>,
        prompt: bool,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        fake_session_with_options(trees, prompt, None).await
    }

    /// A session whose socket records `method` but never answers it.
    pub(crate) async fn fake_session_ignoring(
        method: &'static str,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        fake_session_with_options(
            vec![json!({"contexts": [{"context": "ctx-active"}]})],
            false,
            Some(method),
        )
        .await
    }

    async fn fake_session_with_options(
        trees: Vec<Value>,
        prompt: bool,
        silent: Option<&'static str>,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let calls = Calls::default();
        let log = calls.clone();
        tokio::spawn(async move {
            let mut trees = std::collections::VecDeque::from(trees);
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            loop {
                tokio::select! {
                    m = ws.next() => {
                        let Some(Ok(Message::Text(t))) = m else { break };
                        let v: Value = serde_json::from_str(&t).unwrap();
                        log.lock().unwrap().push(v["method"].as_str().unwrap().to_string());
                        if Some(v["method"].as_str().unwrap_or_default()) == silent {
                            continue;
                        }
                        if prompt && v["method"] == "browsingContext.navigate" {
                            let event = json!({"type":"event", "method":"browsingContext.userPromptOpened", "params":{
                                "context":"ctx-new", "type":"confirm", "message":"sure?", "handler":"ignore"
                            }});
                            ws.send(Message::Text(event.to_string())).await.unwrap();
                            continue;
                        }
                        let result = if prompt && v["method"] == "browsingContext.create" {
                            json!({"context":"ctx-new"})
                        } else if v["method"] == "browsingContext.navigate" {
                            json!({ "url": v["params"]["url"] })
                        } else if v["method"] == "browsingContext.getTree" {
                            if trees.len() > 1 {
                                trees.pop_front().unwrap()
                            } else {
                                trees.front().cloned().unwrap_or(json!({}))
                            }
                        } else {
                            json!({})
                        };
                        let reply = json!({"type":"success","id":v["id"],"result":result});
                        ws.send(Message::Text(reply.to_string())).await.unwrap();
                    }
                    Some(ev) = rx.recv() => {
                        ws.send(Message::Text(ev.to_string())).await.unwrap();
                    }
                }
            }
        });
        let bidi = BidiClient::connect(&url).await.unwrap();
        let events = bidi.subscribe_matching(lifecycle_event);
        let session = FirefoxSession {
            process: None,
            headless: true,
            bidi: Arc::new(bidi),
            auto_dialog: Arc::new(AtomicBool::new(false)),
            auto_dialog_changed: Arc::new(Notify::new()),
            contexts: Contexts::from_tree(&json!({"contexts": [{"context": "ctx-active"}]}))
                .unwrap(),
            refs: HashMap::new(),
            visited_origins: HashSet::new(),
            events,
        };
        (session, tx, calls)
    }

    #[test]
    fn proxy_capability_maps_http_and_socks() {
        let http = proxy_capability("http://proxy:3128", Some("a.com, b.com")).unwrap();
        assert_eq!(http["httpProxy"], "proxy:3128");
        assert_eq!(http["sslProxy"], "proxy:3128");
        assert_eq!(http["noProxy"], json!(["a.com", "b.com"]));
        let socks = proxy_capability("socks5://proxy:1080", None).unwrap();
        assert_eq!(socks["socksProxy"], "proxy:1080");
        assert_eq!(socks["socksVersion"], 5);
        assert!(socks.get("noProxy").is_none());
    }

    #[tokio::test]
    async fn context_events_update_the_registry_and_reset_page_state() {
        let (mut session, tx, _) = fake_session().await;
        session.contexts.record(&json!({"context": "survivor"}));
        session.refs.insert(("ctx-active".into(), "node".into()), 1);
        let mut state = crate::native::actions::DaemonState::new();
        state.firefox = Some(session);
        state.active_frame_id = Some("frame".into());
        for (method, params) in [
            (
                "contextCreated",
                json!({"context": "popup", "parent": null, "userContext": "jar"}),
            ),
            (
                "contextDestroyed",
                json!({"context": "ctx-active", "parent": null}),
            ),
        ] {
            let method = format!("browsingContext.{method}");
            tx.send(json!({"type": "event", "method": method, "params": params}))
                .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                state.drain_firefox_events();
                if state.firefox.as_ref().unwrap().contexts.active() == "survivor" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let ff = state.firefox.as_ref().unwrap();
        assert_eq!(ff.contexts.tab("popup").unwrap().user_context, "jar");
        assert!(ff.contexts.tab("ctx-active").is_none());
        assert!(ff.refs.is_empty());
        assert!(state.active_frame_id.is_none());
    }

    #[tokio::test]
    async fn initial_viewport_is_sent_to_firefox() {
        let (session, _tx, calls) = fake_session().await;
        session.set_viewport((800, 600)).await.unwrap();
        assert_eq!(*calls.lock().unwrap(), ["browsingContext.setViewport"]);
    }

    #[tokio::test]
    async fn tracks_navigation_events_from_any_context() {
        let (mut session, tx, _) = fake_session().await;
        tx.send(json!({"type": "event", "method": "browsingContext.domContentLoaded", "params": {"context": "popup", "url": "https://example.com/page"}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                session.drain_events(&mut None);
                if session.visited_origins.contains("https://example.com") {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn prompt_events_set_and_clear_the_dialog() {
        let mut d = None;
        let open = json!({"method":"browsingContext.userPromptOpened","params":{"context":"c1","type":"confirm","message":"sure?"}});
        apply_event(&open, &mut d, false);
        assert_eq!(d.as_ref().unwrap().dialog_type, "confirm");
        assert_eq!(d.as_ref().unwrap().session_id.as_deref(), Some("c1"));
        let other = json!({"method":"browsingContext.userPromptClosed","params":{"context":"c2"}});
        apply_event(&other, &mut d, false);
        assert!(d.is_some());
        let close = json!({"method":"browsingContext.userPromptClosed","params":{"context":"c1"}});
        apply_event(&close, &mut d, false);
        assert!(d.is_none());
    }

    #[test]
    fn automatic_prompt_filter_keeps_manual_prompts_and_ignores_stale_accepts() {
        for kind in ["alert", "beforeunload", "beforeUnload", "confirm", "prompt"] {
            let automatic = matches!(kind, "alert" | "beforeunload" | "beforeUnload");
            for enabled in [false, true] {
                for handler in ["ignore", "accept"] {
                    let mut dialog = None;
                    apply_event(
                        &json!({"method": "browsingContext.userPromptOpened", "params": {
                            "context": "ctx-active", "type": kind, "handler": handler
                        }}),
                        &mut dialog,
                        enabled,
                    );
                    assert_eq!(
                        dialog.is_none(),
                        handler == "accept" || (automatic && enabled),
                        "{kind}, {handler}, enabled={enabled}"
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn race_prompt_waits_for_auto_alert_but_returns_for_manual_confirm() {
        let (session, events, _) = fake_session().await;
        session.auto_dialog.store(true, Ordering::Release);
        let waiting = session.race_prompt(std::future::pending::<Result<(), String>>());
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        events
            .send(
                json!({"type":"event", "method":"browsingContext.userPromptOpened",
            "params":{"context":"ctx-active", "type":"alert", "handler":"ignore"}}),
            )
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        events
            .send(
                json!({"type":"event", "method":"browsingContext.userPromptOpened",
            "params":{"context":"ctx-active", "type":"confirm", "handler":"ignore"}}),
            )
            .unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), &mut waiting)
            .await
            .unwrap()
            .unwrap()
            .is_none());

        session.auto_dialog.store(false, Ordering::Release);
        let racer = session.race_prompt(std::future::pending::<Result<(), String>>());
        tokio::pin!(racer);
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut racer)
            .await
            .is_err());
        let background = json!({"type":"event","method":"browsingContext.userPromptOpened","params":{"context":"other"}});
        for _ in 0..2_000 {
            events
                .send(json!({"type":"event","method":"network.beforeRequestSent","params":{"context":"other"}}))
                .unwrap();
        }
        events.send(background).unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(100), &mut racer)
            .await
            .is_err());
        let ours = json!({"type":"event","method":"browsingContext.userPromptOpened","params":{"context":"ctx-active"}});
        events.send(ours).unwrap();
        assert!(racer.await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unrelated_events_do_not_crowd_out_lifecycle_events() {
        let (mut session, tx, _) = fake_session().await;
        session.contexts.record(&json!({"context": "doomed"}));
        let mut barrier = session.bidi.subscribe_buffered();
        tx.send(
            json!({"type":"event", "method":"browsingContext.contextDestroyed",
            "params":{"context":"doomed"}}),
        )
        .unwrap();
        tx.send(json!({"type":"event", "method":"browsingContext.userPromptOpened",
            "params":{"context":"ctx-active", "type":"confirm", "message":"sure?", "handler":"ignore"}}))
            .unwrap();
        for i in 0..2_000 {
            let event = if i % 2 == 0 {
                json!({"type":"event", "method":"network.beforeRequestSent", "params":{"context":"other"}})
            } else {
                json!({"type":"event", "method":"log.entryAdded", "params":{"text":"noise"}})
            };
            tx.send(event).unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..2_002 {
                assert!(barrier.recv().await.is_some());
            }
        })
        .await
        .unwrap();
        let mut dialog = None;
        session.drain_events(&mut dialog);
        assert!(session.contexts.tab("doomed").is_none());
        assert_eq!(
            dialog.as_ref().and_then(|d| d.session_id.as_deref()),
            Some("ctx-active")
        );
        assert!(!session.events.has_lost_events());
        assert!(!session.bidi.take_event_loss());
    }

    #[tokio::test]
    async fn lifecycle_overflow_renews_events_and_reconciles_the_tree() {
        let (mut session, tx, calls) = fake_session_with_trees(vec![json!({"contexts": [
            {"context": "ctx-active"},
            {"context": "popup", "originalOpener": "ctx-active"}
        ]})])
        .await;
        for _ in 0..70_000 {
            tx.send(
                json!({"type":"event", "method":"browsingContext.domContentLoaded",
                "params":{"context":"flood"}}),
            )
            .unwrap();
        }
        tokio::time::timeout(Duration::from_secs(30), async {
            while !session.events.has_lost_events() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        session.drain_events(&mut None);
        assert!(!session.events.has_lost_events());

        let trees = |calls: &Calls| {
            calls
                .lock()
                .unwrap()
                .iter()
                .filter(|method| *method == "browsingContext.getTree")
                .count()
        };
        let before = trees(&calls);
        assert!(session.reconcile_event_loss(&mut None).await.unwrap());
        assert_eq!(trees(&calls), before + 1);
        assert_eq!(session.contexts.active(), "ctx-active");
        assert!(session.contexts.tab("popup").is_some());

        tx.send(json!({"type":"event", "method":"browsingContext.contextCreated", "params":{"context":"later"}}))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while session.contexts.tab("later").is_none() {
                session.drain_events(&mut None);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn prompt_race_returns_an_error_when_the_connection_closes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let (mut session, _events, _) = fake_session().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.close(None).await.unwrap();
        });
        session.bidi = BidiClient::connect(&url).await.unwrap().into();
        let waiting = session.race_prompt(std::future::pending::<Result<(), String>>());
        let error = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.contains("connection closed"), "{error}");
        server.await.unwrap();
    }
}
