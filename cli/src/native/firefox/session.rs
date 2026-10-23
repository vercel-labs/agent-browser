//! A running Firefox: the owned process, both protocol clients and per-session state.

use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::broadcast::{self, error::TryRecvError};

use super::bidi::{remote_to_json, BidiClient};
use super::process::{check_firefox_version, find_firefox, DevtoolsEndpoint, FirefoxProcess};
use super::rdp::RdpClient;
use crate::native::actions::PendingDialog;
use crate::native::cdp::chrome::LaunchOptions;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const DEVTOOLS_CONNECT_WINDOW: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const SUBSCRIBED_EVENTS: [&str; 2] = [
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
];

/** One launched Firefox. Dropping it kills the process tree and deletes the profile. */
pub struct FirefoxSession {
    process: Option<FirefoxProcess>,
    pub bidi: BidiClient,
    pub rdp: RdpClient,
    pub accessibility_actor: String,
    /// Active browsing context id: a top-level tab, or a frame after `frame <sel>`.
    pub context: String,
    /// Active top-level browsing context id (tab), restored by `frame main`.
    pub top_context: String,
    /// Stable `t<N>` ids keyed by BiDi context; assigned on first sight and never reused.
    pub tab_ids: HashMap<String, u32>,
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
    /// Finds and launches Firefox, opens the BiDi session and enables the accessibility actor.
    /// With `auto_dialog`, Firefox itself accepts alert and beforeunload prompts, like Chrome's default.
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
        let events = bidi.subscribe();
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
        bidi.send("session.subscribe", json!({"events": SUBSCRIBED_EVENTS}))
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
        Ok(Self {
            process: Some(process),
            bidi,
            rdp,
            accessibility_actor,
            tab_ids: HashMap::from([(context.clone(), 1)]),
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
            events,
        })
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

    /// True once the Firefox process has exited.
    pub fn has_exited(&mut self) -> bool {
        self.process
            .as_mut()
            .is_some_and(FirefoxProcess::has_exited)
    }

    /// Ends the BiDi session, then kills Firefox and deletes its profile.
    pub async fn close(self) {
        let _ = self
            .bidi
            .send_timeout("session.end", json!({}), SHUTDOWN_TIMEOUT)
            .await;
    }

    /// Applies buffered BiDi events to the pending dialog slot.
    pub fn drain_events(&mut self, dialog: &mut Option<PendingDialog>) {
        loop {
            match self.events.try_recv() {
                Ok(e) => apply_event(&e, dialog),
                Err(TryRecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    }

    /// Runs `fut` until it finishes or a prompt opens in `context` or its owning tab. Returns `None` for the prompt.
    pub async fn race_prompt<T>(
        &self,
        context: &str,
        fut: impl Future<Output = Result<T, String>>,
    ) -> Result<Option<T>, String> {
        let mut events = self.bidi.subscribe();
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return r.map(Some),
                e = events.recv() => match e {
                    Ok(e) if e["method"] == "browsingContext.userPromptOpened"
                        && e["params"]["handler"] != "accept"
                        && [context, &self.context, &self.top_context]
                            .contains(&e["params"]["context"].as_str().unwrap_or_default()) => return Ok(None),
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

fn apply_event(e: &Value, dialog: &mut Option<PendingDialog>) {
    let p = &e["params"];
    match e["method"].as_str() {
        Some("browsingContext.userPromptOpened") if p["handler"] != "accept" => {
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

    /// Same as `fake_session`, with `responder` deciding each command's `result`.
    pub(crate) async fn fake_session_with(
        responder: impl Fn(&Value) -> Value + Send + 'static,
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
                        let reply =
                            json!({"type":"success","id":v["id"],"result":responder(&v)});
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
        let (a, mut b) = tokio::io::duplex(4096);
        let greeting = json!({"from":"root"}).to_string();
        b.write_all(format!("{}:{}", greeting.len(), greeting).as_bytes())
            .await
            .unwrap();
        std::mem::forget(b);
        let rdp = RdpClient::from_stream(a).await.unwrap();
        let session = FirefoxSession {
            process: None,
            bidi,
            rdp,
            accessibility_actor: "acc".into(),
            context: "ctx-active".into(),
            tab_ids: HashMap::from([("ctx-active".into(), 1)]),
            next_tab_id: 2,
            top_context: "ctx-active".into(),
            refs: HashMap::new(),
            next_node_id: 0,
            frames: None,
            documents: HashMap::new(),
            tab_labels: HashMap::new(),
            touches: Vec::new(),
            shadow_ids: Default::default(),
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

    #[test]
    fn prompt_events_set_and_clear_the_dialog() {
        let mut d = None;
        let open = json!({"method":"browsingContext.userPromptOpened","params":{"context":"c1","type":"confirm","message":"sure?"}});
        apply_event(&open, &mut d);
        assert_eq!(d.as_ref().unwrap().dialog_type, "confirm");
        assert_eq!(d.as_ref().unwrap().session_id.as_deref(), Some("c1"));
        let other = json!({"method":"browsingContext.userPromptClosed","params":{"context":"c2"}});
        apply_event(&other, &mut d);
        assert!(d.is_some());
        let close = json!({"method":"browsingContext.userPromptClosed","params":{"context":"c1"}});
        apply_event(&close, &mut d);
        assert!(d.is_none());
    }

    #[tokio::test]
    async fn race_prompt_returns_none_when_a_prompt_opens_on_the_active_context() {
        let (s, tx, _) = fake_session().await;
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
        let ours = json!({"type":"event","method":"browsingContext.userPromptOpened","params":{"context":"ctx-active"}});
        tx.send(ours).unwrap();
        assert!(racer.await.unwrap().is_none());
    }
}
