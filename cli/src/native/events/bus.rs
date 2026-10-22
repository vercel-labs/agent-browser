use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;

use super::buffer::EventBuffer;
use super::envelope::{EventEnvelope, EventTab};
use super::source::{EventSource, SourceSignal};
use super::sources::dom::{normalize_selector, DomObserver, WORLD_NAME};
use crate::native::cdp::client::{CdpClient, RawCdpMessage};
use crate::native::network::DomainFilter;
use crate::native::policy::ActionPolicy;
use crate::native::stream::StreamServer;

const MAX_ACTIVE: usize = 20;
const MAX_LISTED: usize = 50;
const RETENTION: Duration = Duration::from_secs(600);
const MAX_TTL: Duration = Duration::from_secs(1_800);
const LOST: &str =
    "Event subscription was lost; the daemon may have restarted or the subscription expired";

pub struct Bus {
    session_id: String,
    domain_filter: Arc<RwLock<Option<DomainFilter>>>,
    policy: Arc<RwLock<Option<ActionPolicy>>>,
    stream_server: RwLock<Option<Arc<StreamServer>>>,
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
    filter_key: String,
    rule: Value,
    owner: Option<String>,
    idempotency_key: Option<String>,
    once: bool,
    expires_at: chrono::DateTime<chrono::Utc>,
    created: Instant,
    ended: Option<Instant>,
    status: &'static str,
    reason: Option<String>,
    tab: EventTab,
    buffer: EventBuffer,
    filtered: u64,
    rejected: u64,
    satisfied: bool,
}

struct SharedSource {
    source: Arc<dyn EventSource>,
    subs: HashSet<String>,
    current: String,
    pending: bool,
    url: String,
    tab: EventTab,
}

struct ReceiverTask {
    client: Arc<CdpClient>,
    task: JoinHandle<()>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub id: String,
    pub status: &'static str,
    pub events: Vec<EventEnvelope>,
    pub cursor: String,
    pub head: u64,
    pub consumed: u64,
    pub waiting: bool,
}

pub struct DomStart<'request> {
    pub engine: &'request str,
    pub client: Arc<CdpClient>,
    pub session_id: &'request str,
    pub tab: EventTab,
    pub current_url: &'request str,
    pub selector: &'request str,
    pub state: &'request str,
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
    pub once: bool,
    pub ttl: Duration,
    pub owner: Option<String>,
    pub idempotency_key: Option<String>,
}

impl Bus {
    pub fn new(
        session_id: String,
        domain_filter: Arc<RwLock<Option<DomainFilter>>>,
        policy: Arc<RwLock<Option<ActionPolicy>>>,
    ) -> Self {
        Self {
            session_id,
            domain_filter,
            policy,
            stream_server: RwLock::new(None),
            state: Mutex::new(BusState::default()),
            notify: Notify::new(),
            operation: Mutex::new(()),
            receiver: Mutex::new(None),
        }
    }

    pub async fn start_dom(self: &Arc<Self>, req: DomStart<'_>) -> Result<Value, String> {
        if req.engine != "chrome" {
            return Err("Browser events are supported only with the Chrome engine".into());
        }
        let selector = normalize_selector(req.selector)?;
        validate_ttl(req.ttl)?;
        if !matches!(req.state, "visible" | "hidden") {
            return Err("Event state must be visible or hidden".into());
        }
        self.attach(req.client.clone()).await?;
        let _operation = self.operation.lock().await;
        self.expire().await;
        {
            let mut state = self.state.lock().await;
            prune(&mut state);
            if let Some(sub) =
                idempotent(&state, req.owner.as_deref(), req.idempotency_key.as_deref())
            {
                return Ok(start_response(sub));
            }
            check_capacity(&state)?;
        }
        let key = format!("{}:{}:{}", req.tab.target_id, WORLD_NAME, selector);
        let existing = self.state.lock().await.sources.get(&key).map(|shared| {
            (
                shared.source.clone(),
                shared.current.clone(),
                shared.url.clone(),
            )
        });
        let (source, initial, url): (Arc<dyn EventSource>, _, _) = match existing {
            Some(existing) => existing,
            None => {
                let (observer, initial) = DomObserver::install(
                    &req.client,
                    req.session_id,
                    &req.tab.target_id,
                    &selector,
                )
                .await?;
                (
                    Arc::new(observer),
                    signal_key(&initial).to_owned(),
                    req.current_url.to_owned(),
                )
            }
        };
        if source.session_id() != req.session_id {
            return Err("Event observer session changed while subscribing".into());
        }
        let expires = chrono::Utc::now() + chrono::Duration::from_std(req.ttl).unwrap();
        let start = SourceStart {
            source,
            initial,
            tab: req.tab,
            current_url: &url,
            filter_key: req.state.to_owned(),
            rule: json!({"selector": selector, "state": req.state}),
            once: req.once,
            ttl: req.ttl,
            owner: req.owner,
            idempotency_key: req.idempotency_key,
        };
        let response = register(
            &mut *self.state.lock().await,
            &self.session_id,
            key,
            start,
            expires,
        );
        self.publish().await;
        Ok(response)
    }

