use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, watch, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;

use super::buffer::EventBuffer;
use super::envelope::{EventEnvelope, EventTab};
use super::source::{EventSource, SourceSignal};
use super::sources::dom::{DomObserver, WORLD_NAME};
use crate::native::cdp::client::{CdpClient, RawCdpMessage};
use crate::native::network::DomainFilter;
use crate::native::policy::ActionPolicy;

const MAX_ACTIVE: usize = 20;
const MAX_LISTED: usize = 50;
const RETENTION: Duration = Duration::from_secs(600);
#[cfg(test)]
const MAX_TTL: Duration = Duration::from_secs(1_800);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SubscriptionStatus {
    Active,
    Ended,
}

/// Subscription lifecycle, keeping the end reason and time together.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SubscriptionLifecycle {
    Active,
    Ended { reason: String, at: Instant },
}

impl SubscriptionLifecycle {
    fn is_active(&self) -> bool {
        matches!(self, Self::Active)
    }

    fn status(&self) -> SubscriptionStatus {
        match self {
            Self::Active => SubscriptionStatus::Active,
            Self::Ended { .. } => SubscriptionStatus::Ended,
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Self::Active => None,
            Self::Ended { reason, .. } => Some(reason),
        }
    }

    fn ended_at(&self) -> Option<Instant> {
        match self {
            Self::Active => None,
            Self::Ended { at, .. } => Some(*at),
        }
    }
}

/// Install and delivery state of a shared event source.
#[derive(Clone, Debug)]
enum SourceState {
    Discrete,
    Installing { buffered: Vec<RawCdpMessage> },
    Live { current: String },
    Rearming { previous: String },
}

impl SourceState {
    /// Applies one match, returning whether it emits and whether it rearms.
    fn admit(&mut self, forced: Option<&str>, initial: bool, filter_key: &str) -> Option<bool> {
        match self {
            Self::Installing { .. } => None,
            Self::Discrete => Some(false),
            Self::Rearming { .. } => match forced {
                Some("afterGap") => {
                    *self = Self::Live {
                        current: filter_key.to_owned(),
                    };
                    Some(false)
                }
                Some("afterNavigation") | None => {
                    *self = Self::Live {
                        current: filter_key.to_owned(),
                    };
                    Some(true)
                }
                Some(_) => None,
            },
            Self::Live { current } => match forced {
                Some("afterGap") => {
                    *current = filter_key.to_owned();
                    Some(false)
                }
                Some(_) => None,
                None if initial => None,
                None => {
                    if *current == filter_key {
                        None
                    } else {
                        *current = filter_key.to_owned();
                        Some(false)
                    }
                }
            },
        }
    }

    /// Moves the source into live delivery, returning messages buffered during install.
    fn go_live(&mut self, current: &str) -> Vec<RawCdpMessage> {
        match std::mem::replace(
            self,
            Self::Live {
                current: current.to_owned(),
            },
        ) {
            Self::Installing { buffered } => buffered,
            _ => Vec::new(),
        }
    }
}

const LOST: &str =
    "Event subscription was lost; the daemon may have restarted or the subscription expired";

pub struct Bus {
    session_id: String,
    domain_filter: Arc<RwLock<Option<DomainFilter>>>,
    policy: Arc<std::sync::RwLock<Option<ActionPolicy>>>,
    head_sink: RwLock<Option<watch::Sender<HashMap<String, u64>>>>,
    state: Mutex<BusState>,
    notify: Notify,
    operation: Mutex<()>,
    receiver: Mutex<Option<ReceiverTask>>,
}

#[derive(Default)]
struct BusState {
    subs: HashMap<String, Subscription>,
    sources: HashMap<String, SharedSource>,
}

struct Subscription {
    id: String,
    session_id: String,
    source_key: String,
    policy_actions: Vec<String>,
    approved_actions: Vec<String>,
    filter_key: String,
    rule: Value,
    owner: Option<String>,
    idempotency_key: Option<String>,
    once: bool,
    expires_at: chrono::DateTime<chrono::Utc>,
    created: Instant,
    lifecycle: SubscriptionLifecycle,
    tab: EventTab,
    buffer: EventBuffer,
    filtered: u64,
    rejected: u64,
    satisfied: bool,
}

struct SharedSource {
    source: Arc<dyn EventSource>,
    subs: HashSet<String>,
    state: SourceState,
    url: String,
    tab: EventTab,
}

impl SharedSource {
    /// Arms a live source for the observation that follows a navigation.
    fn rearm(&mut self) {
        if let SourceState::Live { current } = &self.state {
            let previous = current.clone();
            self.state = SourceState::Rearming { previous };
        }
    }
}

struct ReceiverTask {
    client: Arc<CdpClient>,
    task: JoinHandle<()>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub id: String,
    pub status: SubscriptionStatus,
    pub events: Vec<EventEnvelope>,
    pub cursor: String,
    pub head: u64,
    pub consumed: u64,
    pub waiting: bool,
}

pub struct ReadRequest<'request> {
    pub id: &'request str,
    pub since: Option<&'request str>,
    pub owner: Option<&'request str>,
    pub wait: Duration,
    pub limit: usize,
    pub ack: bool,
}

pub struct DomStart<'request> {
    pub client: Arc<CdpClient>,
    pub session_id: &'request str,
    pub tab: EventTab,
    pub current_url: &'request str,
    pub selector: &'request str,
    pub state: &'request str,
    pub approved_actions: Vec<String>,
    pub once: bool,
    pub ttl: Duration,
    pub owner: Option<String>,
    pub idempotency_key: Option<String>,
}

pub(super) struct SourceStart<'request> {
    pub source: Arc<dyn EventSource>,
    pub initial: String,
    pub tab: EventTab,
    pub current_url: &'request str,
    pub filter_key: String,
    pub rule: Value,
    pub approved_actions: Vec<String>,
    pub once: bool,
    pub ttl: Duration,
    pub owner: Option<String>,
    pub idempotency_key: Option<String>,
}

impl Bus {
    pub fn new(
        session_id: String,
        domain_filter: Arc<RwLock<Option<DomainFilter>>>,
        policy: Arc<std::sync::RwLock<Option<ActionPolicy>>>,
    ) -> Self {
        Self {
            session_id,
            domain_filter,
            policy,
            head_sink: RwLock::new(None),
            state: Mutex::new(BusState::default()),
            notify: Notify::new(),
            operation: Mutex::new(()),
            receiver: Mutex::new(None),
        }
    }

