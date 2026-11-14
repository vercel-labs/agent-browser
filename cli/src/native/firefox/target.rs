//! Resolves refs and selectors to BiDi node handles for Firefox element commands.

use serde_json::{json, Value};

use super::bidi::SANDBOX;
use super::daemon::session;
use super::session::FirefoxSession;
use crate::native::actions::DaemonState;
use crate::native::element::{build_find_element_js, parse_ref, RefEntry};

/// A node handle: the BiDi browsing context that owns it and its `sharedId`.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub context: String,
    pub shared_id: String,
}

impl Target {
    pub(crate) fn node_ref(&self) -> Value {
        json!({"sharedId": self.shared_id})
    }
}

/// Finds the first element matching a CSS or `xpath=` selector in `context`.
pub async fn find_element(
    ff: &FirefoxSession,
    context: &str,
    selector: &str,
) -> Result<Option<String>, String> {
    let js = format!("() => {}", build_find_element_js(selector));
    let found = ff.bidi.call_function(context, &js, None, vec![]).await?;
    Ok(found["sharedId"].as_str().map(String::from))
}

/// Whether `this` is attached to the document its browsing context shows now. A node of a
/// document the page has since replaced stays connected to that old document, so both are checked.
pub(crate) const IN_DOCUMENT_JS: &str =
    "function() { return this.isConnected && this.ownerDocument === document; }";

/// Whether the node is still in the page. It is asked in the sandbox, where the page cannot
/// answer for it.
async fn is_connected(ff: &FirefoxSession, target: &Target) -> bool {
    let r = ff
        .bidi
        .call_function_on_in_sandbox(&target.context, SANDBOX, IN_DOCUMENT_JS, target.node_ref())
        .await;
    r.is_ok_and(|v| v == true)
}

/// Resolves `@eN`, `eN`, `ref=eN`, a CSS selector or `xpath=...` to a live node.
/// A ref whose node is gone is recovered by role and name only when exactly one element matches,
/// the ref had no `nth`, and the match is in the document the ref was taken from.
pub async fn resolve_target(
    state: &mut DaemonState,
    selector_or_ref: &str,
) -> Result<Target, String> {
    let Some(ref_id) = parse_ref(selector_or_ref) else {
        let ff = session(state)?;
        let context = ff.contexts.active().to_string();
        let shared_id = find_element(ff, &context, selector_or_ref)
            .await
            .map_err(|error| format!("Could not resolve selector '{selector_or_ref}': {error}"))?
            .ok_or_else(|| format!("Element not found: {selector_or_ref}"))?;
        return Ok(Target { context, shared_id });
    };
    let entry = state
        .ref_map
        .get(&ref_id)
        .ok_or_else(|| format!("Unknown ref: {ref_id}"))?
        .clone();
    let ff = session(state)?;
    let known = entry.backend_node_id.and_then(|id| {
        ff.refs
            .iter()
            .find(|(_, v)| **v == id)
            .map(|((context, shared_id), _)| Target {
                context: context.clone(),
                shared_id: shared_id.clone(),
            })
    });
    if let Some(target) = &known {
        if is_connected(ff, target).await {
            return Ok(target.clone());
        }
    }
    recover(ff, &entry, known.map(|t| t.context)).await
}

/// Whether the node belongs to the document the last snapshot recorded for `context`. A page
/// that navigated since then has a new document, and a ref must not carry over to it.
async fn in_snapshot_document(ff: &FirefoxSession, context: &str, shared_id: &str) -> bool {
    let Some(recorded) = ff.documents.get(context) else {
        return false;
    };
    let document = ff
        .bidi
        .call_function_on_in_sandbox(
            context,
            SANDBOX,
            "function() { return this.ownerDocument; }",
            json!({ "sharedId": shared_id }),
        )
        .await;
    document.is_ok_and(|document| document["sharedId"] == recorded.as_str())
}