    pub(super) async fn subscribe_cdp(
        self: &Arc<Self>,
        client: Arc<CdpClient>,
        req: SourceStart<'_>,
    ) -> Result<Value, String> {
        self.attach(client).await?;
        let _operation = self.operation.lock().await;
        self.expire().await;
        let mut state = self.state.lock().await;
        prune(&mut state);
        if let Some(sub) = idempotent(&state, req.owner.as_deref(), req.idempotency_key.as_deref())
        {
            return Ok(start_response(sub));
        }
        check_capacity(&state)?;
        let key = uuid::Uuid::new_v4().simple().to_string();
        let expires = chrono::Utc::now() + chrono::Duration::from_std(req.ttl).unwrap();
        let response = register(&mut state, &self.session_id, key, req, expires);
        drop(state);
        self.publish().await;
        Ok(response)
    }

    pub async fn set_stream(&self, server: Option<Arc<StreamServer>>) {
        *self.stream_server.write().await = server;
        self.nudge_all_heads().await;
    }

    pub async fn policy_actions_for_subscription(&self, id: &str) -> Result<Vec<String>, String> {
        self.expire().await;
        let state = self.state.lock().await;
        state
            .subs
            .get(id)
            .map(|sub| sub.policy_actions.clone())
            .ok_or_else(|| LOST.into())
    }

    async fn nudge_all_heads(&self) {
        let server = self.stream_server.read().await.clone();
        if let Some(server) = server {
            let heads: Vec<_> = {
                let state = self.state.lock().await;
                state
                    .subs
                    .values()
                    .map(|sub| (sub.id.clone(), sub.buffer.head()))
                    .collect()
            };
            for (id, head) in heads {
                server.nudge_event(&id, head);
            }
        }
    }

    async fn publish(&self) {
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
    }

    async fn cleanup(&self, sources: Vec<Arc<dyn EventSource>>, client: Option<Arc<CdpClient>>) {
        if let Some(client) = client {
            for source in sources {
                source.stop(&client).await;
            }
        }
        self.publish().await;
    }

