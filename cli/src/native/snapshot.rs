use std::collections::HashMap;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::{
    AXNode, AXProperty, AXValue, EvaluateParams, EvaluateResult, GetFullAXTreeResult,
};
use super::element::{resolve_ax_session, RefEntry, RefMap};

#[cfg(test)]
mod document_identity_regressions {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    async fn observe_documents(documents: &[Option<&str>], sessions: &[&str]) -> Vec<String> {
        let document = Arc::new(Mutex::new(None::<String>));
        let server_document = document.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(message)) = ws.next().await {
                let Ok(text) = message.to_text() else {
                    continue;
                };
                let request: Value = serde_json::from_str(text).unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "Page.getFrameTree" => json!({"frameTree": {"frame": {
                        "id": "child", "loaderId": server_document.lock().unwrap().clone()
                    }}}),
                    "Accessibility.getFullAXTree" => json!({"nodes": [{
                        "nodeId": "1", "role": {"type": "role", "value": "button"},
                        "name": {"type": "string", "value": "Buy"}, "backendDOMNodeId": 42
                    }]}),
                    "Runtime.evaluate" => json!({"result": {"type": "object", "value": []}}),
                    _ => json!({}),
                };
                ws.send(tokio_tungstenite::tungstenite::Message::Text(
                    json!({"id": request["id"], "result": result}).to_string(),
                ))
                .await
                .unwrap();
            }
        });
        let client = CdpClient::connect(&url).await.unwrap();
        let mut refs = RefMap::new();
        let mut observations = Vec::new();
        for (loader, session) in documents.iter().zip(sessions) {
            *document.lock().unwrap() = loader.map(str::to_string);
            refs.begin_snapshot();
            take_snapshot(
                &client,
                "parent-session",
                &SnapshotOptions::default(),
                &mut refs,
                Some("child"),
                &HashMap::from([("child".into(), session.to_string())]),
            )
            .await
            .unwrap();
            observations.push(refs.entries_sorted()[0].0.clone());
        }
        server.abort();
        observations
    }

    #[tokio::test]
    async fn durable_refs_invalidate_replaced_iframe_document() {
        let refs =
            observe_documents(&[Some("a"), Some("a"), Some("b")], &["child-session"; 3]).await;
        assert_eq!(refs[0], refs[1], "same document should preserve refs");
        assert_ne!(
            refs[1], refs[2],
            "replacement document must not reuse a ref for backend ID 42"
        );
    }

    #[tokio::test]
    async fn durable_refs_invalidate_replaced_iframe_session() {
        let refs = observe_documents(&[Some("a"); 2], &["session-a", "session-b"]).await;
        assert_ne!(
            refs[0], refs[1],
            "backend IDs belong to their effective CDP session"
        );
    }

    #[tokio::test]
    async fn durable_refs_do_not_share_unknown_document() {
        let refs = observe_documents(
            &[None, None, Some("a"), None, Some("a")],
            &["child-session"; 5],
        )
        .await;
        let unique: std::collections::HashSet<_> = refs.iter().collect();
        assert_eq!(
            unique.len(),
            refs.len(),
            "unknown identity must break continuity: {refs:?}"
        );
    }
}

const INTERACTIVE_ROLES: &[&str] = &[
    "button",
    "link",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "listbox",
    "menuitem",
    "menuitemcheckbox",
    "menuitemradio",
    "option",
    "searchbox",
    "slider",
    "spinbutton",
    "switch",
    "tab",
    "treeitem",
    "Iframe",
];

const CONTENT_ROLES: &[&str] = &[
    "heading",
    "cell",
    "gridcell",
    "columnheader",
    "rowheader",
    "listitem",
    "article",
    "region",
    "main",
    "navigation",
];

const STRUCTURAL_ROLES: &[&str] = &[
    "generic",
    "group",
    "list",
    "table",
    "row",
    "rowgroup",
    "grid",
    "treegrid",
    "menu",
    "menubar",
    "toolbar",
    "tablist",
    "tree",
    "directory",
    "document",
    "application",
    "presentation",
    "none",
    "WebArea",
    "RootWebArea",
];

const ZERO_WIDTH_CHARS: &[char] = &[
    '\u{FEFF}', // BOM / Zero Width No-Break Space
    '\u{200B}', // Zero Width Space
    '\u{200C}', // Zero Width Non-Joiner
    '\u{200D}', // Zero Width Joiner
    '\u{2060}', // Word Joiner
];

fn normalize_snapshot_name(name: &str) -> String {
    name.chars()
        .filter_map(|ch| match ch {
            '\u{00A0}' => Some(' '),
            ch if ZERO_WIDTH_CHARS.contains(&ch) => None,
            ch => Some(ch),
        })
        .collect()
}

#[derive(Default)]
pub struct SnapshotOptions {
    pub selector: Option<String>,
    pub interactive: bool,
    pub compact: bool,
    pub depth: Option<usize>,
    pub urls: bool,
}

struct TreeNode {
    role: String,
    name: String,
    level: Option<i64>,
    checked: Option<String>,
    expanded: Option<bool>,
    selected: Option<bool>,
    disabled: Option<bool>,
    required: Option<bool>,
    value_text: Option<String>,
    backend_node_id: Option<i64>,
    children: Vec<usize>,
    parent_idx: Option<usize>,
    has_ref: bool,
    ref_id: Option<String>,
    depth: usize,
    cursor_info: Option<CursorElementInfo>,
    url: Option<String>,
}

impl TreeNode {
    // Create an empty node
    fn empty() -> Self {
        Self {
            role: String::new(),
            name: String::new(),
            level: None,
            checked: None,
            expanded: None,
            selected: None,
            disabled: None,
            required: None,
            value_text: None,
            backend_node_id: None,
            children: Vec::new(),
            parent_idx: None,
            has_ref: false,
            ref_id: None,
            depth: 0,
            cursor_info: None,
            url: None,
        }
    }

    fn clear(&mut self) {
        self.role = String::new();
        self.name = String::new();
        self.level = None;
        self.checked = None;
        self.expanded = None;
        self.selected = None;
        self.disabled = None;
        self.required = None;
        self.value_text = None;
        self.backend_node_id = None;
        self.children.clear();
        self.parent_idx = None;
        self.has_ref = false;
        self.url = None;
        self.ref_id = None;
        self.depth = 0;
        self.cursor_info = None;
    }
}

/// The type of a hidden form input found inside a cursor-interactive element.
#[derive(Clone, Copy)]
enum HiddenInputKind {
    Radio,
    Checkbox,
}

impl HiddenInputKind {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "radio" => Some(Self::Radio),
            "checkbox" => Some(Self::Checkbox),
            _ => None,
        }
    }

    fn as_role(&self) -> &str {
        match self {
            Self::Radio => "radio",
            Self::Checkbox => "checkbox",
        }
    }
}

/// Information about a cursor-interactive element (elements with cursor:pointer, onclick, tabindex, etc.)
#[derive(Clone)]
struct CursorElementInfo {
    kind: String, // "clickable", "focusable", "editable"
    hints: Vec<String>,
    text: String, // textContent from the DOM element (fallback when ARIA name is empty)
    hidden_input_kind: Option<HiddenInputKind>,
    hidden_input_checked: Option<String>, // "true", "false", or "mixed" (tristate)
}

struct RoleNameTracker {
    counts: HashMap<String, usize>,
}

impl RoleNameTracker {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }

    fn track(&mut self, role: &str, name: &str) -> usize {
        let key = format!("{}:{}", role, name);
        let count = self.counts.entry(key).or_insert(0);
        let nth = *count;
        *count += 1;
        nth
    }

    fn get_duplicates(&self) -> HashMap<String, usize> {
        self.counts
            .iter()
            .filter(|(_, &count)| count > 1)
            .map(|(key, &count)| (key.clone(), count))
            .collect()
    }
}