    pub async fn start_dom(self: &Arc<Self>, req: DomStart<'_>) -> Result<Value, String> {
        self.attach(req.client.clone()).await?;
        let _operation = self.operation.lock().await;
        self.ensure_attached(&req.client).await?;
        self.expire_stored().await;
        let selector = req.selector.to_owned();
        let rule = json!({"selector": selector, "state": req.state});
        let key = format!("{}:{}:{}", req.tab.target_id, WORLD_NAME, selector);
        {
            let mut state = self.state.lock().await;
            prune(&mut state);
            if let Some(sub) = idempotent(
                &state,
                req.owner.as_deref(),
                req.idempotency_key.as_deref(),
                &rule,
                req.once,
                &req.tab.target_id,
            )? {
                return Ok(start_response(sub));
            }
            check_capacity(&state)?;
            let live = match state.sources.get(&key) {
                Some(shared) if shared.source.session_id() != req.session_id => {
                    return Err("Event observer session changed while subscribing".into());
                }
                Some(shared) => match &shared.state {
                    SourceState::Live { current } => {
                        Some((shared.source.clone(), current.clone(), shared.url.clone()))
                    }
                    SourceState::Rearming { previous } => {
                        Some((shared.source.clone(), previous.clone(), shared.url.clone()))
                    }
                    SourceState::Installing { .. } | SourceState::Discrete => None,
                },
                None => None,
            };
            if let Some((source, initial, url)) = live {
                let start = SourceStart {
                    source,
                    initial,
                    tab: req.tab,
                    current_url: &url,
                    filter_key: req.state.to_owned(),
                    rule,
                    approved_actions: req.approved_actions,
                    once: req.once,
                    ttl: req.ttl,
                    owner: req.owner,
                    idempotency_key: req.idempotency_key,
                };
                let response = register(
                    &mut state,
                    &self.session_id,
                    key,
                    start,
                    SourceState::Discrete,
                );
                drop(state);
                self.publish().await;
                return Ok(response);
            }
        }
        let observer = Arc::new(DomObserver::prepare(&req.tab.target_id, req.session_id));
        let source: Arc<dyn EventSource> = observer.clone();
        self.state.lock().await.sources.insert(
            key.clone(),
            SharedSource {
                source: source.clone(),
                subs: HashSet::new(),
                state: SourceState::Installing {
                    buffered: Vec::new(),
                },
                url: req.current_url.to_owned(),
                tab: req.tab.clone(),
            },
        );
        let initial = match observer.activate(&req.client, &selector).await {
            Ok(initial) => initial,
            Err(error) => {
                self.state.lock().await.sources.remove(&key);
                return Err(error);
            }
        };
        let current = signal_key(&initial).to_owned();
        let (response, buffered) = {
            let mut state = self.state.lock().await;
            let buffered = state
                .sources
                .get_mut(&key)
                .map(|shared| shared.state.go_live(&current))
                .unwrap_or_default();
            let start = SourceStart {
                source,
                initial: current.clone(),
                tab: req.tab,
                current_url: req.current_url,
                filter_key: req.state.to_owned(),
                rule,
                approved_actions: req.approved_actions,
                once: req.once,
                ttl: req.ttl,
                owner: req.owner,
                idempotency_key: req.idempotency_key,
            };
            let response = register(
                &mut state,
                &self.session_id,
                key.clone(),
                start,
                SourceState::Live { current },
            );
            (response, buffered)
        };
        for raw in buffered {
            self.process_raw(raw).await;
        }
        self.publish().await;
        Ok(response)
    }

    pub(super) async fn subscribe_cdp(
        self: &Arc<Self>,
        client: Arc<CdpClient>,
        req: SourceStart<'_>,
    ) -> Result<(Value, bool), String> {
        self.attach(client.clone()).await?;
        let _operation = self.operation.lock().await;
        self.ensure_attached(&client).await?;
        self.expire_stored().await;
        let mut state = self.state.lock().await;
        prune(&mut state);
        if let Some(sub) = idempotent(
            &state,
            req.owner.as_deref(),
            req.idempotency_key.as_deref(),
            &req.rule,
            req.once,
            &req.tab.target_id,
        )? {
            return Ok((start_response(sub), false));
        }
        check_capacity(&state)?;
        let key = uuid::Uuid::new_v4().simple().to_string();
        let response = register(
            &mut state,
            &self.session_id,
            key,
            req,
            SourceState::Discrete,
        );
        drop(state);
        self.publish().await;
        Ok((response, true))
    }

    pub async fn cancel_setup(&self, id: &str) -> Result<bool, String> {
        let mut state = self.state.lock().await;
        if !state.subs.contains_key(id) {
            return Ok(false);
        }
        let sources = finish_many(&mut state, [id.to_owned()], "setup_failed");
        state.subs.remove(id);
        drop(state);
        self.cleanup(sources, self.current_client().await).await;
        Ok(true)
    }

    pub async fn set_head_sink(&self, sink: Option<watch::Sender<HashMap<String, u64>>>) {
        *self.head_sink.write().await = sink;
        self.nudge_all_heads().await;
    }

    pub async fn policy_for_subscription(
        &self,
        id: &str,
    ) -> Result<(Vec<String>, Vec<String>), String> {
        self.expire().await;
        let state = self.state.lock().await;
        state
            .subs
            .get(id)
            .map(|sub| (sub.policy_actions.clone(), sub.approved_actions.clone()))
            .ok_or_else(|| LOST.into())
    }

    /// Drops the actions every subscription was confirmed for, so an approval
    /// given under the previous policy does not carry over.
    pub async fn clear_approvals(&self) {
        let mut state = self.state.lock().await;
        for sub in state.subs.values_mut() {
            sub.approved_actions.clear();
        }
    }

    async fn nudge_all_heads(&self) {
        let Some(sink) = self.head_sink.read().await.clone() else {
            return;
        };
        let state = self.state.lock().await;
        let heads = state
            .subs
            .values()
            .map(|sub| (sub.id.clone(), sub.buffer.head()))
            .collect();
        sink.send_replace(heads);
        drop(state);
    }

    async fn publish(&self) {
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
    }

    async fn cleanup(&self, sources: Vec<Arc<dyn EventSource>>, client: Option<Arc<CdpClient>>) {
        self.publish().await;
        let Some(client) = client else {
            return;
        };
        tokio::spawn(async move {
            for source in sources {
                source.stop(&client).await;
            }
        });
    }

