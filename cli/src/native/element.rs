use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::*;

const CASCADE_DEFAULT_PROPERTIES: &[&str] = &[
    "display",
    "position",
    "top",
    "right",
    "bottom",
    "left",
    "z-index",
    "width",
    "height",
    "margin",
    "padding",
    "font-size",
    "font-weight",
    "line-height",
    "color",
    "background-color",
    "border",
    "border-radius",
    "opacity",
    "transform",
];

#[derive(Debug, Clone)]
pub struct RefEntry {
    pub backend_node_id: Option<i64>,
    pub role: String,
    pub name: String,
    pub nth: Option<usize>,
    pub selector: Option<String>,
    pub frame_id: Option<String>,
}

pub struct RefMap {
    map: HashMap<String, RefEntry>,
    next_ref: usize,
}

impl RefMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next_ref: 1,
        }
    }

    pub fn add(
        &mut self,
        ref_id: String,
        backend_node_id: Option<i64>,
        role: &str,
        name: &str,
        nth: Option<usize>,
    ) {
        self.add_with_frame(ref_id, backend_node_id, role, name, nth, None);
    }

    pub fn add_with_frame(
        &mut self,
        ref_id: String,
        backend_node_id: Option<i64>,
        role: &str,
        name: &str,
        nth: Option<usize>,
        frame_id: Option<&str>,
    ) {
        self.map.insert(
            ref_id,
            RefEntry {
                backend_node_id,
                role: role.to_string(),
                name: name.to_string(),
                nth,
                selector: None,
                frame_id: frame_id.map(|s| s.to_string()),
            },
        );
    }

    pub fn add_selector(
        &mut self,
        ref_id: String,
        selector: String,
        role: &str,
        name: &str,
        nth: Option<usize>,
    ) {
        self.map.insert(
            ref_id,
            RefEntry {
                backend_node_id: None,
                role: role.to_string(),
                name: name.to_string(),
                nth,
                selector: Some(selector),
                frame_id: None,
            },
        );
    }

    pub fn get(&self, ref_id: &str) -> Option<&RefEntry> {
        self.map.get(ref_id)
    }

    pub fn entries_sorted(&self) -> Vec<(String, RefEntry)> {
        let mut entries = self
            .map
            .iter()
            .map(|(ref_id, entry)| (ref_id.clone(), entry.clone()))
            .collect::<Vec<_>>();

        entries.sort_by_key(|(ref_id, _)| {
            ref_id
                .strip_prefix('e')
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(usize::MAX)
        });

        entries
    }

    pub fn remove(&mut self, ref_id: &str) {
        self.map.remove(ref_id);
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.next_ref = 1;
    }

    pub fn next_ref_num(&self) -> usize {
        self.next_ref
    }

    pub fn set_next_ref_num(&mut self, n: usize) {
        self.next_ref = n;
    }
}

pub fn parse_ref(input: &str) -> Option<String> {
    let trimmed = input.trim();

    if let Some(stripped) = trimmed.strip_prefix('@') {
        if stripped.starts_with('e') && stripped[1..].chars().all(|c| c.is_ascii_digit()) {
            return Some(stripped.to_string());
        }
    }

    if let Some(stripped) = trimmed.strip_prefix("ref=") {
        if stripped.starts_with('e') && stripped[1..].chars().all(|c| c.is_ascii_digit()) {
            return Some(stripped.to_string());
        }
    }

    if trimmed.starts_with('e')
        && trimmed.len() > 1
        && trimmed[1..].chars().all(|c| c.is_ascii_digit())
    {
        return Some(trimmed.to_string());
    }

    None
}