pub async fn take_snapshot(
    client: &CdpClient,
    session_id: &str,
    options: &SnapshotOptions,
    ref_map: &mut RefMap,
    frame_id: Option<&str>,
    iframe_sessions: &HashMap<String, String>,
) -> Result<String, String> {
    client
        .send_command_no_params("DOM.enable", Some(session_id))
        .await?;
    client
        .send_command_no_params("Accessibility.enable", Some(session_id))
        .await?;

    // If a CSS selector is provided, resolve the set of backendNodeIds that
    // belong to the DOM subtree rooted at the matched element.  We use this
    // set to pick the right AX subtree root(s) later.
    let selector_backend_ids: Option<std::collections::HashSet<i64>> =
        if let Some(ref selector) = options.selector {
            let js = format!(
                "document.querySelector({})",
                serde_json::to_string(selector).unwrap_or_default()
            );
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

            // A throwing evaluation (e.g. an invalid CSS selector like a
            // snapshot ref "@e1") still yields an objectId — for the
            // exception object. Passing that to DOM.describeNode produces the
            // cryptic "Object id doesn't reference a Node"; fail clearly
            // instead, and only accept results that are actually DOM nodes.
            if let Some(exception) = result.exception_details {
                let detail = exception
                    .exception
                    .and_then(|e| e.description)
                    .unwrap_or(exception.text);
                return Err(format!("Invalid selector '{}': {}", selector, detail));
            }
            if result.result.subtype.as_deref() != Some("node") {
                return Err(format!("Selector '{}' did not match any element", selector));
            }
            let object_id = result
                .result
                .object_id
                .ok_or_else(|| format!("Selector '{}' did not match any element", selector))?;

            // Request the full DOM subtree (depth: -1), including shadow roots
            // (pierce), so we can collect all backendNodeIds that live under
            // the matched element.
            let describe: Value = client
                .send_command(
                    "DOM.describeNode",
                    Some(serde_json::json!({ "objectId": object_id, "depth": -1, "pierce": true })),
                    Some(session_id),
                )
                .await?;

            let root_node = describe
                .get("node")
                .ok_or_else(|| format!("Could not resolve DOM node for selector '{}'", selector))?;

            let mut ids = std::collections::HashSet::new();
            collect_backend_node_ids(root_node, &mut ids);

            if ids.is_empty() {
                return Err(format!(
                    "Could not resolve backendNodeId for selector '{}'",
                    selector
                ));
            }

            Some(ids)
        } else {
            None
        };

    let (ax_params, effective_session_id) =
        resolve_ax_session(frame_id, session_id, iframe_sessions);
    // Same-origin children share a CDP session but have separate loaders;
    // out-of-process children also have their own effective session identity.
    let frame_tree = client
        .send_command_no_params("Page.getFrameTree", Some(effective_session_id))
        .await
        .ok();
    let loader = frame_tree
        .as_ref()
        .and_then(|tree| frame_loader(&tree["frameTree"], frame_id));
    let document_is_known =
        ref_map.observe_document(session_id, frame_id, effective_session_id, loader);
    // Ensure domains are enabled on the iframe session (defensive fallback
    // in case the attach-time enable in execute_command was missed).
    if effective_session_id != session_id {
        let _ = client
            .send_command_no_params("DOM.enable", Some(effective_session_id))
            .await;
        let _ = client
            .send_command_no_params("Accessibility.enable", Some(effective_session_id))
            .await;
    }
    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed(
            "Accessibility.getFullAXTree",
            &ax_params,
            Some(effective_session_id),
        )
        .await?;

    let (mut tree_nodes, root_indices) = build_tree(&ax_tree.nodes);

    // When a selector is given, find AX nodes whose backendDOMNodeId falls
    // within the target DOM subtree and pick the top-level ones as roots.
    let effective_roots = if let Some(ref id_set) = selector_backend_ids {
        // Mark which tree_nodes belong to the target DOM subtree.
        let in_subtree: Vec<bool> = tree_nodes
            .iter()
            .map(|n| n.backend_node_id.is_some_and(|bid| id_set.contains(&bid)))
            .collect();

        // An AX node is a "top-level" match if it is in the subtree but none
        // of its ancestors (in the AX tree) are. Checking only the parent is
        // not enough: ignored nodes lose their backendDOMNodeId in
        // build_tree, so a wrapper <div> between two matched nodes would
        // turn everything below it into an extra root and render it twice.
        let has_ancestor_in_subtree = |mut idx: usize| {
            while let Some(pidx) = tree_nodes[idx].parent_idx {
                if in_subtree[pidx] {
                    return true;
                }
                idx = pidx;
            }
            false
        };
        let roots: Vec<usize> = (0..tree_nodes.len())
            .filter(|&idx| in_subtree[idx] && !has_ancestor_in_subtree(idx))
            .collect();

        if roots.is_empty() {
            return Err(format!(
                "No accessibility node found for selector '{}'",
                options.selector.as_deref().unwrap_or("")
            ));
        }
        roots
    } else {
        root_indices
    };

    // Pre-collect cursor-interactive elements so we can mark them with refs during tree building
    let cursor_elements: HashMap<i64, CursorElementInfo> =
        find_cursor_interactive_elements(client, session_id)
            .await
            .unwrap_or_default();

    promote_hidden_inputs(&mut tree_nodes, &cursor_elements);

    assign_refs(
        &mut tree_nodes,
        &cursor_elements,
        ref_map,
        session_id,
        frame_id,
        document_is_known,
    );

    if options.urls {
        let link_nodes: Vec<(usize, i64)> = tree_nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.role == "link" && n.has_ref && n.backend_node_id.is_some())
            .filter_map(|(i, n)| n.backend_node_id.map(|bid| (i, bid)))
            .collect();

        if !link_nodes.is_empty() {
            // CDP has no batch resolve API, so we parallelize individual calls.
            // Phase 1: resolve all backend node IDs to JS object IDs in parallel.
            let resolve_futs = link_nodes.iter().map(|&(idx, bid)| async move {
                let resolved = client
                    .send_command(
                        "DOM.resolveNode",
                        Some(serde_json::json!({ "backendNodeId": bid })),
                        Some(session_id),
                    )
                    .await;
                let obj_id = resolved.ok().and_then(|r| {
                    r.get("object")
                        .and_then(|o| o.get("objectId"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
                (idx, obj_id)
            });
            let resolved: Vec<(usize, Option<String>)> =
                futures_util::future::join_all(resolve_futs).await;

            // Phase 2: fetch hrefs for all resolved objects in parallel.
            let href_futs: Vec<_> = resolved
                .iter()
                .filter_map(|(idx, obj_id)| {
                    let oid = obj_id.as_ref()?;
                    Some(async move {
                        let result = client
                            .send_command(
                                "Runtime.callFunctionOn",
                                Some(serde_json::json!({
                                    "objectId": oid,
                                    "functionDeclaration": "function() { return this.href || ''; }",
                                    "returnByValue": true,
                                })),
                                Some(session_id),
                            )
                            .await;
                        let href = result.ok().and_then(|r| {
                            r.get("result")
                                .and_then(|r| r.get("value"))
                                .and_then(|v| v.as_str())
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_string())
                        });
                        (*idx, href)
                    })
                })
                .collect();
            let hrefs: Vec<(usize, Option<String>)> =
                futures_util::future::join_all(href_futs).await;

            for (idx, href) in hrefs {
                if let Some(url) = href {
                    tree_nodes[idx].url = Some(url);
                }
            }
        }
    }

    let mut output = String::new();
    for &root_idx in &effective_roots {
        render_tree(&tree_nodes, root_idx, 0, &mut output, options);
    }

    // Recurse into child iframes: for each Iframe node with a backend_node_id,
    // resolve the child frame ID and take a snapshot of its content.
    // We only recurse from the main frame (frame_id == None) to avoid
    // unbounded depth; nested iframes within iframes are not expanded.
    if frame_id.is_none() {
        let mut iframe_snapshots: Vec<(String, String)> = Vec::new(); // (ref_id, child_snapshot)
        for node in tree_nodes.iter() {
            if node.role != "Iframe" || !node.has_ref {
                continue;
            }
            let Some(bid) = node.backend_node_id else {
                continue;
            };
            let ref_id = node.ref_id.as_deref().unwrap_or("");
            if let Ok(child_fid) = resolve_iframe_frame_id(client, session_id, bid).await {
                // Snapshot the child frame; errors are silently ignored
                // (e.g. cross-origin iframes)
                if let Ok(child_text) = Box::pin(take_snapshot(
                    client,
                    session_id,
                    options,
                    ref_map,
                    Some(&child_fid),
                    iframe_sessions,
                ))
                .await
                {
                    if !child_text.is_empty()
                        && child_text != "(empty page)"
                        && child_text != "(no interactive elements)"
                    {
                        iframe_snapshots.push((ref_id.to_string(), child_text));
                    }
                }
            }
        }

        // Insert each child snapshot after its Iframe line in the output
        for (ref_id, child_text) in iframe_snapshots {
            let marker = format!("[ref={}]", ref_id);
            if let Some(pos) = output.find(&marker) {
                // Find the end of the Iframe line
                let line_end = output[pos..]
                    .find('\n')
                    .map(|i| pos + i)
                    .unwrap_or(output.len());
                // Determine the indent of the Iframe line
                let line_start = output[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
                let iframe_line = &output[line_start..line_end];
                let iframe_indent = iframe_line.len() - iframe_line.trim_start().len();
                let child_indent = iframe_indent + 2; // one level deeper
                let prefix = " ".repeat(child_indent);

                let indented_child: String = child_text
                    .lines()
                    .map(|line| format!("{}{}\n", prefix, line))
                    .collect();

                // Ensure there's a newline to insert after
                if line_end == output.len() {
                    output.push('\n');
                    output.push_str(&indented_child);
                } else {
                    output.insert_str(line_end + 1, &indented_child);
                }
            }
        }
    }

    if options.compact {
        output = compact_tree(&output, options.interactive);
    }

    let trimmed = output.trim().to_string();

    if trimmed.is_empty() {
        if options.interactive {
            return Ok("(no interactive elements)".to_string());
        }
        return Ok("(empty page)".to_string());
    }

    Ok(trimmed)
}

/// Gives refs to interactive, named content, and cursor-interactive nodes, reusing durable refs for known documents.
fn assign_refs(
    tree_nodes: &mut [TreeNode],
    cursor_elements: &HashMap<i64, CursorElementInfo>,
    ref_map: &mut RefMap,
    session_id: &str,
    frame_id: Option<&str>,
    document_is_known: bool,
) -> Vec<usize> {
    let mut tracker = RoleNameTracker::new();
    let mut next_ref: usize = ref_map.next_ref_num();

    let mut nodes_with_refs: Vec<(usize, usize)> = Vec::new();

    for (idx, node) in tree_nodes.iter().enumerate() {
        let role = node.role.as_str();
        let should_ref = INTERACTIVE_ROLES.contains(&role)
            || (CONTENT_ROLES.contains(&role) && !node.name.is_empty())
            || node
                .backend_node_id
                .is_some_and(|bid| cursor_elements.contains_key(&bid));

        if should_ref {
            let nth = tracker.track(role, &node.name);
            nodes_with_refs.push((idx, nth));
        }
    }

    let duplicates = tracker.get_duplicates();

    for (idx, nth) in &nodes_with_refs {
        let node = &tree_nodes[*idx];
        let backend_node_id = node.backend_node_id;
        let key = format!("{}:{}", node.role, node.name);
        let actual_nth = if duplicates.contains_key(&key) {
            Some(*nth)
        } else {
            None
        };

        let ref_id = if let Some(backend_node_id) = backend_node_id.filter(|_| document_is_known) {
            if let Some(existing) = ref_map.durable_ref(session_id, frame_id, backend_node_id) {
                existing.to_string()
            } else {
                let allocated = format!("e{}", next_ref);
                next_ref += 1;
                ref_map.remember_durable_ref(session_id, frame_id, backend_node_id, &allocated);
                allocated
            }
        } else {
            let allocated = format!("e{}", next_ref);
            next_ref += 1;
            allocated
        };

        ref_map.add_with_frame(
            ref_id.clone(),
            backend_node_id,
            &node.role,
            &node.name,
            actual_nth,
            frame_id,
        );

        let cursor_info = backend_node_id
            .and_then(|bid| cursor_elements.get(&bid))
            .cloned();
        let node = &mut tree_nodes[*idx];
        node.has_ref = true;
        node.ref_id = Some(ref_id);
        node.cursor_info = cursor_info;
    }

    ref_map.set_next_ref_num(next_ref);
    nodes_with_refs.into_iter().map(|(idx, _)| idx).collect()
}

/// A ref that intersects the viewport, with its `getBoundingClientRect` box in CSS pixels.
pub struct ViewportRef {
    pub ref_id: String,
    pub role: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

const VIEWPORT_OBJECT_GROUP: &str = "agent-browser-viewport";

/// Upper bounds on what one viewport scan reads back, so a page cannot make the daemon allocate without limit.
const VIEWPORT_MAX_ELEMENTS: usize = 10_000;
const VIEWPORT_MAX_HOSTS: usize = 500;

/// Elements that can carry a ref without cursor-interactive styling: the implicit and explicit roles in
/// `INTERACTIVE_ROLES` and `CONTENT_ROLES`.
const VIEWPORT_CANDIDATES: &str = "a,area,button,input,select,textarea,option,iframe,h1,h2,h3,h4,h5,h6,td,th,li,article,section,main,nav,[role],[tabindex],[contenteditable]";

/// Assigns refs to the elements that intersect the viewport, asking Chrome for accessibility data on those
/// elements only, so the cost tracks what is on screen rather than the size of the page.
/// Refs from an earlier snapshot of the same document are kept. Errors on an empty viewport or missing accessibility
/// data so the caller can fall back to the full snapshot.
pub async fn take_viewport_refs(
    client: &CdpClient,
    session_id: &str,
    ref_map: &mut RefMap,
) -> Result<Vec<ViewportRef>, String> {
    client
        .send_command_no_params("DOM.enable", Some(session_id))
        .await?;
    client
        .send_command_no_params("Accessibility.enable", Some(session_id))
        .await?;
    let frame_tree = client
        .send_command_no_params("Page.getFrameTree", Some(session_id))
        .await
        .ok();
    let loader = frame_tree
        .as_ref()
        .and_then(|tree| frame_loader(&tree["frameTree"], None));
    let same_document = ref_map.is_current_document(session_id, None, session_id, loader);
    let document_is_known = ref_map.observe_document(session_id, None, session_id, loader);
    if !same_document {
        ref_map.begin_snapshot();
    }

    let scanned = collect_viewport_nodes(client, session_id).await;
    let _ = client
        .send_command(
            "Runtime.releaseObjectGroup",
            Some(serde_json::json!({ "objectGroup": VIEWPORT_OBJECT_GROUP })),
            Some(session_id),
        )
        .await;
    let (nodes, rects, cursor_elements) = scanned?;

    let prior: HashMap<String, RefEntry> = nodes
        .iter()
        .filter_map(|n| ref_map.durable_ref(session_id, None, n.backend_d_o_m_node_id?))
        .filter_map(|id| Some((id.to_string(), ref_map.get(id)?.clone())))
        .collect();
    let (mut tree_nodes, _) = build_tree(&nodes);
    promote_hidden_inputs(&mut tree_nodes, &cursor_elements);
    let ref_indices = assign_refs(
        &mut tree_nodes,
        &cursor_elements,
        ref_map,
        session_id,
        None,
        document_is_known,
    );

    let mut refs = Vec::with_capacity(ref_indices.len());
    for idx in ref_indices {
        let node = &tree_nodes[idx];
        let Some(ref_id) = node.ref_id.clone() else {
            continue;
        };
        match prior.get(&ref_id) {
            Some(entry) if entry.role == node.role && entry.name == node.name => {
                ref_map.restore(ref_id.clone(), entry.clone())
            }
            _ => ref_map.restrict_to_backend_node(&ref_id),
        }
        let [x, y, width, height] = rects[idx];
        refs.push(ViewportRef {
            ref_id,
            role: node.role.clone(),
            name: node.name.clone(),
            x,
            y,
            width,
            height,
        });
    }
    Ok(refs)
}

type ViewportNodes = (Vec<AXNode>, Vec<[f64; 4]>, HashMap<i64, CursorElementInfo>);

/// Finds candidate elements that intersect the viewport in one script and fetches each one's accessibility node.
async fn collect_viewport_nodes(
    client: &CdpClient,
    session_id: &str,
) -> Result<ViewportNodes, String> {
    let scan = format!(
        r#"
function(sel, inShadowRoot, everyElement) {{
    {CURSOR_INFO_JS}
    var W = window.innerWidth || 0, H = window.innerHeight || 0;
    if (!W || !H) return null;
    var els = Object.create(null), info = Object.create(null), hosts = Object.create(null), count = 0, hostCount = 0;
    function visit(root, inShadow) {{
        var all = root.querySelectorAll('*');
        for (var i = 0; i < all.length; i++) {{
            var el = all[i];
            if (el.shadowRoot) visit(el.shadowRoot, true);
            var r = el.getBoundingClientRect();
            var visible = r.width > 0 && r.height > 0 && r.right > 0 && r.bottom > 0 && r.left < W && r.top < H;
            var custom = el.localName.indexOf('-') > 0 && (visible || (!r.width && !r.height));
            var builtIn = visible && (/^(date|time|datetime-local|month|week)$/.test(el.type) && el.localName === 'input' || (el.localName === 'video' || el.localName === 'audio') && el.controls);
            if (!el.shadowRoot && hostCount < 500 && (custom || builtIn)) hosts[hostCount++] = el;
            if (!visible) continue;
            var cursor = !inShadow && el !== document.body && document.body && document.body.contains(el) ? __abCursorInfo(el) : null;
            if (!cursor && !everyElement && !el.matches(sel)) continue;
            els[count] = el;
            info[count++] = {{ rect: [r.x, r.y, r.width, r.height], cursor: cursor }};
        }}
    }}
    visit(this, inShadowRoot || everyElement);
    els.info = info;
    els.count = count;
    els.hosts = hosts;
    return els;
}}
"#
    );
    let call = |object_id: String, function: &str, args: Value, by_value: bool| {
        client.send_command(
            "Runtime.callFunctionOn",
            Some(serde_json::json!({
                "objectId": object_id,
                "functionDeclaration": function,
                "arguments": args,
                "returnByValue": by_value,
                "objectGroup": VIEWPORT_OBJECT_GROUP,
            })),
            Some(session_id),
        )
    };
    let document = client
        .send_command(
            "Runtime.evaluate",
            Some(serde_json::json!({ "expression": "document", "objectGroup": VIEWPORT_OBJECT_GROUP })),
            Some(session_id),
        )
        .await?;
    let mut roots = vec![(
        object_id(&document["result"]).ok_or("No document handle")?,
        false,
        false,
    )];
    let mut info = Vec::new();
    let mut element_ids: Vec<Option<String>> = Vec::new();
    for depth in 0..4 {
        let mut closed_roots = Vec::new();
        for (root, in_shadow, every_element) in roots {
            let args = serde_json::json!([
                { "value": VIEWPORT_CANDIDATES },
                { "value": in_shadow },
                { "value": every_element },
            ]);
            let array = call(root, &scan, args, false).await?;
            if let Some(exception) = array.get("exceptionDetails") {
                return Err(format!("Viewport scan failed: {}", exception["text"]));
            }
            let Some(array_id) = object_id(&array["result"]) else {
                if depth == 0 {
                    return Err("Viewport scan returned no element list".to_string());
                }
                continue;
            };
            let limit = VIEWPORT_MAX_ELEMENTS.saturating_sub(info.len());
            let found = call(
                array_id.clone(),
                "function(n) { var out = Object.create(null), c = 0; for (; c < n && c < this.count; c++) out[c] = this.info[c]; out.count = c; return out; }",
                serde_json::json!([{ "value": limit }]),
                true,
            )
            .await?;
            let found = indexed_values(&found["result"]["value"], limit);
            element_ids.extend(array_object_ids(client, session_id, &array_id, found.len()).await?);
            info.extend(found);
            let hosts = call(
                array_id,
                "function() { return this.hosts; }",
                serde_json::json!([]),
                false,
            )
            .await?;
            let Some(hosts_id) = object_id(&hosts["result"]) else {
                continue;
            };
            for host in array_object_ids(client, session_id, &hosts_id, VIEWPORT_MAX_HOSTS)
                .await?
                .into_iter()
                .flatten()
            {
                closed_roots.extend(closed_shadow_roots(client, session_id, &host).await);
            }
        }
        if closed_roots.is_empty() {
            break;
        }
        roots = closed_roots
            .into_iter()
            .map(|(root, user_agent)| (root, true, user_agent))
            .collect();
    }

    let ax_nodes = futures_util::future::join_all(element_ids.iter().map(|id| async move {
        let result: GetFullAXTreeResult = client
            .send_command_typed(
                "Accessibility.getPartialAXTree",
                &serde_json::json!({ "objectId": id.as_deref()?, "fetchRelatives": false }),
                Some(session_id),
            )
            .await
            .ok()?;
        result.nodes.into_iter().next()
    }))
    .await;

    let mut nodes = Vec::new();
    let mut rects = Vec::new();
    let mut cursor_elements = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    for (mut node, item) in ax_nodes
        .into_iter()
        .zip(&info)
        .filter_map(|(n, i)| Some((n?, i)))
    {
        let Some(bid) = node.backend_d_o_m_node_id.filter(|bid| seen.insert(*bid)) else {
            continue;
        };
        if item["cursor"].is_object() {
            cursor_elements.insert(bid, cursor_info_from_json(&item["cursor"]));
        }
        let rect = |k: usize| item["rect"][k].as_f64().unwrap_or(0.0);
        rects.push([rect(0), rect(1), rect(2), rect(3)]);
        node.child_ids = None;
        nodes.push(node);
    }
    if nodes.is_empty() && !info.is_empty() {
        return Err("Accessibility data is unavailable for the viewport elements".to_string());
    }
    Ok((nodes, rects, cursor_elements))
}

/// Reads `count` entries keyed "0", "1", ... from an object returned by a page script, at most `limit`.
fn indexed_values(value: &Value, limit: usize) -> Vec<Value> {
    let count = value["count"].as_u64().unwrap_or(0) as usize;
    (0..count.min(limit))
        .map_while(|i| value.get(i.to_string()).cloned())
        .collect()
}

fn object_id(remote: &Value) -> Option<String> {
    remote["objectId"].as_str().map(str::to_string)
}

/// Returns the element handles of a remote array at positions below `len`; the page cannot grow the result.
async fn array_object_ids(
    client: &CdpClient,
    session_id: &str,
    array_id: &str,
    len: usize,
) -> Result<Vec<Option<String>>, String> {
    let props = client
        .send_command(
            "Runtime.getProperties",
            Some(serde_json::json!({ "objectId": array_id, "ownProperties": true })),
            Some(session_id),
        )
        .await?;
    let mut ids = vec![None; len];
    for prop in props["result"].as_array().into_iter().flatten() {
        if let Some(slot) = prop["name"]
            .as_str()
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|i| ids.get_mut(i))
        {
            *slot = object_id(&prop["value"]);
        }
    }
    Ok(ids)
}

/// Resolves the closed and browser-internal shadow roots of one host, which page scripts cannot reach.
/// Each root comes with whether it is a user-agent root, whose controls get their roles from the browser.
async fn closed_shadow_roots(
    client: &CdpClient,
    session_id: &str,
    host: &str,
) -> Vec<(String, bool)> {
    let Ok(described) = client
        .send_command(
            "DOM.describeNode",
            Some(serde_json::json!({ "objectId": host, "depth": 1, "pierce": true })),
            Some(session_id),
        )
        .await
    else {
        return Vec::new();
    };
    let mut roots = Vec::new();
    for root in described["node"]["shadowRoots"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let user_agent = root["shadowRootType"] == "user-agent";
        if root["shadowRootType"] != "closed" && !user_agent {
            continue;
        }
        if let Ok(resolved) = client
            .send_command(
                "DOM.resolveNode",
                Some(serde_json::json!({
                    "backendNodeId": root["backendNodeId"],
                    "objectGroup": VIEWPORT_OBJECT_GROUP,
                })),
                Some(session_id),
            )
            .await
        {
            roots.extend(object_id(&resolved["object"]).map(|id| (id, user_agent)));
        }
    }
    roots
}

/// Resolve the child frame ID for an iframe element given its backendNodeId.
fn frame_loader<'a>(tree: &'a Value, frame_id: Option<&str>) -> Option<&'a str> {
    let frame = &tree["frame"];
    if frame_id.is_none() || frame["id"].as_str() == frame_id {
        return frame["loaderId"].as_str().filter(|id| !id.is_empty());
    }
    tree["childFrames"]
        .as_array()?
        .iter()
        .find_map(|child| frame_loader(child, frame_id))
}

async fn resolve_iframe_frame_id(
    client: &CdpClient,
    session_id: &str,
    backend_node_id: i64,
) -> Result<String, String> {
    // depth: 1 ensures contentDocument is included in the response
    let describe: Value = client
        .send_command(
            "DOM.describeNode",
            Some(serde_json::json!({ "backendNodeId": backend_node_id, "depth": 1 })),
            Some(session_id),
        )
        .await?;

    // Try contentDocument.frameId first (standard for iframes)
    if let Some(frame_id) = describe
        .get("node")
        .and_then(|n| n.get("contentDocument"))
        .and_then(|cd| cd.get("frameId"))
        .and_then(|v| v.as_str())
    {
        return Ok(frame_id.to_string());
    }

    // Fallback: the node itself may have a frameId
    describe
        .get("node")
        .and_then(|n| n.get("frameId"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Could not resolve iframe frame ID".to_string())
}

/// Defines `__abCursorInfo(el)`, which returns the cursor-interactive details of one element or null.
/// Shared by the full-page scan and the viewport scan so both apply the same rules.
const CURSOR_INFO_JS: &str = r#"
    var __abInteractiveRoles = {
        'button':1, 'link':1, 'textbox':1, 'checkbox':1, 'radio':1, 'combobox':1, 'listbox':1,
        'menuitem':1, 'menuitemcheckbox':1, 'menuitemradio':1, 'option':1, 'searchbox':1,
        'slider':1, 'spinbutton':1, 'switch':1, 'tab':1, 'treeitem':1
    };
    var __abInteractiveTags = {
        'a':1, 'button':1, 'input':1, 'select':1, 'textarea':1, 'details':1, 'summary':1
    };
    function __abCursorInfo(el) {
        if (el.closest && el.closest('[hidden], [aria-hidden="true"]')) return null;

        var tagName = el.tagName.toLowerCase();
        if (__abInteractiveTags[tagName]) return null;

        var role = el.getAttribute('role');
        if (role && __abInteractiveRoles[role.toLowerCase()]) return null;

        var computedStyle = getComputedStyle(el);
        var hasCursorPointer = computedStyle.cursor === 'pointer';
        var hasOnClick = el.hasAttribute('onclick') || el.onclick !== null;
        var tabIndex = el.getAttribute('tabindex');
        var hasTabIndex = tabIndex !== null && tabIndex !== '-1';
        var ce = el.getAttribute('contenteditable');
        var isEditable = ce === '' || ce === 'true';

        if (!hasCursorPointer && !hasOnClick && !hasTabIndex && !isEditable) return null;

        if (hasCursorPointer && !hasOnClick && !hasTabIndex && !isEditable) {
            var parent = el.parentElement;
            if (parent && getComputedStyle(parent).cursor === 'pointer') return null;
        }

        var text = (el.textContent || '').trim().slice(0, 100);

        var rect = el.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) return null;

        var hiddenInputType = null;
        var hiddenInputChecked = null;
        var hiddenInput = el.querySelector('input[type="radio"], input[type="checkbox"]');
        if (hiddenInput) {
            var hiddenInputStyle = getComputedStyle(hiddenInput);
            var isInputHidden = hiddenInputStyle.display === 'none' || hiddenInputStyle.visibility === 'hidden' || hiddenInput.hidden;
            if (isInputHidden) {
                hiddenInputType = hiddenInput.type;
                hiddenInputChecked = hiddenInput.indeterminate ? 'mixed' : String(hiddenInput.checked);
            }
        }

        var kind = hasCursorPointer || hasOnClick ? 'clickable' : isEditable ? 'editable' : 'focusable';
        var hints = (hasCursorPointer ? ',cursor:pointer' : '') + (hasOnClick ? ',onclick' : '') +
            (hasTabIndex ? ',tabindex' : '') + (isEditable ? ',contenteditable' : '');

        return {
            text: text,
            kind: kind,
            hints: hints,
            hiddenInputType: hiddenInputType,
            hiddenInputChecked: hiddenInputChecked
        };
    }
"#;

async fn find_cursor_interactive_elements(
    client: &CdpClient,
    session_id: &str,
) -> Result<HashMap<i64, CursorElementInfo>, String> {
    // Single JS evaluation that matches the v0.19.0 Node.js findCursorInteractiveElements():
    // - Uses querySelectorAll('*') to walk all elements
    // - Checks getComputedStyle(el).cursor === 'pointer'
    // - Checks onclick attribute/handler and tabindex
    // - Skips interactiveTags (a, button, input, select, textarea, details, summary)
    // - Skips elements with interactive ARIA roles
    // - Deduplicates inherited cursor:pointer from parent
    // - Skips empty text and zero-size elements
    // - Tags each matched element with data-__ab-ci for batch backendNodeId resolution
    let js = format!(
        r#"
(function() {{
    var results = Object.create(null), count = 0;
    results.count = 0;
    if (!document.body) return results;
    {CURSOR_INFO_JS}
    var allElements = document.body.querySelectorAll('*');
    for (var i = 0; i < allElements.length; i++) {{
        var el = allElements[i];
        var info = __abCursorInfo(el);
        if (!info) continue;
        el.setAttribute('data-__ab-ci', String(count));
        results[count++] = info;
    }}
    results.count = count;
    return results;
}})()
"#
    );

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

    let elements = indexed_values(&result.result.value.unwrap_or(Value::Null), usize::MAX);

    if elements.is_empty() {
        return Ok(HashMap::new());
    }

    // Batch-resolve backendNodeIds: use DOM.getDocument to get the root nodeId,
    // then DOM.querySelectorAll to get all tagged elements in a single call.
    let doc: Value = client
        .send_command(
            "DOM.getDocument",
            Some(serde_json::json!({ "depth": 0 })),
            Some(session_id),
        )
        .await?;

    let root_node_id = doc
        .get("root")
        .and_then(|r| r.get("nodeId"))
        .and_then(|v| v.as_i64())
        .ok_or("DOM.getDocument did not return root nodeId")?;

    let query_result: Value = client
        .send_command(
            "DOM.querySelectorAll",
            Some(serde_json::json!({
                "nodeId": root_node_id,
                "selector": "[data-__ab-ci]"
            })),
            Some(session_id),
        )
        .await?;

    let node_ids: Vec<i64> = query_result
        .get("nodeIds")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    // Resolve backendNodeIds for each DOM node using concurrent CDP calls.
    let describe_futures: Vec<_> = node_ids
        .iter()
        .map(|&node_id| {
            client.send_command(
                "DOM.describeNode",
                Some(serde_json::json!({ "nodeId": node_id })),
                Some(session_id),
            )
        })
        .collect();

    let describe_results = futures_util::future::join_all(describe_futures).await;

    // Build a map from data-__ab-ci index to backendNodeId.
    let mut idx_to_backend: HashMap<usize, i64> = HashMap::new();
    for desc in describe_results.into_iter().flatten() {
        let backend_id = desc
            .get("node")
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64());
        let ci_attr = desc
            .get("node")
            .and_then(|n| n.get("attributes"))
            .and_then(|a| a.as_array())
            .and_then(|attrs| {
                // attributes is a flat array: [name, value, name, value, ...]
                attrs
                    .iter()
                    .enumerate()
                    .find(|(_, v)| v.as_str() == Some("data-__ab-ci"))
                    .and_then(|(i, _)| attrs.get(i + 1))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<usize>().ok())
            });
        if let (Some(bid), Some(idx)) = (backend_id, ci_attr) {
            idx_to_backend.insert(idx, bid);
        }
    }

    // Clean up the data attributes we injected for backendNodeId resolution.
    let cleanup_js =
        r#"(function(){ var els = document.querySelectorAll('[data-__ab-ci]'); for (var i = 0; i < els.length; i++) els[i].removeAttribute('data-__ab-ci'); return els.length; })()"#.to_string();
    if let Err(e) = client
        .send_command_typed::<EvaluateParams, EvaluateResult>(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: cleanup_js,
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await
    {
        eprintln!("[agent-browser] Warning: failed to clean up data-__ab-ci attributes: {e}");
    }

    // Build the map
    let mut map: HashMap<i64, CursorElementInfo> = HashMap::new();
    for (i, elem) in elements.iter().enumerate() {
        if let Some(bid) = idx_to_backend.get(&i).copied() {
            map.insert(bid, cursor_info_from_json(elem));
        }
    }

    Ok(map)
}

/// The JSON shape returned by `__abCursorInfo`.
#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct CursorInfoJson {
    text: String,
    kind: String,
    hints: String,
    hidden_input_type: Option<String>,
    hidden_input_checked: Option<String>,
}

