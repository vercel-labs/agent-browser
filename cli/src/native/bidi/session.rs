use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::attach::AttachTarget;
use super::client::{remote_to_json, BidiClient};
use super::emulation::{self, Settings as EmulationSettings};
use super::firefox::{check_firefox_version, find_firefox, DevtoolsEndpoint, FirefoxProcess};
use super::firefox_devtools::RdpClient;
use super::network::{AbortOnDrop, NetContext, NETWORK_EVENTS};
use super::types::UserPromptOpened;
use crate::native::actions::{EmulatedMedia, PendingDialog};
use crate::native::cdp::chrome::LaunchOptions;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
const DEVTOOLS_CONNECT_WINDOW: Duration = Duration::from_secs(10);
const ATTACH_DEVTOOLS_CONNECT_WINDOW: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const CONTEXT_EVENTS: [&str; 2] = [
    "browsingContext.contextCreated",
    "browsingContext.contextDestroyed",
];
const PROMPT_EVENTS: [&str; 2] = [
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
];

/// A Firefox session. Dropping it releases protocol clients and kills only an owned process.
pub struct FirefoxSession {
    process: Option<FirefoxProcess>,
    pub headless: bool,
    pub bidi: Arc<BidiClient>,
    pub rdp: RdpClient,
    pub accessibility_actor: String,
    /// Active browsing context id: a top-level tab, or a frame after `frame <sel>`.
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
    /// Ids of requests that have started and not yet finished or failed.
    pub inflight: HashSet<String>,
    /// Origins of pages this session navigated to, kept so state saves still cover them after the tab moves on.
    pub visited_origins: HashSet<String>,
    pub(crate) dispatcher: Option<AbortOnDrop>,
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

async fn within_attach_deadline<T>(
    deadline: Option<tokio::time::Instant>,
    future: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, future)
            .await
            .map_err(|_| "Firefox attach timed out after 15 seconds".to_string())?,
        None => future.await,
    }
}

