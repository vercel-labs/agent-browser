use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::client::{remote_to_json, BidiClient};
use super::emulation::{self, Settings as EmulationSettings};
use super::firefox::{check_firefox_version, find_firefox, DevtoolsEndpoint, FirefoxProcess};
use super::firefox_devtools::RdpClient;
use super::network::{AbortOnDrop, NetContext, NETWORK_EVENTS};
use super::types::UserPromptOpened;
use crate::native::actions::{EmulatedMedia, PendingDialog};
use crate::native::cdp::chrome::LaunchOptions;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const DEVTOOLS_CONNECT_WINDOW: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const CONTEXT_EVENTS: [&str; 2] = [
    "browsingContext.contextCreated",
    "browsingContext.contextDestroyed",
];
const PROMPT_EVENTS: [&str; 2] = [
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
];

/// One launched Firefox. Dropping it kills the process tree and deletes the profile.
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
            Ok(c) => return Ok(c),
            Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
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
            "unhandledPromptBehavior": if auto_dialog {
                json!({"default": "ignore", "alert": "accept", "beforeUnload": "accept"})
            } else {
                json!({"default": "ignore"})
            },
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
            process: Some(process),
            headless: options.headless,
            bidi,
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
            || self.bidi.reader.is_finished()
    }

    /// Ends the BiDi session, then kills Firefox and deletes its profile.
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
    while let Some(parent) = context_parents.get(owner) {
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

fn owner_context<'a>(mut context: &'a str, parents: &'a HashMap<String, String>) -> &'a str {
    while let Some(parent) = parents.get(context).filter(|parent| !parent.is_empty()) {
        context = parent;
    }
    context
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
        fake_session_with_replies(move |v: &Value| FakeReply::Result(responder(v))).await
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
            net: NetContext::default(),
            intercepting: false,
            inflight: HashSet::new(),
            visited_origins: HashSet::new(),
            dispatcher: None,
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