    async fn ensure_attached(&self, client: &Arc<CdpClient>) -> Result<(), String> {
        let receiver = self.receiver.lock().await;
        if *client.subscribe_closed().borrow()
            || !receiver
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.client, client))
        {
            return Err("Browser connection is closed".into());
        }
        Ok(())
    }

    async fn attach(self: &Arc<Self>, client: Arc<CdpClient>) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        if *client.subscribe_closed().borrow() {
            return Err("Browser connection is closed".into());
        }
        let previous = {
            let mut receiver = self.receiver.lock().await;
            if receiver
                .as_ref()
                .is_some_and(|old| Arc::ptr_eq(&old.client, &client))
            {
                return Ok(());
            }
            receiver.take()
        };
        if let Some(old) = previous {
            old.task.abort();
            self.end_all_with_client("browser_closed", Some(old.client))
                .await;
        }
        let mut receiver = client.subscribe_raw();
        let mut closed = client.subscribe_closed();
        if *closed.borrow() {
            return Err("Browser connection is closed".into());
        }
        let bus = self.clone();
        let receiver_client = client.clone();
        let task = tokio::spawn(async move {
            let mut expiry = tokio::time::interval(Duration::from_millis(250));
            expiry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    received = receiver.recv() => match received {
                        Ok(raw) => bus.receive_raw(&receiver_client, raw).await,
                        Err(broadcast::error::RecvError::Lagged(missed)) => bus.handle_gap(&receiver_client, missed).await,
                        Err(broadcast::error::RecvError::Closed) => {
                            bus.close_receiver(receiver_client.clone(), "browser_closed").await;
                            return;
                        }
                    },
                    _ = expiry.tick() => bus.expire().await,
                    _ = closed.changed() => {
                        bus.close_receiver(receiver_client.clone(), "browser_closed").await;
                        return;
                    },
                }
            }
        });
        *self.receiver.lock().await = Some(ReceiverTask { client, task });
        Ok(())
    }

    async fn close_receiver(&self, client: Arc<CdpClient>, reason: &str) {
        let _operation = self.operation.lock().await;
        let should_clear = {
            let mut receiver = self.receiver.lock().await;
            if receiver
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.client, &client))
            {
                receiver.take();
                true
            } else {
                false
            }
        };
        if should_clear {
            self.end_all_with_client(reason, Some(client)).await;
        }
    }

    async fn receive_raw(&self, client: &Arc<CdpClient>, raw: RawCdpMessage) {
        let Ok(event) = serde_json::from_str::<Value>(&raw.text) else {
            return;
        };
        let method = event["method"].as_str().unwrap_or("");
        let session = raw
            .session_id
            .as_deref()
            .or_else(|| event["sessionId"].as_str());
        match method {
            "Target.targetDestroyed" | "Target.targetCrashed" => {
                self.process_raw(raw.clone()).await;
                if let Some(target) = event.pointer("/params/targetId").and_then(Value::as_str) {
                    let reason = if method.ends_with("Crashed") {
                        "tab_crashed"
                    } else {
                        "tab_closed"
                    };
                    self.end_matching(reason, client, |source| source.target_id() == target)
                        .await;
                }
                return;
            }
            "Inspector.targetCrashed" => {
                if let Some(session) = session {
                    self.end_matching("tab_crashed", client, |source| {
                        source.session_id() == session
                    })
                    .await;
                }
                return;
            }
            "Page.frameNavigated" if event.pointer("/params/frame/parentId").is_none() => {
                if let (Some(session), Some(url)) = (
                    session,
                    event.pointer("/params/frame/url").and_then(Value::as_str),
                ) {
                    let mut state = self.state.lock().await;
                    for shared in state.sources.values_mut() {
                        if shared.source.session_id() == session {
                            shared.url = url.to_owned();
                            shared.rearm();
                        }
                        shared.source.update_url(session, url);
                    }
                }
            }
            _ => {}
        }
        self.process_raw(raw.clone()).await;
        if matches!(
            method,
            "Page.navigatedWithinDocument" | "Runtime.bindingCalled" | "Page.frameNavigated"
        ) {
            if let Some(session) = session {
                self.observe_pending(session, client).await;
            }
        }
    }

    pub async fn process_raw(&self, raw: RawCdpMessage) {
        let signals: Vec<_> = {
            let mut state = self.state.lock().await;
            let mut signals = Vec::new();
            for (key, shared) in state.sources.iter_mut() {
                if let SourceState::Installing { buffered } = &mut shared.state {
                    if raw.session_id.as_deref() == Some(shared.source.session_id()) {
                        buffered.push(raw.clone());
                    }
                    continue;
                }
                if let Some(signal) = shared.source.classify(&raw) {
                    signals.push((key.clone(), signal));
                }
            }
            signals
        };
        for (key, signal) in signals {
            match signal {
                SourceSignal::Rearmed => {
                    if let Some(shared) = self.state.lock().await.sources.get_mut(&key) {
                        shared.rearm();
                    }
                }
                SourceSignal::Rejected => {
                    let mut state = self.state.lock().await;
                    let ids = state
                        .sources
                        .get(&key)
                        .map(|shared| shared.subs.clone())
                        .unwrap_or_default();
                    for id in ids {
                        if let Some(sub) = state
                            .subs
                            .get_mut(&id)
                            .filter(|sub| sub.lifecycle.is_active())
                        {
                            sub.rejected += 1;
                        }
                    }
                }
                signal => self.emit_signal(&key, signal, None).await,
            }
        }
    }

    async fn observe_pending(&self, session: &str, client: &Arc<CdpClient>) {
        let sources: Vec<_> = {
            let state = self.state.lock().await;
            state
                .sources
                .iter()
                .filter(|(_, shared)| {
                    shared.source.session_id() == session
                        && matches!(shared.state, SourceState::Rearming { .. })
                })
                .map(|(key, shared)| (key.clone(), shared.source.clone()))
                .collect()
        };
        for (key, source) in sources {
            if let Ok(Some(signal)) = source.observe(client).await {
                self.emit_signal(&key, signal, Some("afterNavigation"))
                    .await;
            }
        }
    }

    async fn handle_gap(&self, client: &Arc<CdpClient>, missed: u64) {
        let sources: Vec<_> = {
            let mut state = self.state.lock().await;
            for sub in state
                .subs
                .values_mut()
                .filter(|sub| sub.lifecycle.is_active())
            {
                sub.buffer.append(EventEnvelope::new(
                    "subscription.gap",
                    &self.session_id,
                    &sub.tab,
                    json!({"missed": missed}),
                ));
            }
            state
                .sources
                .iter()
                .map(|(key, shared)| (key.clone(), shared.source.clone()))
                .collect()
        };
        self.publish().await;
        for (key, source) in sources {
            if let Ok(Some(signal)) = source.observe(client).await {
                self.emit_signal(&key, signal, Some("afterGap")).await;
            }
        }
    }

    async fn emit_signal(&self, key: &str, signal: SourceSignal, forced: Option<&str>) {
        let SourceSignal::Match {
            key: filter_key,
            name,
            mut data,
            initial,
            location,
        } = signal
        else {
            return;
        };
        let actions: Vec<_> = {
            let state = self.state.lock().await;
            state
                .sources
                .get(key)
                .map(|shared| shared.source.policy_actions())
                .unwrap_or_default()
        };
        if crate::native::actions::check_event_source_policy(&self.policy, &actions).is_err() {
            let ids = {
                let state = self.state.lock().await;
                state
                    .sources
                    .get(key)
                    .map(|shared| shared.subs.clone())
                    .unwrap_or_default()
            };
            let stopped = finish_many(&mut *self.state.lock().await, ids, "policy_denied");
            self.cleanup(stopped, self.current_client().await).await;
            return;
        }
        let (ids, url, tab, rearmed) = {
            let mut state = self.state.lock().await;
            let Some(shared) = state.sources.get_mut(key) else {
                return;
            };
            let Some(rearmed) = shared.state.admit(forced, initial, &filter_key) else {
                return;
            };
            (
                shared.subs.clone(),
                shared.url.clone(),
                shared.tab.clone(),
                rearmed,
            )
        };
        let (url, tab) = location
            .map(|loc| (loc.url, loc.tab.unwrap_or_else(|| tab.clone())))
            .unwrap_or((url, tab));
        if let (Some(flag), Some(data)) = (
            forced.or_else(|| rearmed.then_some("afterNavigation")),
            data.as_object_mut(),
        ) {
            data.insert(flag.to_owned(), Value::Bool(true));
        }
        let allowed = self
            .domain_filter
            .read()
            .await
            .as_ref()
            .is_none_or(|filter| url == "about:blank" || filter.check_url(&url).is_ok());
        let mut state = self.state.lock().await;
        let mut done = Vec::new();
        for id in ids {
            let Some(sub) = state
                .subs
                .get_mut(&id)
                .filter(|sub| sub.lifecycle.is_active())
            else {
                continue;
            };
            if rearmed {
                sub.buffer.append(EventEnvelope::new(
                    "subscription.rearmed",
                    &self.session_id,
                    &tab,
                    json!({}),
                ));
            }
            if sub.filter_key != filter_key {
                continue;
            }
            if !allowed {
                sub.filtered += 1;
                continue;
            }
            sub.buffer.append(EventEnvelope::new(
                &name,
                &self.session_id,
                &tab,
                data.clone(),
            ));
            if sub.once {
                done.push(id);
            }
        }
        let stopped = finish_many(&mut state, done, "matched");
        drop(state);
        self.cleanup(stopped, self.current_client().await).await;
    }

    async fn end_matching(
        &self,
        reason: &str,
        client: &Arc<CdpClient>,
        matches: impl Fn(&dyn EventSource) -> bool,
    ) {
        let stopped = {
            let mut state = self.state.lock().await;
            let ids: Vec<_> = state
                .sources
                .values()
                .filter(|shared| matches(shared.source.as_ref()))
                .flat_map(|shared| shared.subs.iter().cloned())
                .collect();
            finish_many(&mut state, ids, reason)
        };
        self.cleanup(stopped, Some(client.clone())).await;
    }

    pub async fn end_all(&self, reason: &str) {
        let receiver = self.receiver.lock().await.take();
        let client = receiver.as_ref().map(|receiver| receiver.client.clone());
        if let Some(receiver) = receiver {
            receiver.task.abort();
        }
        self.end_all_with_client(reason, client).await;
    }

    async fn end_all_with_client(&self, reason: &str, client: Option<Arc<CdpClient>>) {
        let sources = {
            let mut state = self.state.lock().await;
            let ids: Vec<_> = state.subs.keys().cloned().collect();
            finish_many(&mut state, ids, reason)
        };
        self.cleanup(sources, client).await;
    }

    async fn expire(&self) {
        let _operation = self.operation.lock().await;
        self.expire_stored().await;
    }

    async fn expire_stored(&self) {
        let client = self.current_client().await;
        let now = chrono::Utc::now();
        let mut state = self.state.lock().await;
        let ids: Vec<_> = state
            .subs
            .values()
            .filter(|sub| sub.lifecycle.is_active() && sub.expires_at <= now)
            .map(|sub| sub.id.clone())
            .collect();
        let stopped = finish_many(&mut state, ids, "expired");
        prune(&mut state);
        drop(state);
        self.cleanup(stopped, client).await;
    }

    async fn current_client(&self) -> Option<Arc<CdpClient>> {
        let receiver = self.receiver.lock().await;
        receiver.as_ref().map(|receiver| receiver.client.clone())
    }

    pub async fn read(&self, req: ReadRequest<'_>) -> Result<ReadResult, String> {
        let deadline = tokio::time::Instant::now() + req.wait;
        self.expire().await;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut state = self.state.lock().await;
            prune(&mut state);
            let sub = state.subs.get_mut(req.id).ok_or_else(|| LOST.to_owned())?;
            check_owner(sub, req.owner)?;
            let since = req
                .since
                .map(str::to_owned)
                .unwrap_or_else(|| sub.buffer.consumed_cursor());
            let events = sub
                .buffer
                .read(Some(&since), req.limit)
                .map_err(|_| "Invalid event cursor".to_owned())?;
            let cursor = events
                .last()
                .map(|event| event.meta.cursor.clone())
                .unwrap_or_else(|| since.clone());
            if req.ack && !events.is_empty() {
                sub.buffer
                    .ack_cursor(&cursor)
                    .map_err(|_| "Invalid event cursor".to_owned())?;
            }
            let result = ReadResult {
                id: req.id.to_owned(),
                status: sub.lifecycle.status(),
                events,
                cursor,
                head: sub.buffer.head(),
                consumed: sub.buffer.consumed(),
                waiting: false,
            };
            let ended = !sub.lifecycle.is_active();
            drop(state);
            if !result.events.is_empty() || req.wait.is_zero() || ended {
                return Ok(result);
            }
            if tokio::time::Instant::now() >= deadline
                || tokio::time::timeout_at(deadline, notified).await.is_err()
            {
                return Ok(ReadResult {
                    waiting: true,
                    ..result
                });
            }
        }
    }

    pub async fn ack(&self, id: &str, cursor: &str, owner: Option<&str>) -> Result<(), String> {
        self.expire().await;
        let mut state = self.state.lock().await;
        let sub = state.subs.get_mut(id).ok_or("Unknown event subscription")?;
        check_owner(sub, owner)?;
        sub.buffer
            .ack_cursor(cursor)
            .map_err(|_| "Invalid event cursor".into())
    }

    pub async fn stop(&self, id: &str, owner: Option<&str>) -> Result<bool, String> {
        let _operation = self.operation.lock().await;
        self.expire_stored().await;
        let client = self.current_client().await;
        let mut state = self.state.lock().await;
        check_owner(
            state.subs.get(id).ok_or("Unknown event subscription")?,
            owner,
        )?;
        let sources = finish_many(&mut state, [id.to_owned()], "stopped");
        drop(state);
        self.cleanup(sources, client).await;
        Ok(true)
    }

    pub async fn list(&self, owner: Option<&str>, unread: bool) -> Vec<Value> {
        self.expire().await;
        let state = self.state.lock().await;
        let mut subs: Vec<_> = state
            .subs
            .values()
            .filter(|sub| owner.is_none_or(|owner| sub.owner.as_deref() == Some(owner)))
            .filter(|sub| !unread || sub.buffer.head() > sub.buffer.consumed())
            .collect();
        subs.sort_by_key(|sub| sub.created);
        subs.into_iter().rev().take(MAX_LISTED).map(|sub| json!({
            "id": sub.id, "status": sub.lifecycle.status(), "reason": sub.lifecycle.reason(), "rule": sub.rule, "owner": sub.owner,
            "head": sub.buffer.head(), "consumed": sub.buffer.consumed(), "dropped": sub.buffer.dropped(),
            "filtered": sub.filtered, "rejected": sub.rejected, "tab": sub.tab, "expiresAt": sub.expires_at.to_rfc3339()
        })).collect()
    }

    /// Adds an active subscription whose first event is consumed and whose
    /// next `unread` events are not.
    #[cfg(test)]
    pub(crate) async fn add_test_subscription(&self, unread: usize) -> String {
        let tab = EventTab {
            id: "test-tab".into(),
            label: "Test".into(),
            target_id: "test-target".into(),
        };
        let mut buffer = EventBuffer::new();
        let id = buffer.id().to_owned();
        buffer.append(EventEnvelope::new(
            "subscription.ready",
            &self.session_id,
            &tab,
            json!({}),
        ));
        for index in 0..unread {
            buffer.append(EventEnvelope::new(
                "subscription.rearmed",
                &self.session_id,
                &tab,
                json!({ "index": index }),
            ));
        }
        buffer.ack(1);
        let subscription = Subscription {
            id: id.clone(),
            session_id: self.session_id.clone(),
            source_key: "test".into(),
            policy_actions: Vec::new(),
            approved_actions: Vec::new(),
            filter_key: "test".into(),
            rule: json!({}),
            owner: None,
            idempotency_key: None,
            once: false,
            expires_at: chrono::Utc::now() + chrono::Duration::seconds(60),
            created: Instant::now(),
            lifecycle: SubscriptionLifecycle::Active,
            tab,
            buffer,
            filtered: 0,
            rejected: 0,
            satisfied: false,
        };
        self.state
            .lock()
            .await
            .subs
            .insert(id.clone(), subscription);
        id
    }
}