async fn connect_devtools(
    endpoint: &DevtoolsEndpoint,
    window: Duration,
) -> Result<RdpClient, String> {
    let deadline = tokio::time::Instant::now() + window;
    tokio::time::timeout(window, async {
        loop {
            let attempt = match endpoint {
                #[cfg(unix)]
                DevtoolsEndpoint::Unix(path) => RdpClient::connect_unix(path).await,
                #[cfg(not(unix))]
                DevtoolsEndpoint::Unix(_) => Err("unix sockets are unavailable".to_string()),
                DevtoolsEndpoint::Tcp { host, port } => RdpClient::connect_tcp(host, *port).await,
            };
            match attempt {
                Ok(client) => return Ok(client),
                Err(error) if tokio::time::Instant::now() >= deadline => return Err(error),
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await
    .map_err(|_| "Firefox DevTools connection timed out".to_string())?
}

impl FirefoxSession {
    /// Finds and launches Firefox, opens the BiDi session, starts the network dispatcher
    /// and enables the accessibility actor.
    /// With `auto_dialog`, Firefox itself accepts alert and beforeunload prompts, like Chrome's default.
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

        let target = AttachTarget {
            bidi_url: format!("{}/session", process.bidi_url),
            devtools: process.devtools.clone(),
            devtools_explicit: false,
        };
        Self::initialize(&target, Some(process), options, net, auto_dialog, None).await
    }

    /// Opens a BiDi session on an already running Firefox and connects its DevTools server.
    pub async fn attach(
        target: &AttachTarget,
        options: &LaunchOptions,
        net: NetContext,
        auto_dialog: bool,
    ) -> Result<Self, String> {
        Self::initialize(
            target,
            None,
            options,
            net,
            auto_dialog,
            Some(tokio::time::Instant::now() + ATTACH_TIMEOUT),
        )
        .await
    }

    async fn initialize(
        target: &AttachTarget,
        process: Option<FirefoxProcess>,
        options: &LaunchOptions,
        net: NetContext,
        auto_dialog: bool,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Self, String> {
        let attached = process.is_none();
        let connect = async {
            tokio::time::timeout(START_TIMEOUT, BidiClient::connect(&target.bidi_url))
                .await
                .map_err(|_| "Firefox BiDi connection timed out".to_string())?
        };
        let bidi = Arc::new(within_attach_deadline(deadline, connect).await?);
        let events = bidi.subscribe_buffered();
        let mut caps = json!({
            "acceptInsecureCerts": options.ignore_https_errors,
            "unhandledPromptBehavior": if auto_dialog {
                json!({"default": "ignore", "alert": "accept", "beforeUnload": "accept"})
            } else {
                json!({"default": "ignore"})
            },
        });
        if let Some(proxy) = options.proxy.as_deref() {
            caps["proxy"] = proxy_capability(proxy, options.proxy_bypass.as_deref());
        }
        let create_session = bidi.send(
            "session.new",
            json!({"capabilities": {"alwaysMatch": caps}}),
        );
        let created = match within_attach_deadline(deadline, create_session).await {
            Err(error) if attached && error.contains("Maximum number of active sessions") => {
                return Err(format!(
                    "Firefox at {} already has a WebDriver BiDi session. Another client is attached; close it and retry.",
                    super::attach::target_label(&target.bidi_url)
                ));
            }
            result => result?,
        };
        let cleanup_bidi = bidi.clone();
        let result = within_attach_deadline(
            deadline,
            Self::open_session(bidi, target, process, options, net, events, created),
        )
        .await;
        if attached && result.is_err() {
            let _ = cleanup_bidi
                .send_timeout("session.end", json!({}), SHUTDOWN_TIMEOUT)
                .await;
        }
        result
    }

    async fn open_session(
        bidi: Arc<BidiClient>,
        target: &AttachTarget,
        process: Option<FirefoxProcess>,
        options: &LaunchOptions,
        net: NetContext,
        events: mpsc::Receiver<Value>,
        created: Value,
    ) -> Result<Self, String> {
        let attached = process.is_none();
        let dispatch_events = bidi.subscribe_buffered();
        check_firefox_version(
            created["capabilities"]["browserVersion"]
                .as_str()
                .unwrap_or(""),
        )?;
        let events_to_watch: Vec<&str> = CONTEXT_EVENTS
            .into_iter()
            .chain(PROMPT_EVENTS)
            .chain(NETWORK_EVENTS)
            .collect();
        bidi.send("session.subscribe", json!({"events": events_to_watch}))
            .await?;
        bidi.send(
            "network.addDataCollector",
            json!({"dataTypes": ["response"], "maxEncodedDataSize": 10_000_000}),
        )
        .await?;

        let window = if attached {
            ATTACH_DEVTOOLS_CONNECT_WINDOW
        } else {
            DEVTOOLS_CONNECT_WINDOW
        };
        let rdp = match connect_devtools(&target.devtools, window).await {
            Ok(rdp) => rdp,
            Err(_) if attached => return Err(target.devtools_unreachable_error()),
            Err(error) => return Err(error),
        };
        let root = rdp.request("root", "getRoot", json!({})).await?;
        let accessibility_actor = root["parentAccessibilityActor"]
            .as_str()
            .ok_or("Firefox DevTools did not expose the accessibility actor")?
            .to_string();
        rdp.request(&accessibility_actor, "enable", json!({}))
            .await?;

        let tree = bidi.send("browsingContext.getTree", json!({})).await?;
        let mut contexts: Vec<String> = tree["contexts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["context"].as_str().map(str::to_string))
            .collect();
        if contexts.is_empty() && attached {
            let created = bidi
                .send("browsingContext.create", json!({"type": "tab"}))
                .await?;
            contexts.push(
                created["context"]
                    .as_str()
                    .ok_or("Firefox did not return the created browsing context")?
                    .to_string(),
            );
        }
        let context = contexts
            .first()
            .ok_or("Firefox reported no browsing context")?
            .clone();
        let next_tab_id = contexts.len() as u32 + 1;
        let emulation = Arc::new(std::sync::RwLock::new(EmulationSettings {
            user_agent: options.user_agent.clone(),
            media: options.color_scheme.as_ref().map(|scheme| EmulatedMedia {
                media: None,
                features: vec![("prefers-color-scheme".to_string(), scheme.clone())],
            }),
            ..EmulationSettings::default()
        }));
        let dispatcher = Some(super::network::spawn_dispatcher(
            bidi.clone(),
            rdp.clone(),
            net.clone(),
            dispatch_events,
            emulation.clone(),
        ));
        let session = Self {
            process,
            headless: !attached && options.headless,
            bidi,
            rdp,
            accessibility_actor,
            tab_ids: contexts
                .into_iter()
                .enumerate()
                .map(|(index, context)| (context, index as u32 + 1))
                .collect(),
            context_parents: HashMap::new(),
            next_tab_id,
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
            events,
            net,
            intercepting: false,
            inflight: HashSet::new(),
            visited_origins: HashSet::new(),
            dispatcher,
        };
        let _ = emulation::apply_to_context(&session, &session.context).await;
        Ok(session)
    }

    /// Records the origin of a URL the session navigated to.
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
            || self.bidi.is_closed()
    }

    /// Returns whether this session attached to an existing Firefox.
    pub fn is_attached(&self) -> bool {
        self.process.is_none()
    }

    /// Ends the BiDi session and releases any owned Firefox process.
    pub async fn close(self) {
        let _ = self
            .bidi
            .send_timeout("session.end", json!({}), SHUTDOWN_TIMEOUT)
            .await;
    }

    /// Applies buffered BiDi events to the pending dialog slot and returns them for further handling.
    pub fn drain_events(&mut self, dialog: &mut Option<PendingDialog>) -> Vec<Value> {
        let mut drained = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            if e["method"] == "browsingContext.contextDestroyed"
                && e["params"]["context"] == self.top_context
            {
                self.top_context.clear();
            }
            apply_event(&e, dialog, &mut self.context_parents);
            drained.push(e);
        }
        drained
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
                        && e["params"]["handler"] != "accept"
                        && [context, &self.context, &self.top_context].iter().any(|active|
                            owner_tab(active, &parents)
                                == owner_tab(e["params"]["context"].as_str().unwrap_or_default(), &parents)
                        ) => return Ok(None),
                    Some(e) => apply_event(&e, &mut None, &mut parents),
                    None => return fut.await.map(Some),
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

/// Records frame parentage from browsing context lifecycle events.
pub(crate) fn record_context(e: &Value, context_parents: &mut HashMap<String, String>) {
    let p = &e["params"];
    match (
        e["method"].as_str(),
        p["context"].as_str(),
        p["parent"].as_str(),
    ) {
        (Some("browsingContext.contextCreated"), Some(context), Some(parent)) => {
            context_parents.insert(context.to_string(), parent.to_string());
        }
        (Some("browsingContext.contextDestroyed"), Some(context), _) => {
            context_parents.remove(context);
        }
        _ => {}
    }
}

/// Returns the top-level tab that owns `context`.
pub(crate) fn owner_tab<'a>(
    context: &'a str,
    context_parents: &'a HashMap<String, String>,
) -> &'a str {
    let mut owner = context;
    while let Some(parent) = context_parents
        .get(owner)
        .filter(|parent| !parent.is_empty())
    {
        owner = parent;
    }
    owner
}

/// True when a dialog with this owner context would block the tab `top_context`.
pub(crate) fn dialog_blocks_tab(owner_context: Option<&str>, top_context: &str) -> bool {
    owner_context.is_none_or(|context| context == top_context)
}

fn apply_event(
    e: &Value,
    dialog: &mut Option<PendingDialog>,
    context_parents: &mut HashMap<String, String>,
) {
    record_context(e, context_parents);
    let p = &e["params"];
    match e["method"].as_str() {
        Some("browsingContext.userPromptOpened") if p["handler"] != "accept" => {
            let prompt: UserPromptOpened = serde_json::from_value(p.clone()).unwrap_or_default();
            *dialog = Some(PendingDialog {
                dialog_type: prompt.kind.unwrap_or_else(|| "alert".to_string()),
                message: prompt.message,
                url: String::new(),
                default_prompt: prompt.default_value,
                owner_context: prompt
                    .context
                    .as_deref()
                    .map(|context| owner_tab(context, context_parents).to_string()),
                session_id: prompt.context,
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::Message;

    pub(crate) type Calls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
    pub(crate) type Messages = std::sync::Arc<std::sync::Mutex<Vec<Value>>>;

    async fn put<S: tokio::io::AsyncWrite + Unpin>(s: &mut S, v: Value) {
        let b = v.to_string();
        s.write_all(format!("{}:{}", b.len(), b).as_bytes())
            .await
            .unwrap();
    }

    async fn get<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> Option<Value> {
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

    /// A session with no process, a BiDi socket that answers an empty `success`
    /// to every command and an idle DevTools stream. The sender injects BiDi events
    /// and `Calls` records the methods the socket received.
    pub(crate) async fn fake_session() -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        fake_session_with(|_: &Value| json!({})).await
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

    /// A BiDi command reply: a `result` payload or a protocol `error`/`message` pair.
    pub(crate) enum FakeReply {
        Result(Value),
        Error {
            error: &'static str,
            message: &'static str,
        },
    }

    /// Same as `fake_session`, with `responder` deciding each command's `result`.
    pub(crate) async fn fake_session_with(
        responder: impl Fn(&Value) -> Value + Send + 'static,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        let (url, tx, calls, _) = fake_bidi_with(responder).await;
        let bidi = Arc::new(BidiClient::connect(&url).await.unwrap());
        (session_over_bidi(bidi).await, tx, calls)
    }

    /// Same as `fake_session_with`, but `responder` can also reject a command the way Firefox
    /// does with a BiDi `error` reply.
    pub(crate) async fn fake_session_with_replies(
        responder: impl Fn(&Value) -> FakeReply + Send + 'static,
    ) -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
    ) {
        let (url, tx, calls, _) = fake_bidi_with_replies(responder).await;
        let bidi = Arc::new(BidiClient::connect(&url).await.unwrap());
        (session_over_bidi(bidi).await, tx, calls)
    }

    async fn session_over_bidi(bidi: Arc<BidiClient>) -> FirefoxSession {
        let events = bidi.subscribe_buffered();
        let (a, mut b) = tokio::io::duplex(4096);
        let greeting = json!({"from":"root"}).to_string();
        b.write_all(format!("{}:{}", greeting.len(), greeting).as_bytes())
            .await
            .unwrap();
        std::mem::forget(b);
        let rdp = RdpClient::from_stream(a).await.unwrap();
        test_session(bidi, rdp, events)
    }

    async fn fake_bidi_with(
        responder: impl Fn(&Value) -> Value + Send + 'static,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
        Messages,
    ) {
        fake_bidi_with_replies(move |v: &Value| FakeReply::Result(responder(v))).await
    }

    async fn fake_bidi_with_replies(
        responder: impl Fn(&Value) -> FakeReply + Send + 'static,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
        Messages,
    ) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let calls = Calls::default();
        let log = calls.clone();
        let messages = Messages::default();
        let message_log = messages.clone();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            loop {
                tokio::select! {
                    m = ws.next() => {
                        let Some(Ok(Message::Text(t))) = m else { break };
                        let v: Value = serde_json::from_str(&t).unwrap();
                        log.lock().unwrap().push(v["method"].as_str().unwrap().to_string());
                        message_log.lock().unwrap().push(v.clone());
                        let reply = match responder(&v) {
                            FakeReply::Result(result) => {
                                json!({"type":"success","id":v["id"],"result":result})
                            }
                            FakeReply::Error { error, message } => json!({
                                "type": "error",
                                "id": v["id"],
                                "error": error,
                                "message": message
                            }),
                        };
                        ws.send(Message::Text(reply.to_string())).await.unwrap();
                    }
                    Some(ev) = rx.recv() => {
                        ws.send(Message::Text(ev.to_string())).await.unwrap();
                    }
                }
            }
        });
        (url, tx, calls, messages)
    }

    fn test_session(
        bidi: Arc<BidiClient>,
        rdp: RdpClient,
        events: mpsc::Receiver<Value>,
    ) -> FirefoxSession {
        FirefoxSession {
            process: None,
            headless: true,
            bidi,
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
            events,
            net: NetContext::default(),
            intercepting: false,
            inflight: HashSet::new(),
            visited_origins: HashSet::new(),
            dispatcher: None,
        }
    }

    /// A session whose BiDi socket has closed, as when its Firefox goes away.
    pub(crate) async fn fake_session_dead() -> FirefoxSession {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            drop(tokio_tungstenite::accept_async(s).await.unwrap());
        });
        let bidi = Arc::new(BidiClient::connect(&url).await.unwrap());
        tokio::time::timeout(Duration::from_secs(1), async {
            while !bidi.is_closed() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        session_over_bidi(bidi).await
    }

    /// Runs `attach` against a BiDi socket that answers `steps` in order and returns the error.
    async fn attach_error(
        steps: &[(&str, Value)],
        timeout: Duration,
        delayed_method: Option<&str>,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}/session", listener.local_addr().unwrap());
        let steps: Vec<(String, Value)> = steps
            .iter()
            .map(|(method, reply)| (method.to_string(), reply.clone()))
            .collect();
        let delayed_method = delayed_method.map(str::to_owned);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            for (method, reply) in steps {
                let message = tokio::time::timeout(Duration::from_secs(2), ws.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let request: Value = serde_json::from_str(&message.into_text().unwrap()).unwrap();
                assert_eq!(request["method"], method, "{request}");
                if delayed_method.as_deref() == Some(method.as_str()) {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                let response = match reply.get("error").and_then(Value::as_str) {
                    Some(error) => json!({
                        "type": "error",
                        "id": request["id"],
                        "error": error,
                        "message": reply["message"].as_str().unwrap_or(""),
                    }),
                    None => json!({
                        "type": "success",
                        "id": request["id"],
                        "result": reply,
                    }),
                };
                ws.send(Message::Text(response.to_string())).await.unwrap();
            }
        });
        let target = AttachTarget {
            bidi_url: url,
            devtools: DevtoolsEndpoint::Tcp {
                host: "127.0.0.1".into(),
                port: 6000,
            },
            devtools_explicit: false,
        };
        let error = FirefoxSession::initialize(
            &target,
            None,
            &LaunchOptions::default(),
            NetContext::default(),
            false,
            Some(tokio::time::Instant::now() + timeout),
        )
        .await
        .err()
        .unwrap();
        server.await.unwrap();
        error
    }

    #[tokio::test]
    async fn attach_timeout_ends_created_session() {
        let error = attach_error(
            &[
                (
                    "session.new",
                    json!({"capabilities": {"browserVersion": "150.0"}}),
                ),
                ("session.subscribe", json!({})),
                ("session.end", json!({})),
            ],
            Duration::from_millis(50),
            Some("session.subscribe"),
        )
        .await;
        assert!(error.contains("attach timed out"), "{error}");
    }

    #[tokio::test]
    async fn attach_ends_created_session_after_initialization_failure() {
        let error = attach_error(
            &[
                (
                    "session.new",
                    json!({"capabilities": {"browserVersion": "150.0"}}),
                ),
                (
                    "session.subscribe",
                    json!({"error": "unsupported operation", "message": "subscription rejected"}),
                ),
                ("session.end", json!({})),
            ],
            ATTACH_TIMEOUT,
            None,
        )
        .await;
        assert_eq!(error, "unsupported operation: subscription rejected");
    }

    #[tokio::test]
    async fn attach_reports_another_client_holding_the_bidi_session() {
        let error = attach_error(
            &[(
                "session.new",
                json!({
                    "error": "invalid argument",
                    "message": "Maximum number of active sessions reached",
                }),
            )],
            ATTACH_TIMEOUT,
            None,
        )
        .await;
        assert!(error.contains("Another client is attached"), "{error}");
    }

    #[tokio::test]
    async fn attach_rejects_unreachable_devtools_and_ends_the_bidi_session() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Keep the port reserved without an RDP greeting, so it cannot serve DevTools.
        let (url, _tx, calls, _) = fake_bidi_with(|v: &Value| match v["method"].as_str() {
            Some("session.new") => json!({"capabilities": {"browserVersion": "150.0"}}),
            _ => json!({}),
        })
        .await;
        let target =
            super::super::attach::parse_attach_url(&format!("{url}/session?devtools={port}"))
                .unwrap()
                .unwrap();
        let error = FirefoxSession::attach(
            &target,
            &LaunchOptions::default(),
            NetContext::default(),
            false,
        )
        .await
        .err()
        .expect("attach must fail when the required DevTools server is unreachable");
        assert_eq!(error, target.devtools_unreachable_error());
        let calls = calls.lock().unwrap();
        assert_eq!(calls.last().map(String::as_str), Some("session.end"));
        assert!(!calls
            .iter()
            .any(|method| method == "browsingContext.getTree"));
    }

    /// A DevTools server on TCP that answers the accessibility handshake and ignores the rest.
    async fn fake_devtools() -> DevtoolsEndpoint {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            put(&mut s, json!({"from":"root"})).await;
            while let Some(v) = get(&mut s).await {
                let to = v["to"].as_str().unwrap_or("");
                let reply = if v["type"] == "getRoot" {
                    json!({"from": "root", "parentAccessibilityActor": "acc"})
                } else {
                    json!({"from": to})
                };
                put(&mut s, reply).await;
            }
        });
        DevtoolsEndpoint::Tcp {
            host: "127.0.0.1".into(),
            port,
        }
    }

    #[tokio::test]
    async fn attach_creates_a_tab_when_firefox_reports_none() {
        let (url, _tx, calls, _) = fake_bidi_with(|v: &Value| match v["method"].as_str() {
            Some("session.new") => json!({"capabilities": {"browserVersion": "150.0"}}),
            Some("browsingContext.getTree") => json!({"contexts": []}),
            Some("browsingContext.create") => json!({"context": "ctx-new"}),
            _ => json!({}),
        })
        .await;
        let target = AttachTarget {
            bidi_url: url,
            devtools: fake_devtools().await,
            devtools_explicit: true,
        };
        let session = FirefoxSession::attach(
            &target,
            &LaunchOptions::default(),
            NetContext::default(),
            false,
        )
        .await
        .unwrap();
        assert_eq!(session.context, "ctx-new");
        assert!(calls
            .lock()
            .unwrap()
            .iter()
            .any(|method| method == "browsingContext.create"));
    }

    /// A session whose BiDi server records full messages, answers `browsingContext.create`
    /// with a new context and whose DevTools stream answers tab configuration requests.
    pub(crate) async fn fake_session_with_rdp() -> (
        FirefoxSession,
        tokio::sync::mpsc::UnboundedSender<Value>,
        Calls,
        Messages,
        Messages,
    ) {
        let (url, tx, calls, bidi_messages) = fake_bidi_with(|v: &Value| {
            if v["method"] == "browsingContext.create" {
                json!({"context": "ctx-new"})
            } else if v["method"] == "browsingContext.getTree" {
                json!({"contexts": [{"context": "ctx-active", "url": "https://example.com/", "userContext": "default"}]})
            } else {
                json!({})
            }
        })
        .await;
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
        let session = test_session(Arc::new(bidi), rdp, events);
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

    #[test]
    fn prompt_events_set_and_clear_the_dialog() {
        let mut d = None;
        let mut context_parents = HashMap::new();
        let frame = json!({"method":"browsingContext.contextCreated","params":{"context":"frame","parent":"top"}});
        apply_event(&frame, &mut d, &mut context_parents);
        let open = json!({"method":"browsingContext.userPromptOpened","params":{"context":"frame","type":"confirm","message":"sure?"}});
        apply_event(&open, &mut d, &mut context_parents);
        assert_eq!(d.as_ref().unwrap().dialog_type, "confirm");
        assert_eq!(d.as_ref().unwrap().session_id.as_deref(), Some("frame"));
        assert_eq!(d.as_ref().unwrap().owner_context.as_deref(), Some("top"));
        let other =
            json!({"method":"browsingContext.userPromptClosed","params":{"context":"other"}});
        apply_event(&other, &mut d, &mut context_parents);
        assert!(d.is_some());
        let close =
            json!({"method":"browsingContext.userPromptClosed","params":{"context":"frame"}});
        apply_event(&close, &mut d, &mut context_parents);
        assert!(d.is_none());
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
