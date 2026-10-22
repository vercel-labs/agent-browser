use serde_json::{json, Value};

use super::super::source::EventSource;
use crate::native::cdp::client::{CdpClient, RawCdpMessage};

const MAX_LABEL_CHARS: usize = 160;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
const WORLD_NAME: &str = "__agent_browser_events";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native::events) enum DomState {
    Visible,
    Hidden,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native::events) struct Observation {
    pub state: DomState,
    pub label: Option<String>,
    pub truncated: bool,
    pub initial: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::native::events) enum DomSignal {
    Observation(Observation),
    SameDocumentNavigation,
    RejectedPayload,
}

pub(in crate::native::events) struct DomObserver {
    pub(in crate::native::events) target_id: String,
    pub(in crate::native::events) selector: String,
    session_id: String,
    world_name: String,
    binding_name: String,
    nonce: String,
    script_id: String,
    observe_key: String,
    stop_key: String,
}

pub(in crate::native::events) fn normalize_selector(selector: &str) -> Result<String, String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err("Selector must not be empty".into());
    }
    Ok(selector.to_owned())
}

pub(in crate::native::events) fn observer_script(
    binding_name: &str,
    selector: &str,
    nonce: &str,
) -> String {
    include_str!("../../event-observer.js")
        .replace("__BINDING_JSON__", &json!(binding_name).to_string())
        .replace("__SELECTOR_JSON__", &json!(selector).to_string())
        .replace("\"__NONCE__\"", &json!(nonce).to_string())
}

async fn isolated_context(
    client: &CdpClient,
    session_id: &str,
    world_name: &str,
) -> Result<i64, String> {
    let tree = client
        .send_command("Page.getFrameTree", None, Some(session_id))
        .await?;
    let frame_id = tree
        .pointer("/frameTree/frame/id")
        .and_then(Value::as_str)
        .ok_or("Page.getFrameTree returned no main frame")?;
    let world = client
        .send_command(
            "Page.createIsolatedWorld",
            Some(json!({"frameId": frame_id, "worldName": world_name})),
            Some(session_id),
        )
        .await?;
    world
        .get("executionContextId")
        .and_then(Value::as_i64)
        .ok_or_else(|| "Page.createIsolatedWorld returned no execution context".into())
}

fn parse_observation(payload: &str, expected_nonce: &str) -> Result<Observation, ()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(());
    }
    let value: Value = serde_json::from_str(payload).map_err(|_| ())?;
    let object = value
        .as_object()
        .filter(|object| object.len() == 5)
        .ok_or(())?;
    if object.get("nonce").and_then(Value::as_str) != Some(expected_nonce) {
        return Err(());
    }
    let initial = match object.get("kind").and_then(Value::as_str) {
        Some("initial") => true,
        Some("change") => false,
        _ => return Err(()),
    };
    let state = match object.get("state").and_then(Value::as_str) {
        Some("visible") => DomState::Visible,
        Some("hidden") => DomState::Hidden,
        _ => return Err(()),
    };
    let mut truncated = object.get("truncated").and_then(Value::as_bool).ok_or(())?;
    let label = match object.get("label") {
        Some(Value::Null) => None,
        Some(Value::String(label)) => {
            let mut characters = label.chars();
            let capped: String = characters.by_ref().take(MAX_LABEL_CHARS).collect();
            if characters.next().is_some() {
                truncated = true;
                Some(capped)
            } else {
                Some(label.clone())
            }
        }
        _ => return Err(()),
    };
    Ok(Observation {
        state,
        label,
        truncated,
        initial,
    })
}

