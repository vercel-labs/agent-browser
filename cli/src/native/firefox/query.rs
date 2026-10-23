//! Firefox read/query commands, semantic locators, frame switching and eval handles.

use serde_json::{json, Value};

use super::actions::session;
use super::bidi::BidiClient;
use super::target::{resolve_target, Target};
use crate::native::actions::{self as chrome, DaemonState};
use crate::native::element::{
    build_count_elements_js, parse_ref, ELEMENT_BOUNDING_BOX_JS, ELEMENT_CHECKED_JS,
    ELEMENT_ENABLED_JS, ELEMENT_INNER_HTML_JS, ELEMENT_INNER_TEXT_JS, ELEMENT_INPUT_VALUE_JS,
    ELEMENT_STYLES_ALL_JS, ELEMENT_TEXT_JS, ELEMENT_VISIBLE_JS,
};

const TEMP_NODE_ID: i64 = -1;

const SHADOW_ROOTS_JS: &str = r#"function() {
    const out = [];
    const walk = (root) => {
        for (const el of root.querySelectorAll('*')) {
            if (el.shadowRoot) { out.push(el.shadowRoot); walk(el.shadowRoot); }
        }
    };
    walk(document);
    return out;
}"#;

const NAME_FILTER_JS: &str = r#"function(name, nodes) {
  const norm = s => (s || '').replace(/\s+/g, ' ').trim().toLowerCase();
  const want = norm(name);
  const nameOf = el => {
    const lb = el.getAttribute('aria-labelledby');
    if (lb) {
      const root = el.getRootNode();
      const t = lb.trim().split(/\s+/).map(id => { const r = root.getElementById ? root.getElementById(id) : null; return r ? r.textContent : ''; }).join(' ');
      if (norm(t)) return t;
    }
    const al = el.getAttribute('aria-label');
    if (al && norm(al)) return al;
    if (el.labels && el.labels.length) return Array.from(el.labels).map(l => l.textContent).join(' ');
    const alt = el.getAttribute('alt');
    if (alt && norm(alt)) return alt;
    if (el.tagName === 'INPUT' && /^(button|submit|reset)$/i.test(el.type) && el.value) return el.value;
    return el.textContent || el.getAttribute('title') || '';
  };
  return nodes.find(el => norm(nameOf(el)).includes(want)) || null;
}"#;

const FIND_TEXT_JS: &str = r#"function(value, exact) {
    for (const el of document.querySelectorAll('*')) {
        if (el.children.length === 0 && (exact ? el.textContent.trim() === value : el.textContent.includes(value))) return el;
    }
    return null;
}"#;

const FIND_LABEL_JS: &str = r#"function(value, exact) {
    const matches = (s) => !!s && (exact ? s.trim() === value : s.includes(value));
    const label = Array.from(document.querySelectorAll('label')).find(el => matches(el.textContent));
    if (label) {
        const forId = label.getAttribute('for');
        const target = forId ? document.getElementById(forId) : label.querySelector('input,select,textarea');
        if (target) return target;
    }
    const aria = Array.from(document.querySelectorAll('[aria-label]')).find(el => matches(el.getAttribute('aria-label')));
    if (aria) return aria;
    return Array.from(document.querySelectorAll('[aria-labelledby]')).find(el => matches(
        el.getAttribute('aria-labelledby').split(/\s+/)
            .map(id => { const r = document.getElementById(id); return r ? r.textContent : ''; })
            .join(' ')
    )) || null;
}"#;

fn selector(cmd: &Value) -> Result<&str, String> {
    cmd.get("selector")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing 'selector' parameter".to_string())
}

fn target_ref(target: &Target) -> Value {
    json!({"sharedId": target.shared_id})
}

/// Runs a Chrome element function body with `this` set to the resolved node.
async fn call_on_target(bidi: &BidiClient, target: &Target, js: &str) -> Result<Value, String> {
    bidi.call_function(&target.context, js, Some(target_ref(target)), Vec::new())
        .await
}