pub async fn resolve_element_center(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(f64, f64, String), String> {
    if let Some(ref_id) = parse_ref(selector_or_ref) {
        let entry = ref_map
            .get(&ref_id)
            .ok_or_else(|| format!("Unknown ref: {}", ref_id))?;

        let effective_session_id =
            resolve_frame_session(entry.frame_id.as_deref(), session_id, iframe_sessions);

        // Try cached backend_node_id first (fast path)
        if let Some(backend_node_id) = entry.backend_node_id {
            let result: Result<DomGetBoxModelResult, String> = client
                .send_command_typed(
                    "DOM.getBoxModel",
                    &DomGetBoxModelParams {
                        backend_node_id: Some(backend_node_id),
                        node_id: None,
                        object_id: None,
                    },
                    Some(effective_session_id),
                )
                .await;

            if let Ok(r) = result {
                let (x, y) = box_model_center(&r.model);
                return Ok((x, y, effective_session_id.to_string()));
            }
            // backend_node_id is stale; re-query the accessibility tree below
        }

        // Fallback: re-query the accessibility tree to find a fresh node by role/name
        let fresh_id = find_node_id_by_role_name(
            client,
            session_id,
            &entry.role,
            &entry.name,
            entry.nth,
            entry.frame_id.as_deref(),
            iframe_sessions,
        )
        .await?;
        let result: DomGetBoxModelResult = client
            .send_command_typed(
                "DOM.getBoxModel",
                &DomGetBoxModelParams {
                    backend_node_id: Some(fresh_id),
                    node_id: None,
                    object_id: None,
                },
                Some(effective_session_id),
            )
            .await?;
        let (x, y) = box_model_center(&result.model);
        return Ok((x, y, effective_session_id.to_string()));
    }

    // CSS selector
    let (x, y) = resolve_by_selector(client, session_id, selector_or_ref).await?;
    Ok((x, y, session_id.to_string()))
}

pub async fn resolve_element_object_id(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(String, String), String> {
    if let Some(ref_id) = parse_ref(selector_or_ref) {
        let entry = ref_map
            .get(&ref_id)
            .ok_or_else(|| format!("Unknown ref: {}", ref_id))?;

        let effective_session_id =
            resolve_frame_session(entry.frame_id.as_deref(), session_id, iframe_sessions);

        // Try cached backend_node_id first (fast path)
        if let Some(backend_node_id) = entry.backend_node_id {
            let result: Result<DomResolveNodeResult, String> = client
                .send_command_typed(
                    "DOM.resolveNode",
                    &DomResolveNodeParams {
                        backend_node_id: Some(backend_node_id),
                        node_id: None,
                        object_group: Some("agent-browser".to_string()),
                    },
                    Some(effective_session_id),
                )
                .await;

            if let Ok(r) = result {
                if let Some(object_id) = r.object.object_id {
                    return Ok((object_id, effective_session_id.to_string()));
                }
            }
            // backend_node_id is stale; re-query the accessibility tree below
        }

        // Fallback: re-query the accessibility tree to find a fresh node by role/name
        let fresh_id = find_node_id_by_role_name(
            client,
            session_id,
            &entry.role,
            &entry.name,
            entry.nth,
            entry.frame_id.as_deref(),
            iframe_sessions,
        )
        .await?;
        let result: DomResolveNodeResult = client
            .send_command_typed(
                "DOM.resolveNode",
                &DomResolveNodeParams {
                    backend_node_id: Some(fresh_id),
                    node_id: None,
                    object_group: Some("agent-browser".to_string()),
                },
                Some(effective_session_id),
            )
            .await?;
        let object_id = result
            .object
            .object_id
            .ok_or_else(|| format!("No objectId for ref {}", ref_id))?;
        return Ok((object_id, effective_session_id.to_string()));
    }

    // Selector fallback (CSS or XPath)
    let js = build_find_element_js(selector_or_ref);
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(false),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    let object_id = result
        .result
        .object_id
        .ok_or_else(|| format!("Element not found: {}", selector_or_ref))?;
    Ok((object_id, session_id.to_string()))
}

/// Determine which CDP session and parameters to use for an AX tree query.
/// Cross-origin iframes have a dedicated session (no frameId needed);
/// same-origin iframes use the parent session with a frameId parameter.
pub(super) fn resolve_ax_session<'a>(
    frame_id: Option<&str>,
    session_id: &'a str,
    iframe_sessions: &'a HashMap<String, String>,
) -> (serde_json::Value, &'a str) {
    if let Some(frame_id) = frame_id {
        if let Some(iframe_sid) = iframe_sessions.get(frame_id) {
            (serde_json::json!({}), iframe_sid.as_str())
        } else {
            (serde_json::json!({ "frameId": frame_id }), session_id)
        }
    } else {
        (serde_json::json!({}), session_id)
    }
}

/// Resolve the effective CDP session for an element's frame.
/// If the element's frame_id has a dedicated cross-origin iframe session, return it.
/// Otherwise, return the parent session.
fn resolve_frame_session<'a>(
    frame_id: Option<&str>,
    session_id: &'a str,
    iframe_sessions: &'a HashMap<String, String>,
) -> &'a str {
    frame_id
        .and_then(|fid| iframe_sessions.get(fid))
        .map(|s| s.as_str())
        .unwrap_or(session_id)
}