/// Converts one `__abCursorInfo` result into the snapshot's cursor-interactive record.
fn cursor_info_from_json(elem: &Value) -> CursorElementInfo {
    let info: CursorInfoJson = serde_json::from_value(elem.clone()).unwrap_or_default();
    CursorElementInfo {
        kind: info.kind,
        hints: info
            .hints
            .split(',')
            .filter(|hint| !hint.is_empty())
            .map(str::to_string)
            .collect(),
        text: info.text.trim().to_string(),
        hidden_input_kind: info
            .hidden_input_type
            .as_deref()
            .and_then(HiddenInputKind::parse),
        hidden_input_checked: info.hidden_input_checked,
    }
}

/// Promote LabelText/generic nodes that wrap a hidden radio/checkbox input.
/// When a `<label>` contains a `display:none` `<input type="radio">`, Chrome excludes
/// the input from the AX tree entirely, leaving only the label with role="LabelText"
/// and an empty name. We detect these via cursor-interactive scanning and promote
/// the label to the correct input role so consumers see role="radio" in data.refs.
fn promote_hidden_inputs(
    tree_nodes: &mut [TreeNode],
    cursor_elements: &HashMap<i64, CursorElementInfo>,
) {
    for node in tree_nodes.iter_mut() {
        if !matches!(node.role.as_str(), "LabelText" | "generic") {
            continue;
        }
        let cursor_info = match node
            .backend_node_id
            .and_then(|bid| cursor_elements.get(&bid))
        {
            Some(info) => info,
            None => continue,
        };
        if let Some(input_kind) = cursor_info.hidden_input_kind {
            node.role = input_kind.as_role().to_string();
            if node.name.is_empty() && !cursor_info.text.is_empty() {
                node.name = cursor_info.text.clone();
            }
            if let Some(ref checked) = cursor_info.hidden_input_checked {
                node.checked = Some(checked.clone());
            }
        }
    }
}

