//! Experimental subscriptions for visibility transitions in the current Chrome document.
//! Build with `event-prototype`. A socket subscription lives as long as its connection;
//! a registered subscription is held by the daemon until it ends or is stopped.

use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex as StdMutex, MutexGuard};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, watch, Mutex, Notify};
use tokio::task::JoinHandle;

use super::actions::{error_response, success_response, DaemonState};
use super::cdp::client::{CdpClient, RawCdpMessage};
use super::policy::PolicyResult;
use super::stream::StreamServer;

const MAX_ACTIVE: usize = 20;
const MAX_LISTED: usize = 50;
const MAX_EVENTS: usize = 200;
const ARMED: &str = "armed";

#[derive(Deserialize)]
struct Request {
    selector: String,
    #[serde(default = "default_once")]
    once: bool,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

fn default_once() -> bool {
    true
}

impl Request {
    /// Parses a subscription request and resolves its timeout within `1..=max_ms`.
    fn parse(command: &Value, default_ms: u64, max_ms: u64) -> Result<(Self, Duration), String> {
        let request: Self = serde_json::from_value(command.clone()).map_err(|e| e.to_string())?;
        let timeout_ms = request.timeout_ms.unwrap_or(default_ms);
        if request.selector.is_empty() || !(1..=max_ms).contains(&timeout_ms) {
            return Err(format!(
                "Use a nonempty selector and a time limit between 1 and {max_ms} ms"
            ));
        }
        Ok((request, Duration::from_millis(timeout_ms)))
    }
}

fn envelope(id: &str, target: &str, name: &str, data: Value) -> Value {
    json!({
        "eventId": uuid::Uuid::new_v4().to_string(),
        "name": name,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "subscriptionId": id,
        "data": {"tabId": target, "observation": data},
        "cursor": null
    })
}

trait Sink {
    async fn emit(&mut self, message: Value) -> Result<(), String>;
}

#[derive(PartialEq)]
enum Cancel {
    Requested,
    Silent,
}

struct Watch {
    client: Arc<CdpClient>,
    session: String,
    target: String,
    binding: String,
    id: String,
    receiver: broadcast::Receiver<RawCdpMessage>,
}

impl Watch {
    async fn create(state: &Mutex<DaemonState>) -> Result<Self, String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        Self::from_state(&*state.lock().await, id)
    }

    fn from_state(state: &DaemonState, id: String) -> Result<Self, String> {
        if state.engine != "chrome" {
            return Err("The event prototype currently requires Chrome".into());
        }
        let browser = state.browser.as_ref().ok_or("Open a page first")?;
        let client = browser.client.clone();
        Ok(Self {
            receiver: client.subscribe_raw(),
            client,
            session: browser.active_session_id()?.to_owned(),
            target: browser.active_target_id()?.to_owned(),
            binding: format!("__ab_event_{id}"),
            id,
        })
    }

    async fn install(&self, selector: &str) -> Result<Value, String> {
        self.client
            .send_command(
                "Runtime.addBinding",
                Some(json!({"name": self.binding})),
                Some(&self.session),
            )
            .await?;
        let source = include_str!("event-observer.js")
            .replace("__BINDING_JSON__", &json!(self.binding).to_string())
            .replace("__SELECTOR_JSON__", &json!(selector).to_string());
        let result = self
            .client
            .send_command(
                "Runtime.evaluate",
                Some(json!({"expression": source, "returnByValue": true})),
                Some(&self.session),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err("Observer setup failed; check the CSS selector and document".into());
        }
        result
            .pointer("/result/value")
            .cloned()
            .ok_or_else(|| "Observer did not return its initial state".into())
    }

    async fn cleanup(&self) {
        let key = json!(format!("{}_stop", self.binding));
        let binding = json!(self.binding);
        let _ = self
            .client
            .send_command(
                "Runtime.evaluate",
                Some(json!({"expression": format!(
                    "globalThis[{key}]?.(); delete globalThis[{binding}]"
                )})),
                Some(&self.session),
            )
            .await;
        let _ = self
            .client
            .send_command(
                "Runtime.removeBinding",
                Some(json!({"name": self.binding})),
                Some(&self.session),
            )
            .await;
    }

