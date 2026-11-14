use std::collections::HashMap;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::*;

#[derive(Debug, Clone)]
pub struct RefEntry {
    pub backend_node_id: Option<i64>,
    pub role: String,
    pub name: String,
    pub nth: Option<usize>,
    pub selector: Option<String>,
    pub frame_id: Option<String>,
    allow_role_fallback: bool,
}

#[derive(Clone)]
struct DocumentRefs {
    session: String,
    loader: String,
    refs: HashMap<i64, String>,
}

#[derive(Clone)]
pub struct RefMap {
    map: HashMap<String, RefEntry>,
    documents: HashMap<(String, Option<String>), DocumentRefs>,
    next_ref: usize,
}

impl RefMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            documents: HashMap::new(),
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
                allow_role_fallback: true,
            },
        );
    }

    /// Add a ref that must keep resolving to this exact backend node. This is
    /// used for actions that select a concrete DOM node before interacting;
    /// falling back by role/name could silently target a different element.
    pub fn add_exact_backend_node(&mut self, ref_id: String, backend_node_id: i64) {
        self.map.insert(
            ref_id,
            RefEntry {
                backend_node_id: Some(backend_node_id),
                role: String::new(),
                name: String::new(),
                nth: None,
                selector: None,
                frame_id: None,
                allow_role_fallback: false,
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
                allow_role_fallback: true,
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

    pub fn ref_ids(&self) -> std::collections::HashSet<String> {
        self.map.keys().cloned().collect()
    }

    pub fn remove(&mut self, ref_id: &str) {
        self.map.remove(ref_id);
    }

    pub fn begin_snapshot(&mut self) {
        self.map.clear();
    }

    /// Observe the document behind a page/frame pair. Unknown documents never
    /// retain refs, while a changed session or loader replaces the prior bucket.
    pub fn observe_document(
        &mut self,
        page_session: &str,
        frame: Option<&str>,
        session: &str,
        loader: Option<&str>,
    ) -> bool {
        let key = (page_session.to_string(), frame.map(str::to_string));
        let Some(loader) = loader.filter(|id| !id.is_empty()) else {
            self.invalidate_frame(page_session, frame);
            return false;
        };
        let changed = self
            .documents
            .get(&key)
            .is_none_or(|document| document.session != session || document.loader != loader);
        if changed {
            if frame.is_none() {
                self.documents
                    .retain(|(entry_page, _), _| entry_page != page_session);
            }
            self.documents.insert(
                key,
                DocumentRefs {
                    session: session.to_string(),
                    loader: loader.to_string(),
                    refs: HashMap::new(),
                },
            );
        }
        true
    }

    fn invalidate_frame(&mut self, page_session: &str, frame: Option<&str>) {
        if frame.is_none() {
            self.documents
                .retain(|(entry_page, _), _| entry_page != page_session);
        } else {
            self.documents
                .remove(&(page_session.to_string(), frame.map(str::to_string)));
        }
    }

    pub fn durable_ref(
        &self,
        page_session: &str,
        frame: Option<&str>,
        backend_node_id: i64,
    ) -> Option<&str> {
        self.documents
            .get(&(page_session.to_string(), frame.map(str::to_string)))?
            .refs
            .get(&backend_node_id)
            .map(String::as_str)
    }

    pub fn remember_durable_ref(
        &mut self,
        page_session: &str,
        frame: Option<&str>,
        backend_node_id: i64,
        ref_id: &str,
    ) {
        if let Some(document) = self
            .documents
            .get_mut(&(page_session.to_string(), frame.map(str::to_string)))
        {
            document.refs.insert(backend_node_id, ref_id.to_string());
        }
    }

    pub fn invalidate_page(&mut self, page_session: &str) {
        self.documents
            .retain(|(entry_page, _), _| entry_page != page_session);
        self.map.clear();
    }

    /// Drop all document identities while preserving the monotonic ref counter.
    pub fn invalidate_all_documents(&mut self) {
        self.documents.clear();
        self.map.clear();
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

/// Mirror of DaemonState.active_frame_id, refreshed before every command
/// (commands are serialized by the daemon's state lock, so this cannot
/// race). It lets CSS-selector resolution honor `frame <sel>` without
/// threading a parameter through every interaction signature; snapshot refs
/// already carry their frame through the ref map.
static ACTIVE_FRAME: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
    std::sync::OnceLock::new();

pub fn set_active_frame(frame_id: Option<&str>) {
    *ACTIVE_FRAME
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap() = frame_id.map(String::from);
}

fn active_frame() -> Option<String> {
    ACTIVE_FRAME.get().and_then(|m| m.lock().unwrap().clone())
}

/// Object handle for the <iframe> element that owns a frame, resolved on the
/// parent session. Works for same-process frames where no dedicated CDP
/// session exists.
pub(super) async fn frame_owner_object_id(
    client: &CdpClient,
    session_id: &str,
    frame_id: &str,
) -> Result<String, String> {
    let owner = client
        .send_command(
            "DOM.getFrameOwner",
            Some(serde_json::json!({ "frameId": frame_id })),
            Some(session_id),
        )
        .await?;
    let backend_node_id = owner
        .get("backendNodeId")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("Could not resolve the owner element of frame {}", frame_id))?;
    let result: DomResolveNodeResult = client
        .send_command_typed(
            "DOM.resolveNode",
            &DomResolveNodeParams {
                backend_node_id: Some(backend_node_id),
                node_id: None,
                object_group: Some("agent-browser".to_string()),
            },
            Some(session_id),
        )
        .await?;
    result
        .object
        .object_id
        .ok_or_else(|| format!("No objectId for the owner element of frame {}", frame_id))
}

/// Find a selector inside a same-process iframe and return its center in
/// top-level viewport coordinates (input events dispatch in that space).
/// Same-origin access to contentDocument is what makes this possible; a
/// cross-origin frame never takes this path because it has its own session.
async fn resolve_center_in_same_process_frame(
    client: &CdpClient,
    session_id: &str,
    frame_id: &str,
    selector: &str,
    check: PressCheck,
) -> Result<(f64, f64), String> {
    let owner_object_id = frame_owner_object_id(client, session_id, frame_id).await?;
    let find_expr = build_find_element_js_in("doc", selector);
    let function = format!(
        r#"async function() {{
            const doc = this.contentDocument;
            if (!doc) return null;
            const el = {find_expr};
            if (!el) return null;
            const clickPoint = {CLICK_POINT_JS};
            const point = await clickPoint(el, doc);
            const blockerAt = {BLOCKER_AT_JS};
            const notActionable = {ACTIONABILITY_JS};
            const topDoc = (point.view && point.view.document) || doc;
            return {{ x: point.x, y: point.y, reason: notActionable(el, point.inView), inView: point.inView, blocker: blockerAt(doc, el, point.lx, point.ly) || blockerAt(topDoc, el, point.x, point.y) }};
        }}"#,
    );
    let result = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(serde_json::json!({
                "objectId": owner_object_id,
                "functionDeclaration": function,
                "returnByValue": true,
                "awaitPromise": true,
            })),
            Some(session_id),
        )
        .await?;
    let value = result.get("result").and_then(|r| r.get("value"));
    if let Some(err) = check.rejection(selector, value) {
        return Err(err);
    }
    if let Some(blocker) = value
        .and_then(|v| v.get("blocker"))
        .and_then(|v| v.as_str())
    {
        return Err(intercepted_error(selector, blocker));
    }
    let x = value.and_then(|v| v.get("x")).and_then(|v| v.as_f64());
    let y = value.and_then(|v| v.get("y")).and_then(|v| v.as_f64());
    match (x, y) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(format!(
            "Element not found in the selected frame: {}",
            selector
        )),
    }
}

