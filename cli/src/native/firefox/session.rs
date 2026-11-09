//! A running Firefox: the owned process, both protocol clients and per-session state.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};

use super::bidi::{remote_to_json, BidiClient};
use super::emulation::{self, Settings as EmulationSettings};
use super::network::{AbortOnDrop, NetContext, NETWORK_EVENTS};
use super::process::{check_firefox_version, find_firefox, DevtoolsEndpoint, FirefoxProcess};
use super::rdp::RdpClient;
use crate::native::actions::{EmulatedMedia, PendingDialog};
use crate::native::cdp::chrome::LaunchOptions;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const DEVTOOLS_CONNECT_WINDOW: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const CONTEXT_EVENTS: [&str; 3] = [
    "browsingContext.contextCreated",
    "browsingContext.contextDestroyed",
    "browsingContext.domContentLoaded",
];
const PROMPT_EVENTS: [&str; 2] = [
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
];

/** One launched Firefox. Dropping it kills the process tree and deletes the profile. */
pub struct FirefoxSession {
    process: Option<FirefoxProcess>,
    pub headless: bool,
    pub bidi: Arc<BidiClient>,
    pub auto_dialog: Arc<AtomicBool>,
    pub auto_dialog_changed: Arc<Notify>,
    pub rdp: RdpClient,
    pub accessibility_actor: String,
    /// Active top-level browsing context (tab) id.
    pub context: String,
    /// Active top-level browsing context id (tab), restored by `frame main`.
    pub top_context: String,
    /// Stable `t<N>` ids keyed by BiDi context; assigned on first sight and never reused.
    pub tab_ids: HashMap<String, u32>,
    pub(crate) context_parents: HashMap<String, String>,
    next_tab_id: u32,
    /// Durable ref ids keyed by `(context, sharedId)`; reset when a document changes.
    pub refs: HashMap<(String, String), i64>,
    pub next_node_id: i64,
    pub frames: Option<super::snapshot::FrameWatch>,
    /// Document node `sharedId` last snapshotted per context, used to reset `refs` on a new document.
    pub documents: HashMap<String, String>,
    pub tab_labels: HashMap<String, String>,
    /// Touch points of the active touch sequence, released by a later `input_touch` end.
    pub touches: Vec<(f64, f64)>,
    /// Durable ids of nodes inside open shadow roots, which a stale ref must never recover by role and name.
    pub shadow_ids: std::collections::HashSet<i64>,
    /// Emulation applied to the active tab and replayed onto new tabs.
    pub emulation: Arc<std::sync::RwLock<EmulationSettings>>,
    /// Launch download directory, restored after `download` redirects it.
    pub download_dir: Option<String>,
    events: mpsc::Receiver<Value>,
    pub net: NetContext,
    /// True once the single global network intercept is installed; it is never installed twice.
    pub intercepting: bool,
    /// Unfinished requests mapped to their reported browsing contexts.
    pub inflight: HashMap<String, Option<String>>,
    pub(crate) dispatcher: Option<AbortOnDrop>,
    /// BiDi user context created for a window opened by `window new`, keyed by its top-level context.
    pub window_contexts: HashMap<String, String>,
    pub visited_origins: HashSet<String>,
    /// Version Firefox reported when the session opened, used for HAR browser metadata.
    pub browser_version: String,
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

async fn connect_devtools(endpoint: &DevtoolsEndpoint) -> Result<RdpClient, String> {
    tokio::time::timeout(DEVTOOLS_CONNECT_WINDOW, connect_devtools_loop(endpoint))
        .await
        .map_err(|_| "Firefox DevTools connection timed out".to_string())?
}

async fn connect_devtools_loop(endpoint: &DevtoolsEndpoint) -> Result<RdpClient, String> {
    let deadline = tokio::time::Instant::now() + DEVTOOLS_CONNECT_WINDOW;
    loop {
        let attempt = match endpoint {
            #[cfg(unix)]
            DevtoolsEndpoint::Unix(path) => RdpClient::connect_unix(path).await,
            #[cfg(not(unix))]
            DevtoolsEndpoint::Unix(_) => Err("unix sockets are unavailable".to_string()),
            DevtoolsEndpoint::Port(port) => RdpClient::connect_tcp(*port).await,
        };
        match attempt {
            Ok(client) => return Ok(client),
            Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

impl FirefoxSession {
    /// Finds and launches Firefox, opens the BiDi session, starts the network dispatcher
    /// and enables the accessibility actor.
    /// The daemon handles automatic prompts so the setting can change without relaunching Firefox.
    pub async fn launch(
        options: &LaunchOptions,
        net: NetContext,
        auto_dialog: bool,
    ) -> Result<Self, String> {
        let path = find_firefox(options.executable_path.as_deref())?;
        let (headless, download) = (options.headless, options.download_path.clone());
        let restrict_webrtc = options.restrict_webrtc;
        let process = tokio::task::spawn_blocking(move || {
            FirefoxProcess::launch(
                &path,
                headless,
                download.as_deref(),
                restrict_webrtc,
                START_TIMEOUT,
            )
        })
        .await
        .map_err(|e| format!("Firefox launch task failed: {e}"))??;

        let bidi = Arc::new(
            tokio::time::timeout(
                START_TIMEOUT,
                BidiClient::connect(&format!("{}/session", process.bidi_url)),
            )
            .await
            .map_err(|_| "Firefox BiDi connection timed out".to_string())??,
        );
        let events = bidi.subscribe_buffered();
        let dispatch_events = bidi.subscribe_buffered();
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
        let browser_version = created["capabilities"]["browserVersion"]
            .as_str()
            .unwrap_or("")
            .to_string();
        check_firefox_version(&browser_version)?;
        let events_to_watch: Vec<&str> = CONTEXT_EVENTS
            .into_iter()
            .chain(PROMPT_EVENTS)
            .chain(NETWORK_EVENTS)
            .collect();
        bidi.send("session.subscribe", json!({"events": events_to_watch}))
            .await?;
        bidi.send(
            "network.addDataCollector",
            json!({
                "dataTypes": ["request", "response"],
                "maxEncodedDataSize": 10_000_000
            }),
        )
        .await?;

        let rdp = connect_devtools(&process.devtools).await?;
        let root = rdp.request("root", "getRoot", json!({})).await?;
        let accessibility_actor = root["parentAccessibilityActor"]
            .as_str()
            .ok_or("Firefox DevTools did not expose the accessibility actor")?
            .to_string();
        rdp.request(&accessibility_actor, "enable", json!({}))
            .await?;

        let tree = bidi.send("browsingContext.getTree", json!({})).await?;
        let context = tree["contexts"][0]["context"]
            .as_str()
            .ok_or("Firefox reported no browsing context")?
            .to_string();
        let emulation = Arc::new(std::sync::RwLock::new(EmulationSettings {
            viewport: options
                .viewport_size
                .map(|(width, height)| (i64::from(width), i64::from(height), 1.0)),
            user_agent: options.user_agent.clone(),
            media: options.color_scheme.as_ref().map(|scheme| EmulatedMedia {
                media: None,
                features: vec![("prefers-color-scheme".to_string(), scheme.clone())],
            }),
            ..EmulationSettings::default()
        }));
        let dispatcher = Some(super::network::spawn_dispatcher(
            bidi.clone(),
            net.clone(),
            dispatch_events,
            emulation.clone(),
        ));
        let mut session = Self {
            process: Some(process),
            headless: options.headless,
            bidi,
            auto_dialog: Arc::new(AtomicBool::new(auto_dialog)),
            auto_dialog_changed: Arc::new(Notify::new()),
            rdp,
            accessibility_actor,
            tab_ids: HashMap::from([(context.clone(), 1)]),
            context_parents: HashMap::new(),
            next_tab_id: 2,
            top_context: context.clone(),
            context,
            refs: HashMap::new(),
            next_node_id: 0,
            frames: None,
            documents: HashMap::new(),
            tab_labels: HashMap::new(),
            touches: Vec::new(),
            shadow_ids: Default::default(),
            emulation,
            download_dir: options.download_path.clone(),
            window_contexts: HashMap::new(),
            visited_origins: HashSet::new(),
            events,
            net,
            intercepting: false,
            inflight: HashMap::new(),
            dispatcher,
            browser_version,
        };
        if let Some(contexts) = tree["contexts"].as_array() {
            for context in contexts {
                if let Some(id) = context["context"].as_str() {
                    session.tab_id(id);
                }
            }
            session.record_context_origins(contexts);
        }
        emulation::apply_to_context(&session, &session.context).await?;
        Ok(session)
    }

    fn record_context_origins(&mut self, contexts: &[Value]) {
        for context in contexts {
            if let Some(url) = context["url"].as_str() {
                self.note_visit(url);
            }
            if let Some(children) = context["children"].as_array() {
                if let Some(parent) = context["context"].as_str() {
                    for child in children {
                        if let Some(child) = child["context"].as_str() {
                            self.context_parents
                                .insert(child.to_string(), parent.to_string());
                        }
                    }
                }
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

    /// The BiDi storage partition for the active tab's window, set only by `window new`.
    pub fn active_partition(&self) -> Option<Value> {
        self.window_contexts
            .get(&self.context)
            .map(|user_context| json!({"type": "storageKey", "userContext": user_context}))
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

    /// Applies buffered BiDi events to the session's own bookkeeping and returns them for the
    /// recorders, which read the batch once through the single intake in `network.rs`.
    pub fn drain_events(&mut self, dialog: &mut Option<PendingDialog>) -> Vec<Value> {
        let mut drained = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            self.track_request(&e);
            self.track_tabs(&e, dialog);
            if matches!(
                e["method"].as_str(),
                Some("browsingContext.contextCreated" | "browsingContext.domContentLoaded")
            ) {
                if let Some(url) = e["params"]["url"].as_str() {
                    self.note_visit(url);
                }
            }
            apply_event(
                &e,
                dialog,
                &mut self.context_parents,
                self.auto_dialog.load(Ordering::Acquire),
            );
            drained.push(e);
        }
        drained
    }

    /// Registers new top-level tabs and drops every destroyed context, frames included, through
    /// the one cleanup path.
    fn track_tabs(&mut self, event: &Value, dialog: &mut Option<PendingDialog>) {
        let params = &event["params"];
        let Some(context) = params["context"].as_str() else {
            return;
        };
        match event["method"].as_str() {
            Some("browsingContext.contextCreated") => {
                if params["parent"].is_null() {
                    self.tab_id(context);
                }
            }
            Some("browsingContext.contextDestroyed") => {
                self.forget_context(context, dialog);
            }
            _ => {}
        }
    }

    /// Tracks request lifetimes and removes requests belonging to destroyed contexts.
    pub(super) fn track_request(&mut self, event: &Value) {
        let params = &event["params"];
        let id = params["request"]["request"].as_str();
        match event["method"].as_str() {
            Some("network.beforeRequestSent") => {
                let url = params["request"]["url"].as_str().unwrap_or("");
                if !url.starts_with("ws:") && !url.starts_with("wss:") {
                    if let Some(id) = id {
                        self.inflight.insert(
                            id.to_string(),
                            params["context"].as_str().map(str::to_string),
                        );
                    }
                }
            }
            Some("network.responseCompleted" | "network.fetchError") => {
                if let Some(id) = id {
                    self.inflight.remove(id);
                }
            }
            Some("browsingContext.contextDestroyed") => {
                if let Some(context) = params["context"].as_str() {
                    self.inflight.retain(|_, owner| {
                        !owner.as_deref().is_some_and(|owner| {
                            is_descendant_of(context, owner, &self.context_parents)
                        })
                    });
                }
            }
            _ => {}
        }
    }

    /// True while the active tab or one of its descendant frames has an unfinished request.
    pub fn has_inflight_in_active_tab(&self) -> bool {
        self.inflight
            .values()
            .filter_map(|context| context.as_deref())
            .any(|context| self.context_is_in_active_tab(context))
    }

    /// True when `context` is the active tab or one of its frames.
    pub(crate) fn context_is_in_active_tab(&self, context: &str) -> bool {
        is_descendant_of(&self.top_context, context, &self.context_parents)
    }

    /// Records the frame tree under the active tab so frames created since the last drain can be placed.
    pub(crate) async fn refresh_frame_tree(&mut self) -> Result<(), String> {
        let tree = self
            .bidi
            .send("browsingContext.getTree", json!({"root": self.top_context}))
            .await?;
        let contexts = tree["contexts"].as_array().cloned().unwrap_or_default();
        self.record_context_origins(&contexts);
        Ok(())
    }

    /// True when `context` is the active tab or one of its frames, reading the live frame tree
    /// when the drained events have not described it yet.
    pub(crate) async fn active_tab_contains(&mut self, context: &str) -> bool {
        if self.context_is_in_active_tab(context) {
            return true;
        }
        self.refresh_frame_tree().await.is_ok() && self.context_is_in_active_tab(context)
    }

    /// Forgets a browsing context that is gone along with every context nested inside it: stable
    /// ids, labels, element refs, documents, shadow ids and any prompt they owned. Returns true
    /// when the active context was among them, which then falls back to its top-level tab, or to
    /// the lowest surviving tab id when that tab itself is gone.
    pub fn forget_context(&mut self, context: &str, dialog: &mut Option<PendingDialog>) -> bool {
        let gone: HashSet<String> = self
            .context_parents
            .keys()
            .filter(|id| is_context_or_descendant(id, context, &self.context_parents))
            .cloned()
            .chain(std::iter::once(context.to_string()))
            .collect();
        let stale_ids: HashSet<i64> = self
            .refs
            .iter()
            .filter(|((id, _), _)| gone.contains(id))
            .map(|(_, id)| *id)
            .collect();
        self.refs.retain(|(id, _), _| !gone.contains(id));
        self.documents.retain(|id, _| !gone.contains(id));
        self.shadow_ids.retain(|id| !stale_ids.contains(id));
        if dialog.as_ref().is_some_and(|d| {
            [&d.owner_context, &d.session_id]
                .into_iter()
                .flatten()
                .any(|owner| gone.contains(owner))
        }) {
            *dialog = None;
        }
        self.context_parents.retain(|id, _| !gone.contains(id));
        if self.tab_ids.remove(context).is_some() {
            self.tab_labels.remove(context);
            if self.top_context == context {
                self.top_context = self
                    .tab_ids
                    .iter()
                    .min_by_key(|(_, id)| *id)
                    .map(|(context, _)| context.clone())
                    .unwrap_or_default();
                self.context = self.top_context.clone();
                self.refs.clear();
                return true;
            }
        }
        if !gone.contains(&self.context) {
            return false;
        }
        self.context = self.top_context.clone();
        true
    }

    /// Accepts the prompts the daemon answers itself, following `auto_dialog` as it changes.
    pub fn spawn_auto_dismiss(&self) -> tokio::task::JoinHandle<()> {
        let client = Arc::clone(&self.bidi);
        let mut events = client.subscribe_buffered();
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
                        Some(event) => track(&event, &mut pending),
                        None => break,
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

    /// Runs `fut` until it finishes or a prompt opens in `context` or its owning tab. Returns `None` for the prompt.
    pub async fn race_prompt<T>(
        &self,
        context: &str,
        fut: impl Future<Output = Result<T, String>>,
    ) -> Result<Option<T>, String> {
        let mut events = self.bidi.subscribe_buffered();
        let mut parents = self.context_parents.clone();
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return r.map(Some),
                e = events.recv() => match e {
                    Some(e) if e["method"] == "browsingContext.userPromptOpened"
                        && !auto_handles_prompt(&e["params"], self.auto_dialog.load(Ordering::Acquire))
                        && [context, &self.context, &self.top_context].iter().any(|active|
                            owner_context(active, &parents)
                                == owner_context(e["params"]["context"].as_str().unwrap_or_default(), &parents)
                        ) => return Ok(None),
                    Some(e) => apply_event(&e, &mut None, &mut parents, self.auto_dialog.load(Ordering::Acquire)),
                    None => return fut.await.map(Some),
                },
            }
        }
    }

    /// Evaluates `expression` in the active context and returns the decoded value.
    pub async fn eval(&self, expression: &str) -> Result<Value, String> {
        self.eval_in_context(expression, &self.context, false).await
    }

    /// Evaluates `expression` in the active top-level tab and returns the decoded value.
    pub async fn eval_top(&self, expression: &str) -> Result<Value, String> {
        self.eval_in_context(expression, &self.top_context, false)
            .await
    }

    /// Like `eval`, but a script exception yields `null` so callers can keep polling.
    pub async fn eval_lenient(&self, expression: &str) -> Result<Value, String> {
        self.eval_in_context(expression, &self.context, true).await
    }

    async fn eval_in_context(
        &self,
        expression: &str,
        context: &str,
        lenient: bool,
    ) -> Result<Value, String> {
        let r = self
            .bidi
            .send(
                "script.evaluate",
                json!({
                    "expression": expression,
                    "target": {"context": context},
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

    /// URL of the active top-level tab, read from the browsing context tree.
    pub async fn url(&self) -> Result<String, String> {
        let tree = self
            .bidi
            .send("browsingContext.getTree", json!({"root": self.top_context}))
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

fn apply_event(
    e: &Value,
    dialog: &mut Option<PendingDialog>,
    context_parents: &mut HashMap<String, String>,
    auto_dialog: bool,
) {
    let p = &e["params"];
    match e["method"].as_str() {
        Some("browsingContext.contextCreated") => {
            if let (Some(context), Some(parent)) = (p["context"].as_str(), p["parent"].as_str()) {
                context_parents.insert(context.to_string(), parent.to_string());
            }
        }
        Some("browsingContext.contextDestroyed") => {
            if let Some(context) = p["context"].as_str() {
                context_parents.remove(context);
            }
        }
        Some("browsingContext.userPromptOpened") if !auto_handles_prompt(p, auto_dialog) => {
            let context = p["context"].as_str();
            *dialog = Some(PendingDialog {
                dialog_type: p["type"].as_str().unwrap_or("alert").to_string(),
                message: p["message"].as_str().unwrap_or("").to_string(),
                url: String::new(),
                default_prompt: p["defaultValue"].as_str().map(String::from),
                session_id: context.map(String::from),
                owner_context: context
                    .map(|context| owner_context(context, context_parents).to_string()),
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

fn is_context_or_descendant<'a>(
    mut context: &'a str,
    root: &str,
    parents: &'a HashMap<String, String>,
) -> bool {
    loop {
        if context == root {
            return true;
        }
        let Some(parent) = parents.get(context).filter(|parent| !parent.is_empty()) else {
            return false;
        };
        context = parent;
    }
}

fn owner_context<'a>(mut context: &'a str, parents: &'a HashMap<String, String>) -> &'a str {
    while let Some(parent) = parents.get(context).filter(|parent| !parent.is_empty()) {
        context = parent;
    }
    context
}

/// True when `context` is `root` or a frame nested inside it.
fn is_descendant_of<'a>(
    root: &str,
    mut context: &'a str,
    parents: &'a HashMap<String, String>,
) -> bool {
    loop {
        if root == context {
            return true;
        }
        match parents.get(context) {
            Some(parent) => context = parent,
            None => return false,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::AsyncWriteExt;
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
        fake_session_with(|v: &Value| {
            if v["method"] == "browsingContext.getTree" {
                json!({"contexts": [{"context": "ctx-active"}]})
            } else {
                json!({})
            }
        })
        .await
    }

    pub(crate) async fn fake_session_with_trees(
        trees: Vec<Value>,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        let trees = std::sync::Mutex::new(std::collections::VecDeque::from(trees));
        fake_session_with(move |v| {
            if v["method"] != "browsingContext.getTree" {
                return json!({});
            }
            let mut trees = trees.lock().unwrap();
            if trees.len() > 1 {
                trees.pop_front().unwrap()
            } else {
                trees.front().cloned().unwrap_or(json!({}))
            }
        })
        .await
    }

    /// Like `fake_session`, but `script` supplies the raw result of each `script.callFunction`.
    pub(crate) async fn fake_session_with_script<F>(
        script: F,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    )
    where
        F: Fn(&Value) -> Value + Send + 'static,
    {
        fake_session_with(move |v: &Value| {
            if v["method"] == "script.callFunction" {
                script(&v["params"])
            } else {
                json!({})
            }
        })
        .await
    }

    /// A BiDi command reply: a `result` payload, a protocol `error`/`message` pair, or no reply.
    pub(crate) enum FakeReply {
        Result(Value),
        Error {
            error: &'static str,
            message: &'static str,
        },
        Silent,
    }

    /// Same as `fake_session`, with `responder` deciding each command's `result`.
    pub(crate) async fn fake_session_with(
        responder: impl Fn(&Value) -> Value + Send + 'static,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        fake_session_with_replies(move |v: &Value| FakeReply::Result(responder(v))).await
    }

    /// Same as `fake_session_with`, but `responder` chooses the reply, so a test can reject a
    /// command the way Firefox does or leave it unanswered.
    pub(crate) async fn fake_session_with_replies(
        responder: impl Fn(&Value) -> FakeReply + Send + 'static,
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
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            loop {
                tokio::select! {
                    m = ws.next() => {
                        let Some(Ok(Message::Text(t))) = m else { break };
                        let v: Value = serde_json::from_str(&t).unwrap();
                        log.lock().unwrap().push(v["method"].as_str().unwrap().to_string());
                        match responder(&v) {
                            FakeReply::Result(result) => {
                                let reply = json!({"type":"success","id":v["id"],"result":result});
                                ws.send(Message::Text(reply.to_string())).await.unwrap();
                            }
                            FakeReply::Error { error, message } => {
                                let reply = json!({
                                    "type": "error",
                                    "id": v["id"],
                                    "error": error,
                                    "message": message
                                });
                                ws.send(Message::Text(reply.to_string())).await.unwrap();
                            }
                            FakeReply::Silent => {}
                        }
                    }
                    Some(ev) = rx.recv() => {
                        ws.send(Message::Text(ev.to_string())).await.unwrap();
                    }
                }
            }
        });
        let bidi = BidiClient::connect(&url).await.unwrap();
        let bidi = Arc::new(bidi);
        let events = bidi.subscribe_buffered();
        let (a, mut b) = tokio::io::duplex(4096);
        let greeting = json!({"from":"root"}).to_string();
        b.write_all(format!("{}:{}", greeting.len(), greeting).as_bytes())
            .await
            .unwrap();
        std::mem::forget(b);
        let rdp = RdpClient::from_stream(a).await.unwrap();
        let session = FirefoxSession {
            process: None,
            headless: true,
            bidi,
            auto_dialog: Arc::new(AtomicBool::new(false)),
            auto_dialog_changed: Arc::new(Notify::new()),
            rdp,
            accessibility_actor: "acc".into(),
            context: "ctx-active".into(),
            tab_ids: HashMap::from([("ctx-active".into(), 1)]),
            context_parents: HashMap::new(),
            next_tab_id: 2,
            top_context: "ctx-active".into(),
            refs: HashMap::new(),
            next_node_id: 0,
            frames: None,
            documents: HashMap::new(),
            tab_labels: HashMap::new(),
            touches: Vec::new(),
            shadow_ids: Default::default(),
            emulation: Arc::default(),
            download_dir: None,
            window_contexts: HashMap::new(),
            visited_origins: HashSet::new(),
            events,
            net: NetContext::default(),
            intercepting: false,
            inflight: HashMap::new(),
            dispatcher: None,
            browser_version: String::new(),
        };
        (session, tx, calls)
    }

    pub(crate) type Messages = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;

    /// A session whose BiDi server records full messages, answers `browsingContext.create`
    /// with a new context and whose DevTools stream answers tab configuration requests.
    pub(crate) async fn fake_session_with_rdp() -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
        Messages,
        Messages,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

        async fn put(s: &mut DuplexStream, v: Value) {
            let b = v.to_string();
            s.write_all(format!("{}:{}", b.len(), b).as_bytes())
                .await
                .unwrap();
        }

        async fn get(s: &mut DuplexStream) -> Option<Value> {
            let mut len = Vec::new();
            loop {
                let b = s.read_u8().await.ok()?;
                if b == b':' {
                    break;
                }
                len.push(b);
            }
            let n: usize = String::from_utf8(len).ok()?.parse().ok()?;
            let mut buf = vec![0; n];
            s.read_exact(&mut buf).await.ok()?;
            serde_json::from_slice(&buf).ok()
        }

        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let calls = Calls::default();
        let methods = calls.clone();
        let bidi_messages = Messages::default();
        let bidi_log = bidi_messages.clone();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            loop {
                tokio::select! {
                    m = ws.next() => {
                        let Some(Ok(Message::Text(t))) = m else { break };
                        let v: Value = serde_json::from_str(&t).unwrap();
                        methods.lock().unwrap().push(v["method"].as_str().unwrap().to_string());
                        bidi_log.lock().unwrap().push(v.clone());
                        let result = if v["method"] == "browsingContext.create" {
                            json!({"context": "ctx-new"})
                        } else if v["method"] == "browsingContext.getTree" {
                            json!({"contexts": [{"context": "ctx-active", "url": "https://example.com/", "userContext": "default"}]})
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
        let events = bidi.subscribe_buffered();

        let (a, mut b) = tokio::io::duplex(65536);
        let rdp_messages = Messages::default();
        let rdp_log = rdp_messages.clone();
        tokio::spawn(async move {
            put(&mut b, json!({"from":"root"})).await;
            while let Some(v) = get(&mut b).await {
                rdp_log.lock().unwrap().push(v.clone());
                let to = v["to"].as_str().unwrap_or("");
                let kind = v["type"].as_str().unwrap_or("");
                let reply = match (to, kind) {
                    ("root", "listTabs") => json!({
                        "from": "root",
                        "tabs": [{"actor": "tab1", "selected": true}],
                    }),
                    ("tab1", "getWatcher") => json!({"from": "tab1", "actor": "watcher1"}),
                    ("watcher1", "getTargetConfigurationActor") => json!({
                        "from": "watcher1",
                        "configuration": {"actor": "cfg1"},
                    }),
                    ("cfg1", "updateConfiguration") => json!({
                        "from": "cfg1",
                        "configuration": {},
                    }),
                    _ => json!({"from": to}),
                };
                put(&mut b, reply).await;
            }
        });
        let rdp = RdpClient::from_stream(a).await.unwrap();
        let session = FirefoxSession {
            process: None,
            headless: true,
            bidi: Arc::new(bidi),
            auto_dialog: Arc::new(AtomicBool::new(false)),
            auto_dialog_changed: Arc::new(Notify::new()),
            rdp,
            accessibility_actor: "acc".into(),
            context: "ctx-active".into(),
            tab_ids: HashMap::from([("ctx-active".into(), 1)]),
            context_parents: HashMap::new(),
            next_tab_id: 2,
            top_context: "ctx-active".into(),
            refs: HashMap::new(),
            next_node_id: 0,
            shadow_ids: Default::default(),
            frames: None,
            documents: HashMap::new(),
            tab_labels: HashMap::new(),
            touches: Vec::new(),
            emulation: Arc::default(),
            download_dir: None,
            events,
            visited_origins: HashSet::new(),
            net: NetContext::default(),
            intercepting: false,
            inflight: HashMap::new(),
            dispatcher: None,
            browser_version: String::new(),
            window_contexts: HashMap::new(),
        };
        (session, tx, calls, bidi_messages, rdp_messages)
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
        session.context = "child".into();
        session
            .context_parents
            .insert("child".into(), "ctx-active".into());
        session.refs.insert(("child".into(), "node".into()), 1);
        session.documents.insert("child".into(), "document".into());
        session.shadow_ids.insert(1);
        let mut state = crate::native::actions::DaemonState::new();
        state
            .ref_map
            .add_with_frame("e1".into(), Some(1), "button", "OK", None, Some("child"));
        state.firefox = Some(session);
        state.active_frame_id = Some("child".into());

        tx.send(json!({"type":"event", "method":"browsingContext.contextDestroyed", "params":{"context":"child", "parent":"ctx-active"}})).unwrap();
        tx.send(json!({"type":"event", "method":"browsingContext.domContentLoaded", "params":{"url":"https://child-event.example/"}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                crate::native::firefox::network::drain(&mut state).await;
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
        let ff = state.firefox.as_ref().unwrap();
        assert_eq!(ff.context, "ctx-active");
        assert!(state.active_frame_id.is_none());
        assert!(ff.refs.is_empty());
        assert!(ff.documents.is_empty());
        assert!(ff.shadow_ids.is_empty());
        assert!(state.ref_map.get("e1").is_none());

        tx.send(json!({"type":"event", "method":"browsingContext.contextDestroyed", "params":{"context":"ctx-active", "parent":null}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                crate::native::firefox::network::drain(&mut state).await;
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
                crate::native::firefox::network::drain(&mut state).await;
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
    async fn a_frame_missing_from_drained_events_is_still_in_the_active_tab() {
        let (mut ff, _, _) = fake_session_with(|v: &Value| {
            if v["method"] == "browsingContext.getTree" {
                json!({"contexts": [{"context": "ctx-active", "children": [{"context": "ctx-frame"}]}]})
            } else {
                json!({})
            }
        })
        .await;
        assert!(!ff.context_is_in_active_tab("ctx-frame"));
        assert!(ff.active_tab_contains("ctx-frame").await);
        assert!(ff.context_is_in_active_tab("ctx-frame"));
        assert!(!ff.active_tab_contains("ctx-other").await);
    }

    #[tokio::test]
    async fn drain_events_keeps_more_than_broadcast_capacity() {
        let (mut ff, tx, _) = fake_session().await;
        let mut received = ff.bidi.subscribe_buffered();
        for i in 0..2048 {
            tx.send(json!({"type":"event","method":"log.entryAdded","params":{"index":i}}))
                .unwrap();
        }
        for _ in 0..2048 {
            received.recv().await.unwrap();
        }
        assert_eq!(ff.drain_events(&mut None).len(), 2048);
    }

    #[tokio::test]
    async fn page_url_uses_the_top_context_when_a_frame_is_active() {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let (mut session, _tx, _) = fake_session_with(move |request| {
            seen.lock().unwrap().push(request.clone());
            json!({"contexts": [{"context": "ctx-page", "url": "https://page.example/"}]})
        })
        .await;
        session.context = "ctx-frame".into();

        assert_eq!(session.url().await.unwrap(), "https://page.example/");
        assert_eq!(requests.lock().unwrap()[0]["params"]["root"], "ctx-active");
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
        let mut context_parents = HashMap::new();
        let frame = json!({"method":"browsingContext.contextCreated","params":{"context":"frame","parent":"top"}});
        apply_event(&frame, &mut d, &mut context_parents, false);
        let open = json!({"method":"browsingContext.userPromptOpened","params":{"context":"frame","type":"confirm","message":"sure?"}});
        apply_event(&open, &mut d, &mut context_parents, false);
        assert_eq!(d.as_ref().unwrap().dialog_type, "confirm");
        assert_eq!(d.as_ref().unwrap().session_id.as_deref(), Some("frame"));
        assert_eq!(d.as_ref().unwrap().owner_context.as_deref(), Some("top"));
        let other =
            json!({"method":"browsingContext.userPromptClosed","params":{"context":"other"}});
        apply_event(&other, &mut d, &mut context_parents, false);
        assert!(d.is_some());
        let close =
            json!({"method":"browsingContext.userPromptClosed","params":{"context":"frame"}});
        apply_event(&close, &mut d, &mut context_parents, false);
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
                        &mut HashMap::new(),
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
        let waiting =
            session.race_prompt("ctx-active", std::future::pending::<Result<(), String>>());
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
    async fn inflight_requests_follow_tab_ancestry_and_end_on_completion_error_or_destroy() {
        let (mut session, _, _) = fake_session().await;
        session.record_context_origins(&[json!({
            "context": "ctx-active",
            "children": [{"context": "frame", "children": [{"context": "nested"}]}],
        })]);
        for (id, context) in [("background", Some("other")), ("unknown", None)] {
            session.track_request(&json!({
                "method": "network.beforeRequestSent",
                "params": {"context": context, "request": {"request": id, "url": "http://example.com/"}},
            }));
        }
        assert!(!session.has_inflight_in_active_tab());
        session.top_context = "other".into();
        assert!(session.has_inflight_in_active_tab());
        session.track_request(&json!({
            "method": "network.fetchError",
            "params": {"request": {"request": "background"}},
        }));
        assert!(!session.has_inflight_in_active_tab());
        session.top_context = "ctx-active".into();
        session.context = "frame".into();
        for id in ["completed", "destroyed"] {
            session.track_request(&json!({
                "method": "network.beforeRequestSent",
                "params": {"context": "nested", "request": {"request": id, "url": "http://example.com/"}},
            }));
        }
        assert!(session.has_inflight_in_active_tab());
        session.track_request(&json!({
            "method": "network.responseCompleted",
            "params": {"request": {"request": "completed"}},
        }));
        assert!(!session.inflight.contains_key("completed"));
        assert!(session.has_inflight_in_active_tab());
        session.track_request(&json!({
            "method": "browsingContext.contextDestroyed",
            "params": {"context": "frame"},
        }));
        assert!(!session.inflight.contains_key("destroyed"));
        assert!(!session.has_inflight_in_active_tab());
    }

    #[tokio::test]
    async fn race_prompt_returns_none_when_a_prompt_opens_on_the_active_context() {
        let (mut s, tx, _) = fake_session().await;
        s.context_parents
            .insert("frame-parent".into(), "ctx-active".into());
        let hang = std::future::pending::<Result<(), String>>();
        let racer = s.race_prompt("ctx-active", hang);
        tokio::pin!(racer);
        assert!(tokio::time::timeout(Duration::from_millis(50), &mut racer)
            .await
            .is_err());
        let background = json!({"type":"event","method":"browsingContext.userPromptOpened","params":{"context":"other"}});
        tx.send(background).unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(100), &mut racer)
            .await
            .is_err());
        tx.send(json!({"type":"event","method":"browsingContext.contextCreated","params":{"context":"frame-child","parent":"frame-parent"}})).unwrap();
        let ours = json!({"type":"event","method":"browsingContext.userPromptOpened","params":{"context":"frame-child"}});
        tx.send(ours).unwrap();
        assert!(racer.await.unwrap().is_none());
    }
}