    fn message(&self, name: &str, data: Value) -> Value {
        envelope(&self.id, &self.target, name, data)
    }

    fn classify(&self, raw: &RawCdpMessage) -> Option<(String, Value, bool)> {
        let event: Value = serde_json::from_str(&raw.text).ok()?;
        let method = event.get("method")?.as_str()?;
        if method == "Target.targetDestroyed"
            && event.pointer("/params/targetId")?.as_str()? == self.target
        {
            return Some((
                "subscription.ended".into(),
                json!({"reason": "tab_closed"}),
                true,
            ));
        }
        if event.get("sessionId").and_then(Value::as_str) != Some(self.session.as_str()) {
            return None;
        }
        if method == "Runtime.executionContextsCleared" {
            return Some((
                "subscription.ended".into(),
                json!({"reason": "document_changed"}),
                true,
            ));
        }
        if method != "Runtime.bindingCalled"
            || event.pointer("/params/name")?.as_str()? != self.binding
        {
            return None;
        }
        let payload: Value =
            serde_json::from_str(event.pointer("/params/payload")?.as_str()?).ok()?;
        let name = match payload.get("state")?.as_str()? {
            "visible" => "element.visible",
            "hidden" => "element.hidden",
            _ => return None,
        };
        Some((name.into(), payload, false))
    }

    /// Runs the one watch loop shared by socket and registered subscriptions.
    /// Every transition goes to `sink`, and the loop stops when `cancel` resolves.
    async fn run<S, C>(
        &mut self,
        once: bool,
        timeout: Duration,
        sink: &mut S,
        cancel: C,
    ) -> Result<(), String>
    where
        S: Sink,
        C: Future<Output = Result<Cancel, String>>,
    {
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline, cancel);
        loop {
            tokio::select! {
                result = &mut cancel => {
                    if result? == Cancel::Requested {
                        sink.emit(self.message("subscription.ended", json!({"reason": "cancelled"}))).await?;
                    }
                    return Ok(());
                }
                _ = &mut deadline => {
                    sink.emit(self.message("subscription.ended", json!({"reason": "timeout"}))).await?;
                    return Ok(());
                }
                received = self.receiver.recv() => {
                    let event = match received {
                        Ok(event) => event,
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            sink.emit(self.message("subscription.ended", json!({"reason": "event_gap", "missed": count}))).await?;
                            return Ok(());
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            return Err("Browser event connection closed".into());
                        }
                    };
                    let Some((name, data, terminal)) = self.classify(&event) else {
                        continue;
                    };
                    sink.emit(self.message(&name, data)).await?;
                    if terminal {
                        return Ok(());
                    }
                    if once && name == "element.visible" {
                        sink.emit(self.message("subscription.ended", json!({"reason": "matched"}))).await?;
                        return Ok(());
                    }
                }
            }
        }
    }
}

struct SocketSink<'a, W>(&'a mut W);

impl<W: AsyncWrite + Unpin> Sink for SocketSink<'_, W> {
    async fn emit(&mut self, message: Value) -> Result<(), String> {
        send(self.0, &message).await
    }
}

async fn socket_cancel<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Cancel, String> {
    let mut input = Vec::new();
    if reader
        .read_until(b'\n', &mut input)
        .await
        .map_err(|e| e.to_string())?
        == 0
    {
        return Ok(Cancel::Silent);
    }
    let cancellation: Value = serde_json::from_slice(&input).unwrap_or(Value::Null);
    if cancellation.get("action").and_then(Value::as_str) != Some("events_unsubscribe") {
        return Err("Only events_unsubscribe is accepted on an event connection".into());
    }
    Ok(Cancel::Requested)
}