fn signal_key(signal: &SourceSignal) -> &str {
    match signal {
        SourceSignal::Match { key, .. } => key,
        _ => "unknown",
    }
}

/// Registers a subscription, creating its shared source in `fresh` when absent.
fn register(
    state: &mut BusState,
    session: &str,
    key: String,
    req: SourceStart<'_>,
    fresh: SourceState,
) -> Value {
    let policy_actions = req.source.policy_actions();
    let approved_actions = req
        .approved_actions
        .into_iter()
        .filter(|action| policy_actions.iter().any(|policy| policy == action))
        .collect();
    let current = req.initial.clone();
    let shared = state
        .sources
        .entry(key.clone())
        .or_insert_with(|| SharedSource {
            source: req.source,
            subs: HashSet::new(),
            state: fresh,
            url: req.current_url.to_owned(),
            tab: req.tab.clone(),
        });
    let already_satisfied = current == req.filter_key;
    let mut buffer = EventBuffer::new();
    let id = buffer.id().to_owned();
    buffer.append(EventEnvelope::new(
        "subscription.ready",
        session,
        &req.tab,
        json!({"state": current, "alreadySatisfied": already_satisfied}),
    ));
    buffer.ack(buffer.head());
    let sub = Subscription {
        id: id.clone(),
        session_id: session.to_owned(),
        source_key: key,
        policy_actions,
        approved_actions,
        filter_key: req.filter_key,
        rule: req.rule,
        owner: req.owner,
        idempotency_key: req.idempotency_key,
        once: req.once,
        expires_at: chrono::Utc::now() + chrono::Duration::from_std(req.ttl).unwrap(),
        created: Instant::now(),
        lifecycle: SubscriptionLifecycle::Active,
        tab: req.tab,
        buffer,
        filtered: 0,
        rejected: 0,
        satisfied: already_satisfied,
    };
    let response = start_response(&sub);
    state.subs.insert(id.clone(), sub);
    shared.subs.insert(id);
    response
}

