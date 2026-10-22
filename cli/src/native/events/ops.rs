use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::bus::{Bus, DomStart, SourceStart};
use super::envelope::EventTab;
use super::filter::Matchers;
use super::source::{EventLocation, EventSource, SourceSignal};
use super::sources::cdp::{self, Entry};
use crate::native::actions::DaemonState;
use crate::native::cdp::client::{CdpClient, RawCdpMessage};

const DEFAULT_TTL_SECS: u64 = 600;
const MAX_TTL_SECS: u64 = 1_800;
const MAX_WAIT_MS: u64 = 50_000;
const DEFAULT_READ_LIMIT: usize = 100;
const MAX_READ_LIMIT: usize = 200;
const MAX_EVENT_BYTES: usize = 16 * 1024;

struct ReadRequest {
    subscription_id: String,
    since: Option<String>,
    wait_ms: u64,
    limit: usize,
    ack: bool,
}

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
        match cmd
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("visible")
        {
            "visible" | "hidden" => {}
            _ => return Err("Event state must be visible or hidden".into()),
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
    let ttl_secs = ttl_seconds(cmd)?;
    let browser = state.browser.as_ref().ok_or("Open a page first")?;
    let session_id = browser.active_session_id()?.to_owned();
    let target_id = browser.active_target_id()?.to_owned();
    let page = browser
        .pages_list()
        .into_iter()
        .find(|page| page.target_id == target_id)
        .ok_or("Open a page first")?;
    let tab: EventTab = browser
        .event_tab_for_target(&target_id)
        .ok_or("Open a page first")?;
    let client = browser.client.clone();
    let pages = browser.pages_list();
    let download_path = browser.download_path.clone();
    let owner = optional_string(cmd, "owner")?;
    let idempotency_key = optional_string(cmd, "key")?;
    let once = cmd.get("once").and_then(Value::as_bool).unwrap_or(false);
    let ttl = Duration::from_secs(ttl_secs);
    if let Some(selector) = cmd.get("selector").and_then(Value::as_str) {
        let state_name = cmd
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("visible");
        state
            .events
            .start_dom(DomStart {
                engine: &state.engine,
                client,
                session_id: &session_id,
                tab,
                current_url: &page.url,
                selector,
                state: state_name,
                once,
                ttl,
                owner,
                idempotency_key,
            })
            .await
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
            let params = match download_path {
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
                        tab: EventTab {
                            id: crate::native::browser::format_tab_id(page.tab_id),
                            label: page.label.unwrap_or_default(),
                            target_id: page.target_id,
                        },
                        url: page.url,
                    },
                )
            })
            .collect();
        let source = Arc::new(CdpMethodSource::new(
            entry,
            session_id.clone(),
            target_id,
            targets,
            matchers,
        ));
        let filter_key = method.to_owned();
        state
            .events
            .subscribe_source(SourceStart {
                engine: &state.engine,
                client,
                source,
                initial: SourceSignal::Rearmed,
                tab,
                current_url: &page.url,
                filter_key,
                rule: json!({
                    "method": method,
                    "match": equals,
                    "matchRegex": regexes
                }),
                once,
                ttl,
                owner,
                idempotency_key,
            })
            .await
    }
}

pub(crate) async fn read(cmd: &Value, bus: &Arc<Bus>) -> Result<Value, String> {
    let request = read_request(cmd)?;
    let result = bus
        .read_with_ack(
            &request.subscription_id,
            request.since.as_deref(),
            Duration::from_millis(request.wait_ms),
            request.limit,
            request.ack,
        )
        .await?;
    serde_json::to_value(result).map_err(|error| error.to_string())
}

pub(crate) async fn ack(cmd: &Value, bus: &Arc<Bus>) -> Result<Value, String> {
    let id = subscription_id(cmd)?;
    let cursor = cmd
        .get("cursor")
        .and_then(Value::as_str)
        .ok_or("Missing event cursor")?;
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

fn read_request(cmd: &Value) -> Result<ReadRequest, String> {
    let subscription_id = subscription_id(cmd)?.to_owned();
    let wait_ms = optional_u64(cmd, "waitMs")?.unwrap_or(0).min(MAX_WAIT_MS);
    let limit = optional_u64(cmd, "limit")?
        .map(|value| value.min(MAX_READ_LIMIT as u64) as usize)
        .unwrap_or(DEFAULT_READ_LIMIT);
    if limit == 0 {
        return Err("Event read limit must be at least 1".into());
    }
    let ack = match cmd.get("ack") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("ack must be a boolean".into()),
    };
    Ok(ReadRequest {
        subscription_id,
        since: optional_string(cmd, "since")?,
        wait_ms,
        limit: limit.min(MAX_READ_LIMIT),
        ack,
    })
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
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{name} values must be strings"))
            })
            .collect(),
        Some(_) => Err(format!("{name} must be an array of strings")),
    }
}

struct CdpMethodSource {
    entry: &'static Entry,
    session_id: String,
    target_id: String,
    targets: RwLock<HashMap<String, CdpTarget>>,
    matchers: Matchers,
    policy_actions: [&'static str; 1],
}

struct CdpTarget {
    session_id: String,
    tab: EventTab,
    url: String,
}

impl CdpMethodSource {
    fn new(
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
            policy_actions: [entry.policy_action],
        }
    }
}

