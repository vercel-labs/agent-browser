use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use super::bus::{Bus, DomStart, ReadRequest, SourceStart};
use super::filter::Matchers;
use super::sources::cdp::{self, CdpMethodSource, CdpTarget, Entry};
use super::sources::dom::{normalize_selector, DOM_POLICY_ACTIONS};
use crate::native::actions::DaemonState;

const DEFAULT_TTL_SECS: u64 = 600;
const MAX_TTL_SECS: u64 = 1_800;
const MAX_WAIT_MS: u64 = 50_000;
const DEFAULT_READ_LIMIT: u64 = 100;
const MAX_READ_LIMIT: u64 = 200;

#[derive(Debug)]
pub(crate) struct StartRequest {
    rule: StartRule,
    once: bool,
    ttl: Duration,
    owner: Option<String>,
    key: Option<String>,
}

#[derive(Debug)]
enum StartRule {
    Dom {
        selector: String,
        state: String,
    },
    Cdp {
        entry: &'static Entry,
        equals: Vec<String>,
        regexes: Vec<String>,
        matchers: Matchers,
    },
}

pub(crate) fn source_policy_actions(cmd: &Value) -> Vec<&'static str> {
    if cmd.get("selector").and_then(Value::as_str).is_some() {
        DOM_POLICY_ACTIONS.to_vec()
    } else {
        cmd.get("method")
            .and_then(Value::as_str)
            .and_then(|method| cdp::lookup(method).ok())
            .map(|entry| vec![entry.policy_action])
            .unwrap_or_default()
    }
}

pub(crate) fn validate_start_request(cmd: &Value) -> Result<StartRequest, String> {
    let selector = cmd.get("selector").and_then(Value::as_str);
    let method = cmd.get("method").and_then(Value::as_str);
    if selector.is_some() == method.is_some() {
        return Err("Specify exactly one of selector or method".into());
    }
    let ttl = Duration::from_secs(ttl_seconds(cmd)?);
    let owner = optional_string(cmd, "owner")?;
    let key = optional_string(cmd, "key")?;
    let rule = if let Some(selector) = selector {
        let selector = normalize_selector(selector)?;
        let state = cmd["state"].as_str().unwrap_or("visible");
        if !matches!(state, "visible" | "hidden") {
            return Err("Event state must be visible or hidden".into());
        }
        StartRule::Dom {
            selector,
            state: state.to_owned(),
        }
    } else {
        let entry = cdp::lookup(method.unwrap())?;
        let equals = string_array(cmd, "match")?;
        let regexes = string_array(cmd, "matchRegex")?;
        let matchers = Matchers::parse(&equals, &regexes)?;
        StartRule::Cdp {
            entry,
            equals,
            regexes,
            matchers,
        }
    };
    let once = match cmd.get("once") {
        None => false,
        Some(Value::Bool(once)) => *once,
        Some(_) => return Err("once must be a boolean".into()),
    };
    Ok(StartRequest {
        rule,
        once,
        ttl,
        owner,
        key,
    })
}

pub(crate) async fn start(req: StartRequest, state: &mut DaemonState) -> Result<Value, String> {
    if state.engine != "chrome" {
        return Err("Browser events are supported only with the Chrome engine".into());
    }
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
    let approved_actions = state.confirmed_policy_actions.iter().cloned().collect();
    match req.rule {
        StartRule::Dom {
            selector,
            state: dom_state,
        } => {
            state
                .events
                .start_dom(DomStart {
                    client,
                    session_id: &session_id,
                    tab,
                    current_url: &current_url,
                    selector: &selector,
                    state: &dom_state,
                    once: req.once,
                    ttl: req.ttl,
                    owner: req.owner,
                    idempotency_key: req.key,
                    approved_actions,
                })
                .await
        }
        StartRule::Cdp {
            entry,
            equals,
            regexes,
            matchers,
        } => {
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
            let (response, created) = state.events.subscribe_cdp(client.clone(), SourceStart {
                source,
                initial: "unknown".into(),
                tab,
                current_url: &current_url,
                filter_key: entry.method.to_owned(),
                rule: json!({ "method": entry.method, "match": equals, "matchRegex": regexes }),
                once: req.once,
                ttl: req.ttl,
                owner: req.owner,
                idempotency_key: req.key,
                approved_actions,
            }).await?;
            if created && entry.policy_action == "download" {
                let params = match &browser.download_path {
                    Some(download_path) => json!({
                        "behavior": "allow", "downloadPath": download_path, "eventsEnabled": true
                    }),
                    None => json!({ "behavior": "default", "eventsEnabled": true }),
                };
                if let Err(error) = client
                    .send_command("Browser.setDownloadBehavior", Some(params), None)
                    .await
                {
                    if let Some(id) = response.get("id").and_then(Value::as_str) {
                        let _ = state.events.cancel_setup(id).await;
                    }
                    return Err(error);
                }
            }
            Ok(response)
        }
    }
}

pub(crate) async fn read(cmd: &Value, bus: &Arc<Bus>) -> Result<Value, String> {
    let id = subscription_id(cmd)?;
    let (wait_ms, limit, ack) = read_options(cmd)?;
    let since = optional_string(cmd, "since")?;
    let owner = optional_string(cmd, "owner")?;
    let result = bus
        .read(ReadRequest {
            id,
            owner: owner.as_deref(),
            since: since.as_deref(),
            wait: Duration::from_millis(wait_ms),
            limit,
            ack,
        })
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
    fn start_validation_names_refused_cdp_domains() {
        let error = validate_start_request(&json!({
            "method": "Network.requestWillBeSent"
        }))
        .unwrap_err();

        assert!(error.contains("Network"));
    }
}