/// The four edges of a node, read through its handle.
pub(crate) async fn element_rect(
    bidi: &BidiClient,
    target: &Target,
) -> Result<Option<Value>, String> {
    let value = call_on_target(bidi, target, ELEMENT_BOUNDING_BOX_JS).await?;
    Ok((!value.is_null()).then_some(value))
}

async fn resolve(state: &mut DaemonState, cmd: &Value) -> Result<Target, String> {
    let sel = selector(cmd)?;
    resolve_target(state, sel).await
}

async fn read_text(
    cmd: &Value,
    state: &mut DaemonState,
    js: &str,
) -> Result<(String, String), String> {
    let target = resolve(state, cmd).await?;
    let ff = session(state)?;
    let value = call_on_target(&ff.bidi, &target, js).await?;
    let origin = ff.url().await.unwrap_or_default();
    Ok((value.as_str().unwrap_or("").to_string(), origin))
}

/// `get text`: `{text, origin}`.
pub async fn gettext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (text, origin) = read_text(cmd, state, ELEMENT_TEXT_JS).await?;
    Ok(json!({ "text": text, "origin": origin }))
}

/// `get attr`: `{value, origin}`, a missing attribute reading as `null`.
pub async fn getattribute(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let attribute = cmd
        .get("attribute")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing 'attribute' parameter".to_string())?;
    let target = resolve(state, cmd).await?;
    let ff = session(state)?;
    let js = format!(
        "function() {{ return this.getAttribute({}); }}",
        serde_json::to_string(attribute).unwrap_or_default()
    );
    let value = call_on_target(&ff.bidi, &target, &js).await?;
    let origin = ff.url().await.unwrap_or_default();
    Ok(json!({ "value": value, "origin": origin }))
}

/// `get html` (innerHTML): `{html}`.
pub async fn innerhtml(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (html, _) = read_text(cmd, state, ELEMENT_INNER_HTML_JS).await?;
    Ok(json!({ "html": html }))
}

/// `innertext`: `{text}`.
pub async fn innertext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (text, _) = read_text(cmd, state, ELEMENT_INNER_TEXT_JS).await?;
    Ok(json!({ "text": text }))
}

/// `get value`: `{value}`.
pub async fn inputvalue(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (value, _) = read_text(cmd, state, ELEMENT_INPUT_VALUE_JS).await?;
    Ok(json!({ "value": value }))
}

/// `get count`: `{count, selector}`.
pub async fn count(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let sel = selector(cmd)?;
    let ff = session(state)?;
    let value = ff.eval(&build_count_elements_js(sel)).await?;
    Ok(json!({ "count": value.as_i64().unwrap_or(0), "selector": sel }))
}

/// `get box`: the `getBoundingClientRect` fields as Chrome returns them.
pub async fn boundingbox(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let target = resolve(state, cmd).await?;
    let ff = session(state)?;
    let value = call_on_target(&ff.bidi, &target, ELEMENT_BOUNDING_BOX_JS).await?;
    if value.is_null() {
        return Err(format!(
            "Could not get bounding box for: {}",
            selector(cmd)?
        ));
    }
    Ok(value)
}

fn styles_js(properties: &[String]) -> String {
    let props = serde_json::to_string(properties).unwrap_or_else(|_| "[]".to_string());
    format!(
        r#"function() {{
            const s = window.getComputedStyle(this);
            const props = {props};
            const result = {{}};
            for (const p of props) result[p] = s.getPropertyValue(p);
            return result;
        }}"#
    )
}

/// `get styles`: `{styles}` for all properties or the requested ones.
pub async fn styles(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let properties = cmd.get("properties").and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect::<Vec<_>>()
    });
    let target = resolve(state, cmd).await?;
    let ff = session(state)?;
    let js = match &properties {
        Some(props) => styles_js(props),
        None => ELEMENT_STYLES_ALL_JS.to_string(),
    };
    let styles = call_on_target(&ff.bidi, &target, &js).await?;
    Ok(json!({ "styles": styles }))
}

