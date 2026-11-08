//! Firefox tab and window commands. Tab ids are stable `t<N>` ids over BiDi browsing contexts.

use serde_json::{json, Value};
use std::time::Duration;

use super::bidi::{remote_to_json, BidiClient};
use super::daemon::{reset_page, session};
use super::navigation::{self, Load, Wait};
use super::session::FirefoxSession;
use crate::native::actions::DaemonState;
use crate::native::browser::{format_tab_id, is_valid_label, TabRef};

/// Opens a tab and makes it active. Must return what Chrome's `handle_tab_new` returns,
/// honouring `url` and `label`.
pub async fn tab_new(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let url = cmd.get("url").and_then(Value::as_str);
    let label = cmd.get("label").and_then(Value::as_str);
    if let Some(label) = label {
        validate_tab_label(session(state)?, label)?;
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
    activate(state, &context).await?;
    if let Some(label) = label {
        session(state)?
            .tab_labels
            .insert(context.clone(), label.to_string());
    }

    let landing = match url {
        Some(url) => {
            navigation::load(
                state,
                Load::Url {
                    url,
                    wait: Wait::Complete,
                },
            )
            .await?
        }
        None => navigation::Landing {
            context: context.clone(),
            url: "about:blank".to_string(),
            prompt: false,
        },
    };
    let total = tab_count(session(state)?).await?;

    let mut result = json!({
        "tabId": format_tab_id(tab_id),
        "targetId": context,
        "label": label,
        "url": landing.url,
        "total": total,
    });
    if landing.prompt {
        result["dialogOpened"] = json!(true);
    }
    Ok(result)
}

/// Fails when `label` is malformed, uses tab-id syntax, or another tab already holds it.
fn validate_tab_label(ff: &FirefoxSession, label: &str) -> Result<(), String> {
    if !is_valid_label(label) {
        return Err(format!(
            "Invalid tab label `{label}`; labels must start with a letter and contain only \
             letters, digits, `-`, and `_`"
        ));
    }
    if !matches!(TabRef::parse(label), Ok(TabRef::Label(_))) {
        return Err(format!(
            "Tab label `{label}` uses syntax reserved for tab IDs"
        ));
    }
    if ff.tab_labels.values().any(|existing| existing == label) {
        return Err(format!(
            "Label `{label}` is already used by another tab; labels must be unique within a \
             session"
        ));
    }
    Ok(())
}

/// Makes `context` the active tab and drops the page state that belonged to the old one.
async fn activate(state: &mut DaemonState, context: &str) -> Result<(), String> {
    let ff = session(state)?;
    ff.bidi
        .send("browsingContext.activate", json!({"context": context}))
        .await?;
    ff.context = context.to_string();
    reset_page(state);
    Ok(())
}

/// The title of `context`, or null when the tab cannot answer before the deadline.
async fn tab_title(bidi: &BidiClient, context: &str) -> Value {
    bidi.send_timeout(
        "script.callFunction",
        json!({
            "functionDeclaration": "() => document.title",
            "target": {"context": context},
            "arguments": [],
            "awaitPromise": true,
            "resultOwnership": "root",
        }),
        Duration::from_millis(250),
    )
    .await
    .map(|result| remote_to_json(&result["result"]))
    .unwrap_or(Value::Null)
}

/// Lists top-level tabs as `{tabs: [{tabId, targetId, label, title, url, type, active}]}`.
pub async fn tab_list(state: &mut DaemonState) -> Result<Value, String> {
    let tree = session(state)?
        .bidi
        .send("browsingContext.getTree", json!({"maxDepth": 0}))
        .await?;
    let contexts: Vec<Value> = tree["contexts"].as_array().cloned().unwrap_or_default();
    let titles = {
        let bidi = &session(state)?.bidi;
        futures_util::future::join_all(
            contexts
                .iter()
                .map(|c| tab_title(bidi, c["context"].as_str().unwrap_or_default())),
        )
        .await
    };
    let mut tabs = Vec::with_capacity(contexts.len());
    for (c, title) in contexts.iter().zip(titles) {
        let context = c["context"].as_str().unwrap_or_default().to_string();
        let tab_id = session(state)?.tab_id(&context);
        let ff = session(state)?;
        let label = ff.tab_labels.get(&context).cloned();
        let active = context == ff.context;
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
/// `handle_tab_switch` returns and drop refs and frame scope along with the switch.
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

    activate(state, &context).await?;

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
    let context = match requested {
        Some(input) => resolve_tab_ref(&list, input)?,
        None => session(state)?.context.clone(),
    };
    let index = list
        .iter()
        .position(|t| t["targetId"].as_str() == Some(context.as_str()))
        .ok_or_else(|| {
            format!("No tab with context id `{context}`; run `agent-browser tab` to list open tabs")
        })?;
    let closed = list[index].clone();
    let successor = successor_tab(&list, index);

    session(state)?
        .bidi
        .send("browsingContext.close", json!({"context": context}))
        .await?;
    let user_context = session(state)?.window_contexts.remove(&context);
    let was_active = super::daemon::forget_context(state, &context)?;
    if let Some(user_context) = user_context {
        let still_open = session(state)?
            .window_contexts
            .values()
            .any(|other| *other == user_context);
        if !still_open {
            let _ = session(state)?
                .bidi
                .send(
                    "browser.removeUserContext",
                    json!({"userContext": user_context}),
                )
                .await;
        }
    }
    if was_active {
        match successor {
            Some(successor) => {
                let _ = activate(state, &successor).await;
            }
            None => reset_page(state),
        }
    }

    Ok(json!({
        "tabId": closed["tabId"],
        "targetId": closed["targetId"],
        "label": closed["label"],
        "closed": true,
    }))
}

/// Opens a new window. Must return what Chrome's `handle_window_new` returns.
pub async fn window_new(_cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let user_context = session(state)?
        .bidi
        .send("browser.createUserContext", json!({}))
        .await?
        .get("userContext")
        .and_then(Value::as_str)
        .ok_or("Firefox did not return a user context for the new window")?
        .to_string();
    let created = session(state)?
        .bidi
        .send(
            "browsingContext.create",
            json!({"type": "window", "userContext": user_context}),
        )
        .await;
    let created = match created {
        Ok(created) => created,
        Err(error) => {
            let _ = session(state)?
                .bidi
                .send(
                    "browser.removeUserContext",
                    json!({"userContext": user_context}),
                )
                .await;
            return Err(error);
        }
    };
    let context = created["context"]
        .as_str()
        .ok_or("Firefox did not return a context id for the new window")?
        .to_string();
    let ff = session(state)?;
    ff.window_contexts.insert(context.clone(), user_context);
    let tab_id = ff.tab_id(&context);
    activate(state, &context).await?;
    let total = tab_count(session(state)?).await?;

    Ok(json!({
        "tabId": format_tab_id(tab_id),
        "total": total,
    }))
}

/// Number of top-level browsing contexts (tabs and windows).
pub(crate) async fn tab_count(ff: &FirefoxSession) -> Result<usize, String> {
    let tree = ff
        .bidi
        .send("browsingContext.getTree", json!({"maxDepth": 0}))
        .await?;
    tree["contexts"]
        .as_array()
        .map(Vec::len)
        .ok_or_else(|| "Firefox did not return browsing contexts".to_string())
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
    use crate::native::firefox::session::tests::fake_session_with_navigation_prompt;

    #[tokio::test]
    async fn tab_new_returns_with_a_pending_navigation_prompt() {
        let url = "https://example.com/confirm";
        let tree = json!({"contexts": [
            {"context": "ctx-new", "url": url},
            {"context": "ctx-active", "url": "about:blank"}
        ]});
        let (ff, _events, _) = fake_session_with_navigation_prompt(vec![tree], true).await;
        let mut state = DaemonState::new();
        state.firefox = Some(ff);
        state.active_frame_id = Some("old-frame".into());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            tab_new(&json!({"url": url, "label": "new"}), &mut state),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["targetId"], "ctx-new");
        assert_eq!(result["url"], url);
        assert_eq!(result["dialogOpened"], true);
        assert_eq!(result["total"], 2);
        assert_eq!(state.firefox.as_ref().unwrap().context, "ctx-new");
        assert_eq!(state.firefox.as_ref().unwrap().tab_labels["ctx-new"], "new");
        assert!(state.active_frame_id.is_none());
        state.drain_firefox_events();
        let dialog = state.pending_dialog.as_ref().unwrap();
        assert_eq!(dialog.dialog_type, "confirm");
        assert_eq!(dialog.session_id.as_deref(), Some("ctx-new"));
    }

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

    #[tokio::test]
    async fn tab_labels_reject_stable_id_syntax_and_reuse() {
        let (ff, _events, _) = super::super::session::tests::fake_session().await;
        assert!(validate_tab_label(&ff, "docs").is_ok());
        for label in ["t1", "T1"] {
            assert_eq!(
                validate_tab_label(&ff, label).unwrap_err(),
                format!("Tab label `{label}` uses syntax reserved for tab IDs")
            );
        }
        let mut ff = ff;
        ff.tab_labels.insert("ctx-a".into(), "docs".into());
        assert!(validate_tab_label(&ff, "docs")
            .unwrap_err()
            .contains("already used by another tab"));
        assert!(validate_tab_label(&ff, "9docs")
            .unwrap_err()
            .contains("Invalid tab label"));
    }

    #[tokio::test]
    async fn busy_tab_title_lookup_is_bounded() {
        use futures_util::StreamExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let _ = ws.next().await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        });
        let bidi = BidiClient::connect(&url).await.unwrap();
        let title = tokio::time::timeout(Duration::from_millis(500), tab_title(&bidi, "ctx"))
            .await
            .unwrap();
        assert_eq!(title, Value::Null);
        drop(bidi);
        server.abort();
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
