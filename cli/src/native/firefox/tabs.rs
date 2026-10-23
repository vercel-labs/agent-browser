//! Firefox tab and window commands. Tab ids are stable `t<N>` ids over BiDi browsing contexts.

use serde_json::{json, Value};

use super::actions::session;
use crate::native::actions::DaemonState;
use crate::native::browser::{format_tab_id, is_valid_label, TabRef};

/// Opens a tab and makes it active. Must return what Chrome's `handle_tab_new` returns,
/// honouring `url` and `label`.
pub async fn tab_new(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let url = cmd.get("url").and_then(Value::as_str);
    let label = cmd.get("label").and_then(Value::as_str);
    if let Some(label) = label {
        if !is_valid_label(label) {
            return Err(format!(
                "Invalid tab label `{label}`; labels must start with a letter and contain only \
                 letters, digits, `-`, and `_`"
            ));
        }
        if session(state)?
            .tab_labels
            .values()
            .any(|existing| existing == label)
        {
            return Err(format!(
                "Label `{label}` is already used by another tab; labels must be unique within a \
                 session"
            ));
        }
    }

    let created = session(state)?
        .bidi
        .send("browsingContext.create", json!({"type": "tab"}))
        .await?;
    let context = created["context"]
        .as_str()
        .ok_or("Firefox did not return a context id for the new tab")?
        .to_string();
    let tab_id = session(state)?.tab_id(&context);

    let landed = {
        let ff = session(state)?;
        ff.bidi
            .send("browsingContext.activate", json!({"context": context}))
            .await?;
        ff.context = context.clone();
        ff.top_context = context.clone();
        ff.refs.clear();
        if let Some(label) = label {
            ff.tab_labels.insert(context.clone(), label.to_string());
        }
        match url {
            Some(url) => {
                let nav = ff
                    .bidi
                    .send(
                        "browsingContext.navigate",
                        json!({"context": context, "url": url, "wait": "complete"}),
                    )
                    .await?;
                nav["url"].as_str().unwrap_or(url).to_string()
            }
            None => "about:blank".to_string(),
        }
    };
    reset_refs(state);
    let total = tab_count(state).await?;

    Ok(json!({
        "tabId": format_tab_id(tab_id),
        "targetId": context,
        "label": label,
        "url": landed,
        "total": total,
    }))
}

/// Lists top-level tabs as `{tabs: [{tabId, targetId, label, title, url, type, active}]}`.
pub async fn tab_list(state: &mut DaemonState) -> Result<Value, String> {
    let tree = session(state)?
        .bidi
        .send("browsingContext.getTree", json!({"maxDepth": 0}))
        .await?;
    let contexts: Vec<Value> = tree["contexts"].as_array().cloned().unwrap_or_default();
    let mut tabs = Vec::with_capacity(contexts.len());
    for c in &contexts {
        let context = c["context"].as_str().unwrap_or_default().to_string();
        let tab_id = session(state)?.tab_id(&context);
        let title = session(state)?
            .bidi
            .call_function(&context, "() => document.title", None, vec![])
            .await
            .unwrap_or(Value::Null);
        let ff = session(state)?;
        let label = ff.tab_labels.get(&context).cloned();
        let active = context == ff.top_context;
        tabs.push(json!({
            "tabId": format_tab_id(tab_id),
            "targetId": context,
            "label": label,
            "title": title,
            "url": c["url"],
            "type": "page",
            "active": active,
        }));
    }
    Ok(json!({ "tabs": tabs }))
}

/// Switches the active tab by `t<N>` id, label, or context UUID. Must return what Chrome's
/// `handle_tab_switch` returns and clear `state.ref_map` and `ff.refs` after the switch commits.
pub async fn tab_switch(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let requested = cmd
        .get("tabId")
        .and_then(Value::as_str)
        .ok_or("Missing 'tabId' parameter (expected `t<N>`, a label, or a tab context id)")?;
    let tabs = tab_list(state).await?;
    let list = tabs["tabs"].as_array().cloned().unwrap_or_default();
    let context = resolve_tab_ref(&list, requested)?;
    let tab = list
        .iter()
        .find(|t| t["targetId"].as_str() == Some(context.as_str()))
        .cloned()
        .unwrap_or(Value::Null);

    {
        let ff = session(state)?;
        ff.bidi
            .send("browsingContext.activate", json!({"context": context}))
            .await?;
        ff.context = context.clone();
        ff.top_context = context.clone();
        ff.refs.clear();
    }
    reset_refs(state);

    Ok(json!({
        "tabId": tab["tabId"],
        "targetId": tab["targetId"],
        "label": tab["label"],
        "url": tab["url"],
        "title": tab["title"],
    }))
}