async fn read_bool(
    cmd: &Value,
    state: &mut DaemonState,
    js: &str,
    default: bool,
) -> Result<(bool, String), String> {
    let target = resolve(state, cmd).await?;
    let ff = session(state)?;
    let value = call_on_target(&ff.bidi, &target, js).await?;
    let origin = ff.url().await.unwrap_or_default();
    Ok((value.as_bool().unwrap_or(default), origin))
}

/// `is visible`: `{visible, origin}`.
pub async fn isvisible(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (visible, origin) = read_bool(cmd, state, ELEMENT_VISIBLE_JS, false).await?;
    Ok(json!({ "visible": visible, "origin": origin }))
}

/// `is enabled`: `{enabled, origin}`.
pub async fn isenabled(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (enabled, origin) = read_bool(cmd, state, ELEMENT_ENABLED_JS, true).await?;
    Ok(json!({ "enabled": enabled, "origin": origin }))
}

/// `is checked`: `{checked, origin}`.
pub async fn ischecked(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let (checked, origin) = read_bool(cmd, state, ELEMENT_CHECKED_JS, false).await?;
    Ok(json!({ "checked": checked, "origin": origin }))
}

/// `find <selector>`: the same element summary Chrome returns.
pub async fn find(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let sel = selector(cmd)?;
    let js = format!(
        r#"(() => {{
            const els = document.querySelectorAll({});
            return Array.from(els).map((el, i) => ({{
                index: i,
                tagName: el.tagName.toLowerCase(),
                text: el.textContent?.trim().substring(0, 100) || '',
                visible: el.offsetWidth > 0 && el.offsetHeight > 0,
            }}));
        }})()"#,
        serde_json::to_string(sel).unwrap_or_default()
    );
    let elements = session(state)?.eval(&js).await?;
    Ok(json!({ "elements": elements, "selector": sel }))
}

/// The BiDi locator for a role and optional accessible name.
fn role_locator(role: &str, name: Option<&str>) -> Value {
    let mut value = json!({ "role": role });
    if let Some(name) = name {
        value["name"] = json!(name);
    }
    json!({ "type": "accessibility", "value": value })
}

/// The BiDi locator for the non-role semantic strategies.
fn semantic_locator(strategy: &str, value: &str) -> Value {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    let css = |selector: String| json!({ "type": "css", "value": selector });
    match strategy {
        "placeholder" => css(format!(
            "input[placeholder={quoted}], textarea[placeholder={quoted}]"
        )),
        "alttext" => css(format!("[alt={quoted}]")),
        "title" => css(format!("[title={quoted}]")),
        _ => css(format!("[data-testid={quoted}]")),
    }
}

/// Runs `browsingContext.locateNodes` and returns its nodes, optionally scoped to `startNodes`.
async fn locate_nodes(
    bidi: &BidiClient,
    context: &str,
    locator: Value,
    start_nodes: Option<Vec<Value>>,
) -> Result<Vec<Value>, String> {
    let mut params = json!({
        "context": context,
        "locator": locator,
        "serializationOptions": { "maxDomDepth": 0 },
    });
    if let Some(nodes) = start_nodes {
        params["startNodes"] = json!(nodes);
    }
    let found = bidi.send("browsingContext.locateNodes", params).await?;
    Ok(found["nodes"].as_array().cloned().unwrap_or_default())
}