fn finish_many(
    state: &mut BusState,
    ids: impl IntoIterator<Item = String>,
    reason: &str,
) -> Vec<Arc<dyn EventSource>> {
    ids.into_iter()
        .filter_map(|id| {
            let sub = state
                .subs
                .get_mut(&id)
                .filter(|sub| sub.lifecycle.is_active())?;
            sub.lifecycle = SubscriptionLifecycle::Ended {
                reason: reason.to_owned(),
                at: Instant::now(),
            };
            sub.buffer.append(EventEnvelope::new(
                "subscription.ended",
                &sub.session_id,
                &sub.tab,
                json!({"reason": reason}),
            ));
            let key = sub.source_key.clone();
            let shared = state.sources.get_mut(&key)?;
            shared.subs.remove(&id);
            if shared.subs.is_empty() {
                Some(state.sources.remove(&key).unwrap().source)
            } else {
                None
            }
        })
        .collect()
}

fn check_capacity(state: &BusState) -> Result<(), String> {
    if state
        .subs
        .values()
        .filter(|sub| sub.lifecycle.is_active())
        .count()
        >= MAX_ACTIVE
    {
        return Err(format!(
            "At most {MAX_ACTIVE} event subscriptions may be active"
        ));
    }
    Ok(())
}

fn prune(state: &mut BusState) {
    let now = Instant::now();
    state.subs.retain(|_, sub| {
        sub.lifecycle
            .ended_at()
            .is_none_or(|ended| now.duration_since(ended) < RETENTION)
    });
    let mut ended: Vec<_> = state
        .subs
        .iter()
        .filter_map(|(id, sub)| sub.lifecycle.ended_at().map(|at| (id.clone(), at)))
        .collect();
    ended.sort_by_key(|(_, at)| *at);
    for (id, _) in ended
        .into_iter()
        .take(state.subs.len().saturating_sub(MAX_LISTED))
    {
        state.subs.remove(&id);
    }
}

fn idempotent<'a>(
    state: &'a BusState,
    owner: Option<&str>,
    key: Option<&str>,
    rule: &Value,
    once: bool,
    tab_target: &str,
) -> Result<Option<&'a Subscription>, String> {
    let Some(key) = key else {
        return Ok(None);
    };
    let active = state.subs.values().find(|sub| {
        sub.lifecycle.is_active()
            && sub.owner.as_deref() == owner
            && sub.idempotency_key.as_deref() == Some(key)
    });
    let Some(sub) = active else {
        return Ok(None);
    };
    if sub.rule != *rule || sub.once != once || sub.tab.target_id != tab_target {
        return Err(format!(
            "Event subscription key '{key}' is already active with a different rule"
        ));
    }
    Ok(Some(sub))
}

fn start_response(sub: &Subscription) -> Value {
    json!({
        "id": sub.id, "head": sub.buffer.head(), "cursor": sub.buffer.head_cursor(),
        "alreadySatisfied": sub.satisfied, "expiresAt": sub.expires_at.to_rfc3339(), "owner": sub.owner, "tab": sub.tab
    })
}