/// Find a selector inside a same-process iframe and return its object handle.
async fn resolve_object_in_same_process_frame(
    client: &CdpClient,
    session_id: &str,
    frame_id: &str,
    selector: &str,
) -> Result<String, String> {
    let owner_object_id = frame_owner_object_id(client, session_id, frame_id).await?;
    let find_expr = build_find_element_js_in("doc", selector);
    let function = format!(
        "function() {{ const doc = this.contentDocument; if (!doc) return null; return {find_expr}; }}",
    );
    let result = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(serde_json::json!({
                "objectId": owner_object_id,
                "functionDeclaration": function,
                "returnByValue": false,
            })),
            Some(session_id),
        )
        .await?;
    result
        .get("result")
        .and_then(|r| r.get("objectId"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| format!("Element not found in the selected frame: {}", selector))
}

pub async fn resolve_element_center(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    iframe_sessions: &HashMap<String, String>,
    check: PressCheck,
) -> Result<(f64, f64, String), String> {
    if let Some(ref_id) = parse_ref(selector_or_ref) {
        let entry = ref_map
            .get(&ref_id)
            .ok_or_else(|| format!("Unknown ref: {}", ref_id))?;

        let effective_session_id =
            resolve_frame_session(entry.frame_id.as_deref(), session_id, iframe_sessions);

        // Try cached backend_node_id first (fast path)
        if let Some(backend_node_id) = entry.backend_node_id {
            if let Some(object_id) =
                resolve_node_object_id(client, effective_session_id, backend_node_id).await
            {
                if let Some((x, y)) = node_click_point(
                    client,
                    effective_session_id,
                    &object_id,
                    selector_or_ref,
                    check,
                )
                .await?
                {
                    return Ok((x, y, effective_session_id.to_string()));
                }
            }
            if !entry.allow_role_fallback {
                return Err(format!("Element ref {ref_id} is stale"));
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
        let object_id = resolve_node_object_id(client, effective_session_id, fresh_id)
            .await
            .ok_or_else(|| format!("Element not found: {}", selector_or_ref))?;
        let (x, y) = node_click_point(
            client,
            effective_session_id,
            &object_id,
            selector_or_ref,
            check,
        )
        .await?
        .ok_or_else(|| format!("Element not found: {}", selector_or_ref))?;
        return Ok((x, y, effective_session_id.to_string()));
    }

    // CSS selector: honor an active `frame <sel>` selection.
    if let Some(frame_id) = active_frame() {
        // Cross-process iframe: its dedicated session's main frame IS the
        // iframe, so plain document-rooted resolution works there.
        if let Some(frame_session) = iframe_sessions.get(&frame_id) {
            let (x, y) = resolve_by_selector(client, frame_session, selector_or_ref, check).await?;
            return Ok((x, y, frame_session.clone()));
        }
        let (x, y) = resolve_center_in_same_process_frame(
            client,
            session_id,
            &frame_id,
            selector_or_ref,
            check,
        )
        .await?;
        return Ok((x, y, session_id.to_string()));
    }
    let (x, y) = resolve_by_selector(client, session_id, selector_or_ref, check).await?;
    Ok((x, y, session_id.to_string()))
}

/// Origin of a CDP session's viewport in the recorded page's CSS coordinates.
/// Input sent to an OOPIF is local to that session; cursor history is page-local.
/// Walk owner sessions for nested OOPIFs. Content quads include iframe borders
/// and scrolling in the owning viewport.
pub async fn session_viewport_offset(
    client: &CdpClient,
    page_session: &str,
    target_session: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(f64, f64), String> {
    let mut current = target_session.to_string();
    let mut offset = (0.0, 0.0);
    for _ in 0..=iframe_sessions.len() {
        if current == page_session {
            return Ok(offset);
        }
        let frame = iframe_sessions
            .iter()
            .find(|(_, session)| **session == current)
            .map(|(frame, _)| frame)
            .ok_or("Cannot locate recording cursor frame")?;
        let mut owner = None;
        for candidate in
            std::iter::once(page_session).chain(iframe_sessions.values().map(String::as_str))
        {
            if candidate == current {
                continue;
            }
            let Ok(node) = client
                .send_command(
                    "DOM.getFrameOwner",
                    Some(serde_json::json!({"frameId": frame})),
                    Some(candidate),
                )
                .await
            else {
                continue;
            };
            let Some(backend_id) = node["backendNodeId"].as_i64() else {
                continue;
            };
            let model = client
                .send_command(
                    "DOM.getBoxModel",
                    Some(serde_json::json!({"backendNodeId": backend_id})),
                    Some(candidate),
                )
                .await?;
            let quad = model
                .pointer("/model/content")
                .and_then(Value::as_array)
                .ok_or("Cannot locate recording cursor frame bounds")?;
            let x = quad
                .first()
                .and_then(Value::as_f64)
                .ok_or("Missing frame content x")?;
            let y = quad
                .get(1)
                .and_then(Value::as_f64)
                .ok_or("Missing frame content y")?;
            offset.0 += x;
            offset.1 += y;
            owner = Some(candidate.to_string());
            break;
        }
        current = owner.ok_or("Cannot locate recording cursor frame owner")?;
    }
    Err("Cyclic recording cursor frame ownership".to_string())
}

/// Resolve a backend node id to an object handle, or None when the node is
/// gone so the caller can fall back to the accessibility tree.
async fn resolve_node_object_id(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
) -> Option<String> {
    let resolved: DomResolveNodeResult = client
        .send_command_typed(
            "DOM.resolveNode",
            &DomResolveNodeParams {
                backend_node_id: Some(backend_node_id),
                node_id: None,
                object_group: Some("agent-browser".to_string()),
            },
            Some(session_id),
        )
        .await
        .ok()?;
    resolved.object.object_id
}

/// Press point for a resolved node: scroll it in, wait for the page to settle,
/// and report why the point cannot be pressed before input is dispatched.
async fn node_click_point(
    client: &CdpClient,
    session_id: &str,
    object_id: &str,
    target: &str,
    check: PressCheck,
) -> Result<Option<(f64, f64)>, String> {
    let function = format!(
        r#"async function() {{
            if (!this.isConnected) return {{ connected: false }};
            const doc = this.ownerDocument || document;
            const clickPoint = {CLICK_POINT_JS};
            const point = await clickPoint(this, doc);
            const blockerAt = {BLOCKER_AT_JS};
            const notActionable = {ACTIONABILITY_JS};
            const topDoc = (point.view && point.view.document) || doc;
            return {{ connected: true, x: point.x, y: point.y, reason: notActionable(this, point.inView), inView: point.inView, blocker: blockerAt(doc, this, point.lx, point.ly) || blockerAt(topDoc, this, point.x, point.y) }};
        }}"#,
    );
    let result = client
        .send_command(
            "Runtime.callFunctionOn",
            Some(serde_json::json!({
                "objectId": object_id,
                "functionDeclaration": function,
                "returnByValue": true,
                "awaitPromise": true,
            })),
            Some(session_id),
        )
        .await?;
    let value = result.get("result").and_then(|r| r.get("value"));
    if value
        .and_then(|v| v.get("connected"))
        .and_then(|v| v.as_bool())
        == Some(false)
    {
        return Ok(None);
    }
    if let Some(err) = check.rejection(target, value) {
        return Err(err);
    }
    if let Some(blocker) = value
        .and_then(|v| v.get("blocker"))
        .and_then(|v| v.as_str())
    {
        return Err(intercepted_error(target, blocker));
    }
    match (
        value.and_then(|v| v.get("x")).and_then(|v| v.as_f64()),
        value.and_then(|v| v.get("y")).and_then(|v| v.as_f64()),
    ) {
        (Some(x), Some(y)) => Ok(Some((x, y))),
        _ => Err(format!("Element not found: {}", target)),
    }
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
                    if !is_detached(client, effective_session_id, &object_id).await {
                        return Ok((object_id, effective_session_id.to_string()));
                    }
                }
            }
            if !entry.allow_role_fallback {
                return Err(format!("Element ref {ref_id} is stale"));
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

    // Selector fallback (CSS or XPath): honor an active `frame <sel>` selection.
    if let Some(frame_id) = active_frame() {
        if let Some(frame_session) = iframe_sessions.get(&frame_id) {
            let js = build_find_element_js(selector_or_ref);
            let result: EvaluateResult = client
                .send_command_typed(
                    "Runtime.evaluate",
                    &EvaluateParams {
                        expression: js,
                        return_by_value: Some(false),
                        await_promise: Some(false),
                    },
                    Some(frame_session.as_str()),
                )
                .await?;
            let object_id = result
                .result
                .object_id
                .ok_or_else(|| format!("Element not found: {}", selector_or_ref))?;
            return Ok((object_id, frame_session.clone()));
        }
        let object_id =
            resolve_object_in_same_process_frame(client, session_id, &frame_id, selector_or_ref)
                .await?;
        return Ok((object_id, session_id.to_string()));
    }

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

/// Reports whether a resolved node has been removed from its document, releasing it if so.
async fn is_detached(client: &CdpClient, session_id: &str, object_id: &str) -> bool {
    let result: Result<EvaluateResult, String> = client
        .send_command_typed(
            "Runtime.callFunctionOn",
            &CallFunctionOnParams {
                function_declaration: "function() { return this.isConnected; }".to_string(),
                object_id: Some(object_id.to_string()),
                arguments: None,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await;
    let detached = matches!(result, Ok(r) if r.result.value == Some(Value::Bool(false)));
    if detached {
        let _ = client
            .send_command(
                "Runtime.releaseObject",
                Some(serde_json::json!({ "objectId": object_id })),
                Some(session_id),
            )
            .await;
    }
    detached
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

/// Finds a stale ref's replacement by role and name, refusing any match that could be ambiguous.
async fn find_node_id_by_role_name(
    client: &CdpClient,
    session_id: &str,
    role: &str,
    name: &str,
    nth: Option<usize>,
    frame_id: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<i64, String> {
    if nth.is_some() {
        return Err(format!(
            "Stale ref for role={} name={} cannot be recovered: the snapshot had several matching elements. Run snapshot again and use a fresh ref.",
            role, name
        ));
    }
    let (ax_params, effective_session_id) =
        resolve_ax_session(frame_id, session_id, iframe_sessions);
    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed_raw(
            "Accessibility.getFullAXTree",
            &ax_params,
            Some(effective_session_id),
        )
        .await?;

    select_unique_node_id(&ax_tree.nodes, role, name)
}

/// Returns the backendDOMNodeId of the only non-ignored node matching role and name.
fn select_unique_node_id(nodes: &[AXNode], role: &str, name: &str) -> Result<i64, String> {
    let mut found: Option<&AXNode> = None;

    for node in nodes {
        if node.ignored.unwrap_or(false) {
            continue;
        }
        if normalize_ax_role(extract_ax_string(&node.role)) == role
            && extract_ax_string(&node.name) == name
        {
            if found.is_some() {
                return Err(format!(
                    "Stale ref for role={} name={} cannot be recovered: several elements now match. Run snapshot again and use a fresh ref.",
                    role, name
                ));
            }
            found = Some(node);
        }
    }

    match found {
        Some(node) => node.backend_d_o_m_node_id.ok_or_else(|| {
            format!(
                "AX node has no backendDOMNodeId for role={} name={}",
                role, name
            )
        }),
        None => Err(format!(
            "Could not locate element with role={} name={}",
            role, name
        )),
    }
}

/// Use the same iframe role for snapshot refs and stale-ref recovery.
pub(super) fn normalize_ax_role(role: String) -> String {
    if role == "IframePresentational" {
        "Iframe".to_string()
    } else {
        role
    }
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
    build_find_element_js_in("document", selector)
}

/// Same as build_find_element_js but rooted at an arbitrary Document
/// expression (e.g. an iframe's contentDocument).
fn build_find_element_js_in(root: &str, selector: &str) -> String {
    if let Some(xpath) = selector.strip_prefix("xpath=") {
        format!(
            "{root}.evaluate({xpath}, {root}, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue",
            xpath = serde_json::to_string(xpath).unwrap_or_default(),
        )
    } else {
        format!(
            "{root}.querySelector({selector})",
            selector = serde_json::to_string(selector).unwrap_or_default(),
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

/// JS function source for `blockerAt(doc, el, x, y)`: returns a short
/// description of the element that would actually receive a click at (x, y)
/// when that element is unrelated to `el`, or null when the click would land
/// on `el` (or something that activates it). Relations that count as "lands
/// on el": shadow-including ancestors/descendants in either direction, and
/// label/control association (custom checkboxes hide the input under a styled
/// sibling inside the same label).
const BLOCKER_AT_JS: &str = r#"(doc, el, x, y) => {
    // Descend from the given document through same-origin iframes so a point
    // over a frame resolves to the element inside it, in that frame's space.
    let d = doc, lx = x, ly = y;
    let hit = d.elementFromPoint(lx, ly);
    while (hit && (hit.tagName === 'IFRAME' || hit.tagName === 'FRAME') && hit.contentDocument && hit !== el) {
        const r = hit.getBoundingClientRect();
        const ownerWin = hit.ownerDocument.defaultView;
        const style = ownerWin ? ownerWin.getComputedStyle(hit) : null;
        const scaleX = hit.offsetWidth > 0 ? r.width / hit.offsetWidth : 1;
        const scaleY = hit.offsetHeight > 0 ? r.height / hit.offsetHeight : 1;
        const borderLeft = style ? (parseFloat(style.borderLeftWidth) || 0) : hit.clientLeft;
        const borderTop = style ? (parseFloat(style.borderTopWidth) || 0) : hit.clientTop;
        const padLeft = style ? (parseFloat(style.paddingLeft) || 0) : 0;
        const padTop = style ? (parseFloat(style.paddingTop) || 0) : 0;
        const padRight = style ? (parseFloat(style.paddingRight) || 0) : 0;
        const padBottom = style ? (parseFloat(style.paddingBottom) || 0) : 0;
        const contentLeft = r.x + (borderLeft + padLeft) * scaleX;
        const contentTop = r.y + (borderTop + padTop) * scaleY;
        const contentWidth = Math.max(0, hit.clientWidth - padLeft - padRight);
        const contentHeight = Math.max(0, hit.clientHeight - padTop - padBottom);
        const renderedWidth = contentWidth * scaleX;
        const renderedHeight = contentHeight * scaleY;
        if (
            renderedWidth <= 0 || renderedHeight <= 0 ||
            lx < contentLeft || lx >= contentLeft + renderedWidth ||
            ly < contentTop || ly >= contentTop + renderedHeight
        ) {
            break;
        }
        const childWin = hit.contentDocument.defaultView;
        const vw = (childWin && childWin.innerWidth) || contentWidth;
        const vh = (childWin && childWin.innerHeight) || contentHeight;
        lx = (lx - contentLeft) * (vw / renderedWidth);
        ly = (ly - contentTop) * (vh / renderedHeight);
        d = hit.contentDocument;
        hit = d.elementFromPoint(lx, ly);
    }
    if (!hit || hit === el) return null;
    const up = (n) => n.parentNode || n.host || (n.getRootNode && n.getRootNode().host) || null;
    for (let n = hit; n; n = up(n)) { if (n === el) return null; }
    for (let n = el; n; n = up(n)) { if (n === hit) return null; }
    const hitLabel = hit.closest ? hit.closest('label') : null;
    if (hitLabel && (hitLabel.control === el || hitLabel.contains(el))) return null;
    const elLabel = el.closest ? el.closest('label') : null;
    if (elLabel && elLabel.contains(hit)) return null;
    let desc = hit.tagName.toLowerCase();
    if (hit.id) desc += '#' + hit.id;
    else if (typeof hit.className === 'string' && hit.className.trim())
        desc += '.' + hit.className.trim().split(/\s+/).slice(0, 2).join('.');
    if (!hit.id && hit.closest) {
        const anchored = hit.closest('[id]');
        if (anchored && anchored !== hit)
            desc += ' inside ' + anchored.tagName.toLowerCase() + '#' + anchored.id;
    }
    return desc;
}"#;

const CLICK_POINT_JS: &str = r#"(async (el, doc) => {
    const measure = () => {
        const r = el.getBoundingClientRect();
        const lx = r.x + r.width / 2;
        const ly = r.y + r.height / 2;
        let x = lx;
        let y = ly;
        const own = doc.defaultView || window;
        let reachable = lx >= 0 && ly >= 0 && lx < own.innerWidth && ly < own.innerHeight;
        const frames = [];
        let win = own;
        while (win && win.frameElement) {
            const fe = win.frameElement;
            const frameRect = fe.getBoundingClientRect();
            const ownerWin = fe.ownerDocument.defaultView || win.parent;
            const style = ownerWin.getComputedStyle(fe);
            const scaleX = fe.offsetWidth > 0 ? frameRect.width / fe.offsetWidth : 1;
            const scaleY = fe.offsetHeight > 0 ? frameRect.height / fe.offsetHeight : 1;
            const borderLeft = parseFloat(style.borderLeftWidth) || 0;
            const borderTop = parseFloat(style.borderTopWidth) || 0;
            const padLeft = parseFloat(style.paddingLeft) || 0;
            const padTop = parseFloat(style.paddingTop) || 0;
            const padRight = parseFloat(style.paddingRight) || 0;
            const padBottom = parseFloat(style.paddingBottom) || 0;
            const contentLeft = frameRect.x + (borderLeft + padLeft) * scaleX;
            const contentTop = frameRect.y + (borderTop + padTop) * scaleY;
            const contentWidth = Math.max(0, fe.clientWidth - padLeft - padRight);
            const contentHeight = Math.max(0, fe.clientHeight - padTop - padBottom);
            const renderedWidth = contentWidth * scaleX;
            const renderedHeight = contentHeight * scaleY;
            const vw = win.innerWidth;
            const vh = win.innerHeight;
            x = contentLeft + (vw > 0 ? x * (renderedWidth / vw) : 0);
            y = contentTop + (vh > 0 ? y * (renderedHeight / vh) : 0);
            const parentWin = win.parent;
            reachable = reachable
                && renderedWidth > 0 && renderedHeight > 0
                && x >= contentLeft && x < contentLeft + renderedWidth
                && y >= contentTop && y < contentTop + renderedHeight
                && x >= 0 && y >= 0
                && x < parentWin.innerWidth && y < parentWin.innerHeight;
            frames.push(fe);
            win = parentWin;
        }
        const view = win || own;
        reachable = reachable && x >= 0 && y >= 0 && x < view.innerWidth && y < view.innerHeight;
        return { x: x, y: y, lx: lx, ly: ly, frames: frames, view: view, inView: reachable, box: r.width > 0 || r.height > 0 };
    };
    const settle = async (m) => {
        for (let i = 0; i < 5; i++) {
            await new Promise((resolve) => {
                requestAnimationFrame(() => requestAnimationFrame(resolve));
                setTimeout(resolve, 100);
            });
            const next = measure();
            const moved = next.x !== m.x || next.y !== m.y;
            m = next;
            if (!moved) break;
        }
        return m;
    };
    let m = measure();
    if (el.scrollIntoViewIfNeeded) {
        el.scrollIntoViewIfNeeded(true);
        const next = measure();
        m = next.x !== m.x || next.y !== m.y ? await settle(next) : next;
    }
    for (let scrolls = 0; scrolls < 2 && m.box && !m.inView; scrolls++) {
        el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
        for (const frame of [...m.frames].reverse()) frame.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
        m = await settle(measure());
    }
    return { x: m.x, y: m.y, lx: m.lx, ly: m.ly, view: m.view, inView: m.inView };
})"#;

/// Returns why a click at (x, y) cannot reach `el`, or null when it can.
/// It fails open when the engine lacks an API it needs.
const ACTIONABILITY_JS: &str = r#"(el, inView) => {
    try {
        const rect = el.getBoundingClientRect();
        if ((rect.width <= 0 && rect.height <= 0) || ['hidden', 'collapse'].includes(el.ownerDocument.defaultView.getComputedStyle(el).visibility)) return 'not visible';
        if (el.disabled || el.matches(':disabled')) return 'disabled';
        if (!inView) return 'outside the viewport';
    } catch (e) {}
    return null;
}"#;

fn build_selector_js(selector: &str) -> String {
    let find_expr = build_find_element_js(selector);
    format!(
        r#"(async () => {{
            const el = {find_expr};
            if (!el) return null;
            const clickPoint = {CLICK_POINT_JS};
            const point = await clickPoint(el, document);
            const blockerAt = {BLOCKER_AT_JS};
            const notActionable = {ACTIONABILITY_JS};
            const topDoc = (point.view && point.view.document) || document;
            return {{ x: point.x, y: point.y, reason: notActionable(el, point.inView), inView: point.inView, blocker: blockerAt(document, el, point.lx, point.ly) || blockerAt(topDoc, el, point.x, point.y) }};
        }})()"#,
    )
}

async fn resolve_by_selector(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
    check: PressCheck,
) -> Result<(f64, f64), String> {
    let js = build_selector_js(selector);

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js,
                return_by_value: Some(true),
                await_promise: Some(true),
            },
            Some(session_id),
        )
        .await?;

    let val = result.result.value.unwrap_or(Value::Null);
    if let Some(err) = check.rejection(selector, Some(&val)) {
        return Err(err);
    }
    if let Some(blocker) = val.get("blocker").and_then(|v| v.as_str()) {
        return Err(intercepted_error(selector, blocker));
    }
    let x = val.get("x").and_then(|v| v.as_f64());
    let y = val.get("y").and_then(|v| v.as_f64());

    match (x, y) {
        (Some(x), Some(y)) => Ok((x, y)),
        _ => Err(format!("Element not found: {}", selector)),
    }
}

