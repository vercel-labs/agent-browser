//! Resolves refs and selectors to BiDi node handles for Firefox element commands.

use serde_json::json;

use super::actions::session;
use super::session::FirefoxSession;
use crate::native::actions::DaemonState;
use crate::native::element::{build_find_element_js, parse_ref, RefEntry, IS_CONNECTED_JS};

/// A node handle: the BiDi browsing context that owns it and its `sharedId`.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub context: String,
    pub shared_id: String,
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

async fn is_connected(ff: &FirefoxSession, target: &Target) -> bool {
    let this = json!({"sharedId": target.shared_id});
    let r = ff
        .bidi
        .call_function(&target.context, IS_CONNECTED_JS, Some(this), vec![])
        .await;
    r.is_ok_and(|v| v == true)
}

/// Resolves `@eN`, `eN`, `ref=eN`, a CSS selector or `xpath=...` to a live node.
/// A ref whose node is gone is recovered by role and name only when exactly one element matches and the ref had no `nth`.
pub async fn resolve_target(
    state: &mut DaemonState,
    selector_or_ref: &str,
) -> Result<Target, String> {
    let Some(ref_id) = parse_ref(selector_or_ref) else {
        let ff = session(state)?;
        let context = ff.context.clone();
        let shared_id = find_element(ff, &context, selector_or_ref)
            .await?
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
    let context = context.unwrap_or_else(|| ff.context.clone());
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
    use crate::native::firefox::session::tests::{fake_session, Calls};

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
}
