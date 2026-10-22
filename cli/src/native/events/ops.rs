use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use super::bus::{Bus, DomStart, SourceStart};
use super::envelope::EventTab;
use super::filter::Matchers;
use super::source::{EventLocation, EventSource, SourceSignal};
use super::sources::cdp::{self, Entry};
use crate::native::actions::DaemonState;
use crate::native::cdp::client::RawCdpMessage;

const DEFAULT_TTL_SECS: u64 = 600;
const MAX_TTL_SECS: u64 = 1_800;
const MAX_WAIT_MS: u64 = 50_000;
const DEFAULT_READ_LIMIT: u64 = 100;
const MAX_READ_LIMIT: u64 = 200;
const MAX_EVENT_BYTES: usize = 16 * 1024;

pub(crate) fn source_policy_action(cmd: &Value) -> Option<&'static str> {
    let method = cmd.get("method")?.as_str()?;
    cdp::lookup(method).ok().map(|entry| entry.policy_action)
}

pub(crate) fn validate_start_request(cmd: &Value) -> Result<(), String> {
    let has_selector = cmd.get("selector").and_then(Value::as_str).is_some();
    let has_method = cmd.get("method").and_then(Value::as_str).is_some();
    if has_selector == has_method {
        return Err("Specify exactly one of selector or method".into());
    }
    ttl_seconds(cmd)?;
    optional_string(cmd, "owner")?;
    optional_string(cmd, "key")?;
    if let Some(selector) = cmd.get("selector").and_then(Value::as_str) {
        super::sources::dom::normalize_selector(selector)?;
        if !matches!(
            cmd["state"].as_str().unwrap_or("visible"),
            "visible" | "hidden"
        ) {
            return Err("Event state must be visible or hidden".into());
        }
    } else {
        let method = cmd["method"]
            .as_str()
            .ok_or("Event method must be a string")?;
        cdp::lookup(method)?;
        let equals = string_array(cmd, "match")?;
        let regexes = string_array(cmd, "matchRegex")?;
        Matchers::parse(&equals, &regexes)?;
    }
    if cmd.get("once").is_some_and(|value| !value.is_boolean()) {
        return Err("once must be a boolean".into());
    }
    Ok(())
}

pub(crate) async fn start(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    validate_start_request(cmd)?;
    let ttl = Duration::from_secs(ttl_seconds(cmd)?);
    let browser = state.browser.as_ref().ok_or("Open a page first")?;
    let session_id = browser.active_session_id()?.to_owned();
    let target_id = browser.active_target_id()?.to_owned();
    let pages = browser.pages_list();
    let current_url = pages
        .iter()
        .find(|page| page.target_id == target_id)
        .ok_or("Open a page first")?
        .url
        .clone();
    let tab = browser
        .event_tab_for_target(&target_id)
        .ok_or("Open a page first")?;
    let client = browser.client.clone();
    let owner = optional_string(cmd, "owner")?;
    let idempotency_key = optional_string(cmd, "key")?;
    let once = cmd.get("once").and_then(Value::as_bool).unwrap_or(false);
    let response = if let Some(selector) = cmd.get("selector").and_then(Value::as_str) {
        state
            .events
            .start_dom(DomStart {
                engine: &state.engine,
                client,
                session_id: &session_id,
                tab,
                current_url: &current_url,
                selector,
                state: cmd["state"].as_str().unwrap_or("visible"),
                once,
                ttl,
                owner,
                idempotency_key,
            })
            .await?
    } else {
        let method = cmd["method"].as_str().unwrap();
        let entry = cdp::lookup(method)?;
        if !entry.engines.contains(&state.engine.as_str()) {
            return Err(format!(
                "Event source does not support the selected browser engine '{}'",
                state.engine
            ));
        }
        let equals = string_array(cmd, "match")?;
        let regexes = string_array(cmd, "matchRegex")?;
        let matchers = Matchers::parse(&equals, &regexes)?;
        if entry.policy_action == "download" {
            let params = match &browser.download_path {
                Some(download_path) => json!({
                    "behavior": "allow",
                    "downloadPath": download_path,
                    "eventsEnabled": true
                }),
                None => json!({ "behavior": "default", "eventsEnabled": true }),
            };
            client
                .send_command("Browser.setDownloadBehavior", Some(params), None)
                .await?;
        }
        let targets = pages
            .into_iter()
            .map(|page| {
                (
                    page.target_id.clone(),
                    CdpTarget {
                        session_id: page.session_id,
                        tab: browser.event_tab_for_target(&page.target_id).unwrap(),
                        url: page.url,
                    },
                )
            })
            .collect();
        let source = Arc::new(CdpMethodSource::new(
            entry, session_id, target_id, targets, matchers,
        ));
        state
            .events
            .subscribe_cdp(
                client,
                SourceStart {
                    source,
                    initial: "unknown".into(),
                    tab,
                    current_url: &current_url,
                    filter_key: method.to_owned(),
                    rule: json!({
                        "method": method,
                        "match": equals,
                        "matchRegex": regexes
                    }),
                    once,
                    ttl,
                    owner,
                    idempotency_key,
                },
            )
            .await?
    };
    if let Some(id) = response.get("id").and_then(Value::as_str) {
        state
            .events
            .approve_subscription(id, &state.confirmed_policy_actions)
            .await;
    }
    Ok(response)
}

