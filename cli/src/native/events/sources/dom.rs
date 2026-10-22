use serde_json::{json, Value};

use super::super::source::{EventSource, SourceSignal};
use crate::native::cdp::client::{CdpClient, RawCdpMessage};

const MAX_LABEL_CHARS: usize = 160;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
pub(in crate::native::events) const WORLD_NAME: &str = "__agent_browser_events";

pub(in crate::native::events) struct DomObserver {
    pub(in crate::native::events) target_id: String,
    session_id: String,
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

async fn isolated_context(client: &CdpClient, session_id: &str) -> Result<i64, String> {
    let tree = client
        .send_command("Page.getFrameTree", None, Some(session_id))
        .await?;
    let frame_id = tree["frameTree"]["frame"]["id"]
        .as_str()
        .ok_or("Page.getFrameTree returned no main frame")?;
    let world = client
        .send_command(
            "Page.createIsolatedWorld",
            Some(json!({"frameId": frame_id, "worldName": WORLD_NAME})),
            Some(session_id),
        )
        .await?;
    world["executionContextId"]
        .as_i64()
        .ok_or_else(|| "Page.createIsolatedWorld returned no execution context".into())
}

fn parse_observation(payload: &str, expected_nonce: &str) -> Result<SourceSignal, ()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(());
    }
    let value: Value = serde_json::from_str(payload).map_err(|_| ())?;
    if value.as_object().ok_or(())?.len() != 5 || value["nonce"].as_str() != Some(expected_nonce) {
        return Err(());
    }
    let initial = match value["kind"].as_str() {
        Some("initial") => true,
        Some("change") => false,
        _ => return Err(()),
    };
    let state = match value["state"].as_str() {
        Some(state @ ("visible" | "hidden")) => state,
        _ => return Err(()),
    };
    let mut truncated = value["truncated"].as_bool().ok_or(())?;
    let label = match value.get("label").ok_or(())? {
        Value::Null => None,
        Value::String(label) => {
            let capped: String = label.chars().take(MAX_LABEL_CHARS).collect();
            truncated |= capped.len() < label.len();
            Some(capped)
        }
        _ => return Err(()),
    };
    Ok(SourceSignal::Match {
        key: state.into(),
        name: format!("element.{state}"),
        data: json!({"state": state, "label": label, "truncated": truncated}),
        initial,
        location: None,
    })
}

impl DomObserver {
    #[cfg(test)]
    pub(in crate::native::events) fn test_observer() -> Self {
        Self {
            target_id: "target-1".into(),
            session_id: "session-1".into(),
            binding_name: "__ab_event_test".into(),
            nonce: "nonce-1".into(),
            script_id: "script-1".into(),
            observe_key: "__ab_event_test_observe".into(),
            stop_key: "__ab_event_test_stop".into(),
        }
    }

    pub(in crate::native::events) async fn install(
        client: &CdpClient,
        session_id: &str,
        target_id: &str,
        selector: &str,
    ) -> Result<(Self, SourceSignal), String> {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let binding_name = format!("__ab_event_{nonce}");
        let send = |method: &'static str, params| {
            client.send_command(method, Some(params), Some(session_id))
        };
        let source = include_str!("../../event-observer.js")
            .replace("__BINDING_JSON__", &json!(binding_name).to_string())
            .replace("__SELECTOR_JSON__", &json!(selector).to_string())
            .replace("\"__NONCE__\"", &json!(nonce).to_string());
        let mut observer = Self {
            target_id: target_id.to_owned(),
            session_id: session_id.to_owned(),
            observe_key: format!("{binding_name}_observe"),
            stop_key: format!("{binding_name}_stop"),
            binding_name,
            nonce,
            script_id: String::new(),
        };
        let binding = json!({"name": observer.binding_name});
        send(
            "Runtime.addBinding",
            json!({
                "name": observer.binding_name, "executionContextName": WORLD_NAME
            }),
        )
        .await?;
        let script_id = send(
            "Page.addScriptToEvaluateOnNewDocument",
            json!({
                "source": source, "worldName": WORLD_NAME, "runImmediately": true
            }),
        )
        .await
        .and_then(|result| {
            result["identifier"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| {
                    "Page.addScriptToEvaluateOnNewDocument returned no identifier".into()
                })
        });
        if script_id.is_err() {
            let _ = send("Runtime.removeBinding", binding).await;
        }
        observer.script_id = script_id?;
        let initial = observer.observe(client).await;
        if initial.is_err() {
            observer.stop(client).await;
        }
        Ok((observer, initial?))
    }

    pub(in crate::native::events) async fn observe(
        &self,
        client: &CdpClient,
    ) -> Result<SourceSignal, String> {
        let context_id = isolated_context(client, &self.session_id).await?;
        let params = json!({
            "expression": format!("globalThis[{}]()", json!(self.observe_key)),
            "contextId": context_id,
            "returnByValue": true
        });
        let result = client
            .send_command("Runtime.evaluate", Some(params), Some(&self.session_id))
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
}