impl DomObserver {
    pub(in crate::native::events) async fn install(
        client: &CdpClient,
        session_id: &str,
        target_id: &str,
        selector: &str,
    ) -> Result<(Self, Observation), String> {
        let selector = normalize_selector(selector)?;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let binding_name = format!("__ab_event_{nonce}");
        let observer = Self {
            target_id: target_id.to_owned(),
            selector: selector.clone(),
            session_id: session_id.to_owned(),
            world_name: WORLD_NAME.to_owned(),
            observe_key: format!("{binding_name}_observe"),
            stop_key: format!("{binding_name}_stop"),
            binding_name,
            nonce,
            script_id: String::new(),
        };
        client
            .send_command(
                "Runtime.addBinding",
                Some(json!({
                    "name": observer.binding_name,
                    "executionContextName": observer.world_name
                })),
                Some(session_id),
            )
            .await?;
        let result = match client
            .send_command(
                "Page.addScriptToEvaluateOnNewDocument",
                Some(json!({
                    "source": observer_script(&observer.binding_name, &selector, &observer.nonce),
                    "worldName": observer.world_name,
                    "runImmediately": true
                })),
                Some(session_id),
            )
            .await
        {
            Ok(result) => result,
            Err(error) => {
                let _ = client
                    .send_command(
                        "Runtime.removeBinding",
                        Some(json!({"name": observer.binding_name})),
                        Some(session_id),
                    )
                    .await;
                return Err(error);
            }
        };
        let Some(script_id) = result.get("identifier").and_then(Value::as_str) else {
            let _ = client
                .send_command(
                    "Runtime.removeBinding",
                    Some(json!({"name": observer.binding_name})),
                    Some(session_id),
                )
                .await;
            return Err("Page.addScriptToEvaluateOnNewDocument returned no identifier".into());
        };
        let mut observer = observer;
        observer.script_id = script_id.to_owned();
        match observer.observe(client).await {
            Ok(initial) => Ok((observer, initial)),
            Err(error) => {
                observer.remove(client).await;
                Err(error)
            }
        }
    }

    pub(in crate::native::events) async fn observe(
        &self,
        client: &CdpClient,
    ) -> Result<Observation, String> {
        let context_id = isolated_context(client, &self.session_id, &self.world_name).await?;
        let result = client
            .send_command(
                "Runtime.evaluate",
                Some(json!({
                    "expression": format!("globalThis[{}]()", json!(self.observe_key)),
                    "contextId": context_id,
                    "returnByValue": true
                })),
                Some(&self.session_id),
            )
            .await?;
        if result.get("exceptionDetails").is_some() {
            return Err("Observer evaluation failed; check the CSS selector and document".into());
        }
        let value = result
            .pointer("/result/value")
            .ok_or("Observer did not return its current state")?;
        parse_observation(&value.to_string(), &self.nonce)
            .map_err(|_| "Observer returned an invalid observation".into())
    }

    pub(in crate::native::events) async fn remove(&self, client: &CdpClient) {
        if let Ok(context_id) = isolated_context(client, &self.session_id, &self.world_name).await {
            let _ = client
                .send_command(
                    "Runtime.evaluate",
                    Some(json!({
                        "expression": format!("globalThis[{}]?.()", json!(self.stop_key)),
                        "contextId": context_id
                    })),
                    Some(&self.session_id),
                )
                .await;
        }
        if !self.script_id.is_empty() {
            let _ = client
                .send_command(
                    "Page.removeScriptToEvaluateOnNewDocument",
                    Some(json!({"identifier": self.script_id})),
                    Some(&self.session_id),
                )
                .await;
        }
        let _ = client
            .send_command(
                "Runtime.removeBinding",
                Some(json!({"name": self.binding_name})),
                Some(&self.session_id),
            )
            .await;
    }

    pub(in crate::native::events) fn classify(&self, raw: &RawCdpMessage) -> Option<DomSignal> {
        if raw.session_id.as_deref() != Some(&self.session_id) {
            return None;
        }
        let event: Value = serde_json::from_str(&raw.text).ok()?;
        match event.get("method")?.as_str()? {
            "Page.navigatedWithinDocument" => Some(DomSignal::SameDocumentNavigation),
            "Runtime.bindingCalled" => {
                let params = event.get("params")?.as_object()?;
                if params.get("name").and_then(Value::as_str) != Some(&self.binding_name) {
                    return None;
                }
                let Some(payload) = params.get("payload").and_then(Value::as_str) else {
                    return Some(DomSignal::RejectedPayload);
                };
                Some(match parse_observation(payload, &self.nonce) {
                    Ok(observation) => DomSignal::Observation(observation),
                    Err(()) => DomSignal::RejectedPayload,
                })
            }
            _ => None,
        }
    }
}