/// Searches the document, then every open shadow root, returning the first match.
async fn locate_deep(
    bidi: &BidiClient,
    context: &str,
    locator: Value,
) -> Result<Option<Value>, String> {
    if let Some(node) = locate_nodes(bidi, context, locator.clone(), None)
        .await?
        .into_iter()
        .next()
    {
        return Ok(Some(node));
    }
    let roots = bidi
        .call_function(context, SHADOW_ROOTS_JS, None, vec![])
        .await?;
    let roots = roots.as_array().cloned().unwrap_or_default();
    if roots.is_empty() {
        return Ok(None);
    }
    Ok(locate_nodes(bidi, context, locator, Some(roots))
        .await?
        .into_iter()
        .next())
}

/// Every match in the document and in open shadow roots.
async fn locate_all(
    bidi: &BidiClient,
    context: &str,
    locator: Value,
) -> Result<Vec<Value>, String> {
    let mut nodes = locate_nodes(bidi, context, locator.clone(), None).await?;
    let roots = bidi
        .call_function(context, SHADOW_ROOTS_JS, None, vec![])
        .await?;
    let roots = roots.as_array().cloned().unwrap_or_default();
    if !roots.is_empty() {
        nodes.extend(locate_nodes(bidi, context, locator, Some(roots)).await?);
    }
    Ok(nodes)
}

/// Finds a role match whose name contains `name` ignoring case and extra whitespace, as Chrome does without `exact`.
async fn locate_role_loose(
    bidi: &BidiClient,
    context: &str,
    role: &str,
    name: &str,
) -> Result<Option<Value>, String> {
    if let Some(node) = locate_deep(bidi, context, role_locator(role, Some(name))).await? {
        return Ok(Some(node));
    }
    let candidates = locate_all(bidi, context, role_locator(role, None)).await?;
    if candidates.is_empty() {
        return Ok(None);
    }
    let found = bidi
        .call_function(
            context,
            NAME_FILTER_JS,
            None,
            vec![json!(name), json!(candidates)],
        )
        .await?;
    Ok(found["sharedId"].is_string().then_some(found))
}

fn target_from_node(context: &str, node: &Value) -> Result<Target, String> {
    let shared_id = node["sharedId"]
        .as_str()
        .ok_or("Firefox returned a node without a sharedId")?
        .to_string();
    Ok(Target {
        context: context.to_string(),
        shared_id,
    })
}

async fn semantic_target(
    cmd: &Value,
    state: &mut DaemonState,
    strategy: &str,
    param: &str,
) -> Result<Target, String> {
    let value = cmd
        .get(param)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Missing '{param}' parameter"))?;
    let exact = cmd.get("exact").and_then(Value::as_bool).unwrap_or(false);
    let ff = session(state)?;
    let context = ff.context.clone();
    let node = if matches!(strategy, "text" | "label") {
        let js = if strategy == "text" {
            FIND_TEXT_JS
        } else {
            FIND_LABEL_JS
        };
        let found = ff
            .bidi
            .call_function(&context, js, None, vec![json!(value), json!(exact)])
            .await?;
        found["sharedId"].is_string().then_some(found)
    } else {
        let locator = semantic_locator(strategy, value);
        locate_deep(&ff.bidi, &context, locator).await?
    };
    let Some(node) = node.as_ref() else {
        return Err(format!("No element found by {strategy} '{value}'"));
    };
    let target = target_from_node(&context, node)?;
    Ok(target)
}