#[async_trait::async_trait]
impl EventSource for DomObserver {
    fn policy_actions(&self) -> Vec<String> {
        ["isvisible", "gettext", "getattribute"]
            .map(str::to_owned)
            .to_vec()
    }

    fn target_id(&self) -> &str {
        &self.target_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn classify(&self, raw: &RawCdpMessage) -> Option<SourceSignal> {
        if raw.session_id.as_deref() != Some(&self.session_id) {
            return None;
        }
        let event: Value = serde_json::from_str(&raw.text).ok()?;
        if event["method"] == "Page.navigatedWithinDocument" {
            return Some(SourceSignal::Rearmed);
        }
        if event["method"] != "Runtime.bindingCalled"
            || event["params"]["name"].as_str() != Some(&self.binding_name)
        {
            return None;
        }
        Some(
            event["params"]["payload"]
                .as_str()
                .and_then(|payload| parse_observation(payload, &self.nonce).ok())
                .unwrap_or(SourceSignal::Rejected),
        )
    }

    async fn observe(&self, client: &CdpClient) -> Result<Option<SourceSignal>, String> {
        DomObserver::observe(self, client).await.map(Some)
    }

    async fn stop(&self, client: &CdpClient) {
        let send = |method: &'static str, params| {
            client.send_command(method, Some(params), Some(self.session_id.as_str()))
        };
        if let Ok(context_id) = isolated_context(client, &self.session_id).await {
            let _ = send(
                "Runtime.evaluate",
                json!({
                    "expression": format!("globalThis[{}]?.()", json!(self.stop_key)),
                    "contextId": context_id
                }),
            )
            .await;
        }
        if !self.script_id.is_empty() {
            let _ = send(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier": self.script_id}),
            )
            .await;
        }
        let _ = send("Runtime.removeBinding", json!({"name": self.binding_name})).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(payload: &str, name: &str, session_id: &str) -> RawCdpMessage {
        RawCdpMessage {
            text: json!({
                "method": "Runtime.bindingCalled", "sessionId": session_id,
                "params": {"name": name, "payload": payload}
            })
            .to_string(),
            session_id: Some(session_id.into()),
        }
    }

    #[test]
    fn policy_actions_cover_visibility_and_text_reads() {
        assert_eq!(
            DomObserver::test_observer().policy_actions(),
            vec!["isvisible", "gettext", "getattribute"]
        );
    }

    #[test]
    fn normalizes_and_rejects_empty_selectors() {
        assert_eq!(normalize_selector("  .ready  ").unwrap(), ".ready");
        assert!(normalize_selector("  ").is_err());
    }

    #[test]
    fn validates_payloads_and_isolates_bindings() {
        let observer = DomObserver::test_observer();
        let classify =
            |payload: &str| observer.classify(&binding(payload, "__ab_event_test", "session-1"));
        let valid = json!({
            "nonce": "nonce-1", "kind": "change", "state": "visible",
            "label": "Ready", "truncated": false
        });
        let mut invalid = vec!["{".into(), "[]".into(), "{}".into()];
        for (field, value) in [
            ("nonce", json!("forged")),
            ("kind", json!("invalid")),
            ("state", json!("invalid")),
            ("truncated", Value::Null),
            ("label", json!(42)),
            ("label", json!({})),
            ("label", json!("x".repeat(MAX_PAYLOAD_BYTES))),
            ("extra", Value::Null),
        ] {
            let mut payload = valid.clone();
            payload[field] = value;
            invalid.push(payload.to_string());
        }
        for payload in invalid {
            assert!(
                matches!(classify(&payload), Some(SourceSignal::Rejected)),
                "{payload}"
            );
        }
        for (name, session_id) in [
            ("other-binding", "session-1"),
            ("__ab_event_test", "other-session"),
        ] {
            assert!(observer
                .classify(&binding(&valid.to_string(), name, session_id))
                .is_none());
        }
        assert!(matches!(
            classify(&valid.to_string()),
            Some(SourceSignal::Match { initial: false, .. })
        ));
    }

    #[test]
    fn caps_labels_at_unicode_character_boundary() {
        let payload = json!({
            "nonce": "nonce-1", "kind": "initial", "state": "hidden",
            "label": "é".repeat(161), "truncated": false
        });
        let SourceSignal::Match {
            data,
            initial: true,
            ..
        } = parse_observation(&payload.to_string(), "nonce-1").unwrap()
        else {
            panic!("expected an initial match")
        };
        assert_eq!(
            data,
            json!({"state": "hidden", "label": "é".repeat(160), "truncated": true})
        );
    }
}