/// Re-query the accessibility tree to find a node matching role+name+nth,
/// returning its fresh backendDOMNodeId. This uses the same data source
/// (Accessibility.getFullAXTree) that built the ref map during snapshot,
/// so role/name matching is guaranteed to be consistent.
async fn find_node_id_by_role_name(
    client: &CdpClient,
    session_id: &str,
    role: &str,
    name: &str,
    nth: Option<usize>,
    frame_id: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<i64, String> {
    let (ax_params, effective_session_id) =
        resolve_ax_session(frame_id, session_id, iframe_sessions);
    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed(
            "Accessibility.getFullAXTree",
            &ax_params,
            Some(effective_session_id),
        )
        .await?;

    let nth_index = nth.unwrap_or(0);
    let mut match_count: usize = 0;

    for node in &ax_tree.nodes {
        if node.ignored.unwrap_or(false) {
            continue;
        }
        let node_role = extract_ax_string(&node.role);
        let node_name = extract_ax_string(&node.name);
        if node_role == role && node_name == name {
            if match_count == nth_index {
                return node.backend_d_o_m_node_id.ok_or_else(|| {
                    format!(
                        "AX node has no backendDOMNodeId for role={} name={}",
                        role, name
                    )
                });
            }
            match_count += 1;
        }
    }

    Err(format!(
        "Could not locate element with role={} name={}",
        role, name
    ))
}

pub(super) fn extract_ax_string(value: &Option<AXValue>) -> String {
    match value {
        Some(v) => match &v.value {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            _ => String::new(),
        },
        None => String::new(),
    }
}

/// Build a JS expression that finds a DOM element by CSS selector or XPath.
fn build_find_element_js(selector: &str) -> String {
    if let Some(xpath) = selector.strip_prefix("xpath=") {
        format!(
            "document.evaluate({}, document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue",
            serde_json::to_string(xpath).unwrap_or_default()
        )
    } else {
        format!(
            "document.querySelector({})",
            serde_json::to_string(selector).unwrap_or_default()
        )
    }
}

/// Build a JS expression that counts matching DOM elements by CSS selector or XPath.
fn build_count_elements_js(selector: &str) -> String {
    if let Some(xpath) = selector.strip_prefix("xpath=") {
        format!(
            "document.evaluate({}, document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null).snapshotLength",
            serde_json::to_string(xpath).unwrap_or_default()
        )
    } else {
        format!(
            "document.querySelectorAll({}).length",
            serde_json::to_string(selector).unwrap_or_default()
        )
    }
}

fn build_selector_js(selector: &str) -> String {
    let find_expr = build_find_element_js(selector);
    format!(
        r#"(() => {{
            const el = {find_expr};
            if (!el) return null;
            const rect = el.getBoundingClientRect();
            return {{ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }};
        }})()"#,
    )
}

async fn resolve_by_selector(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<(f64, f64), String> {
    let js = build_selector_js(selector);

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    let val = result.result.value.unwrap_or(Value::Null);
    let x = val.get("x").and_then(|v| v.as_f64());
    let y = val.get("y").and_then(|v| v.as_f64());

    match (x, y) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(format!("Element not found: {}", selector)),
    }
}

fn box_model_center(model: &BoxModel) -> (f64, f64) {
    // content quad: [x1,y1, x2,y2, x3,y3, x4,y4]
    if model.content.len() >= 8 {
        let x = (model.content[0] + model.content[2] + model.content[4] + model.content[6]) / 4.0;
        let y = (model.content[1] + model.content[3] + model.content[5] + model.content[7]) / 4.0;
        (x, y)
    } else {
        (0.0, 0.0)
    }
}

pub async fn get_element_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration:
                    "function() { return this.innerText || this.textContent || ''; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

pub async fn get_element_attribute(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    attribute: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: format!(
                    "function() {{ return this.getAttribute({}); }}",
                    serde_json::to_string(attribute).unwrap_or_default()
                ),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result.result.value.unwrap_or(Value::Null))
}