fn build_tree(nodes: &[AXNode]) -> (Vec<TreeNode>, Vec<usize>) {
    let mut tree_nodes: Vec<TreeNode> = Vec::with_capacity(nodes.len());
    let mut id_to_idx: HashMap<String, usize> = HashMap::new();

    for (i, node) in nodes.iter().enumerate() {
        let role = extract_ax_string(&node.role);
        let name = extract_ax_string(&node.name);
        let value_text = extract_ax_string_opt(&node.value);

        let (level, checked, expanded, selected, disabled, required) =
            extract_properties(&node.properties);

        if (node.ignored.unwrap_or(false) && role != "RootWebArea") || role == "InlineTextBox" {
            tree_nodes.push(TreeNode::empty());
            id_to_idx.insert(node.node_id.clone(), i);
            continue;
        }

        tree_nodes.push(TreeNode {
            role,
            name,
            level,
            checked,
            expanded,
            selected,
            disabled,
            required,
            value_text,
            backend_node_id: node.backend_d_o_m_node_id,
            children: Vec::new(),
            parent_idx: None,
            has_ref: false,
            ref_id: None,
            depth: 0,
            cursor_info: None,
            url: None,
        });
        id_to_idx.insert(node.node_id.clone(), i);
    }

    // Build parent-child relationships
    for (i, node) in nodes.iter().enumerate() {
        if let Some(ref child_ids) = node.child_ids {
            for cid in child_ids {
                if let Some(&child_idx) = id_to_idx.get(cid) {
                    tree_nodes[i].children.push(child_idx);
                    tree_nodes[child_idx].parent_idx = Some(i);
                }
            }
        }
    }

    // Process StaticText aggregation
    for i in 0..tree_nodes.len() {
        if tree_nodes[i].role.is_empty() || tree_nodes[i].children.is_empty() {
            continue;
        }

        let children_indices: Vec<usize> = tree_nodes[i].children.clone();

        // Continuous StaticText nodes at the same level are an artifact of HTML structure rather than semantic meaning.
        // They typically represent a single continuous piece of text on the page that was split due to inline elements, formatting tags, or other structural reasons.
        // Thus, continuous StaticText children are aggregated into the first one.
        let mut start = 0;
        while start < children_indices.len() {
            // Skip non-StaticText nodes
            if tree_nodes[children_indices[start]].role != "StaticText" {
                start += 1;
                continue;
            }

            // Find the end of the current StaticText sequence
            let mut end = start + 1;
            while end < children_indices.len()
                && tree_nodes[children_indices[end]].role == "StaticText"
            {
                end += 1;
            }

            // If we have a sequence of at least two StaticText
            if end > start + 1 {
                // Collect and aggregate all names from the sequence
                let aggregated_name: String = (start..end)
                    .map(|idx| tree_nodes[children_indices[idx]].name.clone())
                    .collect();
                // Always aggregate into the first node of the sequence
                tree_nodes[children_indices[start]].name = aggregated_name;
                // Clear the rest of the nodes in the sequence (from start+1 to end-1)
                for j in (start + 1)..end {
                    tree_nodes[children_indices[j]].clear();
                }
            }
            start = end;
        }

        // Deduplicate redundant StaticText
        if children_indices.len() == 1
            && tree_nodes[children_indices[0]].role == "StaticText"
            && tree_nodes[i].name == tree_nodes[children_indices[0]].name
        {
            tree_nodes[children_indices[0]].clear();
        }
    }

    // Set depths
    let mut root_indices = Vec::new();
    let children_exist: Vec<bool> = nodes.iter().map(|_| false).collect();
    let mut is_child = children_exist;
    for node in &tree_nodes {
        for &child in &node.children {
            is_child[child] = true;
        }
    }
    for (i, &is_c) in is_child.iter().enumerate() {
        if !is_c {
            root_indices.push(i);
        }
    }

    fn set_depth(nodes: &mut [TreeNode], idx: usize, depth: usize) {
        nodes[idx].depth = depth;
        let children: Vec<usize> = nodes[idx].children.clone();
        for child_idx in children {
            set_depth(nodes, child_idx, depth + 1);
        }
    }

    for &root in &root_indices {
        set_depth(&mut tree_nodes, root, 0);
    }

    (tree_nodes, root_indices)
}