async fn send<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<(), String> {
    let line = format!("{value}\n");
    tokio::time::timeout(Duration::from_secs(2), async {
        writer.write_all(line.as_bytes()).await?;
        writer.flush().await
    })
    .await
    .map_err(|_| "Event consumer stopped reading".to_string())?
    .map_err(|e| e.to_string())
}

/// Stream prototype events on one socket without holding the daemon command lock.
pub async fn serve<R, W>(
    command: &Value,
    state: Arc<Mutex<DaemonState>>,
    reader: &mut R,
    writer: &mut W,
) where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut watch = None;
    let result: Result<(), String> = async {
        let (request, timeout) = Request::parse(command, 10_000, 60_000)?;
        let installed = watch.insert(Watch::create(&state).await?);
        let initial = installed.install(&request.selector).await?;
        let mut sink = SocketSink(&mut *writer);
        sink.emit(installed.message("subscription.ready", initial))
            .await?;
        installed
            .run(
                request.once,
                timeout,
                &mut sink,
                socket_cancel(&mut *reader),
            )
            .await
    }
    .await;
    if let Err(error) = result {
        let _ = send(
            writer,
            &json!({"name": "subscription.error", "error": error}),
        )
        .await;
    }
    if let Some(watch) = watch {
        let _ = tokio::time::timeout(Duration::from_secs(2), watch.cleanup()).await;
    }
}

struct Entry {
    id: String,
    selector: String,
    tab_id: String,
    started_at: String,
    fired_at: Option<String>,
    status: &'static str,
    reason: Option<String>,
    events: Vec<Value>,
    cancel: Arc<Notify>,
    task: Option<JoinHandle<()>>,
    done: watch::Sender<bool>,
}

impl Entry {
    fn finish(&mut self, status: &'static str, reason: Option<String>) {
        self.status = status;
        self.reason = reason;
        self.done.send_replace(true);
    }

    fn snapshot(&self) -> Value {
        let status = if self.status == ARMED {
            "waiting"
        } else {
            self.status
        };
        json!({
            "id": self.id,
            "status": status,
            "reason": self.reason,
            "events": self.events
        })
    }
}

/// Subscriptions held by the daemon, so a later command can collect what happened.
struct Registry {
    entries: StdMutex<Vec<Entry>>,
    stream: StdMutex<Option<Arc<StreamServer>>>,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);