fn check_owner(sub: &Subscription, owner: Option<&str>) -> Result<(), String> {
    if sub.owner.as_deref() != owner {
        return Err("Event subscription owner does not match".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        filter::Matchers,
        sources::cdp::{self, CdpMethodSource},
    };
    use super::*;
    use futures_util::SinkExt;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    fn bus() -> Arc<Bus> {
        Arc::new(Bus::new("session-1".into(), Arc::default(), Arc::default()))
    }

    fn raw(kind: &str, state: &str, nonce: &str) -> RawCdpMessage {
        RawCdpMessage {
            session_id: Some("session-1".into()),
            text: json!({"method": "Runtime.bindingCalled", "params": {
                "name": "__ab_event_test", "payload": json!({
                    "nonce": nonce, "kind": kind, "state": state, "label": "Ready", "truncated": false
                }).to_string()
            }}).to_string(),
        }
    }

    #[derive(Clone, Copy)]
    enum TestSource {
        Live,
        Discrete,
    }

    async fn add(
        bus: &Bus,
        source: Arc<dyn EventSource>,
        filter: &str,
        once: bool,
        kind: TestSource,
    ) -> String {
        let key = source.target_id().to_owned();
        let mut state = bus.state.lock().await;
        let initial = state
            .sources
            .get(&key)
            .and_then(|shared| match &shared.state {
                SourceState::Live { current } => Some(current.clone()),
                SourceState::Rearming { previous } => Some(previous.clone()),
                SourceState::Installing { .. } | SourceState::Discrete => None,
            })
            .or_else(|| {
                source
                    .classify(&raw("initial", "hidden", "nonce-1"))
                    .map(|signal| signal_key(&signal).to_owned())
            })
            .unwrap_or_else(|| "unknown".into());
        let fresh = match kind {
            TestSource::Live => SourceState::Live {
                current: initial.clone(),
            },
            TestSource::Discrete => SourceState::Discrete,
        };
        let tab = EventTab {
            id: "t1".into(),
            label: "test".into(),
            target_id: source.target_id().into(),
        };
        let req = SourceStart {
            source,
            initial,
            tab,
            current_url: "https://allowed.test",
            filter_key: filter.into(),
            rule: json!({}),
            approved_actions: Vec::new(),
            once,
            ttl: MAX_TTL,
            owner: None,
            idempotency_key: None,
        };
        let response = register(&mut state, "session-1", key, req, fresh);
        assert_eq!(response["head"], 1);
        let id = response["id"].as_str().unwrap().to_owned();
        assert_eq!(response["cursor"], format!("{id}:1"));
        id
    }

    async fn read_all(bus: &Bus, id: &str) -> ReadResult {
        let start = bus.state.lock().await.subs[id].buffer.start_cursor();
        bus.read(ReadRequest {
            id,
            owner: None,
            since: Some(&start),
            wait: Duration::ZERO,
            limit: 10,
            ack: false,
        })
        .await
        .unwrap()
    }

    async fn add_dom(bus: &Bus, filter: &str) -> String {
        add(
            bus,
            Arc::new(DomObserver::test_observer()),
            filter,
            false,
            TestSource::Live,
        )
        .await
    }

    #[tokio::test]
    async fn subscribe_cdp_reports_creation_and_rolls_back_new_subscriptions() {
        let client = client_for(|ws| {
            tokio::spawn(async move {
                let _ws = ws;
                std::future::pending::<()>().await
            })
        })
        .await;
        let bus = bus();
        let tab = EventTab {
            id: "t1".into(),
            label: "test".into(),
            target_id: "target-1".into(),
        };
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab,
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        let (response, created) = bus.subscribe_cdp(client.clone(), req).await.unwrap();
        assert!(created);
        let id = response["id"].as_str().unwrap().to_owned();
        let (_, approved_actions) = bus.policy_for_subscription(&id).await.unwrap();
        assert_eq!(approved_actions, vec!["download".to_owned()]);
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab: EventTab {
                id: "t1".into(),
                label: "test".into(),
                target_id: "target-1".into(),
            },
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        let (again, created_again) = bus.subscribe_cdp(client.clone(), req).await.unwrap();
        assert!(!created_again);
        assert_eq!(again["id"], id);
        assert!(bus.stop(&id, Some("owner-1")).await.unwrap());
        assert_eq!(
            bus.state
                .lock()
                .await
                .subs
                .get(&id)
                .unwrap()
                .lifecycle
                .status(),
            SubscriptionStatus::Ended
        );
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab: EventTab {
                id: "t1".into(),
                label: "test".into(),
                target_id: "target-1".into(),
            },
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        let (new_response, created) = bus.subscribe_cdp(client, req).await.unwrap();
        assert!(created);
        assert_ne!(new_response["id"], id);
        let new_id = new_response["id"].as_str().unwrap();
        assert!(bus.cancel_setup(new_id).await.unwrap());
        assert!(!bus.state.lock().await.subs.contains_key(new_id));
    }

    #[tokio::test]
    async fn subscribe_cdp_rejects_mismatched_idempotency_parameters() {
        let client = client_for(|ws| {
            tokio::spawn(async move {
                let _ws = ws;
                std::future::pending::<()>().await
            })
        })
        .await;
        let bus = bus();
        let tab = EventTab {
            id: "t1".into(),
            label: "test".into(),
            target_id: "target-1".into(),
        };
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab: tab.clone(),
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        bus.subscribe_cdp(client.clone(), req).await.unwrap();
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab,
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin", "variant": "different"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        assert!(bus.subscribe_cdp(client, req).await.is_err());
    }

    #[tokio::test]
    async fn read_requires_matching_owner_and_acks_in_place() {
        let client = client_for(|ws| {
            tokio::spawn(async move {
                let _ws = ws;
                std::future::pending::<()>().await
            })
        })
        .await;
        let bus = bus();
        let tab = EventTab {
            id: "t1".into(),
            label: "test".into(),
            target_id: "target-1".into(),
        };
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab,
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        let (response, _) = bus.subscribe_cdp(client.clone(), req).await.unwrap();
        let id = response["id"].as_str().unwrap().to_owned();
        let start = bus.state.lock().await.subs[&id].buffer.start_cursor();
        bus.process_raw(RawCdpMessage {
            text: json!({"method": "Browser.downloadWillBegin", "params": {"url": "https://allowed.test/file"}}).to_string(),
            session_id: None,
        })
        .await;
        for owner in [None, Some("owner-2")] {
            for ack in [false, true] {
                assert!(bus
                    .read(ReadRequest {
                        id: &id,
                        owner,
                        since: Some(&start),
                        wait: Duration::ZERO,
                        limit: 10,
                        ack,
                    })
                    .await
                    .is_err());
            }
        }
        let read = bus
            .read(ReadRequest {
                id: &id,
                owner: Some("owner-1"),
                since: Some(&start),
                wait: Duration::ZERO,
                limit: 10,
                ack: true,
            })
            .await
            .unwrap();
        assert_eq!(read.status, SubscriptionStatus::Active);
        assert!(!read.events.is_empty());
        assert_eq!(read.consumed, read.head);
    }

    #[tokio::test]
    async fn read_ack_and_stop_require_exact_owner() {
        let client = client_for(|ws| {
            tokio::spawn(async move {
                let _ws = ws;
                std::future::pending::<()>().await
            })
        })
        .await;
        let bus = bus();
        let tab = EventTab {
            id: "t1".into(),
            label: "test".into(),
            target_id: "target-1".into(),
        };
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab,
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        let (response, _) = bus.subscribe_cdp(client, req).await.unwrap();
        let id = response["id"].as_str().unwrap().to_owned();
        let cursor = bus.state.lock().await.subs[&id].buffer.start_cursor();
        assert!(bus.ack(&id, &cursor, None).await.is_err());
        assert!(bus.ack(&id, &cursor, Some("owner-2")).await.is_err());
        bus.ack(&id, &cursor, Some("owner-1")).await.unwrap();
        assert!(bus.stop(&id, None).await.is_err());
        assert!(bus.stop(&id, Some("owner-2")).await.is_err());
        assert!(bus.stop(&id, Some("owner-1")).await.unwrap());
    }

    #[tokio::test]
    async fn head_sink_is_nudged_on_publish() {
        let bus = bus();
        let id = add_dom(&bus, "hidden").await;
        let (sink, mut recv) = tokio::sync::watch::channel(HashMap::<String, u64>::new());
        bus.set_head_sink(Some(sink)).await;
        recv.changed().await.unwrap();
        assert_eq!(recv.borrow().get(&id), Some(&1));
    }

    #[tokio::test]
    async fn read_returns_ended_subscriptions_without_waiting() {
        let client = client_for(|ws| {
            tokio::spawn(async move {
                let _ws = ws;
                std::future::pending::<()>().await
            })
        })
        .await;
        let bus = bus();
        let tab = EventTab {
            id: "t1".into(),
            label: "test".into(),
            target_id: "target-1".into(),
        };
        let req = SourceStart {
            source: cdp_source(),
            initial: "unknown".into(),
            tab,
            current_url: "https://allowed.test",
            filter_key: "Browser.downloadWillBegin".into(),
            rule: json!({"method": "Browser.downloadWillBegin"}),
            approved_actions: vec!["download".into()],
            once: false,
            ttl: MAX_TTL,
            owner: Some("owner-1".into()),
            idempotency_key: Some("setup-1".into()),
        };
        let (response, _) = bus.subscribe_cdp(client, req).await.unwrap();
        let id = response["id"].as_str().unwrap().to_owned();
        bus.stop(&id, Some("owner-1")).await.unwrap();
        let cursor = bus.state.lock().await.subs[&id].buffer.head_cursor();
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            bus.read(ReadRequest {
                id: &id,
                owner: Some("owner-1"),
                since: Some(&cursor),
                wait: Duration::from_millis(200),
                limit: 10,
                ack: false,
            }),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.status, SubscriptionStatus::Ended);
        assert!(!result.waiting);
    }

    async fn client_for(
        server: impl FnOnce(
                tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
            ) -> tokio::task::JoinHandle<()>
            + Send
            + 'static,
    ) -> Arc<CdpClient> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            server(ws).await.unwrap();
        });
        Arc::new(CdpClient::connect(&url).await.unwrap())
    }

    fn cdp_source() -> Arc<CdpMethodSource> {
        Arc::new(CdpMethodSource::new(
            cdp::lookup("Browser.downloadWillBegin").unwrap(),
            "session-1".into(),
            "target-1".into(),
            HashMap::new(),
            Matchers::parse(&[], &[]).unwrap(),
        ))
    }

    struct StalledObserver;

    #[async_trait::async_trait]
    impl EventSource for StalledObserver {
        fn target_id(&self) -> &str {
            "target-stalled"
        }

        fn session_id(&self) -> &str {
            "session-1"
        }

        fn classify(&self, _: &RawCdpMessage) -> Option<SourceSignal> {
            None
        }

        async fn observe(&self, _: &CdpClient) -> Result<Option<SourceSignal>, String> {
            Err("observe failed".into())
        }
        async fn stop(&self, _: &CdpClient) {
            std::future::pending::<()>().await
        }
    }

    #[tokio::test]
    async fn creation_consumes_the_ready_marker() {
        let bus = bus();
        let id = add_dom(&bus, "hidden").await;
        let result = bus
            .read(ReadRequest {
                id: &id,
                owner: None,
                since: None,
                wait: Duration::from_millis(50),
                limit: 10,
                ack: false,
            })
            .await
            .unwrap();
        assert!(result.events.is_empty());
        assert!(result.waiting);
        assert_eq!((result.head, result.consumed), (1, 1));
        assert!(bus.list(None, true).await.is_empty());
    }

    #[tokio::test]
    async fn failed_navigation_observation_stays_pending() {
        let client = client_for(|_| tokio::spawn(async {})).await;
        let bus = bus();
        add(
            &bus,
            Arc::new(StalledObserver),
            "visible",
            false,
            TestSource::Live,
        )
        .await;
        bus.state
            .lock()
            .await
            .sources
            .get_mut("target-stalled")
            .unwrap()
            .rearm();
        bus.observe_pending("session-1", &client).await;
        assert!(matches!(
            bus.state.lock().await.sources["target-stalled"].state,
            SourceState::Rearming { .. }
        ));
        client.close().await;
    }

    #[tokio::test]
    async fn expiry_runs_while_traffic_is_continuous() {
        let client = client_for(|mut ws| {
            tokio::spawn(async move {
                loop {
                    let message =
                        Message::Text(json!({"method": "Runtime.consoleAPICalled"}).to_string());
                    if ws.send(message).await.is_err() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
        })
        .await;
        let bus = bus();
        bus.attach(client.clone()).await.unwrap();
        let id = add(
            &bus,
            cdp_source(),
            "Browser.downloadWillBegin",
            false,
            TestSource::Discrete,
        )
        .await;
        bus.state.lock().await.subs.get_mut(&id).unwrap().expires_at = chrono::Utc::now();
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            bus.state
                .lock()
                .await
                .subs
                .get(&id)
                .unwrap()
                .lifecycle
                .status(),
            SubscriptionStatus::Ended
        );
        client.close().await;
    }

    #[tokio::test]
    async fn transport_loss_ends_subscriptions() {
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel::<()>();
        let client = client_for(|ws| {
            tokio::spawn(async move {
                let _ = closed_rx.await;
                drop(ws);
            })
        })
        .await;
        let bus = bus();
        bus.attach(client.clone()).await.unwrap();
        let id = add(
            &bus,
            cdp_source(),
            "Browser.downloadWillBegin",
            false,
            TestSource::Discrete,
        )
        .await;
        closed_tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let state = bus.state.lock().await;
        let sub = state.subs.get(&id).unwrap();
        assert_eq!(sub.lifecycle.status(), SubscriptionStatus::Ended);
        assert_eq!(sub.lifecycle.reason(), Some("browser_closed"));
    }

    #[tokio::test]
    async fn stop_notifies_waiting_reads_before_observer_cleanup() {
        let client = client_for(|_| tokio::spawn(async {})).await;
        let bus = bus();
        bus.attach(client.clone()).await.unwrap();
        add(
            &bus,
            Arc::new(StalledObserver),
            "visible",
            false,
            TestSource::Live,
        )
        .await;
        let notified = bus.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let cleaning = bus.clone();
        let cleanup = tokio::spawn(async move {
            cleaning
                .cleanup(vec![Arc::new(StalledObserver)], Some(client))
                .await;
        });
        tokio::time::timeout(Duration::from_millis(200), cleanup)
            .await
            .expect("cleanup should not await observer shutdown")
            .unwrap();
        tokio::time::timeout(Duration::from_millis(200), notified)
            .await
            .expect("cleanup should notify readers");
    }

    #[tokio::test]
    async fn rejects_forged_payloads_and_filters_content_at_emit_time() {
        let bus = bus();
        let id = add_dom(&bus, "visible").await;
        for nonce in ["forged".to_owned(), "x".repeat(16 * 1024)] {
            bus.process_raw(raw("change", "visible", &nonce)).await;
        }
        assert_eq!(bus.list(None, false).await[0]["rejected"], 2);
        *bus.domain_filter.write().await = Some(DomainFilter::new("allowed.test"));
        bus.state
            .lock()
            .await
            .sources
            .get_mut("target-1")
            .unwrap()
            .url = "https://blocked.test".into();
        bus.process_raw(raw("change", "visible", "nonce-1")).await;
        assert_eq!(bus.list(None, false).await[0]["filtered"], 1);
        assert_eq!(read_all(&bus, &id).await.events.len(), 1);
    }

    #[tokio::test]
    async fn data_action_denial_and_failed_reload_end_without_content() {
        for missing in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("policy.json");
            std::fs::write(
                &path,
                if missing {
                    r#"{"deny":[]}"#
                } else {
                    r#"{"deny":["download"]}"#
                },
            )
            .unwrap();
            let bus = bus();
            *bus.policy.write().unwrap() =
                Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
            let id = add(
                &bus,
                cdp_source(),
                "Browser.downloadWillBegin",
                false,
                TestSource::Discrete,
            )
            .await;
            if missing {
                std::fs::remove_file(path).unwrap();
            }
            bus.process_raw(RawCdpMessage {
                text: json!({"method": "Browser.downloadWillBegin", "params": {"url": "https://allowed.test/file"}}).to_string(),
                session_id: None,
            }).await;
            let read = read_all(&bus, &id).await;
            assert_eq!(read.status, SubscriptionStatus::Ended);
            assert_eq!(read.events.len(), 2);
            assert_eq!(read.events[1].data, json!({"reason": "policy_denied"}));
        }
    }

    #[tokio::test]
    async fn lifecycle_bounds_retain_ended_buffers_until_retention() {
        for reason in ["expired", "browser_closed", "stopped"] {
            let bus = bus();
            let id = add_dom(&bus, "visible").await;
            match reason {
                "expired" => {
                    bus.state.lock().await.subs.get_mut(&id).unwrap().expires_at =
                        chrono::Utc::now()
                }
                "browser_closed" => bus.end_all(reason).await,
                _ => {
                    bus.stop(&id, None).await.unwrap();
                }
            }
            let read = read_all(&bus, &id).await;
            assert_eq!(read.status, SubscriptionStatus::Ended);
            assert_eq!(read.events.len(), 2);
            assert_eq!(read.events[1].data, json!({"reason": reason}));
            bus.state.lock().await.subs.get_mut(&id).unwrap().lifecycle =
                SubscriptionLifecycle::Ended {
                    reason: reason.into(),
                    at: Instant::now() - RETENTION,
                };
            assert!(bus
                .read(ReadRequest {
                    id: &id,
                    owner: None,
                    since: None,
                    wait: Duration::ZERO,
                    limit: 10,
                    ack: false,
                })
                .await
                .is_err());
        }
        let bus = bus();
        for _ in 0..MAX_ACTIVE {
            add_dom(&bus, "visible").await;
        }
        assert!(check_capacity(&*bus.state.lock().await).is_err());
        bus.end_all("browser_closed").await;
        for _ in 0..=MAX_LISTED {
            let id = add_dom(&bus, "visible").await;
            bus.stop(&id, None).await.unwrap();
        }
        assert_eq!(bus.list(None, false).await.len(), MAX_LISTED);
    }

    #[tokio::test]
    async fn navigation_rearm_ignores_stale_initial_and_trailing_duplicate_change() {
        let bus = bus();
        let id = add_dom(&bus, "visible").await;
        let key = "target-1";
        bus.state.lock().await.sources.get_mut(key).unwrap().rearm();
        bus.emit_signal(
            key,
            SourceSignal::Match {
                key: "visible".into(),
                name: "element.visible".into(),
                data: json!({"state": "visible", "label": "after-nav", "truncated": false}),
                initial: true,
                location: None,
            },
            Some("afterNavigation"),
        )
        .await;
        bus.process_raw(raw("initial", "hidden", "nonce-1")).await;
        bus.process_raw(raw("change", "visible", "nonce-1")).await;
        let read = read_all(&bus, &id).await;
        let names: Vec<_> = read
            .events
            .iter()
            .map(|event| event.meta.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "subscription.ready",
                "subscription.rearmed",
                "element.visible"
            ]
        );
        assert_eq!(read.events[2].data["afterNavigation"], json!(true));
    }

    fn stamped(kind: &str, state: &str, seq: u64, label: &str) -> RawCdpMessage {
        RawCdpMessage {
            session_id: Some("session-1".into()),
            text: json!({"method": "Runtime.bindingCalled", "params": {
                "name": "__ab_event_test", "payload": json!({
                    "nonce": "nonce-1", "kind": kind, "state": state, "label": label,
                    "truncated": false, "doc": "doc-1", "seq": seq
                }).to_string()
            }})
            .to_string(),
        }
    }

    #[tokio::test]
    async fn install_window_buffers_binding_calls_then_replays_them_in_order() {
        let bus = bus();
        let source: Arc<dyn EventSource> = Arc::new(DomObserver::test_observer());
        bus.state.lock().await.sources.insert(
            "target-1".into(),
            SharedSource {
                source: source.clone(),
                subs: HashSet::new(),
                state: SourceState::Installing {
                    buffered: Vec::new(),
                },
                url: "https://allowed.test".into(),
                tab: EventTab {
                    id: "t1".into(),
                    label: "test".into(),
                    target_id: "target-1".into(),
                },
            },
        );
        let arriving = vec![
            stamped("initial", "hidden", 1, "start"),
            stamped("change", "visible", 2, "first"),
            stamped("change", "hidden", 3, "middle"),
            stamped("change", "visible", 4, "second"),
        ];
        for raw in &arriving {
            bus.process_raw(raw.clone()).await;
        }
        assert!(bus.list(None, false).await.is_empty());
        let buffered = {
            let mut state = bus.state.lock().await;
            let shared = state.sources.get_mut("target-1").unwrap();
            let SourceState::Installing { buffered } = &shared.state else {
                panic!("source should still be installing");
            };
            let captured: Vec<_> = buffered.iter().map(|raw| raw.text.clone()).collect();
            assert_eq!(
                captured,
                arriving
                    .iter()
                    .map(|raw| raw.text.clone())
                    .collect::<Vec<_>>()
            );
            shared.state.go_live("hidden")
        };
        assert_eq!(buffered.len(), arriving.len());
        let id = add(&bus, source, "visible", false, TestSource::Live).await;
        for raw in buffered {
            bus.process_raw(raw).await;
        }
        let events = read_all(&bus, &id).await.events;
        let summary: Vec<_> = events
            .iter()
            .map(|event| {
                let label = event.data["label"].as_str().unwrap_or_default();
                format!("{}:{label}", event.meta.name)
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                "subscription.ready:",
                "element.visible:first",
                "element.visible:second"
            ]
        );
        bus.process_raw(stamped("change", "visible", 4, "second"))
            .await;
        assert_eq!(read_all(&bus, &id).await.events.len(), events.len());
    }
}
