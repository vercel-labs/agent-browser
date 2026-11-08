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
    /// Active top-level browsing context (tab) id.
    pub context: String,
    /// Stable `t<N>` ids keyed by BiDi context; assigned on first sight and never reused.
    pub tab_ids: HashMap<String, u32>,
    next_tab_id: u32,
    /// Durable ref ids keyed by `(context, sharedId)`; reset when a document changes.
    pub refs: HashMap<(String, String), i64>,
    pub tab_labels: HashMap<String, String>,
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
    /// Finds and launches Firefox, opens the BiDi session and enables the accessibility actor.
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
        let context = tree["contexts"][0]["context"]
            .as_str()
            .ok_or("Firefox reported no browsing context")?
            .to_string();
        let mut session = Self {
            process: Some(process),
            headless: options.headless,
            bidi,
            auto_dialog: Arc::new(AtomicBool::new(auto_dialog)),
            auto_dialog_changed: Arc::new(Notify::new()),
            tab_ids: HashMap::from([(context.clone(), 1)]),
            next_tab_id: 2,
            context,
            refs: HashMap::new(),
            tab_labels: HashMap::new(),
            visited_origins: HashSet::new(),
            events,
        };
        if let Some(contexts) = tree["contexts"].as_array() {
            for context in contexts {
                if let Some(id) = context["context"].as_str() {
                    session.tab_id(id);
                }
            }
            session.record_context_origins(contexts);
        }
        if let Some(viewport) = options.viewport_size {
            session.set_initial_viewport(viewport).await?;
        }
        Ok(session)
    }

    async fn set_initial_viewport(&self, (width, height): (u32, u32)) -> Result<(), String> {
        self.bidi.send(
            "browsingContext.setViewport",
            json!({"context": self.context, "viewport": {"width": width, "height": height}, "devicePixelRatio": 1}),
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

    /// Returns the stable `t<N>` id for `context`, assigning the next id the first time it is seen.
    pub fn tab_id(&mut self, context: &str) -> u32 {
        if let Some(id) = self.tab_ids.get(context) {
            return *id;
        }
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tab_ids.insert(context.to_string(), id);
        id
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
                            if params["parent"].is_null() {
                                if let Some(context) = params["context"].as_str() {
                                    self.tab_id(context);
                                }
                            }
                            if let Some(url) = params["url"].as_str() {
                                self.note_visit(url);
                            }
                        }
                        Some("browsingContext.domContentLoaded") => {
                            if let Some(url) = params["url"].as_str() {
                                self.note_visit(url);
                            }
                        }
                        Some("browsingContext.contextDestroyed") if params["parent"].is_null() => {
                            if let Some(context) = params["context"].as_str() {
                                self.refs.retain(|(id, _), _| id != context);
                                if dialog.as_ref().and_then(|d| d.session_id.as_deref())
                                    == Some(context)
                                {
                                    *dialog = None;
                                }
                                self.tab_ids.remove(context);
                                self.tab_labels.remove(context);
                                if self.context == context {
                                    self.context = self
                                        .tab_ids
                                        .iter()
                                        .min_by_key(|(_, id)| *id)
                                        .map(|(context, _)| context.clone())
                                        .unwrap_or_default();
                                    self.refs.clear();
                                    active_destroyed = true;
                                }
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
                        && e["params"]["context"] == self.context.as_str() => return Ok(None),
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
                    "target": {"context": self.context},
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
            .send("browsingContext.getTree", json!({"root": self.context}))
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
                        let result = if v["method"] == "browsingContext.getTree" {
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
            context: "ctx-active".into(),
            tab_ids: HashMap::from([("ctx-active".into(), 1)]),
            next_tab_id: 2,
            refs: HashMap::new(),
            tab_labels: HashMap::new(),
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
    async fn tab_ids_are_stable_and_never_reused() {
        let (mut s, _tx, _) = fake_session().await;
        assert_eq!(s.tab_id("ctx-active"), 1);
        assert_eq!(s.tab_id("ctx-new"), 2);
        assert_eq!(s.tab_id("ctx-active"), 1);
        assert_eq!(s.tab_id("ctx-later"), 3);
    }

    #[tokio::test]
    async fn destroyed_contexts_reconcile_tabs_and_active_state() {
        let (mut session, tx, _) = fake_session().await;
        session.tab_id("survivor");
        session
            .tab_labels
            .insert("ctx-active".into(), "docs".into());
        session.refs.insert(("child".into(), "node".into()), 1);
        let mut state = crate::native::actions::DaemonState::new();
        state.firefox = Some(session);
        state.active_frame_id = Some("child".into());

        tx.send(json!({"type":"event", "method":"browsingContext.contextDestroyed", "params":{"context":"child", "parent":"ctx-active"}})).unwrap();
        tx.send(json!({"type":"event", "method":"browsingContext.domContentLoaded", "params":{"url":"https://child-event.example/"}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                state.drain_firefox_events();
                if state
                    .firefox
                    .as_ref()
                    .unwrap()
                    .visited_origins
                    .contains("https://child-event.example")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.firefox.as_ref().unwrap().context, "ctx-active");
        assert_eq!(state.active_frame_id.as_deref(), Some("child"));
        assert_eq!(state.firefox.as_ref().unwrap().refs.len(), 1);

        tx.send(json!({"type":"event", "method":"browsingContext.contextDestroyed", "params":{"context":"ctx-active", "parent":null}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                state.drain_firefox_events();
                if state.firefox.as_ref().unwrap().context == "survivor" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let ff = state.firefox.as_ref().unwrap();
        assert!(!ff.tab_ids.contains_key("ctx-active"));
        assert!(!ff.tab_labels.contains_key("ctx-active"));
        assert!(state.active_frame_id.is_none());
        ff.eval("1 + 1").await.unwrap();
        let err = super::super::tabs::tab_new(&json!({"label":"docs"}), &mut state)
            .await
            .unwrap_err();
        assert_eq!(err, "Firefox did not return a context id for the new tab");

        tx.send(json!({"type":"event", "method":"browsingContext.contextDestroyed", "params":{"context":"ctx-active", "parent":null}})).unwrap();
        tx.send(json!({"type":"event", "method":"browsingContext.contextCreated", "params":{"context":"third", "parent":null}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                state.drain_firefox_events();
                if state
                    .firefox
                    .as_ref()
                    .unwrap()
                    .tab_ids
                    .contains_key("third")
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(state.firefox.as_ref().unwrap().context, "survivor");
    }

    #[tokio::test]
    async fn initial_viewport_is_sent_to_firefox() {
        let (session, _tx, calls) = fake_session().await;
        session.set_initial_viewport((800, 600)).await.unwrap();
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