impl Registry {
    fn new() -> Self {
        Self {
            entries: StdMutex::new(Vec::new()),
            stream: StdMutex::new(None),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_stream(&self, server: Option<Arc<StreamServer>>) {
        *self.stream.lock().unwrap_or_else(|e| e.into_inner()) = server;
    }

    fn new_id(&self) -> String {
        let entries = self.lock();
        loop {
            let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
            let id = format!("ev_{suffix}");
            if !entries.iter().any(|entry| entry.id == id) {
                return id;
            }
        }
    }

    fn register(&self, id: &str, selector: &str, tab_id: &str) -> Result<Arc<Notify>, String> {
        let mut entries = self.lock();
        if entries.iter().filter(|e| e.status == ARMED).count() >= MAX_ACTIVE {
            return Err(format!(
                "Too many active subscriptions (max {MAX_ACTIVE}); stop one with events stop <id>"
            ));
        }
        while entries.len() >= MAX_LISTED {
            let Some(oldest) = entries.iter().position(|e| e.status != ARMED) else {
                break;
            };
            entries.remove(oldest);
        }
        let cancel = Arc::new(Notify::new());
        entries.push(Entry {
            id: id.to_owned(),
            selector: selector.to_owned(),
            tab_id: tab_id.to_owned(),
            started_at: chrono::Utc::now().to_rfc3339(),
            fired_at: None,
            status: ARMED,
            reason: None,
            events: Vec::new(),
            cancel: cancel.clone(),
            task: None,
            done: watch::channel(false).0,
        });
        Ok(cancel)
    }

    fn discard(&self, id: &str) {
        self.lock().retain(|entry| entry.id != id);
    }

    fn attach(&self, id: &str, task: JoinHandle<()>) {
        match self.lock().iter_mut().find(|entry| entry.id == id) {
            Some(entry) => entry.task = Some(task),
            None => task.abort(),
        }
    }

    /// Stores one envelope, settles the status on `subscription.ended`, and
    /// forwards it to dashboard clients. Events after a terminal state are dropped.
    fn record(&self, id: &str, message: Value) {
        let name = message["name"].as_str().unwrap_or_default().to_owned();
        let payload = {
            let mut entries = self.lock();
            let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
                return;
            };
            if entry.status != ARMED {
                return;
            }
            if name != "subscription.ready" {
                if entry.events.len() >= MAX_EVENTS {
                    entry.events.remove(0);
                }
                entry.events.push(message.clone());
            }
            if name.starts_with("element.") && entry.fired_at.is_none() {
                entry.fired_at = message["timestamp"].as_str().map(str::to_owned);
            }
            if name == "subscription.ended" {
                let reason = message
                    .pointer("/data/observation/reason")
                    .and_then(Value::as_str)
                    .unwrap_or("ended")
                    .to_owned();
                let status = match reason.as_str() {
                    "matched" => "matched",
                    "timeout" => "timeout",
                    "cancelled" => "cancelled",
                    _ => "ended",
                };
                entry.finish(status, Some(reason));
            }
            json!({
                "subscriptionId": id,
                "selector": entry.selector,
                "name": name,
                "eventId": message["eventId"],
                "data": message["data"]
            })
        };
        let server = self
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(server) = server {
            server.broadcast_event(&payload);
        }
    }

    fn finish_if_armed(&self, id: &str, status: &'static str, reason: &str) {
        if let Some(entry) = self.lock().iter_mut().find(|entry| entry.id == id) {
            if entry.status == ARMED {
                entry.finish(status, Some(reason.to_owned()));
            }
        }
    }

    async fn wait(&self, id: &str, timeout: Duration) -> Result<Value, String> {
        let mut done = {
            let entries = self.lock();
            let entry = entries
                .iter()
                .find(|entry| entry.id == id)
                .ok_or_else(|| format!("Unknown subscription '{id}'"))?;
            entry.done.subscribe()
        };
        let _ = tokio::time::timeout(timeout, done.wait_for(|finished| *finished)).await;
        self.lock()
            .iter()
            .find(|entry| entry.id == id)
            .map(Entry::snapshot)
            .ok_or_else(|| format!("Unknown subscription '{id}'"))
    }

    fn list(&self) -> Value {
        let entries = self.lock();
        let subscriptions: Vec<Value> = entries
            .iter()
            .rev()
            .take(MAX_LISTED)
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "selector": entry.selector,
                    "tabId": entry.tab_id,
                    "status": entry.status,
                    "eventCount": entry.events.len(),
                    "startedAt": entry.started_at,
                    "firedAt": entry.fired_at
                })
            })
            .collect();
        json!({"subscriptions": subscriptions})
    }

    async fn stop(&self, id: &str) -> Result<Value, String> {
        let (cancel, task) = {
            let mut entries = self.lock();
            let entry = entries
                .iter_mut()
                .find(|entry| entry.id == id)
                .ok_or_else(|| format!("Unknown subscription '{id}'"))?;
            (entry.cancel.clone(), entry.task.take())
        };
        cancel.notify_one();
        if let Some(mut task) = task {
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
        self.finish_if_armed(id, "cancelled", "cancelled");
        Ok(json!({"id": id, "status": "cancelled"}))
    }

    async fn stop_all(&self) {
        let active: Vec<String> = self
            .lock()
            .iter()
            .filter(|entry| entry.status == ARMED)
            .map(|entry| entry.id.clone())
            .collect();
        for id in active {
            let _ = self.stop(&id).await;
        }
    }
}

