use std::sync::Mutex;

use serde_json::{json, Value};

use super::super::source::{EventSource, SourceSignal};
use crate::native::cdp::client::{CdpClient, RawCdpMessage};

const MAX_LABEL_CHARS: usize = 160;
const MAX_PAYLOAD_BYTES: usize = 16 * 1024;
pub(in crate::native::events) const DOM_POLICY_ACTIONS: &[&str] =
    &["isvisible", "gettext", "getattribute"];
pub(in crate::native::events) const WORLD_NAME: &str = "__agent_browser_events";

/// CDP identifiers a DOM observer only learns once it is installed.
struct DomRuntime {
    script_id: String,
    main_frame_id: String,
}

pub(in crate::native::events) struct DomObserver {
    pub(in crate::native::events) target_id: String,
    session_id: String,
    binding_name: String,
    nonce: String,
    observe_key: String,
    stop_key: String,
    runtime: Mutex<Option<DomRuntime>>,
    seen: Mutex<Option<(String, u64)>>,
}

pub(in crate::native::events) fn normalize_selector(selector: &str) -> Result<String, String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err("Selector must not be empty".into());
    }
    Ok(selector.to_owned())
}

async fn isolated_context(client: &CdpClient, session_id: &str) -> Result<(String, i64), String> {
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
    let context_id = world["executionContextId"]
        .as_i64()
        .ok_or_else(|| "Page.createIsolatedWorld returned no execution context".to_owned())?;
    Ok((frame_id.to_owned(), context_id))
}

fn parse_observation(
    payload: &str,
    expected_nonce: &str,
) -> Result<(SourceSignal, Option<(String, u64)>), ()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(());
    }
    let value: Value = serde_json::from_str(payload).map_err(|_| ())?;
    let object = value.as_object().ok_or(())?;
    if value["nonce"].as_str() != Some(expected_nonce) {
        return Err(());
    }
    let stamp = match object.len() {
        5 => None,
        7 => {
            let doc = value
                .get("doc")
                .and_then(Value::as_str)
                .filter(|doc| !doc.is_empty())
                .ok_or(())?;
            let seq = value
                .get("seq")
                .and_then(Value::as_u64)
                .filter(|seq| *seq > 0)
                .ok_or(())?;
            Some((doc.to_owned(), seq))
        }
        _ => return Err(()),
    };
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
    Ok((
        SourceSignal::Match {
            key: state.into(),
            name: format!("element.{state}"),
            data: json!({"state": state, "label": label, "truncated": truncated}),
            initial,
            location: None,
        },
        stamp,
    ))
}

impl DomObserver {
    /// Builds an observer with every identifier it needs before any CDP call.
    pub(in crate::native::events) fn prepare(target_id: &str, session_id: &str) -> Self {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let binding_name = format!("__ab_event_{nonce}");
        Self {
            target_id: target_id.to_owned(),
            session_id: session_id.to_owned(),
            observe_key: format!("{binding_name}_observe"),
            stop_key: format!("{binding_name}_stop"),
            binding_name,
            nonce,
            runtime: Mutex::new(None),
            seen: Mutex::new(None),
        }
    }

    #[cfg(test)]
    pub(in crate::native::events) fn test_observer() -> Self {
        Self {
            target_id: "target-1".into(),
            session_id: "session-1".into(),
            binding_name: "__ab_event_test".into(),
            nonce: "nonce-1".into(),
            observe_key: "__ab_event_test_observe".into(),
            stop_key: "__ab_event_test_stop".into(),
            runtime: Mutex::new(Some(DomRuntime {
                script_id: "script-1".into(),
                main_frame_id: "main-frame".into(),
            })),
            seen: Mutex::new(None),
        }
    }

    /// Installs the binding and observer script, returning the first observation.
    pub(in crate::native::events) async fn activate(
        &self,
        client: &CdpClient,
        selector: &str,
    ) -> Result<SourceSignal, String> {
        let send = |method: &'static str, params| {
            client.send_command(method, Some(params), Some(self.session_id.as_str()))
        };
        let (main_frame_id, _) = isolated_context(client, &self.session_id).await?;
        let binding = json!({"name": self.binding_name});
        send(
            "Runtime.addBinding",
            json!({"name": self.binding_name, "executionContextName": WORLD_NAME}),
        )
        .await?;
        let source = include_str!("../../event-observer.js")
            .replace("__BINDING_JSON__", &json!(self.binding_name).to_string())
            .replace("__SELECTOR_JSON__", &json!(selector).to_string())
            .replace("\"__NONCE__\"", &json!(self.nonce).to_string());
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
        let script_id = match script_id {
            Ok(script_id) => script_id,
            Err(error) => {
                let _ = send("Runtime.removeBinding", binding).await;
                return Err(error);
            }
        };
        *self
            .runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(DomRuntime {
            script_id,
            main_frame_id,
        });
        match self.observe(client).await {
            Ok(signal) => Ok(signal),
            Err(error) => {
                self.stop(client).await;
                Err(error)
            }
        }
    }

    fn main_frame_id(&self) -> Option<String> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|runtime| runtime.main_frame_id.clone())
    }

    fn script_id(&self) -> Option<String> {
        self.runtime
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .map(|runtime| runtime.script_id.clone())
    }

    pub(in crate::native::events) async fn observe(
        &self,
        client: &CdpClient,
    ) -> Result<SourceSignal, String> {
        let (_, context_id) = isolated_context(client, &self.session_id).await?;
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
            .map(|(signal, _)| signal)
            .map_err(|_| "Observer returned an invalid observation".into())
    }
}