#[async_trait]
impl EventSource for CdpMethodSource {
    fn supported_engines(&self) -> &'static [&'static str] {
        self.entry.engines
    }

    fn policy_actions(&self) -> &[&'static str] {
        &self.policy_actions
    }

    fn target_id(&self) -> &str {
        &self.target_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn sharing_key(&self) -> Option<String> {
        None
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
        if !browser_event && !target_event && mapped.is_none() {
            return None;
        }
        if browser_event && mapped.is_none() && session_id.is_some() {
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
                Some(EventLocation {
                    tab: None,
                    url: event_url?.to_owned(),
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

    async fn observe(&self, _client: &CdpClient) -> Result<Option<SourceSignal>, String> {
        Ok(None)
    }

    async fn stop(&self, _client: &CdpClient) {}
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use serde_json::json;

    use super::{read_request, CdpMethodSource, CdpTarget};
    use crate::native::cdp::client::RawCdpMessage;
    use crate::native::events::filter::Matchers;
    use crate::native::events::source::{EventSource, SourceSignal};
    use crate::native::events::sources::cdp;

    #[test]
    fn read_request_clamps_wait_and_defaults_to_no_ack() {
        let request = read_request(&json!({
            "subscriptionId": "sub-1",
            "waitMs": 90_000
        }))
        .unwrap();
        assert_eq!(request.wait_ms, 50_000);
        assert!(!request.ack);
    }

    #[test]
    fn target_events_are_classified_without_a_session_id() {
        let source = CdpMethodSource::new(
            cdp::lookup("Target.targetCreated").unwrap(),
            "page-session".into(),
            "page-target".into(),
            HashMap::new(),
            Matchers::parse(&[], &[]).unwrap(),
        );
        let raw = RawCdpMessage {
            text: r#"{"method":"Target.targetCreated","params":{"targetInfo":{"targetId":"new-target","type":"page","url":"https://example.com"}}}"#.into(),
            session_id: None,
        };

        let signal = source.classify(&raw).unwrap();

        assert!(
            matches!(signal, SourceSignal::Match { ref key, .. } if key == "Target.targetCreated")
        );
    }

    #[test]
    fn method_matchers_filter_projected_fields() {
        let source = CdpMethodSource::new(
            cdp::lookup("Page.javascriptDialogOpening").unwrap(),
            "page-session".into(),
            "page-target".into(),
            HashMap::from([(
                "page-target".into(),
                CdpTarget {
                    session_id: "page-session".into(),
                    tab: crate::native::events::envelope::EventTab {
                        id: "t1".into(),
                        label: String::new(),
                        target_id: "page-target".into(),
                    },
                    url: "https://example.com".into(),
                },
            )]),
            Matchers::parse(&["/type=alert".into()], &[]).unwrap(),
        );
        let classify = |dialog_type: &str| {
            source.classify(&RawCdpMessage {
                text: json!({
                    "method": "Page.javascriptDialogOpening",
                    "params": {"type": dialog_type, "message": "fixture"}
                })
                .to_string(),
                session_id: Some("page-session".into()),
            })
        };

        assert!(classify("alert").is_some());
        assert!(classify("confirm").is_none());
    }

    #[test]
    fn page_events_use_the_background_tab_mapping() {
        let source = CdpMethodSource::new(
            cdp::lookup("Runtime.consoleAPICalled").unwrap(),
            "active-session".into(),
            "active-target".into(),
            HashMap::from([
                (
                    "active-target".into(),
                    CdpTarget {
                        session_id: "active-session".into(),
                        tab: crate::native::events::envelope::EventTab {
                            id: "t1".into(),
                            label: String::new(),
                            target_id: "active-target".into(),
                        },
                        url: "https://active.example".into(),
                    },
                ),
                (
                    "background-target".into(),
                    CdpTarget {
                        session_id: "background-session".into(),
                        tab: crate::native::events::envelope::EventTab {
                            id: "t2".into(),
                            label: "background".into(),
                            target_id: "background-target".into(),
                        },
                        url: "https://background.example".into(),
                    },
                ),
            ]),
            Matchers::parse(&[], &[]).unwrap(),
        );
        let raw = RawCdpMessage {
            text: json!({
                "method": "Runtime.consoleAPICalled",
                "params": {
                    "type": "log",
                    "args": [{"type": "string", "value": "background"}]
                }
            })
            .to_string(),
            session_id: Some("background-session".into()),
        };

        let Some(SourceSignal::Match {
            location: Some(location),
            data,
            ..
        }) = source.classify(&raw)
        else {
            panic!("background event should be classified");
        };

        assert_eq!(location.tab.as_ref().unwrap().id, "t2");
        assert_eq!(location.url, "https://background.example");
        assert_eq!(data["text"], "background");
    }

    #[test]
    fn start_validation_names_refused_cdp_domains() {
        let error = super::validate_start_request(&json!({
            "method": "Network.requestWillBeSent"
        }))
        .unwrap_err();

        assert!(error.contains("Network"));
    }

    #[test]
    fn source_policy_action_matches_the_projected_data() {
        assert_eq!(
            super::source_policy_action(&json!({
                "method": "Page.javascriptDialogOpening"
            })),
            Some("dialog")
        );
    }
}