    async fn attach(self: &Arc<Self>, client: Arc<CdpClient>) -> Result<(), String> {
        let _operation = self.operation.lock().await;
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
        let bus = self.clone();
        let receiver_client = client.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    received = receiver.recv() => match received {
                        Ok(raw) => bus.receive_raw(&receiver_client, raw).await,
                        Err(broadcast::error::RecvError::Lagged(missed)) => bus.handle_gap(&receiver_client, missed).await,
                        Err(broadcast::error::RecvError::Closed) => {
                            bus.end_all_with_client("browser_closed", Some(receiver_client.clone())).await;
                            return;
                        }
                    },
                    _ = tokio::time::sleep(Duration::from_millis(250)) => bus.expire().await,
                }
            }
        });
        *self.receiver.lock().await = Some(ReceiverTask { client, task });
        Ok(())
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
                        }
                        shared.source.update_url(session, url);
                    }
                    for shared in state
                        .sources
                        .values_mut()
                        .filter(|shared| shared.source.session_id() == session)
                    {
                        shared.pending = true;
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
            let state = self.state.lock().await;
            state
                .sources
                .iter()
                .filter_map(|(key, shared)| {
                    shared
                        .source
                        .classify(&raw)
                        .map(|signal| (key.clone(), signal))
                })
                .collect()
        };
        for (key, signal) in signals {
            match signal {
                SourceSignal::Rearmed => {
                    if let Some(shared) = self.state.lock().await.sources.get_mut(&key) {
                        shared.pending = true;
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
                        if let Some(sub) =
                            state.subs.get_mut(&id).filter(|sub| sub.status == "active")
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
                .filter(|(_, shared)| shared.source.session_id() == session && shared.pending)
                .map(|(key, shared)| (key.clone(), shared.source.clone()))
                .collect()
        };
        for (key, source) in sources {
            if let Ok(Some(signal)) = source.observe(client).await {
                self.emit_signal(&key, signal, Some("afterNavigation"))
                    .await;
            }
            let mut state = self.state.lock().await;
            let Some(shared) = state.sources.get_mut(&key).filter(|shared| shared.pending) else {
                continue;
            };
            shared.pending = false;
            let ids = shared.subs.clone();
            let tab = shared.tab.clone();
            for id in ids {
                if let Some(sub) = state.subs.get_mut(&id) {
                    sub.buffer.append(envelope(
                        "subscription.rearmed",
                        &self.session_id,
                        &tab,
                        json!({}),
                    ));
                }
            }
            drop(state);
            self.publish().await;
        }
    }

    async fn handle_gap(&self, client: &Arc<CdpClient>, missed: u64) {
        let sources: Vec<_> = {
            let mut state = self.state.lock().await;
            for sub in state.subs.values_mut().filter(|sub| sub.status == "active") {
                sub.buffer.append(envelope(
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
        if crate::native::actions::check_event_source_policy(&self.policy, &actions)
            .await
            .is_err()
        {
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
        let (ids, url, tab, pending, rearmed) = {
            let mut state = self.state.lock().await;
            let Some(shared) = state.sources.get_mut(key) else {
                return;
            };
            shared.current = filter_key.clone();
            let pending = shared.pending;
            let rearmed = pending && (forced == Some("afterNavigation") || initial);
            if rearmed {
                shared.pending = false;
            }
            (
                shared.subs.clone(),
                shared.url.clone(),
                shared.tab.clone(),
                pending,
                rearmed,
            )
        };
        let (url, tab) = location
            .map(|loc| (loc.url, loc.tab.unwrap_or_else(|| tab.clone())))
            .unwrap_or((url, tab));
        let flag = forced.or_else(|| (initial && pending).then_some("afterNavigation"));
        if initial && flag.is_none() {
            return;
        }
        if let (Some(flag), Some(data)) = (flag, data.as_object_mut()) {
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
            let Some(sub) = state.subs.get_mut(&id).filter(|sub| sub.status == "active") else {
                continue;
            };
            if rearmed {
                sub.buffer.append(envelope(
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
            sub.buffer
                .append(envelope(&name, &self.session_id, &tab, data.clone()));
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
        let keys: Vec<_> = {
            let state = self.state.lock().await;
            state
                .sources
                .iter()
                .filter(|(_, shared)| matches(shared.source.as_ref()))
                .map(|(key, _)| key.clone())
                .collect()
        };
        let mut stopped = Vec::new();
        for key in keys {
            let mut state = self.state.lock().await;
            let ids = state
                .sources
                .get(&key)
                .map(|shared| shared.subs.clone())
                .unwrap_or_default();
            stopped.extend(finish_many(&mut state, ids, reason));
        }
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
            let sources = state
                .sources
                .values()
                .map(|shared| shared.source.clone())
                .collect();
            let ids: Vec<_> = state
                .subs
                .values()
                .filter(|sub| sub.status == "active")
                .map(|sub| sub.id.clone())
                .collect();
            finish_many(&mut state, ids, reason);
            state.sources.clear();
            sources
        };
        self.cleanup(sources, client).await;
    }

    async fn expire(&self) {
        let client = self.current_client().await;
        let now = chrono::Utc::now();
        let mut state = self.state.lock().await;
        let ids: Vec<_> = state
            .subs
            .values()
            .filter(|sub| sub.status == "active" && sub.expires_at <= now)
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

    pub async fn read(
        &self,
        id: &str,
        since: Option<&str>,
        wait: Duration,
        limit: usize,
    ) -> Result<ReadResult, String> {
        self.expire().await;
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let mut state = self.state.lock().await;
            prune(&mut state);
            let sub = state.subs.get(id).ok_or_else(|| LOST.to_owned())?;
            let events = sub
                .buffer
                .read(since, limit)
                .map_err(|_| "Invalid event cursor".to_owned())?;
            let cursor = events
                .last()
                .map(|event| event.meta.cursor.clone())
                .or_else(|| since.map(str::to_owned))
                .unwrap_or_else(|| sub.buffer.start_cursor());
            let mut result = ReadResult {
                id: id.to_owned(),
                status: sub.status,
                events,
                cursor,
                head: sub.buffer.head(),
                consumed: sub.buffer.consumed(),
                waiting: false,
            };
            drop(state);
            if !result.events.is_empty() || wait.is_zero() {
                return Ok(result);
            }
            if tokio::time::Instant::now() >= deadline
                || tokio::time::timeout_at(deadline, notified).await.is_err()
            {
                result.waiting = true;
                return Ok(result);
            }
        }
    }

    pub async fn read_with_ack(
        &self,
        id: &str,
        since: Option<&str>,
        wait: Duration,
        limit: usize,
        acknowledge: bool,
    ) -> Result<ReadResult, String> {
        let mut result = self.read(id, since, wait, limit).await?;
        if acknowledge && !result.events.is_empty() {
            self.ack(id, &result.cursor, None).await?;
            result.consumed = self
                .state
                .lock()
                .await
                .subs
                .get(id)
                .ok_or("Unknown event subscription")?
                .buffer
                .consumed();
        }
        Ok(result)
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
        self.expire().await;
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
            "id": sub.id, "status": sub.status, "reason": sub.reason, "rule": sub.rule, "owner": sub.owner,
            "head": sub.buffer.head(), "consumed": sub.buffer.consumed(), "dropped": sub.buffer.dropped(),
            "filtered": sub.filtered, "rejected": sub.rejected, "tab": sub.tab, "expiresAt": sub.expires_at.to_rfc3339()
        })).collect()
    }
}

fn signal_key(signal: &SourceSignal) -> &str {
    match signal {
        SourceSignal::Match { key, .. } => key,
        _ => "unknown",
    }
}

fn envelope(name: &str, session: &str, tab: &EventTab, data: Value) -> EventEnvelope {
    EventEnvelope::new(name, session, &tab.id, &tab.label, &tab.target_id, data)
}

fn register(
    state: &mut BusState,
    session: &str,
    key: String,
    req: SourceStart<'_>,
    expires: chrono::DateTime<chrono::Utc>,
) -> Value {
    let actions = req.source.policy_actions();
    let current = req.initial.clone();
    let shared = state
        .sources
        .entry(key.clone())
        .or_insert_with(|| SharedSource {
            source: req.source,
            subs: HashSet::new(),
            current: req.initial,
            pending: false,
            url: req.current_url.to_owned(),
            tab: req.tab.clone(),
        });
    let already_satisfied = current == req.filter_key;
    let mut buffer = EventBuffer::new();
    let id = buffer.start_cursor().split(':').next().unwrap().to_owned();
    buffer.append(envelope(
        "subscription.ready",
        session,
        &req.tab,
        json!({"state": current, "alreadySatisfied": already_satisfied}),
    ));
    let sub = Subscription {
        id: id.clone(),
        session_id: session.to_owned(),
        source_key: key,
        policy_actions: actions,
        filter_key: req.filter_key,
        rule: req.rule,
        owner: req.owner,
        idempotency_key: req.idempotency_key,
        once: req.once,
        expires_at: expires,
        created: Instant::now(),
        ended: None,
        status: "active",
        reason: None,
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
                .filter(|sub| sub.status == "active")?;
            sub.status = "ended";
            sub.reason = Some(reason.to_owned());
            sub.ended = Some(Instant::now());
            sub.buffer.append(envelope(
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

fn validate_ttl(ttl: Duration) -> Result<(), String> {
    if ttl < Duration::from_secs(1) || ttl > MAX_TTL {
        return Err("Event subscription TTL must be between 1 second and 30 minutes".into());
    }
    Ok(())
}

fn check_capacity(state: &BusState) -> Result<(), String> {
    if state
        .subs
        .values()
        .filter(|sub| sub.status == "active")
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
        sub.ended
            .is_none_or(|ended| now.duration_since(ended) < RETENTION)
    });
    let mut ended: Vec<_> = state
        .subs
        .iter()
        .filter_map(|(id, sub)| sub.ended.map(|ended| (id.clone(), ended)))
        .collect();
    ended.sort_by_key(|(_, ended)| *ended);
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
) -> Option<&'a Subscription> {
    let key = key?;
    state
        .subs
        .values()
        .find(|sub| sub.owner.as_deref() == owner && sub.idempotency_key.as_deref() == Some(key))
}

fn start_response(sub: &Subscription) -> Value {
    json!({
        "id": sub.id, "head": sub.buffer.head(), "cursor": sub.buffer.head_cursor(),
        "alreadySatisfied": sub.satisfied, "expiresAt": sub.expires_at.to_rfc3339(), "owner": sub.owner, "tab": sub.tab
    })
}

fn check_owner(sub: &Subscription, owner: Option<&str>) -> Result<(), String> {
    if owner.is_some_and(|owner| sub.owner.as_deref() != Some(owner)) {
        return Err("Event subscription owner does not match".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{filter::Matchers, ops::CdpMethodSource, sources::cdp};
    use super::*;

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

    async fn add(bus: &Bus, source: Arc<dyn EventSource>, filter: &str, once: bool) -> String {
        let key = source.target_id().to_owned();
        let mut state = bus.state.lock().await;
        let initial = state
            .sources
            .get(&key)
            .map(|shared| shared.current.clone())
            .or_else(|| {
                source
                    .classify(&raw("initial", "hidden", "nonce-1"))
                    .map(|signal| signal_key(&signal).to_owned())
            })
            .unwrap_or_else(|| "unknown".into());
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
            once,
            ttl: MAX_TTL,
            owner: None,
            idempotency_key: None,
        };
        let response = register(
            &mut state,
            "session-1",
            key,
            req,
            chrono::Utc::now() + chrono::Duration::minutes(30),
        );
        assert_eq!(response["head"], 1);
        let id = response["id"].as_str().unwrap().to_owned();
        assert_eq!(response["cursor"], format!("{id}:1"));
        id
    }

    #[tokio::test]
    async fn rejects_forged_payloads_and_filters_content_at_emit_time() {
        let bus = bus();
        let id = add(
            &bus,
            Arc::new(DomObserver::test_observer()),
            "visible",
            false,
        )
        .await;
        for nonce in ["forged".to_owned(), "x".repeat(16 * 1024)] {
            bus.process_raw(raw("change", "visible", &nonce)).await;
        }
        assert_eq!(bus.list(None, false).await[0]["rejected"], 2);
        *bus.domain_filter.write().await = Some(DomainFilter::new("allowed.test"));
        bus.state
            .lock()
            .await
            .sources
            .values_mut()
            .next()
            .unwrap()
            .url = "https://blocked.test".into();
        bus.process_raw(raw("change", "visible", "nonce-1")).await;
        assert_eq!(bus.list(None, false).await[0]["filtered"], 1);
        assert_eq!(
            bus.read(&id, None, Duration::ZERO, 10)
                .await
                .unwrap()
                .events
                .len(),
            1
        );
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
            *bus.policy.write().await = Some(ActionPolicy::load(path.to_str().unwrap()).unwrap());
            let source = CdpMethodSource::new(
                cdp::lookup("Browser.downloadWillBegin").unwrap(),
                "session-1".into(),
                "target-1".into(),
                HashMap::new(),
                Matchers::parse(&[], &[]).unwrap(),
            );
            let id = add(&bus, Arc::new(source), "Browser.downloadWillBegin", false).await;
            if missing {
                std::fs::remove_file(path).unwrap();
            }
            bus.process_raw(RawCdpMessage {
                text: json!({"method": "Browser.downloadWillBegin", "params": {"url": "https://allowed.test/file"}}).to_string(),
                session_id: None,
            }).await;
            let read = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
            assert_eq!(read.status, "ended");
            assert_eq!(read.events.len(), 2);
            assert_eq!(read.events[1].data, json!({"reason": "policy_denied"}));
        }
    }

    #[tokio::test]
    async fn lifecycle_bounds_retain_ended_buffers_until_retention() {
        for reason in ["expired", "browser_closed", "stopped"] {
            let bus = bus();
            let id = add(
                &bus,
                Arc::new(DomObserver::test_observer()),
                "visible",
                false,
            )
            .await;
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
            let read = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
            assert_eq!(read.status, "ended");
            assert_eq!(read.events.len(), 2);
            assert_eq!(read.events[1].data, json!({"reason": reason}));
            bus.state.lock().await.subs.get_mut(&id).unwrap().ended =
                Some(Instant::now() - RETENTION);
            assert!(bus.read(&id, None, Duration::ZERO, 10).await.is_err());
        }
        let bus = bus();
        for _ in 0..MAX_ACTIVE {
            add(
                &bus,
                Arc::new(DomObserver::test_observer()),
                "visible",
                false,
            )
            .await;
        }
        assert!(check_capacity(&*bus.state.lock().await).is_err());
        bus.end_all("browser_closed").await;
        for _ in 0..=MAX_LISTED {
            let id = add(
                &bus,
                Arc::new(DomObserver::test_observer()),
                "visible",
                false,
            )
            .await;
            bus.stop(&id, None).await.unwrap();
        }
        assert_eq!(bus.list(None, false).await.len(), MAX_LISTED);
    }
}