/// Closes a tab (the active one without `tabId`). Must return what Chrome's `handle_tab_close`
/// returns and refuse to close the last tab.
pub async fn tab_close(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let requested = cmd.get("tabId").and_then(Value::as_str);
    let tabs = tab_list(state).await?;
    let list = tabs["tabs"].as_array().cloned().unwrap_or_default();
    if list.len() <= 1 {
        return Err("Cannot close the last tab".to_string());
    }
    let active = session(state)?.top_context.clone();
    let context = match requested {
        Some(input) => resolve_tab_ref(&list, input)?,
        None => active.clone(),
    };
    let index = list
        .iter()
        .position(|t| t["targetId"].as_str() == Some(context.as_str()))
        .ok_or_else(|| {
            format!("No tab with context id `{context}`; run `agent-browser tab` to list open tabs")
        })?;
    let closed = list[index].clone();
    let successor = successor_tab(&list, index);

    {
        let ff = session(state)?;
        ff.bidi
            .send("browsingContext.close", json!({"context": context}))
            .await?;
        ff.tab_labels.remove(&context);
    }
    if let Some(successor) = successor.filter(|_| context == active) {
        let ff = session(state)?;
        let _ = ff
            .bidi
            .send("browsingContext.activate", json!({"context": successor}))
            .await;
        ff.context = successor.clone();
        ff.top_context = successor;
        ff.refs.clear();
    }
    if state
        .pending_dialog
        .as_ref()
        .and_then(|d| d.session_id.as_deref())
        == Some(context.as_str())
    {
        state.pending_dialog = None;
    }
    reset_refs(state);

    Ok(json!({
        "tabId": closed["tabId"],
        "targetId": closed["targetId"],
        "label": closed["label"],
        "closed": true,
    }))
}

/// Opens a new window. Must return what Chrome's `handle_window_new` returns.
pub async fn window_new(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let created = session(state)?
        .bidi
        .send("browsingContext.create", json!({"type": "window"}))
        .await?;
    let context = created["context"]
        .as_str()
        .ok_or("Firefox did not return a context id for the new window")?
        .to_string();
    let tab_id = session(state)?.tab_id(&context);

    {
        let ff = session(state)?;
        ff.bidi
            .send("browsingContext.activate", json!({"context": context}))
            .await?;
        ff.context = context.clone();
        ff.top_context = context.clone();
        ff.refs.clear();
    }
    reset_refs(state);
    let total = tab_count(state).await?;

    Ok(json!({
        "tabId": format_tab_id(tab_id),
        "total": total,
    }))
}

/// Drops refs and frame scope for the tab whose context just became active.
fn reset_refs(state: &mut DaemonState) {
    state.ref_map.begin_snapshot();
    state.active_iframe_sessions.clear();
    state.active_frame_id = None;
}

/// Number of top-level browsing contexts (tabs and windows).
async fn tab_count(state: &mut DaemonState) -> Result<usize, String> {
    let tree = session(state)?
        .bidi
        .send("browsingContext.getTree", json!({"maxDepth": 0}))
        .await?;
    Ok(tree["contexts"].as_array().map_or(0, Vec::len))
}

/// A context id has the shape of a UUID, which `TabRef`'s hex target-id heuristic does not accept.
fn looks_like_context_id(s: &str) -> bool {
    s.len() >= 32 && s.contains('-') && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn tab_context(tab: &Value) -> Result<String, String> {
    tab["targetId"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "Firefox reported a tab without a context id".to_string())
}

/// Resolves a `t<N>` stable id, a label, or a BiDi context id against `tab_list` output.
/// Labels win over context ids, matching Chrome's label-first handling of hex-shaped strings.
fn resolve_tab_ref(tabs: &[Value], input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err(
            "Empty tab reference; expected `t<N>`, a label, or a tab context id".to_string(),
        );
    }
    if let Ok(TabRef::Label(label)) = TabRef::parse(input) {
        if let Some(tab) = tabs
            .iter()
            .find(|t| t["label"].as_str() == Some(label.as_str()))
        {
            return tab_context(tab);
        }
    }
    let exact = tabs.iter().find(|t| {
        t["tabId"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(input))
            || t["targetId"]
                .as_str()
                .is_some_and(|id| id.eq_ignore_ascii_case(input))
    });
    if let Some(tab) = exact {
        return tab_context(tab);
    }
    if looks_like_context_id(input) {
        return Err(format!(
            "No tab with context id `{input}`; run `agent-browser tab` to list open tabs"
        ));
    }
    match TabRef::parse(input)? {
        TabRef::Id(id) => match tabs.iter().find(|t| {
            t["tabId"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case(&format_tab_id(id)))
        }) {
            Some(tab) => tab_context(tab),
            None => Err(format!(
                "Tab t{id} not found; run `agent-browser tab` to list open tabs"
            )),
        },
        TabRef::Label(label) => Err(format!(
            "No tab with label `{label}`; run `agent-browser tab` to list open tabs"
        )),
        TabRef::Target(id) => Err(format!(
            "No tab with id `{id}`; run `agent-browser tab` to list open tabs"
        )),
    }
}