#[async_trait::async_trait]
impl EventSource for DomObserver {
    fn policy_actions(&self) -> Vec<String> {
        DOM_POLICY_ACTIONS
            .iter()
            .map(|action| (*action).to_owned())
            .collect()
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
            let main_frame_id = self.main_frame_id()?;
            return (event.pointer("/params/frameId").and_then(Value::as_str)
                == Some(main_frame_id.as_str()))
            .then_some(SourceSignal::Rearmed);
        }
        if event["method"] != "Runtime.bindingCalled"
            || event["params"]["name"].as_str() != Some(&self.binding_name)
        {
            return None;
        }
        let Some(payload) = event["params"]["payload"].as_str() else {
            return Some(SourceSignal::Rejected);
        };
        let Ok((signal, stamp)) = parse_observation(payload, &self.nonce) else {
            return Some(SourceSignal::Rejected);
        };
        if let Some((doc, seq)) = stamp {
            let mut seen = self
                .seen
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((seen_doc, seen_seq)) = seen.as_ref() {
                if *seen_doc == doc && seq <= *seen_seq {
                    return None;
                }
            }
            *seen = Some((doc, seq));
        }
        Some(signal)
    }

    async fn observe(&self, client: &CdpClient) -> Result<Option<SourceSignal>, String> {
        DomObserver::observe(self, client).await.map(Some)
    }

    async fn stop(&self, client: &CdpClient) {
        let send = |method: &'static str, params| {
            client.send_command(method, Some(params), Some(self.session_id.as_str()))
        };
        if let Ok((_, context_id)) = isolated_context(client, &self.session_id).await {
            let _ = send(
                "Runtime.evaluate",
                json!({
                    "expression": format!("globalThis[{}]?.()", json!(self.stop_key)),
                    "contextId": context_id
                }),
            )
            .await;
        }
        if let Some(script_id) = self.script_id() {
            let _ = send(
                "Page.removeScriptToEvaluateOnNewDocument",
                json!({"identifier": script_id}),
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
    fn rearms_only_for_main_frame_same_document_navigation() {
        let observer = DomObserver::test_observer();
        let navigation = |frame_id: &str| RawCdpMessage {
            text:
                json!({"method": "Page.navigatedWithinDocument", "params": {"frameId": frame_id}})
                    .to_string(),
            session_id: Some("session-1".into()),
        };
        assert!(observer.classify(&navigation("child-frame")).is_none());
        assert!(matches!(
            observer.classify(&navigation("main-frame")),
            Some(SourceSignal::Rearmed)
        ));
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
        let (
            SourceSignal::Match {
                data,
                initial: true,
                ..
            },
            None,
        ) = parse_observation(&payload.to_string(), "nonce-1").unwrap()
        else {
            panic!("expected an initial match")
        };
        assert_eq!(
            data,
            json!({"state": "hidden", "label": "é".repeat(160), "truncated": true})
        );
    }

    #[test]
    fn deduplicates_stamped_observations_per_document() {
        let observer = DomObserver::test_observer();
        let send = |doc: &str, seq: u64, state: &str| {
            let payload = json!({
                "nonce": "nonce-1", "kind": "change", "state": state,
                "label": "ok", "truncated": false, "doc": doc, "seq": seq
            });
            observer.classify(&binding(
                &payload.to_string(),
                "__ab_event_test",
                "session-1",
            ))
        };
        assert!(matches!(
            send("doc-a", 1, "visible"),
            Some(SourceSignal::Match { .. })
        ));
        assert!(send("doc-a", 1, "visible").is_none());
        assert!(matches!(
            send("doc-a", 2, "hidden"),
            Some(SourceSignal::Match { .. })
        ));
        assert!(send("doc-a", 1, "visible").is_none());
        assert!(matches!(
            send("doc-b", 1, "visible"),
            Some(SourceSignal::Match { .. })
        ));
    }
}
