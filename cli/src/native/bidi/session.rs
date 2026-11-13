use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::{mpsc, Notify};

use super::attach::AttachTarget;
use super::client::{remote_to_json, BidiClient};
use super::contexts::{Contexts, Removed};
use super::emulation::{self, Settings as EmulationSettings};
use super::firefox::{check_firefox_version, find_firefox, DevtoolsEndpoint, FirefoxProcess};
use super::firefox_devtools::RdpClient;
use super::network::{AbortOnDrop, NetContext, NETWORK_EVENTS};
use super::permissions;
use super::types::UserPromptOpened;
use crate::native::actions::{EmulatedMedia, PendingDialog};
use crate::native::cdp::chrome::LaunchOptions;
use crate::session_settings::Key;

const START_TIMEOUT: Duration = Duration::from_secs(30);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(15);
const DEVTOOLS_CONNECT_WINDOW: Duration = Duration::from_secs(10);
const ATTACH_DEVTOOLS_CONNECT_WINDOW: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const ATTACHED_RESET_TIMEOUT: Duration = Duration::from_secs(1);
const CONTEXT_EVENTS: [&str; 3] = [
    "browsingContext.contextCreated",
    "browsingContext.contextDestroyed",
    "browsingContext.domContentLoaded",
];
const PROMPT_EVENTS: [&str; 2] = [
    "browsingContext.userPromptOpened",
    "browsingContext.userPromptClosed",
];

/// A Firefox session. Dropping it releases protocol clients and kills only an owned process.
pub struct FirefoxSession {
    process: Option<FirefoxProcess>,
    headless: bool,
    pub bidi: Arc<BidiClient>,
    pub auto_dialog: Arc<AtomicBool>,
    pub auto_dialog_changed: Arc<Notify>,
    pub rdp: RdpClient,
    pub contexts: Contexts,
    /// Durable ref ids keyed by `(context, sharedId)`; reset when a document changes.
    pub refs: HashMap<(String, String), i64>,
    pub next_node_id: i64,
    pub frames: Option<super::snapshot::FrameWatch>,
    /// Document node `sharedId` last snapshotted per context, used to reset `refs` on a new document.
    pub documents: HashMap<String, String>,
    pub ref_documents: HashMap<String, String>,
    /// Touch points of the active touch sequence, released by a later `input_touch` end.
    pub touches: Vec<(f64, f64)>,
    /// Durable ids of nodes inside open shadow roots, which a stale ref must never recover by role and name.
    pub shadow_ids: std::collections::HashSet<i64>,
    /// Emulation applied to the active tab and replayed onto new tabs.
    pub emulation: Arc<std::sync::RwLock<EmulationSettings>>,
    /// Permissions the session granted, given to each origin as its tabs load it.
    pub permissions: Arc<tokio::sync::Mutex<permissions::Grants>>,
    /// Launch download directory, restored after `download` redirects it.
    pub download_dir: Option<String>,
    /// Settings this session changed in a browser it does not own.
    /// The download folder set on a browser someone else runs. Firefox keeps it and the
    /// emulation overrides after `session.end`, so close puts them back.
    attached_download_path: Option<String>,
    events: mpsc::Receiver<Value>,
    pub net: NetContext,
    /// True once the single global network intercept is installed; it is never installed twice.
    pub intercepting: bool,
    /// Unfinished requests mapped to their reported browsing contexts.
    pub inflight: HashMap<String, Option<String>>,
    /// Origins of pages this session navigated to, kept so state saves still cover them after the tab moves on.
    pub visited_origins: HashSet<String>,
    _dispatcher: Option<AbortOnDrop>,
    /// Version Firefox reported when the session opened, used for HAR browser metadata.
    pub browser_version: String,
}

