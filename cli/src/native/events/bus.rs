use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};
use tokio::task::JoinHandle;

use super::buffer::{CursorError, EventBuffer};
use super::envelope::{EventEnvelope, EventTab};
use super::source::{EventSource, SourceSignal};
use super::sources::dom::{normalize_selector, DomObserver, DomState, WORLD_NAME};
use crate::native::cdp::client::{CdpClient, RawCdpMessage};
use crate::native::network::DomainFilter;
use crate::native::policy::ActionPolicy;
use crate::native::stream::StreamServer;

const MAX_ACTIVE: usize = 20;
const MAX_LISTED: usize = 50;
const RETENTION: Duration = Duration::from_secs(600);
const MAX_TTL: Duration = Duration::from_secs(1_800);

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

struct BusState {
    subscriptions: HashMap<String, Subscription>,
    sources: HashMap<String, SharedSource>,
    shared_keys: HashMap<String, String>,
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
    already_satisfied: bool,
}

struct SharedSource {
    source: Arc<dyn EventSource>,
    subscriptions: HashSet<String>,
    current: Option<SourceSignal>,
    navigation_pending: bool,
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
    pub engine: &'request str,
    pub client: Arc<CdpClient>,
    pub source: Arc<dyn EventSource>,
    pub initial: SourceSignal,
    pub tab: EventTab,
    pub current_url: &'request str,
    pub filter_key: String,
    pub rule: Value,
    pub once: bool,
    pub ttl: Duration,
    pub owner: Option<String>,
    pub idempotency_key: Option<String>,
}