/// The tab that becomes active after closing `closed_index`: the next tab to the right,
/// or the last remaining tab when the closed one was last, as Chrome does.
fn successor_tab(tabs: &[Value], closed_index: usize) -> Option<String> {
    let mut remaining = tabs.to_vec();
    if closed_index >= remaining.len() {
        return None;
    }
    remaining.remove(closed_index);
    let next = closed_index.min(remaining.len().checked_sub(1)?);
    remaining[next]["targetId"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(tab_id: u32, context: &str, label: Option<&str>) -> Value {
        json!({
            "tabId": format_tab_id(tab_id),
            "targetId": context,
            "label": label,
            "title": context,
            "url": "https://example.com/",
            "type": "page",
            "active": false,
        })
    }

    const UUID_A: &str = "8f1c2e5a-4b7d-4e2f-9a3c-1d2e3f4a5b6c";
    const UUID_B: &str = "b2d4f6a8-1c3e-4a5b-8d7f-9e0a1b2c3d4e";

    #[test]
    fn tab_refs_resolve_by_stable_id_label_and_context_id() {
        let tabs = vec![tab(1, "ctx-active", Some("docs")), tab(2, UUID_A, None)];
        assert_eq!(resolve_tab_ref(&tabs, "t1").unwrap(), "ctx-active");
        assert_eq!(resolve_tab_ref(&tabs, "T2").unwrap(), UUID_A);
        assert_eq!(resolve_tab_ref(&tabs, "docs").unwrap(), "ctx-active");
        assert_eq!(resolve_tab_ref(&tabs, UUID_A).unwrap(), UUID_A);
        assert_eq!(
            resolve_tab_ref(&tabs, &UUID_A.to_uppercase()).unwrap(),
            UUID_A
        );
    }

    #[test]
    fn stable_ids_do_not_shift_after_an_earlier_tab_closes() {
        let tabs = vec![tab(2, UUID_A, None), tab(3, "ctx-c", None)];
        assert_eq!(resolve_tab_ref(&tabs, "t2").unwrap(), UUID_A);
        assert_eq!(resolve_tab_ref(&tabs, "t3").unwrap(), "ctx-c");
        assert!(resolve_tab_ref(&tabs, "t1")
            .unwrap_err()
            .contains("Tab t1 not found"));
    }

    #[test]
    fn unknown_tab_refs_teach_without_a_browser() {
        let tabs = vec![tab(1, "ctx-a", Some("docs")), tab(2, UUID_A, None)];
        assert!(resolve_tab_ref(&tabs, "t9")
            .unwrap_err()
            .contains("Tab t9 not found"));
        assert!(resolve_tab_ref(&tabs, "other")
            .unwrap_err()
            .contains("No tab with label"));
        assert!(resolve_tab_ref(&tabs, UUID_B)
            .unwrap_err()
            .contains("No tab with context id"));
        assert!(resolve_tab_ref(&tabs, "3")
            .unwrap_err()
            .contains("positional integers are not accepted"));
        assert!(resolve_tab_ref(&tabs, "")
            .unwrap_err()
            .contains("Empty tab reference"));
    }

    #[test]
    fn closing_the_active_tab_selects_the_neighbor_chrome_would() {
        let tabs = vec![tab(1, "a", None), tab(2, "b", None), tab(3, "c", None)];
        assert_eq!(successor_tab(&tabs, 0).as_deref(), Some("b"));
        assert_eq!(successor_tab(&tabs, 1).as_deref(), Some("c"));
        assert_eq!(successor_tab(&tabs, 2).as_deref(), Some("b"));
        assert_eq!(successor_tab(&tabs[..1], 0), None);
    }
}