async fn recover(
    ff: &mut FirefoxSession,
    entry: &RefEntry,
    context: Option<String>,
) -> Result<Target, String> {
    let (role, name) = (&entry.role, &entry.name);
    if entry
        .backend_node_id
        .is_some_and(|id| ff.shadow_ids.contains(&id))
    {
        return Err(format!("Stale ref for role={role} name={name}: the element was inside a shadow root and is gone. Run snapshot again and use a fresh ref."));
    }
    if entry.nth.is_some() {
        return Err(format!("Stale ref for role={role} name={name} cannot be recovered: the snapshot had several matching elements. Run snapshot again and use a fresh ref."));
    }
    let context = context
        .or_else(|| entry.frame_id.clone())
        .unwrap_or_else(|| ff.contexts.active().to_string());
    let found = ff
        .bidi
        .send(
            "browsingContext.locateNodes",
            json!({
                "context": context,
                "locator": {"type": "accessibility", "value": {"role": role, "name": name}},
                "serializationOptions": {"maxDomDepth": 0},
            }),
        )
        .await?;
    let nodes = found["nodes"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    match nodes {
        [] => Err(format!("Could not locate element with role={role} name={name}: the ref is stale. Run snapshot again and use a fresh ref.")),
        [node] => {
            let shared_id = node["sharedId"]
                .as_str()
                .ok_or("Firefox returned a node without a sharedId")?
                .to_string();
            if !in_snapshot_document(ff, &context, &shared_id).await {
                return Err(format!("Stale ref for role={role} name={name}: the page changed since the snapshot. Run snapshot again and use a fresh ref."));
            }
            if let Some(id) = entry.backend_node_id {
                ff.refs.retain(|_, v| *v != id);
                ff.refs.insert((context.clone(), shared_id.clone()), id);
            }
            Ok(Target { context, shared_id })
        }
        _ => Err(format!("Stale ref for role={role} name={name} cannot be recovered: several elements now match. Run snapshot again and use a fresh ref.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::firefox::session::tests::{
        fake_session, fake_session_with, fake_session_with_script, Calls,
    };

    /// A state whose ref `e1` lost its node, where one element still matches its role and name
    /// and belongs to the document `current`, while the snapshot recorded `doc-1`.
    async fn state_with_a_match_in(current: &'static str) -> DaemonState {
        let (mut ff, _, _) = fake_session_with(move |v: &Value| {
            let remote = |value: Value| json!({"type": "success", "result": value});
            match v["method"].as_str() {
                Some("browsingContext.locateNodes") => json!({"nodes": [{"sharedId": "found"}]}),
                Some("script.callFunction") if v["params"]["target"]["sandbox"].is_string() => {
                    remote(json!({"type": "node", "sharedId": current}))
                }
                Some("script.callFunction") => remote(json!({"type": "boolean", "value": false})),
                _ => json!({}),
            }
        })
        .await;
        ff.refs.insert(("ctx-active".into(), "sid".into()), 7);
        ff.documents.insert("ctx-active".into(), "doc-1".into());
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(7), "button", "OK", None, None);
        state
    }

    #[tokio::test]
    async fn a_ref_is_recovered_inside_the_document_it_was_taken_from() {
        let mut state = state_with_a_match_in("doc-1").await;

        let target = resolve_target(&mut state, "@e1").await.unwrap();

        assert_eq!(target.shared_id, "found");
    }

    #[tokio::test]
    async fn a_ref_is_not_recovered_in_a_document_that_replaced_its_own() {
        let mut state = state_with_a_match_in("doc-2").await;

        let error = resolve_target(&mut state, "@e1").await.unwrap_err();

        assert!(
            error.contains("the page changed since the snapshot"),
            "{error}"
        );
        let ff = state.firefox.as_ref().unwrap();
        assert!(ff
            .refs
            .contains_key(&("ctx-active".to_string(), "sid".to_string())));
    }

    async fn state_with_ref(nth: Option<usize>) -> (DaemonState, Calls) {
        let mut state = DaemonState::new();
        let (mut ff, _tx, calls) = fake_session().await;
        ff.refs.insert(("ctx-active".into(), "sid".into()), 7);
        state.firefox = Some(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(7), "button", "OK", nth, None);
        (state, calls)
    }

    #[tokio::test]
    async fn disconnected_ref_with_no_match_is_stale() {
        let (mut state, calls) = state_with_ref(None).await;
        let e = resolve_target(&mut state, "@e1").await.unwrap_err();
        assert!(e.contains("the ref is stale"), "{e}");
        assert_eq!(
            *calls.lock().unwrap(),
            ["script.callFunction", "browsingContext.locateNodes"]
        );
    }

    #[tokio::test]
    async fn recovery_is_refused_for_refs_with_nth() {
        let (mut state, calls) = state_with_ref(Some(1)).await;
        let e = resolve_target(&mut state, "@e1").await.unwrap_err();
        assert!(
            e.contains("the snapshot had several matching elements"),
            "{e}"
        );
        assert!(!calls
            .lock()
            .unwrap()
            .contains(&"browsingContext.locateNodes".to_string()));
    }

    #[tokio::test]
    async fn unknown_ref_fails_before_touching_the_browser() {
        let (mut state, calls) = state_with_ref(None).await;
        let e = resolve_target(&mut state, "@e9").await.unwrap_err();
        assert_eq!(e, "Unknown ref: e9");
        assert!(calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn durable_ref_targets_its_context_and_shared_id() {
        let (mut ff, _, calls) = fake_session_with_script(|_| {
            json!({
                "type": "success",
                "result": {"type": "boolean", "value": true}
            })
        })
        .await;
        ff.refs.insert(("ctx-frame".into(), "sid".into()), 42);
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state
            .ref_map
            .add_with_frame("e1".into(), Some(42), "button", "OK", None, None);

        let target = resolve_target(&mut state, "@e1").await.unwrap();

        assert_eq!(
            target,
            Target {
                context: "ctx-frame".into(),
                shared_id: "sid".into(),
            }
        );
        assert_eq!(*calls.lock().unwrap(), ["script.callFunction"]);
    }

    #[tokio::test]
    async fn stale_shadow_ref_is_not_recovered_by_role_and_name() {
        let (mut state, calls) = state_with_ref(None).await;
        state.firefox.as_mut().unwrap().shadow_ids.insert(7);

        let error = resolve_target(&mut state, "@e1").await.unwrap_err();

        assert!(error.contains("inside a shadow root"), "{error}");
        assert_eq!(*calls.lock().unwrap(), ["script.callFunction"]);
    }
}