/// The tab the user is looking at in a running Firefox: the focused one, else a visible one, else
/// any tab that runs scripts. `None` when no tab does, as with the privileged `about:home` of a
/// fresh profile.
async fn selected_tab(bidi: &BidiClient, tree: &Value) -> Option<String> {
    const SELECTED_JS: &str =
        "() => 1 + (document.hasFocus() ? 4 : 0) + (document.visibilityState === 'visible' ? 2 : 0)";
    let tabs: Vec<&str> = tree["contexts"]
        .as_array()?
        .iter()
        .filter_map(|tab| tab["context"].as_str())
        .collect();
    let scores = futures_util::future::join_all(
        tabs.iter()
            .map(|tab| bidi.call_function_timeout(tab, SELECTED_JS, ATTACHED_RESET_TIMEOUT)),
    )
    .await;
    let scores: Vec<u64> = scores
        .iter()
        .map(|score| score.as_ref().ok().and_then(Value::as_u64).unwrap_or(0))
        .collect();
    let best = scores.iter().copied().max().filter(|score| *score > 0)?;
    let index = scores.iter().position(|score| *score == best)?;
    Some(tabs[index].to_string())
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
            "unhandledPromptBehavior": {"default": "ignore"},
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
                    "Firefox at {} already has a WebDriver BiDi session. End it from the client that holds it, or restart Firefox: a session whose client exited without ending it stays until Firefox restarts.",
                    super::attach::target_label(&target.bidi_url)
                ));
            }
            result => result?,
        };
        let cleanup_bidi = bidi.clone();
        let result = within_attach_deadline(
            deadline,
            Self::open_session(
                bidi,
                target,
                process,
                options,
                net,
                auto_dialog,
                events,
                created,
            ),
        )
        .await;
        let mut session = match result {
            Ok(session) => session,
            Err(error) => {
                if attached {
                    let _ = cleanup_bidi
                        .send_timeout("session.end", json!({}), SHUTDOWN_TIMEOUT)
                        .await;
                }
                return Err(error);
            }
        };
        let in_place = [Key::UserAgent, Key::DownloadPath];
        if let Err(error) = session.apply_attached_settings(options, &in_place).await {
            session.close().await;
            return Err(error);
        }
        Ok(session)
    }

    #[allow(clippy::too_many_arguments)]
    async fn open_session(
        bidi: Arc<BidiClient>,
        target: &AttachTarget,
        process: Option<FirefoxProcess>,
        options: &LaunchOptions,
        net: NetContext,
        auto_dialog: bool,
        events: mpsc::Receiver<Value>,
        created: Value,
    ) -> Result<Self, String> {
        let attached = process.is_none();
        let dispatch_events = bidi.subscribe_buffered();
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
        let mut contexts = if attached {
            Contexts::from_tree(&tree).unwrap_or_default()
        } else {
            Contexts::from_tree(&tree)?
        };
        if attached {
            let selected = match selected_tab(&bidi, &tree).await {
                Some(selected) => selected,
                None => {
                    let created = bidi
                        .send("browsingContext.create", json!({"type": "tab"}))
                        .await?;
                    contexts.record(&created);
                    created["context"]
                        .as_str()
                        .ok_or("Firefox did not return the created browsing context")?
                        .to_string()
                }
            };
            contexts.activate(&selected);
        }
        let emulation = Arc::new(std::sync::RwLock::new(EmulationSettings {
            viewport: options
                .viewport_size
                .filter(|_| !attached)
                .map(|(width, height)| (i64::from(width), i64::from(height), 1.0)),
            user_agent: options.user_agent.clone(),
            media: options.color_scheme.as_ref().map(|scheme| EmulatedMedia {
                media: None,
                features: vec![("prefers-color-scheme".to_string(), scheme.clone())],
            }),
            ..EmulationSettings::default()
        }));
        let permissions = Arc::new(tokio::sync::Mutex::new(permissions::Grants::default()));
        let dispatcher = Some(super::network::spawn_dispatcher(
            bidi.clone(),
            net.clone(),
            dispatch_events,
            emulation.clone(),
            permissions.clone(),
        ));
        let mut session = Self {
            process,
            headless: !attached && options.headless,
            bidi,
            auto_dialog: Arc::new(AtomicBool::new(auto_dialog)),
            auto_dialog_changed: Arc::new(Notify::new()),
            rdp,
            contexts,
            refs: HashMap::new(),
            next_node_id: 0,
            frames: None,
            documents: HashMap::new(),
            ref_documents: HashMap::new(),
            touches: Vec::new(),
            shadow_ids: Default::default(),
            emulation,
            permissions,
            download_dir: options.download_path.clone(),
            attached_download_path: None,
            events,
            net,
            intercepting: false,
            inflight: HashMap::new(),
            visited_origins: HashSet::new(),
            _dispatcher: dispatcher,
            browser_version,
        };
        if let Some(contexts) = tree["contexts"].as_array() {
            session.record_context_origins(contexts);
        }
        emulation::apply_to_context(&session, session.contexts.active()).await?;
        Ok(session)
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

    /// Rebuilds what events keep current when events were dropped or no context is active: the
    /// context tree is read again, and after a drop the unfinished requests are forgotten, since
    /// their completions may be among the lost events. Returns true when the active context
    /// changed.
    pub async fn recover_lost_events(
        &mut self,
        dialog: &mut Option<PendingDialog>,
    ) -> Result<bool, String> {
        let lost = self.bidi.take_event_loss();
        if !lost && !self.contexts.active().is_empty() {
            return Ok(false);
        }
        let tree = self.bidi.send("browsingContext.getTree", json!({})).await?;
        let removed = self.contexts.sync(&tree);
        if lost {
            self.inflight.clear();
        }
        Ok(self.forget(removed, dialog))
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
            || self.bidi.is_closed()
    }

    /// Returns whether this session attached to a Firefox someone else runs.
    fn is_attached(&self) -> bool {
        self.process.is_none()
    }

    /// Whether downloaded files land on this machine, which only a browser this
    /// session launched guarantees.
    pub fn downloads_are_local(&self) -> bool {
        self.process.is_some()
    }

    /// Whether the default idle timeout must leave this browser running: the daemon
    /// never stops a headed browser or one it does not own on a timer.
    pub fn blocks_default_idle_shutdown(&self) -> bool {
        self.is_attached() || !self.headless
    }

    /// The tabs that are open now, so an override covers every one of them.
    async fn open_tabs(&self, timeout: Duration) -> Result<Vec<String>, String> {
        let tree = self
            .bidi
            .send_timeout("browsingContext.getTree", json!({"maxDepth": 0}), timeout)
            .await?;
        Ok(tree["contexts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|context| context["context"].as_str().map(str::to_owned))
            .collect())
    }

    /// Applies those of `changed` an attached Firefox takes in place and records what close has
    /// to put back. A setting that did not change is left alone, so it cannot undo what a
    /// command such as `useragent` set since.
    pub async fn apply_attached_settings(
        &mut self,
        options: &LaunchOptions,
        changed: &[Key],
    ) -> Result<(), String> {
        if !self.is_attached() {
            return Ok(());
        }
        if changed.contains(&Key::UserAgent)
            && self.emulation.read().unwrap().user_agent != options.user_agent
        {
            let contexts = self.open_tabs(START_TIMEOUT).await?;
            if !contexts.is_empty() {
                self.bidi
                    .send(
                        "emulation.setUserAgentOverride",
                        json!({"userAgent": options.user_agent, "contexts": contexts}),
                    )
                    .await?;
            }
            self.emulation.write().unwrap().user_agent = options.user_agent.clone();
        }
        if changed.contains(&Key::DownloadPath)
            && self.attached_download_path != options.download_path
        {
            self.set_download_behavior(options.download_path.as_deref())
                .await?;
            self.attached_download_path = options.download_path.clone();
        }
        Ok(())
    }

    /// Points the attached browser's downloads at `path`, or back to its own
    /// setting for `None`.
    async fn set_download_behavior(&self, path: Option<&str>) -> Result<(), String> {
        let behavior = match path {
            Some(path) => json!({"type": "allowed", "destinationFolder": path}),
            None => Value::Null,
        };
        self.bidi
            .send(
                "browser.setDownloadBehavior",
                json!({"downloadBehavior": behavior}),
            )
            .await
            .map(|_| ())
            .map_err(|error| {
                format!("Unable to apply --download-path to attached Firefox: {error}")
            })
    }

    /// Undoes the download folder and the emulation overrides this session holds, in every
    /// open tab of a browser it does not own.
    async fn undo_attached_changes(&self) {
        if !self.is_attached() {
            return;
        }
        if self.attached_download_path.is_some() {
            let _ = self
                .bidi
                .send_timeout(
                    "browser.setDownloadBehavior",
                    json!({"downloadBehavior": null}),
                    ATTACHED_RESET_TIMEOUT,
                )
                .await;
        }
        let contexts = self
            .open_tabs(ATTACHED_RESET_TIMEOUT)
            .await
            .unwrap_or_default();
        if contexts.is_empty() {
            return;
        }
        let settings = self.emulation.read().unwrap().clone();
        for (method, params) in emulation::resets(&settings, &contexts) {
            let _ = self
                .bidi
                .send_timeout(method, params, ATTACHED_RESET_TIMEOUT)
                .await;
        }
    }

    /// Undoes what it changed, ends the BiDi session and releases any owned Firefox process.
    pub async fn close(self) {
        self.undo_attached_changes().await;
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
            let removed = self.contexts.apply(&e);
            self.forget(removed, dialog);
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
                &self.contexts,
                self.auto_dialog.load(Ordering::Acquire),
            );
            drained.push(e);
        }
        drained
    }

    /// Tracks request lifetimes.
    pub(super) fn track_request(&mut self, event: &Value) {
        let params = &event["params"];
        let id = params["request"]["request"].as_str();
        match event["method"].as_str() {
            Some("network.beforeRequestSent") => {
                let url = params["request"]["url"].as_str().unwrap_or("");
                if !super::network::is_websocket(url) {
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
        self.contexts.tab_of(context) == Some(self.contexts.active_tab())
    }

    /// Records the frame tree under the active tab so frames created since the last drain can be placed.
    pub(crate) async fn refresh_frame_tree(&mut self) -> Result<(), String> {
        let tree = self
            .bidi
            .send(
                "browsingContext.getTree",
                json!({"root": self.contexts.active_tab()}),
            )
            .await?;
        let contexts = tree["contexts"].as_array().cloned().unwrap_or_default();
        for info in &contexts {
            self.contexts.record(info);
        }
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

    /// Forgets a browsing context that is gone and everything nested in it: element refs,
    /// documents, shadow ids, unfinished requests and any prompt they owned. Returns true when the
    /// active context was among them, which the registry then replaces.
    pub fn forget_context(&mut self, context: &str, dialog: &mut Option<PendingDialog>) -> bool {
        let removed = self.contexts.remove(context);
        self.forget(removed, dialog)
    }

    fn forget(&mut self, removed: Removed, dialog: &mut Option<PendingDialog>) -> bool {
        let gone = &removed.contexts;
        let stale_ids: HashSet<i64> = self
            .refs
            .iter()
            .filter(|((id, _), _)| gone.contains(id))
            .map(|(_, id)| *id)
            .collect();
        self.refs.retain(|(id, _), _| !gone.contains(id));
        self.documents.retain(|id, _| !gone.contains(id));
        self.ref_documents.retain(|id, _| !gone.contains(id));
        self.shadow_ids.retain(|id| !stale_ids.contains(id));
        self.inflight
            .retain(|_, owner| !owner.as_ref().is_some_and(|owner| gone.contains(owner)));
        if dialog.as_ref().is_some_and(|d| {
            [&d.owner_context, &d.session_id]
                .into_iter()
                .flatten()
                .any(|owner| gone.contains(owner))
        }) {
            *dialog = None;
        }
        if removed.active_changed {
            self.forget_refs();
        }
        removed.active_changed
    }

    /// Drops every element ref along with the documents and shadow ids they were recorded for.
    pub fn forget_refs(&mut self) {
        self.refs.clear();
        self.documents.clear();
        self.shadow_ids.clear();
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
        let mut contexts = self.contexts.clone();
        tokio::pin!(fut);
        loop {
            tokio::select! {
                r = &mut fut => return r.map(Some),
                e = events.recv() => match e {
                    Some(e) if e["method"] == "browsingContext.userPromptOpened"
                        && !auto_handles_prompt(&e["params"], self.auto_dialog.load(Ordering::Acquire))
                        && [context, contexts.active()].iter().any(|active|
                            contexts.tab_of(active)
                                == contexts.tab_of(e["params"]["context"].as_str().unwrap_or_default())
                        ) => return Ok(None),
                    Some(e) => {
                        contexts.apply(&e);
                    }
                    None => return fut.await.map(Some),
                },
            }
        }
    }

    /// Evaluates `expression` in the active context and returns the decoded value.
    pub async fn eval(&self, expression: &str) -> Result<Value, String> {
        self.eval_in_context(expression, self.contexts.active(), false)
            .await
    }

    /// Evaluates `expression` in the active top-level tab and returns the decoded value.
    pub async fn eval_top(&self, expression: &str) -> Result<Value, String> {
        self.eval_in_context(expression, self.contexts.active_tab(), false)
            .await
    }

    /// Like `eval`, but a script exception yields `null` so callers can keep polling.
    pub async fn eval_lenient(&self, expression: &str) -> Result<Value, String> {
        self.eval_in_context(expression, self.contexts.active(), true)
            .await
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
            .send(
                "browsingContext.getTree",
                json!({"root": self.contexts.active_tab()}),
            )
            .await?;
        Ok(tree["contexts"][0]["url"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }
}

/// True when a dialog with this owner context would block the tab `top_context`.
pub(crate) fn dialog_blocks_tab(owner_context: Option<&str>, top_context: &str) -> bool {
    owner_context.is_none_or(|context| context == top_context)
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
    contexts: &Contexts,
    auto_dialog: bool,
) {
    let p = &e["params"];
    match e["method"].as_str() {
        Some("browsingContext.userPromptOpened") if !auto_handles_prompt(p, auto_dialog) => {
            let prompt: UserPromptOpened = serde_json::from_value(p.clone()).unwrap_or_default();
            *dialog = Some(PendingDialog {
                dialog_type: prompt.kind.unwrap_or_else(|| "alert".to_string()),
                message: prompt.message,
                url: String::new(),
                default_prompt: prompt.default_value,
                owner_context: prompt
                    .context
                    .as_deref()
                    .map(|context| contexts.tab_of(context).unwrap_or(context).to_string()),
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

    pub(crate) fn active_only() -> Contexts {
        Contexts::from_tree(&json!({"contexts": [{"context": "ctx-active"}]})).unwrap()
    }

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
        let (url, tx, calls, _) = fake_bidi_with(responder).await;
        let bidi = Arc::new(BidiClient::connect(&url).await.unwrap());
        (session_over_bidi(bidi).await, tx, calls)
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
        (url, tx, calls, messages)
    }

    fn test_session(
        bidi: Arc<BidiClient>,
        rdp: RdpClient,
        events: mpsc::Receiver<Value>,
    ) -> FirefoxSession {
        FirefoxSession {
            process: None,
            headless: false,
            bidi,
            auto_dialog: Arc::new(AtomicBool::new(false)),
            auto_dialog_changed: Arc::new(Notify::new()),
            rdp,
            contexts: active_only(),
            refs: HashMap::new(),
            next_node_id: 0,
            frames: None,
            documents: HashMap::new(),
            ref_documents: HashMap::new(),
            touches: Vec::new(),
            shadow_ids: Default::default(),
            emulation: Arc::default(),
            permissions: Arc::default(),
            download_dir: None,
            attached_download_path: None,
            visited_origins: HashSet::new(),
            events,
            net: NetContext::default(),
            intercepting: false,
            inflight: HashMap::new(),
            _dispatcher: None,
            browser_version: "159.0".into(),
        }
    }

    #[tokio::test]
    async fn attached_launch_options_are_undone_before_session_end() {
        for (download_path, user_agent, reject) in [
            (None, None, false),
            (Some("/downloads"), None, false),
            (Some("/downloads"), None, true),
            (None, Some("Agent/1.0"), false),
        ] {
            let (url, _tx, _calls, messages) = fake_bidi_with_replies(move |message| {
                if reject && message["method"] == "browser.setDownloadBehavior" {
                    FakeReply::Error {
                        error: "unknown error",
                        message: "failed",
                    }
                } else if message["method"] == "browsingContext.getTree" {
                    FakeReply::Result(json!({"contexts": [{"context": "ctx-active"}]}))
                } else {
                    FakeReply::Result(json!({}))
                }
            })
            .await;
            let bidi = Arc::new(BidiClient::connect(&url).await.unwrap());
            let mut session = session_over_bidi(bidi).await;
            let options = LaunchOptions {
                download_path: download_path.map(str::to_string),
                user_agent: user_agent.map(str::to_string),
                ..LaunchOptions::default()
            };
            assert_eq!(
                session
                    .apply_attached_settings(&options, &[Key::UserAgent, Key::DownloadPath])
                    .await
                    .is_err(),
                reject
            );
            session.close().await;
            let messages = messages.lock().unwrap();
            let commands: Vec<_> = messages
                .iter()
                .map(|message| (message["method"].clone(), message["params"].clone()))
                .collect();
            let mut expected = Vec::new();
            if download_path.is_some() {
                expected.push((
                    json!("browser.setDownloadBehavior"),
                    json!({"downloadBehavior": {"type": "allowed", "destinationFolder": "/downloads"}}),
                ));
                if !reject {
                    expected.push((
                        json!("browser.setDownloadBehavior"),
                        json!({"downloadBehavior": null}),
                    ));
                }
            }
            if user_agent.is_some() {
                expected.push((json!("browsingContext.getTree"), json!({"maxDepth": 0})));
                expected.push((
                    json!("emulation.setUserAgentOverride"),
                    json!({"userAgent": "Agent/1.0", "contexts": ["ctx-active"]}),
                ));
            }
            expected.push((json!("browsingContext.getTree"), json!({"maxDepth": 0})));
            if user_agent.is_some() {
                expected.push((
                    json!("emulation.setUserAgentOverride"),
                    json!({"userAgent": null, "contexts": ["ctx-active"]}),
                ));
            }
            expected.push((json!("session.end"), json!({})));
            assert_eq!(commands, expected);
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
    async fn attach_reports_the_session_another_client_holds() {
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
        assert!(
            error.contains("already has a WebDriver BiDi session"),
            "{error}"
        );
        assert!(error.contains("restart Firefox"), "{error}");
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
        let options = LaunchOptions {
            viewport_size: Some((1280, 720)),
            ..LaunchOptions::default()
        };
        let session = FirefoxSession::attach(&target, &options, NetContext::default(), false)
            .await
            .unwrap();
        assert_eq!(session.contexts.active(), "ctx-new");
        let calls = calls.lock().unwrap();
        assert!(calls
            .iter()
            .any(|method| method == "browsingContext.create"));
        assert!(
            !calls
                .iter()
                .any(|method| method == "browsingContext.setViewport"),
            "attaching must not resize a tab the user owns"
        );
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

    /// A session whose DevTools stream replies through `responder` and follows each
    /// `evaluateJSAsync` reply with an `evaluationResult` event carrying the reply's `result`.
    pub(crate) async fn fake_session_with_rdp_replies(
        responder: impl Fn(&Value) -> Value + Send + 'static,
    ) -> (FirefoxSession, Messages) {
        let (url, _tx, _calls, _) = fake_bidi_with(|v: &Value| {
            if v["method"] == "browsingContext.getTree" {
                json!({"contexts": [{"context": "ctx-active"}]})
            } else {
                json!({})
            }
        })
        .await;
        let bidi = Arc::new(BidiClient::connect(&url).await.unwrap());
        let events = bidi.subscribe_buffered();
        let (a, mut b) = tokio::io::duplex(65536);
        let rdp_messages = Messages::default();
        let log = rdp_messages.clone();
        tokio::spawn(async move {
            put(&mut b, json!({"from": "root"})).await;
            while let Some(v) = get(&mut b).await {
                log.lock().unwrap().push(v.clone());
                let reply = responder(&v);
                put(&mut b, reply.clone()).await;
                if v["type"] == "evaluateJSAsync" {
                    put(
                        &mut b,
                        json!({
                            "from": v["to"],
                            "type": "evaluationResult",
                            "resultID": reply["resultID"],
                            "result": reply.get("result").cloned().unwrap_or(Value::Null),
                            "exception": reply.get("exception").cloned().unwrap_or(Value::Null),
                        }),
                    )
                    .await;
                }
            }
        });
        let rdp = RdpClient::from_stream(a).await.unwrap();
        (test_session(bidi, rdp, events), rdp_messages)
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
    async fn destroyed_contexts_reconcile_tabs_and_active_state() {
        let (mut session, tx, _) = fake_session().await;
        session.contexts.record(&json!({"context": "survivor"}));
        session.contexts.set_label("ctx-active", "docs");
        session
            .contexts
            .record(&json!({"context": "child", "parent": "ctx-active"}));
        session.contexts.activate("child");
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
                crate::native::bidi::network::drain(&mut state).await;
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
        assert_eq!(ff.contexts.active(), "ctx-active");
        assert!(state.active_frame_id.is_none());
        assert!(ff.refs.is_empty());
        assert!(ff.documents.is_empty());
        assert!(ff.shadow_ids.is_empty());
        assert!(state.ref_map.get("e1").is_none());

        tx.send(json!({"type":"event", "method":"browsingContext.contextDestroyed", "params":{"context":"ctx-active", "parent":null}})).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                crate::native::bidi::network::drain(&mut state).await;
                if state.firefox.as_ref().unwrap().contexts.active() == "survivor" {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let ff = state.firefox.as_ref().unwrap();
        assert!(ff.contexts.tab("ctx-active").is_none());
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
                crate::native::bidi::network::drain(&mut state).await;
                if state
                    .firefox
                    .as_ref()
                    .unwrap()
                    .contexts
                    .tab("third")
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            state.firefox.as_ref().unwrap().contexts.active(),
            "survivor"
        );
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
    async fn dropped_events_rebuild_the_context_tree_and_forget_unfinished_requests() {
        let (mut ff, _tx, _) = fake_session_with(|request| match request["method"].as_str() {
            Some("browsingContext.getTree") => json!({"contexts": [{"context": "ctx-next"}]}),
            _ => json!({}),
        })
        .await;
        ff.inflight
            .insert("unfinished".into(), Some("ctx-active".into()));
        assert!(!ff.recover_lost_events(&mut None).await.unwrap());
        assert_eq!(ff.inflight.len(), 1);

        ff.bidi.lost_events.store(true, Ordering::Relaxed);
        assert!(ff.recover_lost_events(&mut None).await.unwrap());
        assert!(ff.inflight.is_empty());
        assert_eq!(ff.contexts.active(), "ctx-next");
        assert!(!ff.recover_lost_events(&mut None).await.unwrap());
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
        session
            .contexts
            .record(&json!({"context": "ctx-frame", "parent": "ctx-active"}));
        session.contexts.activate("ctx-frame");

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
        let contexts = Contexts::from_tree(
            &json!({"contexts": [{"context": "top", "children": [{"context": "frame"}]}]}),
        )
        .unwrap();
        let open = json!({"method":"browsingContext.userPromptOpened","params":{"context":"frame","type":"confirm","message":"sure?"}});
        apply_event(&open, &mut d, &contexts, false);
        assert_eq!(d.as_ref().unwrap().dialog_type, "confirm");
        assert_eq!(d.as_ref().unwrap().session_id.as_deref(), Some("frame"));
        assert_eq!(d.as_ref().unwrap().owner_context.as_deref(), Some("top"));
        let other =
            json!({"method":"browsingContext.userPromptClosed","params":{"context":"other"}});
        apply_event(&other, &mut d, &contexts, false);
        assert!(d.is_some());
        let close =
            json!({"method":"browsingContext.userPromptClosed","params":{"context":"frame"}});
        apply_event(&close, &mut d, &contexts, false);
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
                        &active_only(),
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
        for tab in [
            json!({"context": "ctx-active", "children": [{"context": "frame", "children": [{"context": "nested"}]}]}),
            json!({"context": "other"}),
        ] {
            session.contexts.record(&tab);
        }
        for (id, context) in [("background", Some("other")), ("unknown", None)] {
            session.track_request(&json!({
                "method": "network.beforeRequestSent",
                "params": {"context": context, "request": {"request": id, "url": "http://example.com/"}},
            }));
        }
        assert!(!session.has_inflight_in_active_tab());
        session.contexts.activate("other");
        assert!(session.has_inflight_in_active_tab());
        session.track_request(&json!({
            "method": "network.fetchError",
            "params": {"request": {"request": "background"}},
        }));
        assert!(!session.has_inflight_in_active_tab());
        session.contexts.activate("frame");
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
        session.forget_context("frame", &mut None);
        assert!(!session.inflight.contains_key("destroyed"));
        assert!(!session.has_inflight_in_active_tab());
    }

    #[tokio::test]
    async fn race_prompt_returns_none_when_a_prompt_opens_on_the_active_context() {
        let (mut s, tx, _) = fake_session().await;
        s.contexts
            .record(&json!({"context": "frame-parent", "parent": "ctx-active"}));
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