pub async fn is_element_visible(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<bool, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const rect = this.getBoundingClientRect();
                    const style = window.getComputedStyle(this);
                    return rect.width > 0 && rect.height > 0 &&
                           style.visibility !== 'hidden' &&
                           style.display !== 'none' &&
                           parseFloat(style.opacity) > 0;
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn is_element_enabled(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<bool, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { return !this.disabled; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(true))
}

pub async fn is_element_checked(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<bool, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    // Mirrors Playwright's getChecked() with follow-label retargeting:
    // 1. If element is a native checkbox/radio input, return .checked
    // 2. If element has an ARIA checked role, return aria-checked
    // 3. Follow label → input association (label.control)
    // 4. Check for nested checkbox/radio input as last resort
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    var el = this;
                    // Native checkbox/radio input
                    var tag = el.tagName && el.tagName.toUpperCase();
                    if (tag === 'INPUT' && (el.type === 'checkbox' || el.type === 'radio')) {
                        return el.checked;
                    }
                    // ARIA role-based checked state
                    var role = el.getAttribute && el.getAttribute('role');
                    var ariaCheckedRoles = ['checkbox','radio','switch','menuitemcheckbox','menuitemradio','option','treeitem'];
                    if (role && ariaCheckedRoles.indexOf(role) !== -1) {
                        return el.getAttribute('aria-checked') === 'true';
                    }
                    // Follow label association (Playwright follow-label retarget)
                    var label = el;
                    if (tag !== 'LABEL') {
                        label = el.closest && el.closest('label');
                    }
                    if (label && label.tagName && label.tagName.toUpperCase() === 'LABEL' && label.control) {
                        var ctrl = label.control;
                        if (ctrl.type === 'checkbox' || ctrl.type === 'radio') {
                            return ctrl.checked;
                        }
                    }
                    // Check for nested native input
                    var input = el.querySelector && el.querySelector('input[type="checkbox"], input[type="radio"]');
                    if (input) return input.checked;
                    return false;
                }"#.to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn get_element_inner_text(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { return this.innerText || ''; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

pub async fn get_element_inner_html(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { return this.innerHTML || ''; }".to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

pub async fn get_element_input_value(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration:
                    "function() { return typeof this.value === 'string' ? this.value : ''; }"
                        .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(result
        .result
        .value
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default())
}

pub async fn set_element_value(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    value: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(), String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let js = format!(
        "function() {{ this.value = {}; this.dispatchEvent(new Event('input', {{bubbles: true}})); this.dispatchEvent(new Event('change', {{bubbles: true}})); }}",
        serde_json::to_string(value).unwrap_or_default()
    );

    client
        .send_command_typed::<_, EvaluateResult>(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    Ok(())
}

pub async fn get_element_bounding_box(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: r#"function() {
                    const r = this.getBoundingClientRect();
                    return { x: r.x, y: r.y, width: r.width, height: r.height };
                }"#
                .to_string(),
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(&effective_session_id),
        )
        .await?;

    result
        .result
        .value
        .ok_or_else(|| format!("Could not get bounding box for: {}", selector_or_ref))
}

pub async fn get_element_count(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<i64, String> {
    let js = build_count_elements_js(selector);

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    Ok(result.result.value.and_then(|v| v.as_i64()).unwrap_or(0))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DomRequestNodeParams {
    object_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DomRequestNodeResult {
    node_id: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CssGetMatchedStylesForNodeParams {
    node_id: i64,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
struct Specificity {
    a: i64,
    b: i64,
    c: i64,
}

impl Specificity {
    fn zero() -> Self {
        Self { a: 0, b: 0, c: 0 }
    }

    fn inline() -> Self {
        Self {
            a: 1_000_000,
            b: 0,
            c: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct CascadeDeclaration {
    selector: String,
    source: String,
    origin: String,
    name: String,
    value: String,
    important: bool,
    active: bool,
    inline: bool,
    specificity: Specificity,
    source_order: usize,
}

fn normalize_property_list(properties: Option<Vec<String>>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();

    for prop in properties
        .unwrap_or_else(|| {
            CASCADE_DEFAULT_PROPERTIES
                .iter()
                .map(|prop| prop.to_string())
                .collect()
        })
        .into_iter()
    {
        let prop = prop.trim().to_ascii_lowercase();
        if !prop.is_empty() && seen.insert(prop.clone()) {
            normalized.push(prop);
        }
    }

    normalized
}

fn property_allowed(name: &str, properties: &[String]) -> bool {
    properties
        .iter()
        .any(|prop| prop.eq_ignore_ascii_case(name.trim()))
}

fn selector_specificity(selector: &Value) -> Specificity {
    let Some(specificity) = selector.get("specificity") else {
        return selector
            .get("text")
            .and_then(|v| v.as_str())
            .map(compute_specificity_fallback)
            .unwrap_or_else(Specificity::zero);
    };

    Specificity {
        a: specificity.get("a").and_then(|v| v.as_i64()).unwrap_or(0),
        b: specificity.get("b").and_then(|v| v.as_i64()).unwrap_or(0),
        c: specificity.get("c").and_then(|v| v.as_i64()).unwrap_or(0),
    }
}

fn compute_specificity_fallback(selector: &str) -> Specificity {
    let mut a = 0;
    let mut b = 0;
    let mut c = 0;
    let chars = selector.chars().collect::<Vec<_>>();
    let mut i = 0;
    let mut expect_type = true;

    while i < chars.len() {
        let ch = chars[i];
        match ch {
            '\\' => {
                i += 2;
            }
            '#' => {
                a += 1;
                i += 1;
                while i < chars.len() && is_selector_ident_char(chars[i]) {
                    i += 1;
                }
                expect_type = false;
            }
            '.' => {
                b += 1;
                i += 1;
                while i < chars.len() && is_selector_ident_char(chars[i]) {
                    i += 1;
                }
                expect_type = false;
            }
            '[' => {
                b += 1;
                i += 1;
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                expect_type = false;
            }
            ':' => {
                if chars.get(i + 1) == Some(&':') {
                    c += 1;
                    i += 2;
                } else {
                    b += 1;
                    i += 1;
                }
                while i < chars.len() && is_selector_ident_char(chars[i]) {
                    i += 1;
                }
                expect_type = false;
            }
            '*' => {
                i += 1;
                expect_type = false;
            }
            '>' | '+' | '~' | ',' => {
                i += 1;
                expect_type = true;
            }
            ch if ch.is_whitespace() => {
                i += 1;
                expect_type = true;
            }
            ch if expect_type && is_selector_ident_start(ch) => {
                c += 1;
                i += 1;
                while i < chars.len() && is_selector_ident_char(chars[i]) {
                    i += 1;
                }
                expect_type = false;
            }
            _ => {
                i += 1;
            }
        }
    }

    Specificity { a, b, c }
}

fn is_selector_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == '-'
}

fn is_selector_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '\\'
}

fn matched_selector(rule_match: &Value) -> (String, Specificity) {
    let selectors = rule_match
        .get("rule")
        .and_then(|rule| rule.get("selectorList"))
        .and_then(|list| list.get("selectors"))
        .and_then(|selectors| selectors.as_array());
    let matching = rule_match
        .get("matchingSelectors")
        .and_then(|matches| matches.as_array());

    if let (Some(selectors), Some(matching)) = (selectors, matching) {
        let mut texts = Vec::new();
        let mut specificity = Specificity::zero();
        for idx in matching.iter().filter_map(|idx| idx.as_u64()) {
            if let Some(selector) = selectors.get(idx as usize) {
                if let Some(text) = selector.get("text").and_then(|v| v.as_str()) {
                    texts.push(text.to_string());
                }
                specificity = specificity.max(selector_specificity(selector));
            }
        }
        if !texts.is_empty() {
            return (texts.join(", "), specificity);
        }
    }

    let text = rule_match
        .get("rule")
        .and_then(|rule| rule.get("selectorList"))
        .and_then(|list| list.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_string();
    let specificity = compute_specificity_fallback(&text);
    (text, specificity)
}

#[allow(clippy::too_many_arguments)]
fn collect_style_declarations(
    style: Option<&Value>,
    selector: &str,
    source: &str,
    origin: &str,
    specificity: Specificity,
    inline: bool,
    base_order: usize,
    properties: &[String],
    declarations: &mut Vec<CascadeDeclaration>,
) {
    let Some(style) = style else {
        return;
    };
    let Some(css_properties) = style.get("cssProperties").and_then(|v| v.as_array()) else {
        return;
    };

    for (property_order, property) in css_properties.iter().enumerate() {
        if property
            .get("disabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        if !property
            .get("parsedOk")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
        {
            continue;
        }

        let Some(name) = property.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if !property_allowed(name, properties) {
            continue;
        }
        let value = property
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        declarations.push(CascadeDeclaration {
            selector: selector.to_string(),
            source: source.to_string(),
            origin: origin.to_string(),
            name: name.to_ascii_lowercase(),
            value,
            important: property
                .get("important")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            active: false,
            inline,
            specificity,
            source_order: base_order + property_order,
        });
    }
}

fn declaration_wins(candidate: &CascadeDeclaration, current: &CascadeDeclaration) -> bool {
    (
        candidate.important,
        candidate.inline,
        candidate.specificity,
        candidate.source_order,
    ) > (
        current.important,
        current.inline,
        current.specificity,
        current.source_order,
    )
}

fn mark_active_declarations(declarations: &mut [CascadeDeclaration]) {
    let mut winners: HashMap<String, usize> = HashMap::new();

    for idx in 0..declarations.len() {
        let name = declarations[idx].name.clone();
        match winners.get(&name).copied() {
            Some(current_idx)
                if declaration_wins(&declarations[idx], &declarations[current_idx]) =>
            {
                winners.insert(name, idx);
            }
            Some(_) => {}
            None => {
                winners.insert(name, idx);
            }
        }
    }

    for idx in winners.values() {
        declarations[*idx].active = true;
    }
}

fn declarations_to_rules(declarations: &[CascadeDeclaration]) -> Vec<Value> {
    let mut rules: Vec<Value> = Vec::new();
    let mut rule_index: BTreeMap<(String, String), usize> = BTreeMap::new();

    for declaration in declarations {
        let key = (declaration.selector.clone(), declaration.source.clone());
        let idx = match rule_index.get(&key).copied() {
            Some(idx) => idx,
            None => {
                let idx = rules.len();
                rule_index.insert(key, idx);
                rules.push(serde_json::json!({
                    "selector": declaration.selector,
                    "source": declaration.source,
                    "origin": declaration.origin,
                    "properties": []
                }));
                idx
            }
        };

        if let Some(properties) = rules[idx]
            .get_mut("properties")
            .and_then(|v| v.as_array_mut())
        {
            properties.push(serde_json::json!({
                "name": declaration.name,
                "value": declaration.value,
                "important": declaration.important,
                "active": declaration.active,
                "status": if declaration.active { "active" } else { "overridden" },
            }));
        }
    }

    rules
}

fn cascade_rules_from_matched_styles(
    matched_styles: &Value,
    properties: &[String],
    include_user_agent: bool,
) -> Vec<Value> {
    let mut declarations = Vec::new();

    collect_style_declarations(
        matched_styles.get("attributesStyle"),
        "attribute style",
        "attributes",
        "attribute",
        Specificity::zero(),
        false,
        0,
        properties,
        &mut declarations,
    );

    if let Some(matches) = matched_styles
        .get("matchedCSSRules")
        .and_then(|v| v.as_array())
    {
        for (rule_order, rule_match) in matches.iter().enumerate() {
            let Some(rule) = rule_match.get("rule") else {
                continue;
            };
            let origin = rule
                .get("origin")
                .and_then(|v| v.as_str())
                .unwrap_or("regular");
            if origin == "user-agent" && !include_user_agent {
                continue;
            }
            let (selector, specificity) = matched_selector(rule_match);
            let source = rule
                .get("styleSheetId")
                .or_else(|| {
                    rule.get("style")
                        .and_then(|style| style.get("styleSheetId"))
                })
                .and_then(|v| v.as_str())
                .unwrap_or(origin);

            collect_style_declarations(
                rule.get("style"),
                &selector,
                source,
                origin,
                specificity,
                false,
                10_000 + (rule_order * 1_000),
                properties,
                &mut declarations,
            );
        }
    }

    collect_style_declarations(
        matched_styles.get("inlineStyle"),
        "element.style",
        "inline",
        "inline",
        Specificity::inline(),
        true,
        usize::MAX / 2,
        properties,
        &mut declarations,
    );

    mark_active_declarations(&mut declarations);
    declarations_to_rules(&declarations)
}

async fn get_computed_styles_for_object(
    client: &CdpClient,
    effective_session_id: &str,
    object_id: String,
    properties: Option<Vec<String>>,
) -> Result<Value, String> {
    let js = match properties {
        Some(props) => {
            let props_json = serde_json::to_string(&props).unwrap_or("[]".to_string());
            format!(
                r#"function() {{
                    const s = window.getComputedStyle(this);
                    const props = {};
                    const result = {{}};
                    for (const p of props) result[p] = s.getPropertyValue(p);
                    return result;
                }}"#,
                props_json
            )
        }
        None => r#"function() {
                    const s = window.getComputedStyle(this);
                    const result = {};
                    for (let i = 0; i < s.length; i++) {
                        const p = s[i];
                        result[p] = s.getPropertyValue(p);
                    }
                    return result;
                }"#
        .to_string(),
    };

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(effective_session_id),
        )
        .await?;

    Ok(result.result.value.unwrap_or(Value::Null))
}

async fn get_cascade_element_summary(
    client: &CdpClient,
    effective_session_id: &str,
    object_id: String,
    properties: &[String],
    include_ancestors: bool,
) -> Result<Value, String> {
    let props_json = serde_json::to_string(properties).unwrap_or("[]".to_string());
    let ancestors_json = if include_ancestors { "true" } else { "false" };
    let js = format!(
        r#"function() {{
            const props = {};
            const includeAncestors = {};
            const readComputed = (el, names) => {{
                const style = window.getComputedStyle(el);
                const out = {{}};
                for (const name of names) out[name] = style.getPropertyValue(name);
                return out;
            }};
            const compactText = (el) => (el.innerText || el.textContent || "")
                .replace(/\s+/g, " ")
                .trim()
                .slice(0, 200);
            const rect = this.getBoundingClientRect();
            const result = {{
                tag: this.tagName ? this.tagName.toLowerCase() : "",
                text: compactText(this),
                box: {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }},
                computed: readComputed(this, props),
            }};

            if (includeAncestors) {{
                const ancestorProps = ["display", "position", "overflow", "overflow-x", "overflow-y", "z-index", "opacity", "transform"];
                result.ancestors = [];
                let node = this.parentElement;
                while (node && node.nodeType === Node.ELEMENT_NODE && result.ancestors.length < 5) {{
                    const ancestorRect = node.getBoundingClientRect();
                    result.ancestors.push({{
                        tag: node.tagName ? node.tagName.toLowerCase() : "",
                        id: node.id || "",
                        class: typeof node.className === "string" ? node.className : "",
                        text: compactText(node),
                        box: {{ x: ancestorRect.x, y: ancestorRect.y, width: ancestorRect.width, height: ancestorRect.height }},
                        computed: readComputed(node, ancestorProps),
                    }});
                    node = node.parentElement;
                }}
            }}

            return result;
        }}"#,
        props_json, ancestors_json
    );

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: js,
                object_id: Some(object_id),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(effective_session_id),
        )
        .await?;

    Ok(result.result.value.unwrap_or(Value::Null))
}

/// Return computed styles by default, or a focused DevTools-style CSS cascade
/// report when cascade mode is enabled.
#[allow(clippy::too_many_arguments)]
pub async fn get_element_styles(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    properties: Option<Vec<String>>,
    cascade: bool,
    include_ancestors: bool,
    include_user_agent: bool,
    iframe_sessions: &HashMap<String, String>,
) -> Result<Value, String> {
    let (object_id, effective_session_id) = resolve_element_object_id(
        client,
        session_id,
        ref_map,
        selector_or_ref,
        iframe_sessions,
    )
    .await?;

    if !cascade {
        return get_computed_styles_for_object(
            client,
            &effective_session_id,
            object_id,
            properties,
        )
        .await;
    }

    let properties = normalize_property_list(properties);
    let summary = get_cascade_element_summary(
        client,
        &effective_session_id,
        object_id.clone(),
        &properties,
        include_ancestors,
    )
    .await?;

    client
        .send_command_no_params("DOM.enable", Some(&effective_session_id))
        .await?;
    client
        .send_command_no_params("CSS.enable", Some(&effective_session_id))
        .await?;
    client
        .send_command_no_params("DOM.getDocument", Some(&effective_session_id))
        .await?;

    let node: DomRequestNodeResult = client
        .send_command_typed(
            "DOM.requestNode",
            &DomRequestNodeParams { object_id },
            Some(&effective_session_id),
        )
        .await?;
    let matched_styles: Value = client
        .send_command_typed(
            "CSS.getMatchedStylesForNode",
            &CssGetMatchedStylesForNodeParams {
                node_id: node.node_id,
            },
            Some(&effective_session_id),
        )
        .await?;

    let rules = cascade_rules_from_matched_styles(&matched_styles, &properties, include_user_agent);
    let mut match_obj = summary
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    match_obj.insert("rules".to_string(), Value::Array(rules));

    Ok(serde_json::json!({
        "selector": selector_or_ref,
        "matches": [Value::Object(match_obj)]
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_ref_at_prefix() {
        assert_eq!(parse_ref("@e1"), Some("e1".to_string()));
        assert_eq!(parse_ref("@e123"), Some("e123".to_string()));
    }

    #[test]
    fn test_parse_ref_equals_prefix() {
        assert_eq!(parse_ref("ref=e1"), Some("e1".to_string()));
    }

    #[test]
    fn test_parse_ref_bare() {
        assert_eq!(parse_ref("e1"), Some("e1".to_string()));
        assert_eq!(parse_ref("e42"), Some("e42".to_string()));
    }

    #[test]
    fn test_parse_ref_invalid() {
        assert_eq!(parse_ref("button"), None);
        assert_eq!(parse_ref("e"), None);
        assert_eq!(parse_ref("1"), None);
        assert_eq!(parse_ref(""), None);
    }

    #[test]
    fn test_ref_map_basic() {
        let mut map = RefMap::new();
        map.add("e1".to_string(), Some(42), "button", "Submit", None);
        assert!(map.get("e1").is_some());
        assert_eq!(map.get("e1").unwrap().role, "button");
        assert!(map.get("e2").is_none());
    }

    #[test]
    fn test_build_selector_js_css() {
        let js = build_selector_js("#submit-btn");
        assert!(js.contains("document.querySelector(\"#submit-btn\")"));
        assert!(!js.contains("document.evaluate"));
    }

    #[test]
    fn test_build_selector_js_xpath() {
        let js = build_selector_js("xpath=//button[@id='ok']");
        assert!(js.contains("document.evaluate(\"//button[@id='ok']\", document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null)"));
        assert!(!js.contains("document.querySelector"));
    }

    #[test]
    fn test_build_selector_js_xpath_empty() {
        let js = build_selector_js("xpath=");
        assert!(js.contains("document.evaluate"));
    }

    #[test]
    fn test_cascade_marks_competing_rules_active_and_overridden() {
        let matched_styles = json!({
            "matchedCSSRules": [{
                "matchingSelectors": [0],
                "rule": {
                    "styleSheetId": "style-sheet-1",
                    "origin": "regular",
                    "selectorList": {
                        "text": ".text-blue",
                        "selectors": [{
                            "text": ".text-blue",
                            "specificity": { "a": 0, "b": 1, "c": 0 }
                        }]
                    },
                    "style": {
                        "cssProperties": [{
                            "name": "color",
                            "value": "blue",
                            "important": false,
                            "parsedOk": true
                        }]
                    }
                }
            }, {
                "matchingSelectors": [0],
                "rule": {
                    "styleSheetId": "style-sheet-1",
                    "origin": "regular",
                    "selectorList": {
                        "text": "#target.text-blue",
                        "selectors": [{
                            "text": "#target.text-blue",
                            "specificity": { "a": 1, "b": 1, "c": 0 }
                        }]
                    },
                    "style": {
                        "cssProperties": [{
                            "name": "color",
                            "value": "red",
                            "important": false,
                            "parsedOk": true
                        }]
                    }
                }
            }]
        });

        let rules =
            cascade_rules_from_matched_styles(&matched_styles, &["color".to_string()], false);
        let declarations = rules
            .iter()
            .flat_map(|rule| {
                let selector = rule
                    .get("selector")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                rule.get("properties")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |prop| (selector.clone(), prop))
            })
            .collect::<Vec<_>>();

        assert!(declarations.iter().any(|(selector, prop)| {
            selector == ".text-blue"
                && prop.get("value").and_then(|v| v.as_str()) == Some("blue")
                && prop.get("active").and_then(|v| v.as_bool()) == Some(false)
                && prop.get("status").and_then(|v| v.as_str()) == Some("overridden")
        }));
        assert!(declarations.iter().any(|(selector, prop)| {
            selector == "#target.text-blue"
                && prop.get("value").and_then(|v| v.as_str()) == Some("red")
                && prop.get("active").and_then(|v| v.as_bool()) == Some(true)
                && prop.get("status").and_then(|v| v.as_str()) == Some("active")
        }));
    }

    #[test]
    fn test_cascade_properties_filter_and_user_agent_default() {
        let matched_styles = json!({
            "matchedCSSRules": [{
                "matchingSelectors": [0],
                "rule": {
                    "origin": "user-agent",
                    "selectorList": {
                        "text": "h1",
                        "selectors": [{
                            "text": "h1",
                            "specificity": { "a": 0, "b": 0, "c": 1 }
                        }]
                    },
                    "style": {
                        "cssProperties": [{
                            "name": "display",
                            "value": "block"
                        }]
                    }
                }
            }, {
                "matchingSelectors": [0],
                "rule": {
                    "styleSheetId": "style-sheet-1",
                    "origin": "regular",
                    "selectorList": {
                        "text": ".title",
                        "selectors": [{
                            "text": ".title",
                            "specificity": { "a": 0, "b": 1, "c": 0 }
                        }]
                    },
                    "style": {
                        "cssProperties": [{
                            "name": "color",
                            "value": "black"
                        }, {
                            "name": "font-size",
                            "value": "20px"
                        }]
                    }
                }
            }]
        });

        let rules =
            cascade_rules_from_matched_styles(&matched_styles, &["color".to_string()], false);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0]["selector"], ".title");
        assert_eq!(rules[0]["properties"].as_array().unwrap().len(), 1);
        assert_eq!(rules[0]["properties"][0]["name"], "color");

        let rules = cascade_rules_from_matched_styles(
            &matched_styles,
            &["display".to_string(), "color".to_string()],
            true,
        );
        assert!(rules.iter().any(|rule| rule["selector"] == "h1"));
    }

    #[test]
    fn test_build_selector_js_not_xpath_prefix() {
        // "xpath" without "=" should be treated as CSS selector
        let js = build_selector_js("xpath//div");
        assert!(js.contains("document.querySelector"));
    }

    #[test]
    fn test_build_count_elements_js_css() {
        let js = build_count_elements_js(".item");
        assert!(js.contains("document.querySelectorAll(\".item\").length"));
        assert!(!js.contains("document.evaluate"));
    }

    #[test]
    fn test_build_count_elements_js_xpath() {
        let js = build_count_elements_js("xpath=//li");
        assert!(js.contains("document.evaluate(\"//li\", document, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null).snapshotLength"));
        assert!(!js.contains("querySelectorAll"));
    }

    #[test]
    fn test_box_model_center() {
        let model = BoxModel {
            content: vec![10.0, 20.0, 110.0, 20.0, 110.0, 60.0, 10.0, 60.0],
            padding: vec![],
            border: vec![],
            margin: vec![],
            width: 100,
            height: 40,
        };
        let (x, y) = box_model_center(&model);
        assert!((x - 60.0).abs() < 0.01);
        assert!((y - 40.0).abs() < 0.01);
    }

    // -----------------------------------------------------------------------
    // resolve_frame_session tests (Issue #925)
    // Cross-origin iframe elements must resolve to the dedicated session.
    // -----------------------------------------------------------------------

    #[test]
    fn test_cross_origin_element_uses_dedicated_session() {
        let mut iframe_sessions = HashMap::new();
        iframe_sessions.insert(
            "cross-origin-frame".to_string(),
            "iframe-session".to_string(),
        );

        let session = resolve_frame_session(
            Some("cross-origin-frame"),
            "parent-session",
            &iframe_sessions,
        );

        assert_eq!(session, "iframe-session");
    }

    #[test]
    fn test_same_origin_element_uses_parent_session() {
        let iframe_sessions = HashMap::new();

        let session = resolve_frame_session(
            Some("same-origin-frame"),
            "parent-session",
            &iframe_sessions,
        );

        assert_eq!(session, "parent-session");
    }

    #[test]
    fn test_main_frame_element_uses_parent_session() {
        let iframe_sessions = HashMap::new();

        let session = resolve_frame_session(None, "parent-session", &iframe_sessions);

        assert_eq!(session, "parent-session");
    }
}