async fn run_subaction(
    cmd: &Value,
    state: &mut DaemonState,
    target: &Target,
) -> Result<Value, String> {
    let subaction = cmd
        .get("subaction")
        .and_then(Value::as_str)
        .unwrap_or("click");
    if subaction == "text" {
        let ff = session(state)?;
        let text = call_on_target(&ff.bidi, target, ELEMENT_TEXT_JS).await?;
        return Ok(json!({ "text": text.as_str().unwrap_or("") }));
    }
    let ref_num = state.ref_map.next_ref_num();
    let ref_id = format!("e{ref_num}");
    let key = (target.context.clone(), target.shared_id.clone());
    let existing = session(state)?.refs.get(&key).copied();
    let node_id = existing.unwrap_or(TEMP_NODE_ID);
    state
        .ref_map
        .add(ref_id.clone(), Some(node_id), "", "", None);
    state.ref_map.set_next_ref_num(ref_num + 1);
    if existing.is_none() {
        session(state)?.refs.insert(key.clone(), TEMP_NODE_ID);
    }
    let mut sub = cmd.clone();
    sub["selector"] = json!(format!("@{ref_id}"));
    let result = match subaction {
        "click" => super::interact::click(&sub, state).await,
        "hover" => super::interact::hover(&sub, state).await,
        "fill" => super::forms::fill(&sub, state).await,
        "check" => super::forms::check(&sub, state).await,
        other => Err(format!(
            "Unknown action '{other}' for find. Valid actions: click, fill, check, hover, text."
        )),
    };
    state.ref_map.remove(&ref_id);
    if existing.is_none() {
        if let Ok(ff) = session(state) {
            ff.refs.remove(&key);
        }
    }
    result
}

/// `find role <role>`: resolve through the accessibility locator, then run the subaction.
pub async fn getbyrole(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let role = cmd
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing 'role' parameter".to_string())?;
    let name = cmd.get("name").and_then(Value::as_str);
    let exact = cmd.get("exact").and_then(Value::as_bool).unwrap_or(false);
    let ff = session(state)?;
    let context = ff.context.clone();
    let node = match name {
        Some(n) if !exact => locate_role_loose(&ff.bidi, &context, role, n).await?,
        _ => locate_deep(&ff.bidi, &context, role_locator(role, name)).await?,
    };
    let Some(node) = node.as_ref() else {
        return Err(format!(
            "No element found: {}",
            chrome::build_role_selector(role, name, exact)
        ));
    };
    let target = target_from_node(&context, node)?;
    run_subaction(cmd, state, &target).await
}

async fn getby(
    cmd: &Value,
    state: &mut DaemonState,
    strategy: &str,
    param: &str,
) -> Result<Value, String> {
    let target = semantic_target(cmd, state, strategy, param).await?;
    run_subaction(cmd, state, &target).await
}

/// `find text <text>`.
pub async fn getbytext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    getby(cmd, state, "text", "text").await
}

/// `find label <label>`.
pub async fn getbylabel(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    getby(cmd, state, "label", "label").await
}

/// `find placeholder <placeholder>`.
pub async fn getbyplaceholder(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    getby(cmd, state, "placeholder", "placeholder").await
}

/// `find alt <text>`.
pub async fn getbyalttext(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    getby(cmd, state, "alttext", "text").await
}

/// `find title <text>`.
pub async fn getbytitle(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    getby(cmd, state, "title", "text").await
}

/// `find testid <id>`.
pub async fn getbytestid(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    getby(cmd, state, "testid", "testId").await
}

fn nth_js(selector: &str, index: i64) -> String {
    format!(
        r#"(() => {{
            const els = document.querySelectorAll({sel});
            const idx = {index} < 0 ? els.length + {index} : {index};
            if (idx < 0 || idx >= els.length) return null;
            return els[idx];
        }})()"#,
        sel = serde_json::to_string(selector).unwrap_or_default(),
        index = index,
    )
}

/// `find nth <index> <selector>` and the `first`/`last` aliases.
pub async fn nth(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let sel = selector(cmd)?;
    let index = cmd
        .get("index")
        .and_then(Value::as_i64)
        .ok_or_else(|| "Missing 'index' parameter".to_string())?;
    let ff = session(state)?;
    let value = ff
        .bidi
        .send(
            "script.evaluate",
            json!({
                "expression": nth_js(sel, index),
                "target": {"context": ff.context},
                "awaitPromise": true,
                "resultOwnership": "root",
            }),
        )
        .await?;
    if value["type"] == "exception" {
        return Err(format!(
            "Evaluation error: {}",
            value["exceptionDetails"]["text"]
                .as_str()
                .unwrap_or("script exception")
        ));
    }
    if value["result"]["sharedId"].is_null() {
        return Err(format!(
            "No element at index {} for selector '{}'",
            index, sel
        ));
    }
    let target = target_from_node(&ff.context, &value["result"])?;
    run_subaction(cmd, state, &target).await
}