impl EventSource for DomObserver {
    fn supported_engines(&self) -> &'static [&'static str] {
        &["chrome"]
    }

    fn policy_actions(&self) -> &'static [&'static str] {
        &["events_start", "events_wait"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn observer() -> DomObserver {
        DomObserver {
            session_id: "session-1".into(),
            target_id: "target-1".into(),
            selector: ".ready".into(),
            world_name: WORLD_NAME.into(),
            binding_name: "__ab_event_test".into(),
            nonce: "nonce-1".into(),
            script_id: "script-1".into(),
            observe_key: "__ab_event_test_observe".into(),
            stop_key: "__ab_event_test_stop".into(),
        }
    }

    fn raw(session_id: &str, method: &str, params: Value) -> RawCdpMessage {
        RawCdpMessage {
            text: json!({"sessionId": session_id, "method": method, "params": params}).to_string(),
            session_id: Some(session_id.into()),
        }
    }

    fn binding(payload: &str, name: &str, session_id: &str) -> RawCdpMessage {
        raw(
            session_id,
            "Runtime.bindingCalled",
            json!({"name": name, "payload": payload}),
        )
    }

    fn payload(nonce: &str, kind: &str, state: &str, label: Value) -> String {
        json!({
            "nonce": nonce,
            "kind": kind,
            "state": state,
            "label": label,
            "truncated": false
        })
        .to_string()
    }

    #[test]
    fn normalizes_and_rejects_empty_selectors() {
        assert_eq!(normalize_selector("  .ready  ").unwrap(), ".ready");
        assert!(normalize_selector("  ").is_err());
    }

    #[test]
    fn classifies_valid_change() {
        let signal = observer().classify(&binding(
            &payload("nonce-1", "change", "visible", json!("Ready")),
            "__ab_event_test",
            "session-1",
        ));
        assert_eq!(
            signal,
            Some(DomSignal::Observation(Observation {
                state: DomState::Visible,
                label: Some("Ready".into()),
                truncated: false,
                initial: false,
            }))
        );
    }

    #[test]
    fn marks_initial_observations() {
        let signal = observer().classify(&binding(
            &payload("nonce-1", "initial", "hidden", Value::Null),
            "__ab_event_test",
            "session-1",
        ));
        assert!(matches!(
            signal,
            Some(DomSignal::Observation(Observation {
                initial: true,
                state: DomState::Hidden,
                ..
            }))
        ));
    }

    #[test]
    fn rejects_wrong_nonce() {
        let signal = observer().classify(&binding(
            &payload("forged", "change", "visible", Value::Null),
            "__ab_event_test",
            "session-1",
        ));
        assert_eq!(signal, Some(DomSignal::RejectedPayload));
    }

    #[test]
    fn ignores_wrong_binding_and_session() {
        let observer = observer();
        assert_eq!(
            observer.classify(&binding(
                &payload("nonce-1", "change", "visible", Value::Null),
                "other-binding",
                "session-1",
            )),
            None
        );
        assert_eq!(
            observer.classify(&binding(
                &payload("nonce-1", "change", "visible", Value::Null),
                "__ab_event_test",
                "other-session",
            )),
            None
        );
    }

    #[test]
    fn rejects_invalid_json_payload() {
        assert_eq!(
            observer().classify(&binding("{", "__ab_event_test", "session-1")),
            Some(DomSignal::RejectedPayload)
        );
        assert_eq!(
            observer().classify(&binding(
                &json!({"nonce": "nonce-1", "kind": "invalid"}).to_string(),
                "__ab_event_test",
                "session-1",
            )),
            Some(DomSignal::RejectedPayload)
        );
    }

    #[test]
    fn rejects_oversized_payloads() {
        let payload = json!({
            "nonce": "nonce-1",
            "kind": "change",
            "state": "visible",
            "label": "x".repeat(MAX_PAYLOAD_BYTES),
            "truncated": false
        })
        .to_string();
        assert_eq!(
            observer().classify(&binding(&payload, "__ab_event_test", "session-1")),
            Some(DomSignal::RejectedPayload)
        );
    }

    #[test]
    fn truncates_oversized_labels() {
        let label = "x".repeat(161);
        let signal = observer().classify(&binding(
            &payload("nonce-1", "change", "visible", json!(label)),
            "__ab_event_test",
            "session-1",
        ));
        let Some(DomSignal::Observation(observation)) = signal else {
            panic!("expected an observation");
        };
        assert_eq!(observation.label.as_deref().unwrap().chars().count(), 160);
        assert!(observation.truncated);
    }

    #[test]
    fn classifies_same_document_navigation() {
        assert_eq!(
            observer().classify(&raw(
                "session-1",
                "Page.navigatedWithinDocument",
                json!({"frameId": "frame-1", "url": "https://example.test/next"}),
            )),
            Some(DomSignal::SameDocumentNavigation)
        );
    }
}
