//! A running Firefox: the owned process, both protocol clients and per-session state.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::Notify;

use super::bidi::{remote_to_json, BidiClient};
use super::contexts::Contexts;
use super::process::{check_firefox_version, find_firefox, FirefoxProcess};
use crate::native::actions::PendingDialog;
use crate::native::cdp::chrome::LaunchOptions;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const SUBSCRIBED_EVENTS: [&str; 5] = [
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
    "browsingContext.contextCreated",
    "browsingContext.contextDestroyed",
    "browsingContext.domContentLoaded",
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
    events: broadcast::Receiver<Value>,
}

/// Builds the BiDi `proxy` capability from a `--proxy` value such as `http://host:3128`.
fn proxy_capability(proxy: &str, bypass: Option<&str>) -> Value {
    let (scheme, hostport) = proxy.split_once("://").unwrap_or(("http", proxy));
    let mut cap = match scheme {
        "socks5" | "socks5h" | "socks" => {
            json!({"proxyType": "manual", "socksProxy": hostport, "socksVersion": 5})
        }
        "socks4" => json!({"proxyType": "manual", "socksProxy": hostport, "socksVersion": 4}),
        _ => json!({"proxyType": "manual", "httpProxy": hostport, "sslProxy": hostport}),
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
    cap
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
        let events = bidi.subscribe();
        let mut caps = json!({
            "acceptInsecureCerts": options.ignore_https_errors,
            "unhandledPromptBehavior": {"default": "ignore"},
        });
        if let Some(proxy) = options.proxy.as_deref() {
            caps["proxy"] = proxy_capability(proxy, options.proxy_bypass.as_deref());
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

    /// The BiDi storage partition of the active context's tab, when it is not the default one.
    pub fn active_partition(&self) -> Option<Value> {
        self.contexts.partition(self.contexts.active())
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

    /// Applies buffered BiDi events and returns whether the active context was destroyed.
    pub fn drain_events(&mut self, dialog: &mut Option<PendingDialog>) -> bool {
        let mut active_destroyed = false;
        loop {
            match self.events.try_recv() {
                Ok(e) => {
                    let params = &e["params"];
                    match e["method"].as_str() {
                        Some("browsingContext.contextCreated") => {
                            self.contexts.record(params);
                            if let Some(url) = params["url"].as_str() {
                                self.note_visit(url);
                            }
                        }
                        Some("browsingContext.domContentLoaded") => {
                            if let Some(url) = params["url"].as_str() {
                                self.note_visit(url);
                            }
                        }
                        Some("browsingContext.contextDestroyed") => {
                            if let Some(context) = params["context"].as_str() {
                                active_destroyed |= self.forget_context(context, dialog);
                            }
                        }
                        _ => {}
                    }
                    apply_event(&e, dialog, self.auto_dialog.load(Ordering::Acquire));
                }
                Err(TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        active_destroyed
    }

    /// Forgets a browsing context that is gone and everything nested in it: element refs and
    /// any prompt they owned. Returns true when the active context was among them, which the
    /// registry then replaces.
    pub fn forget_context(&mut self, context: &str, dialog: &mut Option<PendingDialog>) -> bool {
        let removed = self.contexts.remove(context);
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

    /// Accepts the prompts the daemon answers itself, following `auto_dialog` as it changes.
    pub fn spawn_auto_dismiss(&self) -> tokio::task::JoinHandle<()> {
        let client = Arc::clone(&self.bidi);
        let mut events = client.subscribe();
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
                tokio::select! {
                    biased;
                    event = events.recv() => match event {
                        Ok(event) => track(&event, &mut pending),
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    },
                    _ = changed.notified() => {}
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
        let mut events = self.bidi.subscribe();
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return r.map(Some),
                e = events.recv() => match e {
                    Ok(e) if e["method"] == "browsingContext.userPromptOpened"
                        && !auto_handles_prompt(&e["params"], self.auto_dialog.load(Ordering::Acquire))
                        && e["params"]["context"] == self.contexts.active() => return Ok(None),
                    Err(broadcast::error::RecvError::Closed) => return fut.await.map(Some),
                    _ => {}
                },
            }
        }
    }

    /// Evaluates `expression` in the active context and returns the decoded value.
    pub async fn eval(&self, expression: &str) -> Result<Value, String> {
        self.eval_inner(expression, false).await
    }

    /// Like `eval`, but a script exception yields `null` so callers can keep polling.
    pub async fn eval_lenient(&self, expression: &str) -> Result<Value, String> {
        self.eval_inner(expression, true).await
    }

    async fn eval_inner(&self, expression: &str, lenient: bool) -> Result<Value, String> {
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
                return Ok(Value::Null);
            }
            let d = &r["exceptionDetails"];
            let msg = d["exception"]["value"]
                .as_str()
                .or(d["text"].as_str())
                .unwrap_or("script exception");
            return Err(format!("Evaluation error: {msg}"));
        }
        Ok(remote_to_json(&r["result"]))
    }

    /// URL of the active context, read from the browsing context tree.
    pub async fn url(&self) -> Result<String, String> {
        let tree = self
            .bidi
            .send(
                "browsingContext.getTree",
                json!({"root": self.contexts.active()}),
            )
            .await?;
        Ok(tree["contexts"][0]["url"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }
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
        let events = bidi.subscribe();
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
        let http = proxy_capability("http://proxy:3128", Some("a.com, b.com"));
        assert_eq!(http["httpProxy"], "proxy:3128");
        assert_eq!(http["sslProxy"], "proxy:3128");
        assert_eq!(http["noProxy"], json!(["a.com", "b.com"]));
        let socks = proxy_capability("socks5://proxy:1080", None);
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
    }

    #[tokio::test]
    async fn race_prompt_returns_none_when_a_prompt_opens_on_the_active_context() {
        let (s, tx, _) = fake_session().await;
        let hang = std::future::pending::<Result<(), String>>();
        let racer = s.race_prompt(hang);
        tokio::pin!(racer);
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut racer)
            .await
            .is_err());
        let background = json!({"type":"event","method":"browsingContext.userPromptOpened","params":{"context":"other"}});
        tx.send(background).unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(100), &mut racer)
            .await
            .is_err());
        let ours = json!({"type":"event","method":"browsingContext.userPromptOpened","params":{"context":"ctx-active"}});
        tx.send(ours).unwrap();
        assert!(racer.await.unwrap().is_none());
    }
}