const FRAME_WINDOW_JS: &str = r#"function() {
    if (this.tagName !== 'IFRAME' && this.tagName !== 'FRAME') return null;
    const w = this.contentWindow;
    if (!w) return null;
    return { label: this.name || this.id || this.src || '', window: w };
}"#;

/// `frame <selector>`: make the iframe's browsing context active for later commands.
pub async fn frame(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let sel = selector(cmd)?;
    let from_ref = parse_ref(sel).is_some();
    let target = resolve_target(state, sel).await?;
    let ff = session(state)?;
    let value = call_on_target(&ff.bidi, &target, FRAME_WINDOW_JS).await?;
    let window = value["window"].as_str().unwrap_or_default();
    if window.is_empty() {
        return Err(if from_ref {
            "Ref does not point to an iframe element".to_string()
        } else {
            "Could not find frame for selector".to_string()
        });
    }
    let label = value["label"]
        .as_str()
        .filter(|label| !label.is_empty())
        .unwrap_or(sel)
        .to_string();
    ff.context = window.to_string();
    state.active_frame_id = Some(window.to_string());
    Ok(json!({ "frame": label }))
}

/// `frame main`: return to the active top-level tab.
pub async fn mainframe(state: &mut DaemonState) -> Result<Value, String> {
    let ff = session(state)?;
    ff.context = ff.top_context.clone();
    state.active_frame_id = None;
    Ok(json!({ "frame": "main" }))
}