struct RegistrySink {
    id: String,
}

impl Sink for RegistrySink {
    async fn emit(&mut self, message: Value) -> Result<(), String> {
        REGISTRY.record(&self.id, message);
        Ok(())
    }
}

async fn drive(mut watch: Watch, once: bool, timeout: Duration, cancel: Arc<Notify>) {
    let mut sink = RegistrySink {
        id: watch.id.clone(),
    };
    let cancelled = async {
        cancel.notified().await;
        Ok(Cancel::Requested)
    };
    if let Err(error) = watch.run(once, timeout, &mut sink, cancelled).await {
        let ended = watch.message(
            "subscription.ended",
            json!({"reason": "browser_closed", "error": error}),
        );
        REGISTRY.record(&watch.id, ended);
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), watch.cleanup()).await;
}

/// Starts a daemon-held subscription on the active page and returns once it is armed.
pub async fn handle_start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (request, timeout) = Request::parse(cmd, 60_000, 300_000)?;
    REGISTRY.set_stream(state.stream_server.clone());
    let id = REGISTRY.new_id();
    let watch = Watch::from_state(state, id.clone())?;
    let cancel = REGISTRY.register(&id, &request.selector, &watch.target)?;
    let initial = match watch.install(&request.selector).await {
        Ok(initial) => initial,
        Err(error) => {
            watch.cleanup().await;
            REGISTRY.discard(&id);
            return Err(error);
        }
    };
    let data = json!({
        "id": id,
        "selector": request.selector,
        "tabId": watch.target,
        "state": ARMED,
        "initial": initial["state"]
    });
    REGISTRY.record(&id, watch.message("subscription.ready", initial));
    let task = tokio::spawn(drive(watch, request.once, timeout, cancel));
    REGISTRY.attach(&id, task);
    Ok(data)
}

/// Lists subscriptions held by this daemon, newest first.
pub async fn handle_list(state: &mut DaemonState) -> Result<Value, String> {
    REGISTRY.set_stream(state.stream_server.clone());
    Ok(REGISTRY.list())
}

/// Stops a subscription and removes its page observer. Safe to repeat.
pub async fn handle_stop(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    REGISTRY.set_stream(state.stream_server.clone());
    let id = subscription_id(cmd).ok_or("Missing subscription id")?;
    REGISTRY.stop(id).await
}