fn render_tree(
    nodes: &[TreeNode],
    idx: usize,
    indent: usize,
    output: &mut String,
    options: &SnapshotOptions,
) {
    let node = &nodes[idx];

    // Reduce unnecessary indentation and rendering
    if node.role.is_empty()
        || (node.role == "generic" && !node.has_ref && node.children.len() <= 1)
        || (node.role == "StaticText" && normalize_snapshot_name(&node.name).trim().is_empty())
    {
        // Ignored node -- still render children
        for &child in &node.children {
            render_tree(nodes, child, indent, output, options);
        }
        return;
    }

    if let Some(max_depth) = options.depth {
        if indent > max_depth {
            return;
        }
    }

    let role = &node.role;

    // Skip root WebArea wrapper
    if role == "RootWebArea" || role == "WebArea" {
        for &child in &node.children {
            render_tree(nodes, child, indent, output, options);
        }
        return;
    }

    if options.interactive && !node.has_ref {
        // In interactive mode, skip non-interactive but render children
        for &child in &node.children {
            render_tree(nodes, child, indent, output, options);
        }
        return;
    }

    let prefix = "  ".repeat(indent);
    let mut line = format!("{}- {}", prefix, role);

    // Use ARIA name if available, only fall back to cursor-interactive textContent in interactive mode since their visible text in child nodes is filtered out
    let unescaped_display_name = if !node.name.is_empty() {
        &node.name
    } else if options.interactive {
        if let Some(ref ci) = node.cursor_info {
            &ci.text
        } else {
            &node.name
        }
    } else {
        &node.name
    };
    if !unescaped_display_name.is_empty() {
        if let Ok(display_name) =
            serde_json::to_string(&normalize_snapshot_name(unescaped_display_name))
        {
            line.push_str(&format!(" {}", display_name));
        }
    }

    // Properties
    let mut attrs = Vec::new();

    if let Some(level) = node.level {
        attrs.push(format!("level={}", level));
    }
    if let Some(ref checked) = node.checked {
        attrs.push(format!("checked={}", checked));
    }
    if let Some(expanded) = node.expanded {
        attrs.push(format!("expanded={}", expanded));
    }
    if let Some(selected) = node.selected {
        if selected {
            attrs.push("selected".to_string());
        }
    }
    if let Some(disabled) = node.disabled {
        if disabled {
            attrs.push("disabled".to_string());
        }
    }
    if let Some(required) = node.required {
        if required {
            attrs.push("required".to_string());
        }
    }

    if let Some(ref ref_id) = node.ref_id {
        attrs.push(format!("ref={}", ref_id));
    }

    if let Some(ref url) = node.url {
        attrs.push(format!("url={}", url));
    }

    if !attrs.is_empty() {
        line.push_str(&format!(" [{}]", attrs.join(", ")));
    }

    // Add cursor-interactive kind & hints
    if let Some(ref cursor_info) = node.cursor_info {
        line.push_str(&format!(
            " {} [{}]",
            cursor_info.kind,
            cursor_info.hints.join(", ")
        ));
    }

    // Value
    if let Some(ref val) = node.value_text {
        if !val.is_empty() && val != &node.name {
            line.push_str(&format!(": {}", val));
        }
    }

    output.push_str(&line);
    output.push('\n');

    for &child in &node.children {
        render_tree(nodes, child, indent + 1, output, options);
    }
}