/// `evalhandle`: run the script and return its BiDi remote handle.
pub async fn evalhandle(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let script = cmd
        .get("script")
        .and_then(Value::as_str)
        .ok_or_else(|| "Missing 'script' parameter".to_string())?;
    let ff = session(state)?;
    let value = ff
        .bidi
        .send(
            "script.evaluate",
            json!({
                "expression": script,
                "target": {"context": ff.context},
                "awaitPromise": true,
                "resultOwnership": "root",
            }),
        )
        .await?;
    if value["type"] == "exception" {
        let details = &value["exceptionDetails"];
        let message = details["exception"]["value"]
            .as_str()
            .or(details["text"].as_str())
            .unwrap_or("script exception");
        return Err(format!("Evaluation error: {message}"));
    }
    Ok(json!({
        "handle": value["result"]["handle"].as_str().unwrap_or(""),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{Arc, Mutex};
    use tokio_tungstenite::tungstenite::Message;

    async fn mock_once(reply: Value, requests: Arc<Mutex<Vec<Value>>>) -> BidiClient {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            let message = ws.next().await.unwrap().unwrap();
            let request: Value = serde_json::from_str(&message.into_text().unwrap()).unwrap();
            let id = request["id"].clone();
            requests.lock().unwrap().push(request);
            ws.send(Message::Text(
                json!({"type": "success", "id": id, "result": reply}).to_string(),
            ))
            .await
            .unwrap();
        });
        BidiClient::connect(&url).await.unwrap()
    }

    #[test]
    fn role_locator_uses_the_accessibility_locator() {
        assert_eq!(
            role_locator("button", Some("Save")),
            json!({
                "type": "accessibility",
                "value": {"role": "button", "name": "Save"},
            })
        );
        assert_eq!(
            role_locator("textbox", None),
            json!({"type": "accessibility", "value": {"role": "textbox"}})
        );
    }

    #[test]
    fn attribute_locators_are_css() {
        assert_eq!(
            semantic_locator("placeholder", "Email")["value"],
            r#"input[placeholder="Email"], textarea[placeholder="Email"]"#
        );
        assert_eq!(
            semantic_locator("alttext", "Logo")["value"],
            r#"[alt="Logo"]"#
        );
        assert_eq!(
            semantic_locator("title", "Help")["value"],
            r#"[title="Help"]"#
        );
        assert_eq!(
            semantic_locator("testid", "save")["value"],
            r#"[data-testid="save"]"#
        );
    }

    #[test]
    fn nth_supports_negative_indexes() {
        let js = nth_js("#row", -1);
        assert!(js.contains("els.length + -1"), "{js}");
        assert!(js.contains(r##"querySelectorAll("#row")"##), "{js}");
    }

    #[test]
    fn styles_js_requests_only_the_named_properties() {
        let js = styles_js(&["color".to_string(), "display".to_string()]);
        assert!(js.contains(r#"["color","display"]"#), "{js}");
        assert!(js.contains("getPropertyValue"), "{js}");
    }

    #[tokio::test]
    async fn locate_nodes_sends_the_locator_and_start_nodes() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bidi = mock_once(json!({"nodes": [{"sharedId": "node-1"}]}), requests.clone()).await;

        let nodes = locate_nodes(
            &bidi,
            "ctx",
            semantic_locator("title", "Save"),
            Some(vec![json!({"sharedId": "shadow-root"})]),
        )
        .await
        .unwrap();

        assert_eq!(nodes[0]["sharedId"], "node-1");
        let request = requests.lock().unwrap()[0].clone();
        assert_eq!(request["method"], "browsingContext.locateNodes");
        assert_eq!(request["params"]["context"], "ctx");
        assert_eq!(
            request["params"]["startNodes"],
            json!([{"sharedId": "shadow-root"}])
        );
        assert_eq!(request["params"]["locator"]["type"], "css");
        assert_eq!(request["params"]["serializationOptions"]["maxDomDepth"], 0);
    }

    #[tokio::test]
    async fn element_rect_runs_the_shared_box_js_on_the_handle() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bidi = mock_once(
            json!({"type": "success", "result": {"type": "object", "value": [
                ["x", {"type": "number", "value": 3}],
                ["y", {"type": "number", "value": 4}],
                ["width", {"type": "number", "value": 30}],
                ["height", {"type": "number", "value": 40}]
            ]}}),
            requests.clone(),
        )
        .await;
        let target = Target {
            context: "ctx".into(),
            shared_id: "node-9".into(),
        };

        let rect = element_rect(&bidi, &target).await.unwrap().unwrap();
        assert_eq!(rect, json!({"x": 3, "y": 4, "width": 30, "height": 40}));

        let request = requests.lock().unwrap()[0].clone();
        assert_eq!(request["method"], "script.callFunction");
        assert_eq!(request["params"]["this"], json!({"sharedId": "node-9"}));
        assert_eq!(
            request["params"]["functionDeclaration"],
            json!(ELEMENT_BOUNDING_BOX_JS)
        );
    }

    #[tokio::test]
    async fn mainframe_returns_to_the_active_tab() {
        let mut state = DaemonState::new();
        let (mut ff, _tx, _) = super::super::session::tests::fake_session().await;
        ff.context = "ctx-frame".into();
        state.firefox = Some(ff);
        state.active_frame_id = Some("ctx-frame".into());

        assert_eq!(
            mainframe(&mut state).await.unwrap(),
            json!({"frame": "main"})
        );
        assert_eq!(state.firefox.as_ref().unwrap().context, "ctx-active");
        assert!(state.active_frame_id.is_none());
    }

    #[tokio::test]
    async fn null_rect_reports_no_box() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let bidi = mock_once(
            json!({"type": "success", "result": {"type": "null"}}),
            requests,
        )
        .await;
        let target = Target {
            context: "ctx".into(),
            shared_id: "gone".into(),
        };
        assert!(element_rect(&bidi, &target).await.unwrap().is_none());
    }
}