pub(crate) async fn read(cmd: &Value, bus: &Arc<Bus>) -> Result<Value, String> {
    let id = subscription_id(cmd)?;
    let (wait_ms, limit, ack) = read_options(cmd)?;
    let since = optional_string(cmd, "since")?;
    let result = bus
        .read_with_ack(
            id,
            since.as_deref(),
            Duration::from_millis(wait_ms),
            limit,
            ack,
        )
        .await?;
    serde_json::to_value(result).map_err(|error| error.to_string())
}

pub(crate) async fn ack(cmd: &Value, bus: &Arc<Bus>) -> Result<Value, String> {
    let id = subscription_id(cmd)?;
    let cursor = cmd["cursor"].as_str().ok_or("Missing event cursor")?;
    let owner = optional_string(cmd, "owner")?;
    bus.ack(id, cursor, owner.as_deref()).await?;
    Ok(json!({"id": id, "cursor": cursor}))
}

pub(crate) async fn list(cmd: &Value, bus: &Arc<Bus>) -> Result<Value, String> {
    let owner = optional_string(cmd, "owner")?;
    let unread = cmd.get("unread").and_then(Value::as_bool).unwrap_or(false);
    Ok(json!({
        "subscriptions": bus.list(owner.as_deref(), unread).await
    }))
}

pub(crate) async fn stop(cmd: &Value, bus: &Arc<Bus>) -> Result<Value, String> {
    let id = subscription_id(cmd)?;
    let owner = optional_string(cmd, "owner")?;
    let stopped = bus.stop(id, owner.as_deref()).await?;
    Ok(json!({"id": id, "stopped": stopped}))
}

fn read_options(cmd: &Value) -> Result<(u64, usize, bool), String> {
    let wait_ms = optional_u64(cmd, "waitMs")?.unwrap_or(0).min(MAX_WAIT_MS);
    let limit = optional_u64(cmd, "limit")?
        .unwrap_or(DEFAULT_READ_LIMIT)
        .min(MAX_READ_LIMIT) as usize;
    if limit == 0 {
        return Err("Event read limit must be at least 1".into());
    }
    let ack = match cmd.get("ack") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("ack must be a boolean".into()),
    };
    Ok((wait_ms, limit, ack))
}

fn ttl_seconds(cmd: &Value) -> Result<u64, String> {
    let ttl = optional_u64(cmd, "ttl")?.unwrap_or(DEFAULT_TTL_SECS);
    if ttl == 0 {
        return Err("Event subscription TTL must be at least 1 second".into());
    }
    Ok(ttl.min(MAX_TTL_SECS))
}

fn optional_u64(cmd: &Value, name: &str) -> Result<Option<u64>, String> {
    match cmd.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{name} must be a nonnegative integer")),
        Some(_) => Err(format!("{name} must be an integer")),
    }
}

fn subscription_id(cmd: &Value) -> Result<&str, String> {
    cmd.get("subscriptionId")
        .or_else(|| cmd.get("subscription_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "Missing subscription id".into())
}

fn optional_string(cmd: &Value, name: &str) -> Result<Option<String>, String> {
    match cmd.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("{name} must be a string")),
    }
}

fn string_array(cmd: &Value, name: &str) -> Result<Vec<String>, String> {
    match cmd.get(name) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value @ Value::Array(_)) => {
            Vec::deserialize(value).map_err(|_| format!("{name} values must be strings"))
        }
        Some(_) => Err(format!("{name} must be an array of strings")),
    }
}

pub(super) struct CdpMethodSource {
    entry: &'static Entry,
    session_id: String,
    target_id: String,
    targets: RwLock<HashMap<String, CdpTarget>>,
    matchers: Matchers,
}

pub(super) struct CdpTarget {
    session_id: String,
    tab: EventTab,
    url: String,
}

impl CdpMethodSource {
    pub(super) fn new(
        entry: &'static Entry,
        session_id: String,
        target_id: String,
        targets: HashMap<String, CdpTarget>,
        matchers: Matchers,
    ) -> Self {
        Self {
            entry,
            session_id,
            target_id,
            targets: RwLock::new(targets),
            matchers,
        }
    }
}