/// Reads the subscription id from `subscriptionId`, falling back to `id` for raw clients.
fn subscription_id(cmd: &Value) -> Option<&str> {
    cmd.get("subscriptionId")
        .or_else(|| cmd.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

/// Stops every active subscription when the browser or daemon goes away.
pub async fn stop_all() {
    REGISTRY.stop_all().await;
}

/// Points dashboard broadcasts at the current stream server, or at none.
pub fn set_stream(server: Option<Arc<StreamServer>>) {
    REGISTRY.set_stream(server);
}

/// Waits for a subscription result without holding the daemon command lock while blocked.
/// Policy is checked first under a brief lock, because this action bypasses `execute_command`.
pub async fn wait(cmd: &Value, state: &Mutex<DaemonState>) -> Value {
    let request_id = cmd
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let denied = |reason: &str| {
        error_response(
            &request_id,
            &format!("Action 'events_wait' denied by policy: {reason}"),
        )
    };
    {
        let mut state = state.lock().await;
        if let Some(policy) = state.policy.as_mut() {
            let _ = policy.reload();
            match policy.check("events_wait") {
                PolicyResult::Allow => {}
                PolicyResult::Deny(reason) => return denied(&reason),
                PolicyResult::RequiresConfirmation => {
                    return denied("confirmation is not supported for events_wait")
                }
            }
        }
        if state
            .confirm_actions
            .as_ref()
            .is_some_and(|actions| actions.requires_confirmation("events_wait"))
        {
            return denied("confirmation is not supported for events_wait");
        }
    }
    let Some(id) = subscription_id(cmd) else {
        return error_response(&request_id, "Missing subscription id");
    };
    let timeout_ms = match cmd.get("timeout_ms") {
        None | Some(Value::Null) => 30_000,
        Some(value) => match value.as_u64().filter(|ms| (1..=300_000).contains(ms)) {
            Some(ms) => ms,
            None => {
                return error_response(
                    &request_id,
                    "timeout_ms must be an integer between 1 and 300000",
                )
            }
        },
    };
    match REGISTRY.wait(id, Duration::from_millis(timeout_ms)).await {
        Ok(data) => success_response(&request_id, data),
        Err(error) => error_response(&request_id, &error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visible(id: &str) -> Value {
        envelope(
            id,
            "TARGET",
            "element.visible",
            json!({"selector": "#a", "state": "visible", "label": "Hello"}),
        )
    }

    fn ended(id: &str, reason: &str) -> Value {
        envelope(
            id,
            "TARGET",
            "subscription.ended",
            json!({"reason": reason}),
        )
    }

    fn armed(registry: &Registry, id: &str) {
        registry.register(id, "#a", "TARGET").unwrap();
    }

    #[test]
    fn request_validates_selector_and_timeout() {
        let parse = |value: Value| Request::parse(&value, 60_000, 300_000);
        assert!(parse(json!({"selector": "#a"})).is_ok());
        assert!(parse(json!({"selector": ""})).is_err());
        assert!(parse(json!({"selector": "#a", "timeout_ms": 0})).is_err());
        assert!(parse(json!({"selector": "#a", "timeout_ms": 300_001})).is_err());
        assert!(parse(json!({"selector": "#a", "timeout_ms": 300_000})).is_ok());
        assert!(parse(json!({})).is_err());
        assert!(Request::parse(
            &json!({"selector": "#a", "timeout_ms": 60_001}),
            10_000,
            60_000
        )
        .is_err());
    }

    #[tokio::test]
    async fn matched_event_is_stored_and_wait_returns_immediately() {
        let registry = Registry::new();
        armed(&registry, "ev_1");
        registry.record(
            "ev_1",
            envelope(
                "ev_1",
                "TARGET",
                "subscription.ready",
                json!({"state": "hidden"}),
            ),
        );
        registry.record("ev_1", visible("ev_1"));
        registry.record("ev_1", ended("ev_1", "matched"));
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            registry.wait("ev_1", Duration::from_secs(30)),
        )
        .await
        .expect("wait should not block")
        .unwrap();
        assert_eq!(result["status"], "matched");
        assert_eq!(result["reason"], "matched");
        let names: Vec<&str> = result["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["element.visible", "subscription.ended"]);
        let listed = registry.list();
        assert_eq!(listed["subscriptions"][0]["status"], "matched");
        assert_eq!(listed["subscriptions"][0]["eventCount"], 2);
        assert!(listed["subscriptions"][0]["firedAt"].is_string());
    }

    #[tokio::test]
    async fn wait_times_out_with_waiting_and_unblocks_on_event() {
        let registry = Arc::new(Registry::new());
        armed(&registry, "ev_1");
        let result = registry
            .wait("ev_1", Duration::from_millis(20))
            .await
            .unwrap();
        assert_eq!(result["status"], "waiting");
        assert_eq!(result["events"], json!([]));

        let waiter = {
            let registry = registry.clone();
            tokio::spawn(async move { registry.wait("ev_1", Duration::from_secs(30)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        registry.record("ev_1", visible("ev_1"));
        registry.record("ev_1", ended("ev_1", "matched"));
        let result = waiter.await.unwrap().unwrap();
        assert_eq!(result["status"], "matched");
        assert!(registry
            .wait("missing", Duration::from_millis(1))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn end_reasons_map_to_statuses() {
        let registry = Registry::new();
        for (reason, status) in [
            ("timeout", "timeout"),
            ("document_changed", "ended"),
            ("tab_closed", "ended"),
            ("event_gap", "ended"),
        ] {
            let id = format!("ev_{reason}");
            armed(&registry, &id);
            registry.record(&id, ended(&id, reason));
            let result = registry.wait(&id, Duration::from_millis(1)).await.unwrap();
            assert_eq!(result["status"], status);
            assert_eq!(result["reason"], reason);
        }
    }

    #[tokio::test]
    async fn stop_cancels_and_is_idempotent() {
        let registry = Arc::new(Registry::new());
        let cancel = registry.register("ev_1", "#a", "TARGET").unwrap();
        let task = {
            let registry = registry.clone();
            tokio::spawn(async move {
                cancel.notified().await;
                registry.record("ev_1", ended("ev_1", "cancelled"));
            })
        };
        registry.attach("ev_1", task);
        let first = registry.stop("ev_1").await.unwrap();
        assert_eq!(first, json!({"id": "ev_1", "status": "cancelled"}));
        assert_eq!(registry.stop("ev_1").await.unwrap(), first);
        let result = registry
            .wait("ev_1", Duration::from_millis(1))
            .await
            .unwrap();
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["events"][0]["name"], "subscription.ended");
        assert!(registry.stop("missing").await.is_err());

        armed(&registry, "ev_2");
        registry.stop("ev_2").await.unwrap();
        assert_eq!(
            registry
                .wait("ev_2", Duration::from_millis(1))
                .await
                .unwrap()["status"],
            "cancelled"
        );
    }

    #[tokio::test]
    async fn active_subscriptions_are_capped() {
        let registry = Registry::new();
        for index in 0..MAX_ACTIVE {
            armed(&registry, &format!("ev_{index}"));
        }
        assert!(registry.register("ev_over", "#a", "TARGET").is_err());
        registry.stop("ev_0").await.unwrap();
        assert!(registry.register("ev_over", "#a", "TARGET").is_ok());
    }

    #[tokio::test]
    async fn list_is_newest_first_and_capped() {
        let registry = Registry::new();
        for index in 0..(MAX_LISTED + 10) {
            let id = format!("ev_{index}");
            armed(&registry, &id);
            registry.stop(&id).await.unwrap();
        }
        let listed = registry.list();
        let subscriptions = listed["subscriptions"].as_array().unwrap();
        assert_eq!(subscriptions.len(), MAX_LISTED);
        assert_eq!(subscriptions[0]["id"], format!("ev_{}", MAX_LISTED + 9));
        assert_eq!(subscriptions[MAX_LISTED - 1]["id"], "ev_10");
        assert!(registry
            .wait("ev_0", Duration::from_millis(1))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn events_after_the_terminal_state_are_dropped() {
        let registry = Registry::new();
        armed(&registry, "ev_1");
        registry.record("ev_1", ended("ev_1", "timeout"));
        registry.record("ev_1", visible("ev_1"));
        let result = registry
            .wait("ev_1", Duration::from_millis(1))
            .await
            .unwrap();
        assert_eq!(result["events"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn wait_is_denied_by_policy_without_touching_the_registry() {
        let mut daemon = DaemonState::new();
        daemon.policy = Some(serde_json::from_str(r#"{"deny": ["events_wait"]}"#).unwrap());
        let state = Mutex::new(daemon);
        let response = wait(
            &json!({"id": "r1", "action": "events_wait", "subscriptionId": "ev_x"}),
            &state,
        )
        .await;
        assert_eq!(response["success"], false);
        assert_eq!(
            response["error"],
            "Action 'events_wait' denied by policy: Action 'events_wait' is denied by policy"
        );
        state.lock().await.policy = None;
        let response = wait(
            &json!({"id": "r2", "action": "events_wait", "subscriptionId": "ev_missing"}),
            &state,
        )
        .await;
        assert_eq!(response["success"], false);
        assert!(response["error"]
            .as_str()
            .unwrap()
            .contains("Unknown subscription"));
    }
}