fn compact_tree(tree: &str, interactive: bool) -> String {
    let lines: Vec<&str> = tree.lines().collect();
    if lines.is_empty() {
        return String::new();
    }

    let mut keep = vec![false; lines.len()];

    for (i, line) in lines.iter().enumerate() {
        if line.contains("ref=") || line.contains(": ") {
            keep[i] = true;
            // Mark ancestors
            let my_indent = count_indent(line);
            for j in (0..i).rev() {
                let ancestor_indent = count_indent(lines[j]);
                if ancestor_indent < my_indent {
                    keep[j] = true;
                    if ancestor_indent == 0 {
                        break;
                    }
                }
            }
        }
    }

    let result: Vec<&str> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| keep[*i])
        .map(|(_, line)| *line)
        .collect();

    let output = result.join("\n");
    if output.trim().is_empty() && interactive {
        return "(no interactive elements)".to_string();
    }
    output
}

fn count_indent(line: &str) -> usize {
    let trimmed = line.trim_start();
    (line.len() - trimmed.len()) / 2
}

fn extract_ax_string(value: &Option<AXValue>) -> String {
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

fn extract_ax_string_opt(value: &Option<AXValue>) -> Option<String> {
    match value {
        Some(v) => match &v.value {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        },
        None => None,
    }
}

type NodeProperties = (
    Option<i64>,    // level
    Option<String>, // checked
    Option<bool>,   // expanded
    Option<bool>,   // selected
    Option<bool>,   // disabled
    Option<bool>,   // required
);

fn extract_properties(props: &Option<Vec<AXProperty>>) -> NodeProperties {
    let mut level = None;
    let mut checked = None;
    let mut expanded = None;
    let mut selected = None;
    let mut disabled = None;
    let mut required = None;

    if let Some(properties) = props {
        for prop in properties {
            match prop.name.as_str() {
                "level" => {
                    level = prop.value.value.as_ref().and_then(|v| v.as_i64());
                }
                "checked" => {
                    checked = prop.value.value.as_ref().map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Bool(b) => b.to_string(),
                        _ => "false".to_string(),
                    });
                }
                "expanded" => {
                    expanded = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                "selected" => {
                    selected = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                "disabled" => {
                    disabled = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                "required" => {
                    required = prop.value.value.as_ref().and_then(|v| v.as_bool());
                }
                _ => {}
            }
        }
    }

    (level, checked, expanded, selected, disabled, required)
}

/// Build the set of texts to de-duplicate cursor-interactive elements against.
///
/// All ref-bearing ARIA tree nodes have their names stored in `ref_map` during
/// tree construction, so the ref-map entries are the single source of truth.
/// This avoids fragile parsing of the rendered tree text.
fn build_dedup_set(ref_map: &RefMap) -> std::collections::HashSet<String> {
    ref_map
        .entries_sorted()
        .into_iter()
        .filter(|(_, entry)| !entry.name.is_empty())
        .map(|(_, entry)| entry.name.to_lowercase())
        .collect()
}

/// Recursively collect all `backendNodeId` values from a CDP DOM node tree
/// (as returned by `DOM.describeNode` with `depth: -1`).
fn collect_backend_node_ids(node: &Value, ids: &mut std::collections::HashSet<i64>) {
    if let Some(id) = node.get("backendNodeId").and_then(|v| v.as_i64()) {
        ids.insert(id);
    }
    if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
        for child in children {
            collect_backend_node_ids(child, ids);
        }
    }
    // Shadow DOM and content documents
    if let Some(shadow) = node.get("shadowRoots").and_then(|v| v.as_array()) {
        for child in shadow {
            collect_backend_node_ids(child, ids);
        }
    }
    if let Some(doc) = node.get("contentDocument") {
        collect_backend_node_ids(doc, ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_values_ignores_page_supplied_counts_and_extra_keys() {
        let value =
            serde_json::json!({ "count": 4294967295u64, "0": "a", "1": "b", "4294967290": "x" });
        assert_eq!(indexed_values(&value, usize::MAX), vec!["a", "b"]);
        assert_eq!(indexed_values(&value, 1), vec!["a"]);
        assert!(indexed_values(&serde_json::json!({ "0": "a" }), 10).is_empty());
        assert!(indexed_values(&Value::Null, 10).is_empty());
    }

    #[test]
    fn cursor_info_hints_keep_order_and_skip_empty() {
        let info = cursor_info_from_json(&serde_json::json!({
            "text": "  Card  ",
            "kind": "clickable",
            "hints": ",cursor:pointer,onclick,tabindex",
        }));
        assert_eq!(info.hints, vec!["cursor:pointer", "onclick", "tabindex"]);
        assert_eq!(info.text, "Card");
        assert!(cursor_info_from_json(&serde_json::json!({ "hints": "" }))
            .hints
            .is_empty());
    }

    #[test]
    fn test_normalize_snapshot_name_preserves_word_boundaries() {
        assert_eq!(
            normalize_snapshot_name("Capital\u{00A0}Federal"),
            "Capital Federal"
        );
        assert_eq!(normalize_snapshot_name("zero\u{200B}width"), "zerowidth");
    }

    #[test]
    fn test_interactive_roles() {
        assert!(INTERACTIVE_ROLES.contains(&"button"));
        assert!(INTERACTIVE_ROLES.contains(&"textbox"));
        assert!(!INTERACTIVE_ROLES.contains(&"heading"));
    }

    #[test]
    fn test_content_roles() {
        assert!(CONTENT_ROLES.contains(&"heading"));
        assert!(!CONTENT_ROLES.contains(&"button"));
    }

    #[test]
    fn test_compact_tree_basic() {
        let tree = "- navigation\n  - link \"Home\" [ref=e1]\n  - link \"About\" [ref=e2]\n- main\n  - heading \"Title\"\n  - paragraph\n    - text: Hello\n";
        let result = compact_tree(tree, false);
        assert!(result.contains("[ref=e1]"));
        assert!(result.contains("[ref=e2]"));
        assert!(result.contains("Hello"));
    }

    #[test]
    fn test_compact_tree_radio_checkbox() {
        // Radio/checkbox lines have attributes before ref (e.g. [checked=false, ref=e1])
        // so "ref=" appears without a leading "[" — compact_tree must still keep them.
        let tree = "- form\n  - radio \"Single unit\" [checked=false, ref=e1]\n  - checkbox \"I agree\" [checked=false, ref=e2]\n  - button \"Submit\" [ref=e3]\n";
        let result = compact_tree(tree, true);
        assert!(
            result.contains("radio \"Single unit\""),
            "radio should be kept"
        );
        assert!(
            result.contains("checkbox \"I agree\""),
            "checkbox should be kept"
        );
        assert!(
            result.contains("button \"Submit\""),
            "button should be kept"
        );
    }

    #[test]
    fn test_compact_tree_empty_interactive() {
        let result = compact_tree("- generic\n", true);
        assert_eq!(result, "(no interactive elements)");
    }

    #[test]
    fn test_count_indent() {
        assert_eq!(count_indent("- heading"), 0);
        assert_eq!(count_indent("  - link"), 1);
        assert_eq!(count_indent("    - text"), 2);
    }

    #[test]
    fn test_role_name_tracker() {
        let mut tracker = RoleNameTracker::new();
        assert_eq!(tracker.track("button", "Submit"), 0);
        assert_eq!(tracker.track("button", "Submit"), 1);
        assert_eq!(tracker.track("button", "Cancel"), 0);

        let dups = tracker.get_duplicates();
        assert!(dups.contains_key("button:Submit"));
        assert!(!dups.contains_key("button:Cancel"));
    }

    // -----------------------------------------------------------------------
    // Cursor-interactive text dedup (Issue #841 regression guard)
    // -----------------------------------------------------------------------

    #[test]
    fn test_dedup_set_from_ref_map_names() {
        let mut ref_map = RefMap::new();
        ref_map.add("e1".to_string(), Some(1), "link", "Example Link", None);
        ref_map.add("e2".to_string(), Some(2), "button", "Submit", None);

        let set = build_dedup_set(&ref_map);
        assert!(set.contains("example link"));
        assert!(set.contains("submit"));
        assert!(!set.contains("other text"));
    }

    #[test]
    fn test_dedup_set_case_insensitive() {
        let mut ref_map = RefMap::new();
        ref_map.add("e1".to_string(), Some(1), "button", "Submit Form", None);

        let set = build_dedup_set(&ref_map);
        assert!(set.contains("submit form"));
        assert!(!set.contains("Submit Form"));
    }

    #[test]
    fn test_dedup_set_empty_inputs() {
        let ref_map = RefMap::new();
        let set = build_dedup_set(&ref_map);
        assert!(set.is_empty());
    }

    #[test]
    fn test_dedup_set_skips_empty_names() {
        let mut ref_map = RefMap::new();
        ref_map.add("e1".to_string(), Some(1), "generic", "", None);
        ref_map.add("e2".to_string(), Some(2), "button", "OK", None);

        let set = build_dedup_set(&ref_map);
        assert_eq!(set.len(), 1);
        assert!(set.contains("ok"));
    }

    // -----------------------------------------------------------------------
    // resolve_ax_session tests (Issue #925 regression guard)
    // Cross-origin iframes must use a dedicated session without frameId.
    // Same-origin iframes must use the parent session with frameId.
    // -----------------------------------------------------------------------

    #[test]
    fn test_cross_origin_iframe_uses_dedicated_session() {
        let parent_session = "parent-session";
        let iframe_frame_id = "cross-origin-iframe-frame";
        let iframe_session = "cross-origin-iframe-session";

        let mut iframe_sessions = HashMap::new();
        iframe_sessions.insert(iframe_frame_id.to_string(), iframe_session.to_string());

        let (params, session) =
            resolve_ax_session(Some(iframe_frame_id), parent_session, &iframe_sessions);

        assert_eq!(session, iframe_session);
        assert_eq!(params, serde_json::json!({}));
    }

    #[test]
    fn test_same_origin_iframe_uses_parent_session_with_frame_id() {
        let parent_session = "parent-session";
        let iframe_frame_id = "same-origin-iframe-frame";
        let iframe_sessions = HashMap::new();

        let (params, session) =
            resolve_ax_session(Some(iframe_frame_id), parent_session, &iframe_sessions);

        assert_eq!(session, parent_session);
        assert_eq!(params, serde_json::json!({ "frameId": iframe_frame_id }));
    }

    #[test]
    fn test_main_frame_uses_parent_session() {
        let parent_session = "parent-session";
        let iframe_sessions = HashMap::new();

        let (params, session) = resolve_ax_session(None, parent_session, &iframe_sessions);

        assert_eq!(session, parent_session);
        assert_eq!(params, serde_json::json!({}));
    }

    // -----------------------------------------------------------------------
    // promote_hidden_inputs
    // -----------------------------------------------------------------------

    fn make_node(role: &str, name: &str, backend_node_id: Option<i64>) -> TreeNode {
        let mut node = TreeNode::empty();
        node.role = role.to_string();
        node.name = name.to_string();
        node.backend_node_id = backend_node_id;
        node
    }

    fn make_cursor_info(
        hidden_kind: Option<HiddenInputKind>,
        hidden_checked: Option<&str>,
        text: &str,
    ) -> CursorElementInfo {
        CursorElementInfo {
            kind: "clickable".to_string(),
            hints: vec!["cursor:pointer".to_string()],
            text: text.to_string(),
            hidden_input_kind: hidden_kind,
            hidden_input_checked: hidden_checked.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_promote_label_with_hidden_radio() {
        let mut nodes = vec![
            make_node("LabelText", "", Some(1)),
            make_node("LabelText", "", Some(2)),
            make_node("button", "Submit", Some(3)),
        ];
        let mut cursor_elements = HashMap::new();
        cursor_elements.insert(
            1,
            make_cursor_info(Some(HiddenInputKind::Radio), Some("false"), "Option A"),
        );
        cursor_elements.insert(
            2,
            make_cursor_info(Some(HiddenInputKind::Radio), Some("true"), "Option B"),
        );

        promote_hidden_inputs(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "radio");
        assert_eq!(nodes[0].name, "Option A");
        assert_eq!(nodes[0].checked, Some("false".to_string()));
        assert_eq!(nodes[1].role, "radio");
        assert_eq!(nodes[1].name, "Option B");
        assert_eq!(nodes[1].checked, Some("true".to_string()));
        // button should be untouched
        assert_eq!(nodes[2].role, "button");
    }

    #[test]
    fn test_promote_preserves_existing_name() {
        // If AX tree already has a name, don't overwrite with textContent
        let mut nodes = vec![make_node("LabelText", "AX Name", Some(1))];
        let mut cursor_elements = HashMap::new();
        cursor_elements.insert(
            1,
            make_cursor_info(Some(HiddenInputKind::Radio), Some("false"), "Text Content"),
        );

        promote_hidden_inputs(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "radio");
        assert_eq!(nodes[0].name, "AX Name"); // preserved, not overwritten
    }

    #[test]
    fn test_promote_skips_without_hidden_input() {
        // Cursor-interactive label WITHOUT a hidden input should not be promoted
        let mut nodes = vec![make_node("LabelText", "", Some(1))];
        let mut cursor_elements = HashMap::new();
        cursor_elements.insert(1, make_cursor_info(None, None, "Click me"));

        promote_hidden_inputs(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "LabelText"); // unchanged
    }
}