#[async_trait]
impl EventSource for CdpMethodSource {
    fn policy_actions(&self) -> Vec<String> {
        vec![self.entry.policy_action.to_owned()]
    }

    fn target_id(&self) -> &str {
        &self.target_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn classify(&self, raw: &RawCdpMessage) -> Option<SourceSignal> {
        let event: Value = serde_json::from_str(&raw.text).ok()?;
        if event.get("method").and_then(Value::as_str) != Some(self.entry.method) {
            return None;
        }
        let params = event.get("params")?;
        let browser_event = self.entry.method.starts_with("Browser.");
        let target_event = self.entry.method.starts_with("Target.");
        let session_id = raw
            .session_id
            .as_deref()
            .or_else(|| event.get("sessionId").and_then(Value::as_str));
        let target_id = params
            .pointer("/targetInfo/targetId")
            .or_else(|| params.get("targetId"))
            .and_then(Value::as_str);
        let targets = self.targets.read().ok()?;
        let mapped = if target_event {
            target_id.and_then(|target_id| targets.get(target_id))
        } else {
            session_id.and_then(|session_id| {
                targets
                    .values()
                    .find(|target| target.session_id == session_id)
            })
        };
        if mapped.is_none() && !target_event && (!browser_event || session_id.is_some()) {
            return None;
        }
        let event_url = params
            .pointer("/targetInfo/url")
            .or_else(|| browser_event.then(|| params.get("url")).flatten())
            .and_then(Value::as_str);
        let location = mapped
            .map(|target| EventLocation {
                tab: Some(target.tab.clone()),
                url: event_url.unwrap_or(&target.url).to_owned(),
            })
            .or_else(|| {
                event_url.map(|url| EventLocation {
                    tab: None,
                    url: url.to_owned(),
                })
            });
        let data = cdp::project(self.entry, params, MAX_EVENT_BYTES);
        self.matchers.matches(&data).then_some(SourceSignal::Match {
            key: self.entry.method.to_owned(),
            name: self.entry.method.to_owned(),
            data,
            initial: false,
            location,
        })
    }

    fn update_url(&self, session_id: &str, url: &str) {
        if let Ok(mut targets) = self.targets.write() {
            if let Some(target) = targets
                .values_mut()
                .find(|target| target.session_id == session_id)
            {
                target.url = url.to_owned();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_clamps_and_validates_ack() {
        assert_eq!(read_options(&json!({})).unwrap(), (0, 100, false));
        assert_eq!(
            read_options(&json!({"waitMs": 90_000, "limit": u64::MAX, "ack": true})).unwrap(),
            (50_000, 200, true)
        );
        for ack in [Value::Null, json!(0), json!("true")] {
            assert_eq!(
                read_options(&json!({"ack": ack})).unwrap_err(),
                "ack must be a boolean"
            );
        }
        assert_eq!(
            read_options(&json!({"limit": 0})).unwrap_err(),
            "Event read limit must be at least 1"
        );
    }

    #[test]
    fn projected_matchers_preserve_background_location_and_reject_other_sessions() {
        let source = CdpMethodSource::new(
            cdp::lookup("Page.javascriptDialogOpening").unwrap(),
            "active-session".into(),
            "active-target".into(),
            HashMap::from([(
                "background-target".into(),
                CdpTarget {
                    session_id: "background-session".into(),
                    tab: EventTab {
                        id: "t2".into(),
                        label: "background".into(),
                        target_id: "background-target".into(),
                    },
                    url: "https://background.example".into(),
                },
            )]),
            Matchers::parse(&["/type=alert".into()], &[]).unwrap(),
        );
        let classify = |session_id: &str, dialog_type: &str| {
            source.classify(&RawCdpMessage {
                text: json!({
                    "method": "Page.javascriptDialogOpening",
                    "params": {"type": dialog_type, "message": "fixture"}
                })
                .to_string(),
                session_id: Some(session_id.into()),
            })
        };

        assert!(classify("background-session", "confirm").is_none());
        assert!(classify("other-session", "alert").is_none());
        let Some(SourceSignal::Match {
            location: Some(location),
            data,
            ..
        }) = classify("background-session", "alert")
        else {
            panic!("background event should be classified");
        };

        assert_eq!(location.tab.as_ref().unwrap().id, "t2");
        assert_eq!(location.tab.as_ref().unwrap().label, "background");
        assert_eq!(location.url, "https://background.example");
        assert_eq!(data["message"], "fixture");
    }

    #[test]
    fn start_validation_names_refused_cdp_domains() {
        let error = validate_start_request(&json!({
            "method": "Network.requestWillBeSent"
        }))
        .unwrap_err();

        assert!(error.contains("Network"));
    }
}