struct SubscriptionStart<'request> {
    session_id: &'request str,
    source_key: String,
    policy_actions: Vec<String>,
    filter_key: String,
    rule: Value,
    tab: EventTab,
    owner: Option<String>,
    idempotency_key: Option<String>,
    once: bool,
    expires_at: chrono::DateTime<chrono::Utc>,
    current: &'request SourceSignal,
    already_satisfied: bool,
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
            state: Mutex::new(BusState {
                subscriptions: HashMap::new(),
                sources: HashMap::new(),
                shared_keys: HashMap::new(),
            }),
            notify: Notify::new(),
            operation: Mutex::new(()),
            receiver: Mutex::new(None),
        }
    }

    pub async fn start_dom(self: &Arc<Self>, request: DomStart<'_>) -> Result<Value, String> {
        let DomStart {
            engine,
            client,
            session_id,
            tab,
            current_url,
            selector,
            state,
            once,
            ttl,
            owner,
            idempotency_key,
        } = request;
        if engine != "chrome" {
            return Err("Browser events are supported only with the Chrome engine".into());
        }
        let selector = normalize_selector(selector)?;
        let target_id = tab.target_id.as_str();
        if ttl < Duration::from_secs(1) || ttl > MAX_TTL {
            return Err("Event subscription TTL must be between 1 second and 30 minutes".into());
        }
        let filter_key = match state {
            "visible" => "visible",
            "hidden" => "hidden",
            _ => return Err("Event state must be visible or hidden".into()),
        }
        .to_owned();
        self.attach(client.clone()).await?;
        let _operation = self.operation.lock().await;
        self.expire().await;

        {
            let mut state_guard = self.state.lock().await;
            prune(&mut state_guard);
            if let Some(existing) =
                idempotent_subscription(&state_guard, owner.as_deref(), idempotency_key.as_deref())
            {
                return Ok(start_response(existing));
            }
            check_active_capacity(&state_guard)?;
        }

        let sharing_key = format!("{}:{}:{}", target_id, WORLD_NAME, selector);
        let existing_source = {
            let state_guard = self.state.lock().await;
            state_guard
                .shared_keys
                .get(&sharing_key)
                .and_then(|source_key| state_guard.sources.get(source_key))
                .map(|source| {
                    (
                        source.source.clone(),
                        source.current.clone(),
                        source.url.clone(),
                    )
                })
        };
        let (source, current, source_key, url) = match existing_source {
            Some((source, current, url)) => {
                let source_key = sharing_key.clone();
                (source, current, source_key, url)
            }
            None => {
                let (observer, initial) =
                    DomObserver::install(&client, session_id, target_id, &selector).await?;
                let source: Arc<dyn EventSource> = Arc::new(observer);
                let current = observation_signal(initial);
                (
                    source,
                    Some(current),
                    sharing_key.clone(),
                    current_url.to_owned(),
                )
            }
        };
        if source.session_id() != session_id {
            return Err("Event observer session changed while subscribing".into());
        }
        let state_name = filter_key.clone();
        let current = current.ok_or_else(|| "Event observer has no current state".to_string())?;
        let already_satisfied =
            matches!(&current, SourceSignal::Match { key, .. } if key == &filter_key);
        let expires_at = chrono::Utc::now()
            + chrono::Duration::from_std(ttl).map_err(|error| error.to_string())?;
        let policy_actions = source
            .policy_actions()
            .iter()
            .map(|action| (*action).to_owned())
            .collect();
        let response = {
            let mut state_guard = self.state.lock().await;
            if !state_guard.sources.contains_key(&source_key) {
                state_guard
                    .shared_keys
                    .insert(sharing_key, source_key.clone());
                state_guard.sources.insert(
                    source_key.clone(),
                    SharedSource {
                        source: source.clone(),
                        subscriptions: HashSet::new(),
                        current: Some(current.clone()),
                        navigation_pending: false,
                        url,
                        tab: tab.clone(),
                    },
                );
            }
            add_subscription(
                &mut state_guard,
                SubscriptionStart {
                    session_id: &self.session_id,
                    source_key,
                    policy_actions,
                    filter_key,
                    rule: json!({"selector": selector, "state": state_name}),
                    tab,
                    owner,
                    idempotency_key,
                    once,
                    expires_at,
                    current: &current,
                    already_satisfied,
                },
            )
        };
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
        Ok(response)
    }

    pub(super) async fn subscribe_source(
        self: &Arc<Self>,
        request: SourceStart<'_>,
    ) -> Result<Value, String> {
        self.attach(request.client.clone()).await?;
        let _operation = self.operation.lock().await;
        self.expire().await;
        if !request.source.supported_engines().contains(&request.engine) {
            return Err("Event source does not support the selected browser engine".into());
        }
        if request.ttl < Duration::from_secs(1) || request.ttl > MAX_TTL {
            return Err("Event subscription TTL must be between 1 second and 30 minutes".into());
        }
        let mut state = self.state.lock().await;
        let policy_actions = request
            .source
            .policy_actions()
            .iter()
            .map(|action| (*action).to_owned())
            .collect();
        prune(&mut state);
        if let Some(existing) = idempotent_subscription(
            &state,
            request.owner.as_deref(),
            request.idempotency_key.as_deref(),
        ) {
            let response = start_response(existing);
            let stop_source = state
                .sources
                .get(&existing.source_key)
                .is_none_or(|source| !Arc::ptr_eq(&source.source, &request.source));
            drop(state);
            if stop_source {
                request.source.stop(&request.client).await;
            }
            return Ok(response);
        }
        check_active_capacity(&state)?;
        let sharing_key = request.source.sharing_key();
        let source_key = sharing_key
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
        let existing = sharing_key
            .as_ref()
            .and_then(|key| state.shared_keys.get(key))
            .and_then(|key| state.sources.get(key))
            .map(|source| (source.source.clone(), source.current.clone()));
        let (source, current, stop_duplicate) = match existing {
            Some((source, Some(current))) => {
                let stop_duplicate = !Arc::ptr_eq(&source, &request.source);
                (source, current, stop_duplicate)
            }
            Some((_, None)) => return Err("Event source has no current state".into()),
            None => (request.source.clone(), request.initial.clone(), false),
        };
        if !state.sources.contains_key(&source_key) {
            if let Some(sharing_key) = sharing_key {
                state.shared_keys.insert(sharing_key, source_key.clone());
            }
            state.sources.insert(
                source_key.clone(),
                SharedSource {
                    source,
                    subscriptions: HashSet::new(),
                    current: Some(current.clone()),
                    navigation_pending: false,
                    url: request.current_url.to_owned(),
                    tab: request.tab.clone(),
                },
            );
        } else if let Some(shared) = state.sources.get_mut(&source_key) {
            shared.url = request.current_url.to_owned();
            shared.tab = request.tab.clone();
        }
        let already_satisfied = matches!(
            &current,
            SourceSignal::Match { key, .. } if key == &request.filter_key
        );
        let expires_at = chrono::Utc::now()
            + chrono::Duration::from_std(request.ttl).map_err(|error| error.to_string())?;
        let response = add_subscription(
            &mut state,
            SubscriptionStart {
                session_id: &self.session_id,
                source_key,
                policy_actions,
                filter_key: request.filter_key,
                rule: request.rule,
                tab: request.tab,
                owner: request.owner,
                idempotency_key: request.idempotency_key,
                once: request.once,
                expires_at,
                current: &current,
                already_satisfied,
            },
        );
        drop(state);
        if stop_duplicate {
            request.source.stop(&request.client).await;
        }
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
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
            .subscriptions
            .get(id)
            .map(|subscription| subscription.policy_actions.clone())
            .ok_or_else(|| {
                "Event subscription was lost; the daemon may have restarted or the subscription expired".into()
            })
    }

    async fn nudge_all_heads(&self) {
        let Some(server) = self.stream_server.read().await.clone() else {
            return;
        };
        let heads = self
            .state
            .lock()
            .await
            .subscriptions
            .values()
            .map(|subscription| (subscription.id.clone(), subscription.buffer.head()))
            .collect::<Vec<_>>();
        for (id, head) in heads {
            server.nudge_event(&id, head);
        }
    }

    async fn attach(self: &Arc<Self>, client: Arc<CdpClient>) -> Result<(), String> {
        let _operation = self.operation.lock().await;
        let previous = {
            let mut receiver = self.receiver.lock().await;
            if receiver
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(&current.client, &client))
            {
                return Ok(());
            }
            receiver.take()
        };
        if let Some(previous) = previous {
            previous.task.abort();
            self.end_all_with_client("browser_closed", Some(previous.client))
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
                        Err(broadcast::error::RecvError::Lagged(missed)) => {
                            bus.handle_gap(&receiver_client, missed).await;
                        }
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
        let event: Value = match serde_json::from_str(&raw.text) {
            Ok(event) => event,
            Err(_) => return,
        };
        let method = event.get("method").and_then(Value::as_str).unwrap_or("");
        let session_id = raw
            .session_id
            .as_deref()
            .or_else(|| event.get("sessionId").and_then(Value::as_str));
        match method {
            "Target.targetDestroyed" | "Target.targetCrashed" => {
                if let Some(target_id) = event.pointer("/params/targetId").and_then(Value::as_str) {
                    self.end_target(
                        target_id,
                        if method.ends_with("Crashed") {
                            "tab_crashed"
                        } else {
                            "tab_closed"
                        },
                        client,
                    )
                    .await;
                }
                return;
            }
            "Inspector.targetCrashed" => {
                if let Some(session_id) = session_id {
                    self.end_session(session_id, "tab_crashed", client).await;
                }
                return;
            }
            "Page.frameNavigated" if event.pointer("/params/frame/parentId").is_none() => {
                if let (Some(session_id), Some(url)) = (
                    session_id,
                    event.pointer("/params/frame/url").and_then(Value::as_str),
                ) {
                    self.update_url(session_id, url).await;
                    self.rearm_session(session_id).await;
                }
            }
            _ => {}
        }
        self.process_raw(raw.clone()).await;
        if method == "Page.navigatedWithinDocument" {
            if let Some(session_id) = session_id {
                self.observe_pending(session_id, client, "afterNavigation")
                    .await;
            }
        }
        if method == "Runtime.bindingCalled" {
            if let Some(session_id) = session_id {
                self.observe_pending(session_id, client, "afterNavigation")
                    .await;
            }
        }
        if method == "Page.frameNavigated" {
            if let Some(session_id) = session_id {
                self.observe_pending(session_id, client, "afterNavigation")
                    .await;
            }
        }
    }

    pub async fn process_raw(&self, raw: RawCdpMessage) {
        let mut classified = Vec::new();
        {
            let state = self.state.lock().await;
            for (source_key, source) in &state.sources {
                if let Some(signal) = source.source.classify(&raw) {
                    classified.push((source_key.clone(), signal));
                }
            }
        }
        for (source_key, signal) in classified {
            match signal {
                SourceSignal::Rearmed => self.mark_rearmed(&source_key).await,
                SourceSignal::Rejected => self.count_rejected(&source_key).await,
                signal => self.emit_signal(&source_key, signal, None).await,
            }
        }
    }

    async fn update_url(&self, session_id: &str, url: &str) {
        let mut state = self.state.lock().await;
        for source in state.sources.values_mut() {
            if source.source.session_id() == session_id {
                source.url = url.to_owned();
            }
        }
    }

    async fn mark_rearmed(&self, source_key: &str) {
        if let Some(source) = self.state.lock().await.sources.get_mut(source_key) {
            source.navigation_pending = true;
        }
    }

    async fn record_rearmed(&self, source_key: &str) {
        let mut state = self.state.lock().await;
        let Some(source) = state.sources.get_mut(source_key) else {
            return;
        };
        source.navigation_pending = false;
        let ids: Vec<String> = source.subscriptions.iter().cloned().collect();
        let tab = source.tab.clone();
        for id in ids {
            if let Some(subscription) = state.subscriptions.get_mut(&id) {
                subscription.buffer.append(EventEnvelope::new(
                    "subscription.rearmed",
                    &self.session_id,
                    &tab.id,
                    &tab.label,
                    &tab.target_id,
                    json!({}),
                ));
            }
        }
        drop(state);
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
    }

    async fn rearm_session(&self, session_id: &str) {
        let source_keys: Vec<String> = {
            let state = self.state.lock().await;
            state
                .sources
                .iter()
                .filter(|(_, source)| source.source.session_id() == session_id)
                .map(|(key, _)| key.clone())
                .collect()
        };
        for source_key in source_keys {
            self.mark_rearmed(&source_key).await;
        }
    }

    async fn observe_pending(&self, session_id: &str, client: &Arc<CdpClient>, flag: &str) {
        let sources: Vec<(String, Arc<dyn EventSource>)> = {
            let state = self.state.lock().await;
            state
                .sources
                .iter()
                .filter(|(_, source)| {
                    source.source.session_id() == session_id && source.navigation_pending
                })
                .map(|(key, source)| (key.clone(), source.source.clone()))
                .collect()
        };
        for (source_key, source) in sources {
            let observed = source.observe(client).await;
            if let Ok(Some(signal)) = observed {
                self.emit_signal(&source_key, signal, Some(flag)).await;
            }
            let pending = self
                .state
                .lock()
                .await
                .sources
                .get(&source_key)
                .is_some_and(|source| source.navigation_pending);
            if pending {
                self.record_rearmed(&source_key).await;
            }
        }
    }

    async fn handle_gap(&self, client: &Arc<CdpClient>, missed: u64) {
        let sources = self.append_gap(missed).await;
        for (source_key, source) in sources {
            if let Ok(Some(signal)) = source.observe(client).await {
                self.emit_signal(&source_key, signal, Some("afterGap"))
                    .await;
            }
        }
    }

    async fn append_gap(&self, missed: u64) -> Vec<(String, Arc<dyn EventSource>)> {
        let sources = {
            let mut state = self.state.lock().await;
            for subscription in state
                .subscriptions
                .values_mut()
                .filter(|subscription| subscription.status == "active")
            {
                subscription.buffer.append(EventEnvelope::new(
                    "subscription.gap",
                    &self.session_id,
                    &subscription.tab.id,
                    &subscription.tab.label,
                    &subscription.tab.target_id,
                    json!({"missed": missed}),
                ));
            }
            state
                .sources
                .iter()
                .map(|(key, source)| (key.clone(), source.source.clone()))
                .collect()
        };
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
        sources
    }

    async fn emit_signal(&self, source_key: &str, signal: SourceSignal, forced_flag: Option<&str>) {
        let SourceSignal::Match {
            key,
            name,
            mut data,
            initial,
        } = signal
        else {
            return;
        };
        let policy_actions = {
            let state = self.state.lock().await;
            state
                .sources
                .get(source_key)
                .map(|source| {
                    source
                        .source
                        .policy_actions()
                        .iter()
                        .map(|action| (*action).to_owned())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        if crate::native::actions::check_event_source_policy(&self.policy, &policy_actions)
            .await
            .is_err()
        {
            self.end_source(source_key, "policy_denied").await;
            return;
        }
        let (ids, url, tab, navigation_pending, rearmed) = {
            let mut state = self.state.lock().await;
            let Some(source) = state.sources.get_mut(source_key) else {
                return;
            };
            source.current = Some(SourceSignal::Match {
                key: key.clone(),
                name: name.clone(),
                data: data.clone(),
                initial,
            });
            let pending = source.navigation_pending;
            let rearmed = pending && (forced_flag == Some("afterNavigation") || initial);
            if rearmed {
                source.navigation_pending = false;
            }
            (
                source.subscriptions.iter().cloned().collect::<Vec<_>>(),
                source.url.clone(),
                source.tab.clone(),
                pending,
                rearmed,
            )
        };
        let flag =
            forced_flag.or_else(|| (initial && navigation_pending).then_some("afterNavigation"));
        if initial && flag.is_none() {
            return;
        }
        if let Some(flag) = flag {
            if let Some(object) = data.as_object_mut() {
                object.insert(flag.to_owned(), Value::Bool(true));
            }
        }
        let allowed = self.url_allowed(&url).await;
        let mut state = self.state.lock().await;
        let mut to_finish = Vec::new();
        for id in ids {
            let Some(subscription) = state.subscriptions.get_mut(&id) else {
                continue;
            };
            if subscription.status != "active" {
                continue;
            }
            if rearmed {
                subscription.buffer.append(EventEnvelope::new(
                    "subscription.rearmed",
                    &self.session_id,
                    &tab.id,
                    &tab.label,
                    &tab.target_id,
                    json!({}),
                ));
            }
            if subscription.filter_key != key {
                continue;
            }
            if !allowed {
                subscription.filtered += 1;
                continue;
            }
            subscription.buffer.append(EventEnvelope::new(
                &name,
                &self.session_id,
                &tab.id,
                &tab.label,
                &tab.target_id,
                data.clone(),
            ));
            if subscription.once {
                to_finish.push(id);
            }
        }
        let mut stopped = Vec::new();
        for id in to_finish {
            if let Some(source) = finish_subscription(&mut state, &id, "matched") {
                stopped.push(source);
            }
        }
        drop(state);
        if let Some(client) = self.current_client().await {
            for source in stopped {
                source.stop(&client).await;
            }
        }
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
    }

    async fn end_source(&self, source_key: &str, reason: &str) {
        let mut stopped = Vec::new();
        {
            let mut state = self.state.lock().await;
            let ids = state
                .sources
                .get(source_key)
                .map(|source| source.subscriptions.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for id in ids {
                if let Some(source) = finish_subscription(&mut state, &id, reason) {
                    stopped.push(source);
                }
            }
        }
        if let Some(client) = self.current_client().await {
            for source in stopped {
                source.stop(&client).await;
            }
        }
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
    }

    async fn url_allowed(&self, url: &str) -> bool {
        let filter = self.domain_filter.read().await;
        match filter.as_ref() {
            None => true,
            Some(_) if url == "about:blank" => true,
            Some(filter) => filter.check_url(url).is_ok(),
        }
    }

    async fn count_rejected(&self, source_key: &str) {
        let mut state = self.state.lock().await;
        let ids = state
            .sources
            .get(source_key)
            .map(|source| source.subscriptions.iter().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        for id in ids {
            if let Some(subscription) = state.subscriptions.get_mut(&id) {
                if subscription.status == "active" {
                    subscription.rejected += 1;
                }
            }
        }
    }

    async fn end_target(&self, target_id: &str, reason: &str, client: &Arc<CdpClient>) {
        self.end_matching(reason, client, |source| source.target_id() == target_id)
            .await;
    }

    async fn end_session(&self, session_id: &str, reason: &str, client: &Arc<CdpClient>) {
        self.end_matching(reason, client, |source| source.session_id() == session_id)
            .await;
    }

    async fn end_matching<F>(&self, reason: &str, client: &Arc<CdpClient>, matches: F)
    where
        F: Fn(&dyn EventSource) -> bool,
    {
        let source_keys: Vec<String> = {
            let state = self.state.lock().await;
            state
                .sources
                .iter()
                .filter(|(_, source)| matches(source.source.as_ref()))
                .map(|(key, _)| key.clone())
                .collect()
        };
        let mut stopped = Vec::new();
        for source_key in source_keys {
            let mut state = self.state.lock().await;
            let ids = state
                .sources
                .get(&source_key)
                .map(|source| source.subscriptions.iter().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            for id in ids {
                if let Some(source) = finish_subscription(&mut state, &id, reason) {
                    stopped.push(source);
                }
            }
        }
        for source in stopped {
            source.stop(client).await;
        }
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
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
                .map(|source| source.source.clone())
                .collect::<Vec<_>>();
            let ids: Vec<String> = state
                .subscriptions
                .values()
                .filter(|subscription| subscription.status == "active")
                .map(|subscription| subscription.id.clone())
                .collect();
            for id in ids {
                finish_subscription(&mut state, &id, reason);
            }
            state.sources.clear();
            state.shared_keys.clear();
            sources
        };
        if let Some(client) = client {
            for source in sources {
                source.stop(&client).await;
            }
        }
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
    }

    async fn expire(&self) {
        let client = self.current_client().await;
        let now = chrono::Utc::now();
        let mut state = self.state.lock().await;
        let expired: Vec<String> = state
            .subscriptions
            .values()
            .filter(|subscription| {
                subscription.status == "active" && subscription.expires_at <= now
            })
            .map(|subscription| subscription.id.clone())
            .collect();
        let mut stopped = Vec::new();
        for id in expired {
            if let Some(source) = finish_subscription(&mut state, &id, "expired") {
                stopped.push(source);
            }
        }
        prune(&mut state);
        drop(state);
        if let Some(client) = client {
            for source in stopped {
                source.stop(&client).await;
            }
        }
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
    }

    async fn current_client(&self) -> Option<Arc<CdpClient>> {
        self.receiver
            .lock()
            .await
            .as_ref()
            .map(|receiver| receiver.client.clone())
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
            let result = {
                let mut state = self.state.lock().await;
                prune(&mut state);
                let subscription = state
                    .subscriptions
                    .get(id)
                    .ok_or_else(|| {
                        "Event subscription was lost; the daemon may have restarted or the subscription expired".to_string()
                    })?;
                let events = subscription
                    .buffer
                    .read(since, limit)
                    .map_err(cursor_error)?;
                let cursor = events
                    .last()
                    .map(|event| event.meta.cursor.clone())
                    .or_else(|| since.map(str::to_owned))
                    .unwrap_or_else(|| subscription.buffer.start_cursor());
                ReadResult {
                    id: id.to_owned(),
                    status: subscription.status,
                    events,
                    cursor,
                    head: subscription.buffer.head(),
                    consumed: subscription.buffer.consumed(),
                    waiting: false,
                }
            };
            if !result.events.is_empty() || wait.is_zero() {
                return Ok(result);
            }
            if tokio::time::Instant::now() >= deadline {
                let mut result = result;
                result.waiting = true;
                return Ok(result);
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                let mut result = result;
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
            let state = self.state.lock().await;
            result.consumed = state
                .subscriptions
                .get(id)
                .ok_or_else(|| "Unknown event subscription".to_string())?
                .buffer
                .consumed();
        }
        Ok(result)
    }

    pub async fn ack(&self, id: &str, cursor: &str, owner: Option<&str>) -> Result<(), String> {
        self.expire().await;
        let mut state = self.state.lock().await;
        let subscription = state
            .subscriptions
            .get_mut(id)
            .ok_or_else(|| "Unknown event subscription".to_string())?;
        check_owner(subscription, owner)?;
        subscription.buffer.ack_cursor(cursor).map_err(cursor_error)
    }

    pub async fn stop(&self, id: &str, owner: Option<&str>) -> Result<bool, String> {
        self.expire().await;
        let client = self.current_client().await;
        let stopped = {
            let mut state = self.state.lock().await;
            let subscription = state
                .subscriptions
                .get(id)
                .ok_or_else(|| "Unknown event subscription".to_string())?;
            check_owner(subscription, owner)?;
            finish_subscription(&mut state, id, "stopped")
        };
        if let (Some(source), Some(client)) = (stopped, client) {
            source.stop(&client).await;
        }
        self.nudge_all_heads().await;
        self.notify.notify_waiters();
        Ok(true)
    }

    pub async fn list(&self, owner: Option<&str>, unread: bool) -> Vec<Value> {
        self.expire().await;
        let state = self.state.lock().await;
        let mut subscriptions: Vec<&Subscription> = state
            .subscriptions
            .values()
            .filter(|subscription| {
                owner.is_none_or(|owner| subscription.owner.as_deref() == Some(owner))
            })
            .filter(|subscription| {
                !unread || subscription.buffer.head() > subscription.buffer.consumed()
            })
            .collect();
        subscriptions.sort_by_key(|subscription| subscription.created);
        subscriptions
            .into_iter()
            .rev()
            .take(MAX_LISTED)
            .map(|subscription| {
                json!({
                    "id": subscription.id,
                    "status": subscription.status,
                    "reason": subscription.reason,
                    "rule": subscription.rule,
                    "owner": subscription.owner,
                    "head": subscription.buffer.head(),
                    "consumed": subscription.buffer.consumed(),
                    "dropped": subscription.buffer.dropped(),
                    "filtered": subscription.filtered,
                    "rejected": subscription.rejected,
                    "tab": subscription.tab,
                    "expiresAt": subscription.expires_at.to_rfc3339()
                })
            })
            .collect()
    }

    #[cfg(test)]
    async fn test_add_subscription(
        &self,
        source: Arc<dyn EventSource>,
        current: SourceSignal,
        filter_key: &str,
        once: bool,
    ) -> String {
        let source_key = source.sharing_key().unwrap();
        let sharing_key = source_key.clone();
        let tab = EventTab {
            id: "t1".into(),
            label: "test".into(),
            target_id: source.target_id().into(),
        };
        let policy_actions = source
            .policy_actions()
            .iter()
            .map(|action| (*action).to_owned())
            .collect();
        let mut state = self.state.lock().await;
        check_active_capacity(&state).unwrap();
        state.shared_keys.insert(sharing_key, source_key.clone());
        state
            .sources
            .entry(source_key.clone())
            .or_insert_with(|| SharedSource {
                source,
                subscriptions: HashSet::new(),
                current: Some(current.clone()),
                navigation_pending: false,
                url: "https://allowed.test".into(),
                tab: tab.clone(),
            });
        let already_satisfied =
            matches!(&current, SourceSignal::Match { key, .. } if key == filter_key);
        let now = chrono::Utc::now();
        let response = add_subscription(
            &mut state,
            SubscriptionStart {
                session_id: &self.session_id,
                source_key,
                policy_actions,
                filter_key: filter_key.into(),
                rule: json!({"selector": ".ready", "state": filter_key}),
                tab,
                owner: None,
                idempotency_key: None,
                once,
                expires_at: now + chrono::Duration::hours(1),
                current: &current,
                already_satisfied,
            },
        );
        response["id"].as_str().unwrap().to_owned()
    }

    #[cfg(test)]
    fn test_bus() -> Arc<Self> {
        Arc::new(Self::new(
            "session-1".into(),
            Arc::new(RwLock::new(None)),
            Arc::new(RwLock::new(None)),
        ))
    }
}

fn add_subscription(state: &mut BusState, start: SubscriptionStart<'_>) -> Value {
    let SubscriptionStart {
        session_id,
        source_key,
        policy_actions,
        filter_key,
        rule,
        tab,
        owner,
        idempotency_key,
        once,
        expires_at,
        current,
        already_satisfied,
    } = start;
    let buffer = EventBuffer::new();
    let id = buffer.start_cursor().split(':').next().unwrap().to_owned();
    let mut subscription = Subscription {
        id: id.clone(),
        session_id: session_id.to_owned(),
        source_key: source_key.clone(),
        policy_actions,
        filter_key,
        rule,
        owner,
        idempotency_key,
        once,
        expires_at,
        created: Instant::now(),
        ended: None,
        status: "active",
        reason: None,
        tab: tab.clone(),
        buffer,
        filtered: 0,
        rejected: 0,
        already_satisfied,
    };
    subscription.buffer.append(EventEnvelope::new(
        "subscription.ready",
        session_id,
        &tab.id,
        &tab.label,
        &tab.target_id,
        json!({
            "state": match current {
                SourceSignal::Match { key, .. } => key.as_str(),
                _ => "unknown",
            },
            "alreadySatisfied": already_satisfied
        }),
    ));
    let head = subscription.buffer.head();
    let cursor = subscription.buffer.head_cursor();
    let expires_at_value = expires_at.to_rfc3339();
    state.subscriptions.insert(id.clone(), subscription);
    if let Some(source) = state.sources.get_mut(&source_key) {
        source.subscriptions.insert(id.clone());
    }
    json!({
        "id": id,
        "head": head,
        "cursor": cursor,
        "alreadySatisfied": already_satisfied,
        "expiresAt": expires_at_value,
        "owner": state.subscriptions[&id].owner,
        "tab": tab
    })
}

fn check_active_capacity(state: &BusState) -> Result<(), String> {
    if state
        .subscriptions
        .values()
        .filter(|subscription| subscription.status == "active")
        .count()
        >= MAX_ACTIVE
    {
        Err(format!(
            "At most {MAX_ACTIVE} event subscriptions may be active"
        ))
    } else {
        Ok(())
    }
}

fn observation_signal(observation: super::sources::dom::Observation) -> SourceSignal {
    let (key, state) = match observation.state {
        DomState::Visible => ("visible", "visible"),
        DomState::Hidden => ("hidden", "hidden"),
    };
    SourceSignal::Match {
        key: key.into(),
        name: format!("element.{state}"),
        data: json!({
            "state": state,
            "label": observation.label,
            "truncated": observation.truncated
        }),
        initial: observation.initial,
    }
}

fn finish_subscription(
    state: &mut BusState,
    id: &str,
    reason: &str,
) -> Option<Arc<dyn EventSource>> {
    let subscription = state.subscriptions.get_mut(id)?;
    if subscription.status != "active" {
        return None;
    }
    subscription.status = "ended";
    subscription.reason = Some(reason.to_owned());
    subscription.ended = Some(Instant::now());
    subscription.buffer.append(EventEnvelope::new(
        "subscription.ended",
        &subscription.session_id,
        &subscription.tab.id,
        &subscription.tab.label,
        &subscription.tab.target_id,
        json!({"reason": reason}),
    ));
    let source_key = subscription.source_key.clone();
    if let Some(source) = state.sources.get_mut(&source_key) {
        source.subscriptions.remove(id);
        if source.subscriptions.is_empty() {
            let source = state.sources.remove(&source_key).unwrap();
            state.shared_keys.retain(|_, key| key != &source_key);
            return Some(source.source);
        }
    }
    None
}

fn prune(state: &mut BusState) {
    let now = Instant::now();
    state.subscriptions.retain(|_, subscription| {
        subscription
            .ended
            .is_none_or(|ended| now.duration_since(ended) < RETENTION)
    });
    if state.subscriptions.len() > MAX_LISTED {
        let mut ended: Vec<(String, Instant)> = state
            .subscriptions
            .iter()
            .filter_map(|(id, subscription)| subscription.ended.map(|ended| (id.clone(), ended)))
            .collect();
        ended.sort_by_key(|(_, ended)| *ended);
        for (id, _) in ended
            .into_iter()
            .take(state.subscriptions.len() - MAX_LISTED)
        {
            state.subscriptions.remove(&id);
        }
    }
}

fn idempotent_subscription<'a>(
    state: &'a BusState,
    owner: Option<&str>,
    key: Option<&str>,
) -> Option<&'a Subscription> {
    let key = key?;
    state.subscriptions.values().find(|subscription| {
        subscription.owner.as_deref() == owner
            && subscription.idempotency_key.as_deref() == Some(key)
    })
}

fn start_response(subscription: &Subscription) -> Value {
    json!({
        "id": subscription.id,
        "head": subscription.buffer.head(),
        "cursor": subscription.buffer.head_cursor(),
        "alreadySatisfied": subscription.already_satisfied,
        "expiresAt": subscription.expires_at.to_rfc3339(),
        "owner": subscription.owner,
        "tab": subscription.tab
    })
}

fn check_owner(subscription: &Subscription, owner: Option<&str>) -> Result<(), String> {
    if let Some(owner) = owner {
        if subscription.owner.as_deref() != Some(owner) {
            return Err("Event subscription owner does not match".into());
        }
    }
    Ok(())
}

fn cursor_error(error: CursorError) -> String {
    match error {
        CursorError::CursorInvalid => "Invalid event cursor".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus() -> Arc<Bus> {
        Bus::test_bus()
    }

    fn source() -> Arc<dyn EventSource> {
        Arc::new(DomObserver::test_observer())
    }

    fn observation(key: &str, initial: bool) -> SourceSignal {
        SourceSignal::Match {
            key: key.into(),
            name: format!("element.{key}"),
            data: json!({"state": key, "label": "Ready", "truncated": false}),
            initial,
        }
    }

    fn raw(kind: &str, state: &str, nonce: &str) -> RawCdpMessage {
        RawCdpMessage {
            text: json!({
                "sessionId": "session-1",
                "method": "Runtime.bindingCalled",
                "params": {
                    "name": "__ab_event_test",
                    "payload": json!({
                        "nonce": nonce,
                        "kind": kind,
                        "state": state,
                        "label": "Ready",
                        "truncated": false
                    }).to_string()
                }
            })
            .to_string(),
            session_id: Some("session-1".into()),
        }
    }

    async fn add(bus: &Bus, source: Arc<dyn EventSource>, once: bool) -> String {
        bus.test_add_subscription(source, observation("hidden", true), "visible", once)
            .await
    }

    #[tokio::test]
    async fn shared_selector_delivers_to_both_subscribers() {
        let bus = bus();
        let source_handle = source();
        let first = add(&bus, source_handle.clone(), false).await;
        let second = add(&bus, source_handle, false).await;

        bus.process_raw(raw("change", "visible", "nonce-1")).await;

        let first_events = bus
            .read(&first, None, Duration::ZERO, 10)
            .await
            .unwrap()
            .events;
        let second_events = bus
            .read(&second, None, Duration::ZERO, 10)
            .await
            .unwrap()
            .events;
        assert_eq!(first_events.last().unwrap().meta.name, "element.visible");
        assert_eq!(second_events.last().unwrap().meta.name, "element.visible");

        let current = bus
            .state
            .lock()
            .await
            .sources
            .values()
            .next()
            .unwrap()
            .current
            .clone()
            .unwrap();
        let late = bus
            .test_add_subscription(source(), current, "visible", false)
            .await;
        let ready = bus.read(&late, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(ready.events.len(), 1);
        assert_eq!(ready.events[0].data["state"], "visible");
        assert_eq!(ready.events[0].data["alreadySatisfied"], true);
    }

    #[tokio::test]
    async fn stopping_one_shared_subscription_keeps_the_other_live() {
        let bus = bus();
        let source = source();
        let first = add(&bus, source.clone(), false).await;
        let second = add(&bus, source, false).await;

        bus.stop(&first, None).await.unwrap();
        bus.process_raw(raw("change", "visible", "nonce-1")).await;

        let first = bus.read(&first, None, Duration::ZERO, 10).await.unwrap();
        let second = bus.read(&second, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(first.status, "ended");
        assert_eq!(first.events.last().unwrap().meta.name, "subscription.ended");
        assert_eq!(second.events.last().unwrap().meta.name, "element.visible");
    }

    #[tokio::test]
    async fn once_ends_after_the_first_matching_transition() {
        let bus = bus();
        let id = add(&bus, source(), true).await;

        bus.process_raw(raw("change", "visible", "nonce-1")).await;

        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(result.status, "ended");
        assert_eq!(result.events[1].meta.name, "element.visible");
        assert_eq!(result.events[2].data, json!({"reason": "matched"}));
    }

    #[tokio::test]
    async fn read_waits_until_a_new_event_arrives() {
        let bus = bus();
        let id = add(&bus, source(), false).await;
        let cursor = bus.state.lock().await.subscriptions[&id]
            .buffer
            .head_cursor();
        let waiting_bus = bus.clone();
        let waiting_id = id.clone();
        let read = tokio::spawn(async move {
            waiting_bus
                .read(&waiting_id, Some(&cursor), Duration::from_secs(1), 10)
                .await
                .unwrap()
        });
        tokio::time::sleep(Duration::from_millis(10)).await;

        bus.process_raw(raw("change", "visible", "nonce-1")).await;

        let result = read.await.unwrap();
        assert!(!result.waiting);
        assert_eq!(result.events[0].meta.name, "element.visible");
    }

    #[tokio::test]
    async fn head_is_a_sequence_and_start_includes_a_cursor() {
        let bus = bus();
        let id = add(&bus, source(), false).await;
        let response = start_response(&bus.state.lock().await.subscriptions[&id]);
        assert_eq!(response["head"], 1);
        assert_eq!(response["cursor"], format!("{id}:1"));

        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(result.head, 1);
        assert_eq!(result.cursor, format!("{id}:1"));

        let listed = bus.list(None, false).await;
        assert_eq!(listed[0]["head"], 1);
    }

    #[tokio::test]
    async fn expired_subscription_ends_and_remains_readable() {
        let bus = bus();
        let id = add(&bus, source(), false).await;
        bus.state
            .lock()
            .await
            .subscriptions
            .get_mut(&id)
            .unwrap()
            .expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);

        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(result.status, "ended");
        assert_eq!(
            result.events.last().unwrap().data,
            json!({"reason": "expired"})
        );
    }

    #[tokio::test]
    async fn browser_close_ends_subscriptions_without_erasing_their_buffers() {
        let bus = bus();
        let id = add(&bus, source(), false).await;

        bus.end_all("browser_closed").await;

        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(result.status, "ended");
        assert_eq!(
            result.events.last().unwrap().data,
            json!({"reason": "browser_closed"})
        );
    }

    #[tokio::test]
    async fn lag_records_gap_and_after_gap_observation() {
        let bus = bus();
        let id = add(&bus, source(), false).await;
        let source_key = bus.state.lock().await.subscriptions[&id].source_key.clone();

        bus.append_gap(4).await;
        bus.emit_signal(&source_key, observation("visible", true), Some("afterGap"))
            .await;

        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(result.events[1].meta.name, "subscription.gap");
        assert_eq!(result.events[1].data, json!({"missed": 4}));
        assert_eq!(result.events[2].meta.name, "element.visible");
        assert_eq!(result.events[2].data["afterGap"], true);
    }

    #[tokio::test]
    async fn rearmed_initial_observation_is_marked_after_navigation() {
        let bus = bus();
        let id = add(&bus, source(), false).await;
        let source_key = bus.state.lock().await.subscriptions[&id].source_key.clone();

        bus.mark_rearmed(&source_key).await;
        bus.process_raw(raw("initial", "visible", "nonce-1")).await;

        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(result.events[1].meta.name, "subscription.rearmed");
        assert_eq!(result.events[2].data["afterNavigation"], true);
    }

    #[tokio::test]
    async fn forged_and_oversized_payloads_are_counted_and_dropped() {
        let bus = bus();
        let id = add(&bus, source(), false).await;
        bus.process_raw(raw("change", "visible", "wrong-nonce"))
            .await;
        let oversized = RawCdpMessage {
            text: json!({
                "sessionId": "session-1",
                "method": "Runtime.bindingCalled",
                "params": {
                    "name": "__ab_event_test",
                    "payload": "x".repeat(16 * 1024 + 1)
                }
            })
            .to_string(),
            session_id: Some("session-1".into()),
        };
        bus.process_raw(oversized).await;

        let listed = bus.list(None, false).await;
        assert_eq!(listed[0]["rejected"], 2);
        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(result.events.len(), 1);
    }

    #[tokio::test]
    async fn blocked_origin_filters_matching_events_and_counts_them() {
        let bus = Arc::new(Bus::new(
            "session-1".into(),
            Arc::new(RwLock::new(Some(DomainFilter::new("elsewhere.test")))),
            Arc::new(RwLock::new(None)),
        ));
        let id = add(&bus, source(), false).await;
        bus.process_raw(raw("change", "visible", "nonce-1")).await;

        let listed = bus.list(None, false).await;
        let result = bus.read(&id, None, Duration::ZERO, 10).await.unwrap();
        assert_eq!(listed[0]["filtered"], 1);
        assert!(result
            .events
            .iter()
            .all(|event| event.meta.name != "element.visible"));
    }

    #[tokio::test]
    async fn active_subscription_limit_is_twenty() {
        let bus = bus();
        let source = source();
        for _ in 0..MAX_ACTIVE {
            add(&bus, source.clone(), false).await;
        }

        let state = bus.state.lock().await;
        assert_eq!(
            check_active_capacity(&state).unwrap_err(),
            "At most 20 event subscriptions may be active"
        );
    }

    #[tokio::test]
    async fn listing_keeps_only_fifty_recent_finished_subscriptions() {
        let bus = bus();
        for _ in 0..=MAX_LISTED {
            let id = add(&bus, source(), false).await;
            bus.stop(&id, None).await.unwrap();
        }

        assert_eq!(bus.list(None, false).await.len(), MAX_LISTED);
    }

    #[tokio::test]
    async fn ended_subscription_is_lost_after_retention() {
        let bus = bus();
        let id = add(&bus, source(), false).await;
        bus.stop(&id, None).await.unwrap();
        bus.state
            .lock()
            .await
            .subscriptions
            .get_mut(&id)
            .unwrap()
            .ended = Some(Instant::now() - RETENTION);

        assert!(bus.read(&id, None, Duration::ZERO, 10).await.is_err());
    }
}