fn intercepted_error(target: &str, blocker: &str) -> String {
    format!(
        "Element '{}' is covered by <{}> at its click point, so the input would land on that element instead. Dismiss or interact with the covering element first (it is often a dialog, banner, or sticky header).",
        target, blocker
    )
}

/// What a resolved press point must satisfy before input is dispatched to it.
#[derive(Clone, Copy)]
pub enum PressCheck {
    /// Use the point as measured.
    None,
    /// The element must be visible, enabled, and inside the viewport.
    Click,
    /// The element must be visible and inside the viewport.
    Drag,
}

impl PressCheck {
    /// Why input at the resolved `point` would miss `target`, if it would.
    fn rejection(self, target: &str, point: Option<&Value>) -> Option<String> {
        let reason = point.and_then(|v| v.get("reason")).and_then(|v| v.as_str());
        let (action, reason) = match self {
            PressCheck::None => return None,
            PressCheck::Click => ("click", reason?),
            PressCheck::Drag => {
                let in_view = point
                    .and_then(|v| v.get("inView"))
                    .and_then(|v| v.as_bool());
                match reason {
                    Some("not visible") => ("drag", "not visible"),
                    _ if in_view == Some(false) => ("drag", "outside the viewport"),
                    _ => return None,
                }
            }
        };
        Some(format!(
            "Element '{}' is {}, so the {} would not reach it",
            target, reason, action
        ))
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

/// What a field says about itself, so a recording can tell a credential
/// from other input even when the field has no label.
pub async fn get_field_traits(
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
                function_declaration: "function() { \
                    const label = (this.labels && this.labels[0] && this.labels[0].innerText) || ''; \
                    const attr = (n) => this.getAttribute && this.getAttribute(n) || ''; \
                    return { \
                        type: this.type || '', \
                        autocomplete: attr('autocomplete'), \
                        hint: [attr('name'), this.id, attr('aria-label'), attr('placeholder'), label] \
                            .filter(Boolean).join(' ') \
                    }; }"
                    .to_string(),
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
                function_declaration: "function() { return !this.disabled && !(this.matches && this.matches(':disabled')); }".to_string(),
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

pub async fn get_element_styles(
    client: &CdpClient,
    session_id: &str,
    ref_map: &RefMap,
    selector_or_ref: &str,
    properties: Option<Vec<String>>,
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
            Some(&effective_session_id),
        )
        .await?;

    Ok(result.result.value.unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ax(role: &str, name: &str, id: Option<i64>, ignored: bool) -> AXNode {
        let val = |t: &str, v: &str| AXValue {
            value_type: t.into(),
            value: Some(serde_json::json!(v)),
        };
        AXNode {
            node_id: "1".into(),
            role: Some(val("role", role)),
            name: Some(val("computedString", name)),
            value: None,
            description: None,
            properties: None,
            child_ids: None,
            backend_d_o_m_node_id: id,
            ignored: Some(ignored),
        }
    }

    #[test]
    fn test_select_unique_node_id_returns_the_only_match() {
        let nodes = vec![
            ax("button", "Approve", Some(1), true),
            ax("button", "Approve", Some(7), false),
            ax("button", "Reject", Some(8), false),
        ];
        assert_eq!(select_unique_node_id(&nodes, "button", "Approve"), Ok(7));
    }

    #[test]
    fn test_select_unique_node_id_normalizes_presentational_iframes() {
        let mut nodes = vec![
            ax("IframePresentational", "", Some(1), true),
            ax("IframePresentational", "", Some(7), false),
            ax("Iframe", "Other", Some(8), false),
        ];
        assert_eq!(select_unique_node_id(&nodes, "Iframe", ""), Ok(7));

        nodes.push(ax("Iframe", "", Some(9), false));
        assert!(select_unique_node_id(&nodes, "Iframe", "")
            .unwrap_err()
            .contains("several elements now match"));
    }

    #[test]
    fn test_select_unique_node_id_rejects_multiple_matches() {
        let ambiguous = "Stale ref for role=button name=Approve cannot be recovered: several elements now match. Run snapshot again and use a fresh ref.";
        let two_valid = vec![
            ax("button", "Approve", Some(1), false),
            ax("button", "Approve", Some(2), false),
        ];
        assert_eq!(
            select_unique_node_id(&two_valid, "button", "Approve"),
            Err(ambiguous.to_string())
        );
        let one_without_id = vec![
            ax("button", "Approve", None, false),
            ax("button", "Approve", Some(2), false),
        ];
        assert_eq!(
            select_unique_node_id(&one_without_id, "button", "Approve"),
            Err(ambiguous.to_string())
        );
    }

    #[test]
    fn test_select_unique_node_id_keeps_not_found_and_missing_id_errors() {
        let nodes = vec![ax("button", "Reject", Some(1), false)];
        assert_eq!(
            select_unique_node_id(&nodes, "button", "Approve"),
            Err("Could not locate element with role=button name=Approve".to_string())
        );
        let nodes = vec![ax("button", "Approve", None, false)];
        assert_eq!(
            select_unique_node_id(&nodes, "button", "Approve"),
            Err("AX node has no backendDOMNodeId for role=button name=Approve".to_string())
        );
    }

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
    fn test_ref_map_clear_preserves_monotonic_numbering() {
        let mut map = RefMap::new();
        map.add("e1".to_string(), Some(42), "button", "Submit", None);
        map.set_next_ref_num(2);

        map.begin_snapshot();

        assert!(map.get("e1").is_none());
        assert_eq!(map.next_ref_num(), 2);
    }

    #[test]
    fn test_durable_refs_are_scoped_by_document_and_frame() {
        let mut map = RefMap::new();
        assert!(map.observe_document("page-a", None, "session-a", Some("loader-a")));
        assert!(map.observe_document(
            "page-a",
            Some("frame-a"),
            "frame-session",
            Some("frame-loader")
        ));
        map.remember_durable_ref("page-a", None, 42, "e1");
        map.remember_durable_ref("page-a", Some("frame-a"), 42, "e2");
        assert_eq!(map.durable_ref("page-a", None, 42), Some("e1"));
        assert_eq!(map.durable_ref("page-a", Some("frame-a"), 42), Some("e2"));
        assert_eq!(map.durable_ref("page-b", None, 42), None);

        map.invalidate_page("page-a");
        assert_eq!(map.durable_ref("page-a", None, 42), None);
        assert_eq!(map.next_ref_num(), 1);
    }

    #[test]
    fn invalidating_one_page_preserves_other_page_documents() {
        let mut map = RefMap::new();
        assert!(map.observe_document("page-a", None, "session-a", Some("loader-a")));
        map.remember_durable_ref("page-a", None, 42, "e1");
        assert!(map.observe_document("page-b", None, "session-b", Some("loader-b")));
        map.remember_durable_ref("page-b", None, 42, "e2");

        map.invalidate_page("page-b");

        assert_eq!(map.durable_ref("page-a", None, 42), Some("e1"));
        assert_eq!(map.durable_ref("page-b", None, 42), None);
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
