use std::collections::HashMap;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::{
    AXNode, AXProperty, AXValue, EvaluateParams, EvaluateResult, GetFullAXTreeResult,
};
use super::element::{resolve_ax_session, RefMap};

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

const INVISIBLE_CHARS: &[char] = &[
    '\u{FEFF}', // BOM / Zero Width No-Break Space
    '\u{200B}', // Zero Width Space
    '\u{200C}', // Zero Width Non-Joiner
    '\u{200D}', // Zero Width Joiner
    '\u{2060}', // Word Joiner
    '\u{00A0}', // Non-Breaking Space (&nbsp;)
];

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
    entries: Vec<(usize, String)>,
}

impl RoleNameTracker {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
            entries: Vec::new(),
        }
    }

    fn track(&mut self, role: &str, name: &str, node_idx: usize) -> usize {
        let key = format!("{}:{}", role, name);
        let count = self.counts.entry(key.clone()).or_insert(0);
        let nth = *count;
        *count += 1;
        self.entries.push((node_idx, key));
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

            let object_id = result
                .result
                .object_id
                .ok_or_else(|| format!("Selector '{}' did not match any element", selector))?;

            // Request the full DOM subtree (depth: -1) so we can collect all
            // backendNodeIds that live under the matched element.
            let describe: Value = client
                .send_command(
                    "DOM.describeNode",
                    Some(serde_json::json!({ "objectId": object_id, "depth": -1 })),
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

        // An AX node is a "top-level" match if it is in the subtree but its
        // parent (in the AX tree) is not.
        let mut roots = Vec::new();
        for (idx, node) in tree_nodes.iter().enumerate() {
            if !in_subtree[idx] {
                continue;
            }
            let parent_in_subtree = node.parent_idx.is_some_and(|pidx| in_subtree[pidx]);
            if !parent_in_subtree {
                roots.push(idx);
            }
        }

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

    let mut tracker = RoleNameTracker::new();
    let mut next_ref: usize = ref_map.next_ref_num();

    let mut nodes_with_refs: Vec<(usize, usize)> = Vec::new();

    // Pre-collect cursor-interactive elements so we can mark them with refs during tree building.
    // Use effective_session_id (not session_id) so that when processing a cross-origin iframe
    // the JS runs inside the iframe's own CDP session and can see the iframe's DOM elements.
    // For the main frame effective_session_id == session_id, so there is no change in behaviour.
    let (cursor_elements, cursor_parent_bid_map) =
        find_cursor_interactive_elements(client, effective_session_id)
            .await
            .unwrap_or_else(|_| (HashMap::new(), HashMap::new()));

    // DEBUG: dump AX tree vs cursor element matching info to a file
    if let Ok(debug_path) = std::env::var("AB_DEBUG_CURSOR") {
        let debug_path = if debug_path.is_empty() { "/tmp/ab_debug.log".to_string() } else { debug_path };
        let mut lines = Vec::new();
        lines.push(format!("[DEBUG] frame_id={:?} tree_nodes={} cursor_elements={}", frame_id, tree_nodes.len(), cursor_elements.len()));
        let nodes_with_bid: Vec<_> = tree_nodes.iter()
            .filter(|n| n.backend_node_id.is_some())
            .map(|n| (n.role.clone(), n.name.clone(), n.backend_node_id.unwrap()))
            .take(20)
            .collect();
        lines.push(format!("[DEBUG] first 20 AX nodes with backendNodeId: {nodes_with_bid:?}"));
        let all_ax_bids: Vec<_> = tree_nodes.iter()
            .filter(|n| n.backend_node_id.is_some())
            .map(|n| n.backend_node_id.unwrap())
            .collect();
        lines.push(format!("[DEBUG] all {} AX backendNodeIds: {all_ax_bids:?}", all_ax_bids.len()));
        let cursor_bids: Vec<_> = cursor_elements.keys().cloned().collect();
        lines.push(format!("[DEBUG] all {} cursor backendNodeIds: {cursor_bids:?}", cursor_bids.len()));
        let matches: Vec<_> = tree_nodes.iter()
            .filter(|n| n.backend_node_id.map(|bid| cursor_elements.contains_key(&bid)).unwrap_or(false))
            .map(|n| (n.role.clone(), n.name.clone(), n.backend_node_id.unwrap()))
            .collect();
        lines.push(format!("[DEBUG] {} cursor-matched AX nodes: {matches:?}", matches.len()));
        let nodes_empty_role_count = tree_nodes.iter().filter(|n| n.role.is_empty() && n.backend_node_id.is_some()).count();
        let nodes_empty_role: Vec<_> = tree_nodes.iter()
            .filter(|n| n.role.is_empty() && n.backend_node_id.is_some())
            .map(|n| n.backend_node_id.unwrap())
            .take(10)
            .collect();
        lines.push(format!("[DEBUG] {nodes_empty_role_count} empty-role AX nodes with backendNodeId (first 10): {nodes_empty_role:?}"));
        let content = lines.join("\n") + "\n";
        let _ = std::fs::OpenOptions::new()
            .create(true).append(true)
            .open(&debug_path)
            .and_then(|mut f| { use std::io::Write; f.write_all(content.as_bytes()) });
    }

    promote_hidden_inputs(&mut tree_nodes, &cursor_elements);
    promote_cursor_matched_ignored(&mut tree_nodes, &cursor_elements);
    // Inject synthetic TreeNodes for cursor elements that Chrome completely omitted from the
    // accessibility tree (not even as ignored nodes).  Examples: CSS grid cells with
    // cursor:pointer that carry no ARIA role and no text — Chrome prunes them from the AX tree
    // entirely in some configurations.  We use the parent backendNodeId (resolved alongside the
    // cursor elements) to attach synthetic nodes to the correct AX parent.
    inject_cursor_orphans(&mut tree_nodes, &cursor_elements, &cursor_parent_bid_map);

    for (idx, node) in tree_nodes.iter().enumerate() {
        if should_assign_ref(&node.role, &node.name, node.backend_node_id, &cursor_elements) {
            let nth = tracker.track(&node.role, &node.name, idx);
            nodes_with_refs.push((idx, nth));
        }
    }

    let duplicates = tracker.get_duplicates();

    for (idx, nth) in &nodes_with_refs {
        let node = &tree_nodes[*idx];
        let key = format!("{}:{}", node.role, node.name);
        let actual_nth = if duplicates.contains_key(&key) {
            Some(*nth)
        } else {
            None
        };

        let ref_id = format!("e{}", next_ref);
        next_ref += 1;

        ref_map.add_with_frame(
            ref_id.clone(),
            tree_nodes[*idx].backend_node_id,
            &tree_nodes[*idx].role,
            &tree_nodes[*idx].name,
            actual_nth,
            frame_id,
        );

        tree_nodes[*idx].has_ref = true;
        tree_nodes[*idx].ref_id = Some(ref_id);
    }

    // Populate cursor_info for ref-bearing nodes
    for (idx, _) in &nodes_with_refs {
        if let Some(bid) = tree_nodes[*idx].backend_node_id {
            if let Some(cursor_info) = cursor_elements.get(&bid) {
                tree_nodes[*idx].cursor_info = Some((*cursor_info).clone());
            }
        }
    }

    ref_map.set_next_ref_num(next_ref);

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

/// Resolve the child frame ID for an iframe element given its backendNodeId.
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

/// Returns `true` when an AX tree node should be assigned a snapshot ref.
///
/// A ref is assigned when:
/// - The role is a known interactive role (`button`, `link`, `textbox`, …).
/// - The role is a content role (`heading`, `cell`, …) **and** the node has a
///   non-empty accessible name.
/// - The node's `backendNodeId` appears in `cursor_elements`, meaning it was
///   detected as cursor-interactive by [`find_cursor_interactive_elements`]
///   (e.g. a `<div onclick>` or an element with `cursor:pointer`).
///
/// This function exists as a standalone helper so that it can be unit-tested
/// independently of the async CDP calls in [`take_snapshot`].
fn should_assign_ref(
    role: &str,
    name: &str,
    backend_node_id: Option<i64>,
    cursor_elements: &HashMap<i64, CursorElementInfo>,
) -> bool {
    if INTERACTIVE_ROLES.contains(&role) {
        return true;
    }
    if CONTENT_ROLES.contains(&role) && !name.is_empty() {
        return true;
    }
    backend_node_id.is_some_and(|bid| cursor_elements.contains_key(&bid))
}

/// Returns two maps:
/// 1. `cursor_elements`: backendNodeId → CursorElementInfo for every cursor-interactive element
/// 2. `cursor_parent_bid_map`: backendNodeId → `Option<parent backendNodeId>`.  The parent
///    backend ID is used by `inject_cursor_orphans` to find the correct AX tree parent when a
///    cursor element is completely absent from Chrome's accessibility tree.
async fn find_cursor_interactive_elements(
    client: &CdpClient,
    session_id: &str,
) -> Result<(HashMap<i64, CursorElementInfo>, HashMap<i64, Option<i64>>), String> {
    // Single JS evaluation that matches the v0.19.0 Node.js findCursorInteractiveElements():
    // - Uses querySelectorAll('*') to walk all elements
    // - Checks getComputedStyle(el).cursor === 'pointer'
    // - Checks onclick attribute/handler and tabindex
    // - Skips interactiveTags (a, button, input, select, textarea, details, summary)
    // - Skips elements with interactive ARIA roles
    // - Deduplicates inherited cursor:pointer from parent
    // - Skips empty text and zero-size elements
    // - Tags each matched element with data-__ab-ci for batch backendNodeId resolution
    let js = r#"
(function() {
    var results = [];
    if (!document.body) return results;
    var parentIdxCounter = 0;

    var interactiveRoles = {
        'button':1, 'link':1, 'textbox':1, 'checkbox':1, 'radio':1, 'combobox':1, 'listbox':1,
        'menuitem':1, 'menuitemcheckbox':1, 'menuitemradio':1, 'option':1, 'searchbox':1,
        'slider':1, 'spinbutton':1, 'switch':1, 'tab':1, 'treeitem':1
    };
    var interactiveTags = {
        'a':1, 'button':1, 'input':1, 'select':1, 'textarea':1, 'details':1, 'summary':1
    };

    var allElements = document.body.querySelectorAll('*');
    for (var i = 0; i < allElements.length; i++) {
        var el = allElements[i];

        if (el.closest && el.closest('[hidden], [aria-hidden="true"]')) continue;

        var tagName = el.tagName.toLowerCase();
        if (interactiveTags[tagName]) continue;

        var role = el.getAttribute('role');
        if (role && interactiveRoles[role.toLowerCase()]) continue;

        var computedStyle = getComputedStyle(el);
        var hasCursorPointer = computedStyle.cursor === 'pointer';
        var hasOnClick = el.hasAttribute('onclick') || el.onclick !== null;
        var tabIndex = el.getAttribute('tabindex');
        var hasTabIndex = tabIndex !== null && tabIndex !== '-1';
        var ce = el.getAttribute('contenteditable');
        var isEditable = ce === '' || ce === 'true';

        if (!hasCursorPointer && !hasOnClick && !hasTabIndex && !isEditable) continue;

        // Skip elements that only inherit cursor:pointer from an ancestor
        if (hasCursorPointer && !hasOnClick && !hasTabIndex && !isEditable) {
            var parent = el.parentElement;
            if (parent && getComputedStyle(parent).cursor === 'pointer') continue;
        }

        var text = (el.textContent || '').trim().slice(0, 100);

        var rect = el.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) continue;

        // Detect hidden radio/checkbox inputs inside this element (common pattern:
        // <label> wrapping a display:none <input type="radio"> styled as a card).
        // Note: we only check display/visibility/hidden, NOT opacity:0 or sr-only,
        // because those inputs remain in Chrome's AX tree and already appear as
        // role="radio" without promotion.
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

        el.setAttribute('data-__ab-ci', String(results.length));
        // Tag the parent element so we can resolve its backendNodeId via a second
        // DOM.querySelectorAll call.  We cannot rely on DOM.describeNode returning parentId
        // because Chrome only emits it for nodes already resolved in the CDP session.
        var parentCiP = null;
        if (el.parentElement) {
            if (!el.parentElement.hasAttribute('data-__ab-ci-p')) {
                el.parentElement.setAttribute('data-__ab-ci-p', String(parentIdxCounter++));
            }
            parentCiP = el.parentElement.getAttribute('data-__ab-ci-p');
        }
        results.push({
            text: text,
            tagName: tagName,
            hasOnClick: hasOnClick,
            hasCursorPointer: hasCursorPointer,
            hasTabIndex: hasTabIndex,
            isEditable: isEditable,
            hiddenInputType: hiddenInputType,
            hiddenInputChecked: hiddenInputChecked,
            parentCiP: parentCiP
        });
    }
    return results;
})()
"#;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js.to_string(),
                return_by_value: Some(true),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    let elements: Vec<Value> = result
        .result
        .value
        .and_then(|v| serde_json::from_value::<Vec<Value>>(v).ok())
        .unwrap_or_default();

    if elements.is_empty() {
        return Ok((HashMap::new(), HashMap::new()));
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

    // Also query for the parent elements we tagged with data-__ab-ci-p in the JS above.
    // This lets us resolve parent backendNodeIds without relying on DOM.describeNode returning
    // parentId — which Chrome only emits when the parent has already been resolved in the CDP
    // session (it hasn't, because DOM.getDocument was called with depth=0).
    let parent_query_result = client
        .send_command(
            "DOM.querySelectorAll",
            Some(serde_json::json!({
                "nodeId": root_node_id,
                "selector": "[data-__ab-ci-p]"
            })),
            Some(session_id),
        )
        .await
        .unwrap_or_default();
    let parent_node_ids: Vec<i64> = parent_query_result
        .get("nodeIds")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();

    // DEBUG: log node_ids count
    if let Ok(debug_path) = std::env::var("AB_DEBUG_CURSOR") {
        let debug_path = if debug_path.is_empty() { "/tmp/ab_debug.log".to_string() } else { debug_path };
        let msg = format!("[DEBUG-CI] session={session_id} js_found={} dom_querySelectorAll={} parent_count={}\n",
            elements.len(), node_ids.len(), parent_node_ids.len());
        let _ = std::fs::OpenOptions::new()
            .create(true).append(true)
            .open(&debug_path)
            .and_then(|mut f| { use std::io::Write; f.write_all(msg.as_bytes()) });
    }

    // Resolve backendNodeIds for cursor elements and their parents in one concurrent batch.
    let cursor_describe_futures = node_ids.iter().map(|&node_id| {
        client.send_command(
            "DOM.describeNode",
            Some(serde_json::json!({ "nodeId": node_id })),
            Some(session_id),
        )
    });
    let parent_describe_futures = parent_node_ids.iter().map(|&node_id| {
        client.send_command(
            "DOM.describeNode",
            Some(serde_json::json!({ "nodeId": node_id })),
            Some(session_id),
        )
    });
    let (describe_results, parent_describe_results) = futures_util::future::join(
        futures_util::future::join_all(cursor_describe_futures),
        futures_util::future::join_all(parent_describe_futures),
    )
    .await;

    // Build data-__ab-ci index → backendNodeId for cursor elements.
    let mut idx_to_backend_id: HashMap<usize, i64> = HashMap::new();
    for desc in describe_results.into_iter().flatten() {
        let node = desc.get("node");
        let backend_id = node
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64());
        let ci_attr = node
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
            idx_to_backend_id.insert(idx, bid);
        }
    }

    // Build data-__ab-ci-p value → parent backendNodeId.
    // The data-__ab-ci-p value is the string index set by JS (e.g. "0", "1", …) — we use it
    // as a stable key to join cursor elements to their parents without relying on DOM nodeIds.
    let mut parent_ci_p_to_bid: HashMap<String, i64> = HashMap::new();
    for desc in parent_describe_results.into_iter().flatten() {
        let node = desc.get("node");
        let backend_id = node
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64());
        let ci_p_attr = node
            .and_then(|n| n.get("attributes"))
            .and_then(|a| a.as_array())
            .and_then(|attrs| {
                attrs
                    .iter()
                    .enumerate()
                    .find(|(_, v)| v.as_str() == Some("data-__ab-ci-p"))
                    .and_then(|(i, _)| attrs.get(i + 1))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            });
        if let (Some(bid), Some(key)) = (backend_id, ci_p_attr) {
            parent_ci_p_to_bid.insert(key, bid);
        }
    }

    // Clean up both sets of data attributes we injected.
    let cleanup_js = r#"(function(){
        var els = document.querySelectorAll('[data-__ab-ci],[data-__ab-ci-p]');
        for (var i = 0; i < els.length; i++) {
            els[i].removeAttribute('data-__ab-ci');
            els[i].removeAttribute('data-__ab-ci-p');
        }
        return els.length;
    })()"#
        .to_string();
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

    // Build the cursor-elements map and the cursor→parent-backend map.
    let mut map: HashMap<i64, CursorElementInfo> = HashMap::new();
    // Maps cursor element backendNodeId → its parent's backendNodeId.
    // Needed by inject_cursor_orphans to find the right AX parent when the cursor
    // element itself is absent from the accessibility tree.
    let mut cursor_parent_bid_map: HashMap<i64, Option<i64>> = HashMap::new();

    for (i, elem) in elements.iter().enumerate() {
        let Some(&bid) = idx_to_backend_id.get(&i) else {
            continue;
        };
        // Resolve parent backendNodeId via the parentCiP value we tagged in JS.
        let parent_bid = elem
            .get("parentCiP")
            .and_then(|v| v.as_str())
            .and_then(|key| parent_ci_p_to_bid.get(key).copied());
        cursor_parent_bid_map.insert(bid, parent_bid);

        // Role differentiation: v0.19.0 uses 'clickable' for cursor:pointer or onclick,
        // 'focusable' for tabindex-only elements.
        let has_cursor_pointer = elem
            .get("hasCursorPointer")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_on_click = elem
            .get("hasOnClick")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let has_tab_index = elem
            .get("hasTabIndex")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let is_editable = elem
            .get("isEditable")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let kind = if has_cursor_pointer || has_on_click {
            "clickable"
        } else if is_editable {
            "editable"
        } else {
            "focusable"
        };

        let mut hints: Vec<String> = Vec::new();
        if has_cursor_pointer {
            hints.push("cursor:pointer".to_string());
        }
        if has_on_click {
            hints.push("onclick".to_string());
        }
        if has_tab_index {
            hints.push("tabindex".to_string());
        }
        if is_editable {
            hints.push("contenteditable".to_string());
        }

        let text = elem
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let hidden_input_kind = elem
            .get("hiddenInputType")
            .and_then(|v| v.as_str())
            .and_then(HiddenInputKind::parse);
        let hidden_input_checked = elem
            .get("hiddenInputChecked")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        map.insert(
            bid,
            CursorElementInfo {
                kind: kind.to_string(),
                hints,
                text,
                hidden_input_kind,
                hidden_input_checked,
            },
        );
    }

    Ok((map, cursor_parent_bid_map))
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

/// Promote AX nodes that Chrome's engine marked as "ignored" but that cursor
/// detection found to be independently interactive (cursor:pointer, onclick, …).
///
/// Chrome omits semantic information for unstyled container divs — most visibly
/// CSS grid cells that carry no `role` attribute.  Their AX nodes appear as
/// empty `TreeNode`s in our tree (with `role = ""`), so they never match the
/// INTERACTIVE_ROLES / CONTENT_ROLES checks and are silently skipped.
///
/// When `find_cursor_interactive_elements` did find one of those nodes (its
/// `backendDOMNodeId` appears in `cursor_elements`), we restore a `"generic"`
/// role and, if the element had visible text, its `textContent` as name.
/// Any single StaticText child whose text already matches the promoted name is
/// cleared to avoid redundant output.
fn promote_cursor_matched_ignored(
    tree_nodes: &mut [TreeNode],
    cursor_elements: &HashMap<i64, CursorElementInfo>,
) {
    // First pass: collect promotions (borrow-checker: can't hold a mut ref and
    // index into the same slice simultaneously).
    let promotions: Vec<(usize, String)> = tree_nodes
        .iter()
        .enumerate()
        .filter_map(|(i, node)| {
            if !node.role.is_empty() {
                return None; // already a proper AX node
            }
            let bid = node.backend_node_id?;
            let cursor_info = cursor_elements.get(&bid)?;
            Some((i, cursor_info.text.clone()))
        })
        .collect();

    // Second pass: apply and deduplicate.
    for (i, text) in promotions {
        tree_nodes[i].role = "generic".to_string();
        if tree_nodes[i].name.is_empty() && !text.is_empty() {
            tree_nodes[i].name = text.clone();
        }
        // Clear a redundant single-child StaticText (same text as the promoted name).
        if tree_nodes[i].children.len() == 1 {
            let child_idx = tree_nodes[i].children[0];
            if tree_nodes[child_idx].role == "StaticText"
                && tree_nodes[child_idx].name == tree_nodes[i].name
            {
                tree_nodes[child_idx].clear();
            }
        }
    }
}

/// Inject synthetic `"generic"` TreeNodes for cursor-interactive elements that Chrome omitted
/// from the accessibility tree entirely (not even as ignored/pruned nodes).
///
/// This is the final fallback after both the normal backendDOMNodeId match and
/// `promote_cursor_matched_ignored` have run.  Common trigger: CSS-grid cells with
/// `cursor:pointer` but no ARIA role and no direct event listeners — Chrome decides they
/// carry no accessibility value and skips them entirely.
///
/// For each cursor element whose `backendNodeId` is absent from every existing tree node, we:
/// 1. Look up the element's **parent** backend node ID (from `cursor_parent_bid_map`).
/// 2. Find the first existing tree node that has that parent backend node ID.
/// 3. Append a synthetic `"generic"` child with the cursor element's text content.
///
/// If no parent AX node can be found we skip the element rather than injecting at an arbitrary
/// position (which could confuse agents reading the snapshot).
fn inject_cursor_orphans(
    tree_nodes: &mut Vec<TreeNode>,
    cursor_elements: &HashMap<i64, CursorElementInfo>,
    cursor_parent_bid_map: &HashMap<i64, Option<i64>>,
) {
    // Collect all backend node IDs that already exist in the tree (matched or promoted).
    let tree_bids: std::collections::HashSet<i64> = tree_nodes
        .iter()
        .filter_map(|n| n.backend_node_id)
        .collect();

    // Build parent_bid → first tree node index lookup for quick parent resolution.
    let mut parent_bid_to_tree_idx: HashMap<i64, usize> = HashMap::new();
    for (idx, node) in tree_nodes.iter().enumerate() {
        if let Some(bid) = node.backend_node_id {
            parent_bid_to_tree_idx.entry(bid).or_insert(idx);
        }
    }

    // Collect synthetic nodes to inject (avoid borrow issues with tree_nodes).
    struct Orphan {
        parent_tree_idx: usize,
        backend_node_id: i64,
        text: String,
        kind: String,
        hints: Vec<String>,
    }
    let mut orphans: Vec<Orphan> = Vec::new();

    // DEBUG
    if let Ok(debug_path) = std::env::var("AB_DEBUG_CURSOR") {
        let debug_path = if debug_path.is_empty() { "/tmp/ab_debug.log".to_string() } else { debug_path };
        let unmatched: Vec<_> = cursor_elements.keys().filter(|bid| !tree_bids.contains(bid)).collect();
        let parent_map_sample: Vec<_> = cursor_parent_bid_map.iter().take(5).collect();
        let tree_bid_count = tree_bids.len();
        let msg = format!("[DEBUG-ORPHAN] unmatched_cursor={} tree_bids={} parent_bid_map_len={} sample_parents={:?} tree_bid_sample={:?}\n",
            unmatched.len(), tree_bid_count, cursor_parent_bid_map.len(), parent_map_sample,
            tree_bids.iter().take(10).collect::<Vec<_>>());
        let _ = std::fs::OpenOptions::new()
            .create(true).append(true)
            .open(&debug_path)
            .and_then(|mut f| { use std::io::Write; f.write_all(msg.as_bytes()) });
    }

    for (&cursor_bid, cursor_info) in cursor_elements {
        if tree_bids.contains(&cursor_bid) {
            continue; // already represented in the tree
        }

        // Find the parent's backendNodeId and from there the parent tree node.
        let parent_bid = match cursor_parent_bid_map.get(&cursor_bid) {
            Some(Some(pbid)) => *pbid,
            _ => continue, // no parent info — skip
        };
        let parent_tree_idx = match parent_bid_to_tree_idx.get(&parent_bid) {
            Some(&idx) => idx,
            None => continue, // parent not in AX tree — skip
        };

        orphans.push(Orphan {
            parent_tree_idx,
            backend_node_id: cursor_bid,
            text: cursor_info.text.clone(),
            kind: cursor_info.kind.clone(),
            hints: cursor_info.hints.clone(),
        });
    }

    // Sort by backend_node_id so injection order is deterministic across runs.
    // HashMap iteration order is non-deterministic; without sorting, ref numbers
    // assigned to otherwise-identical snapshots would differ between calls.
    // backend_node_id values are assigned incrementally by Chrome, so this also
    // approximates DOM document order.
    orphans.sort_by_key(|o| o.backend_node_id);

    // Append synthetic nodes and wire parent–child links.
    for orphan in orphans {
        let new_idx = tree_nodes.len();
        // Keep a copy of the text for the reparenting step below (orphan.text is moved).
        let orphan_text = orphan.text.clone();
        tree_nodes.push(TreeNode {
            role: "generic".to_string(),
            name: orphan.text,
            backend_node_id: Some(orphan.backend_node_id),
            parent_idx: Some(orphan.parent_tree_idx),
            // cursor_info is set later by the ref-assignment loop in take_snapshot
            ..TreeNode::empty()
        });
        tree_nodes[orphan.parent_tree_idx].children.push(new_idx);

        // Clear duplicate StaticText siblings that Chrome hoisted from this pruned element.
        //
        // When an element carries role="none/presentation", Chrome omits the element from
        // the AX tree but promotes its text content to the nearest non-pruned ancestor as
        // a StaticText node.  Now that we have injected the element as a named generic,
        // those StaticText siblings are redundant — clear them so the snapshot shows the
        // cell exactly once (as the clickable generic) rather than twice.
        if !orphan_text.is_empty() {
            // Clone the parent children list to avoid simultaneous mutable+immutable borrows.
            let sibling_indices: Vec<usize> =
                tree_nodes[orphan.parent_tree_idx].children.clone();
            let to_clear: Vec<usize> = sibling_indices
                .into_iter()
                .filter(|&ci| {
                    ci != new_idx
                        && tree_nodes
                            .get(ci)
                            .map(|c| c.role == "StaticText" && c.name == orphan_text)
                            .unwrap_or(false)
                })
                .collect();
            for ci in to_clear {
                tree_nodes[ci].clear();
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
            let mut empty = TreeNode::empty();
            // For genuinely-ignored nodes (not InlineTextBox) preserve the
            // backendDOMNodeId so that promote_cursor_matched_ignored can restore
            // them when cursor detection finds them clickable.  Chrome marks
            // structurally-meaningless divs — e.g. CSS grid cells that carry no
            // ARIA role — as "ignored" in the AX tree even though they have
            // cursor:pointer and are independently interactive.
            if role != "InlineTextBox" {
                empty.backend_node_id = node.backend_d_o_m_node_id;
            }
            tree_nodes.push(empty);
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
        || (node.role == "StaticText" && node.name.replace(INVISIBLE_CHARS, "").is_empty())
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
        if let Ok(display_name) = serde_json::to_string(&unescaped_display_name) {
            line.push_str(&format!(" {}", display_name.replace(INVISIBLE_CHARS, "")));
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
            &cursor_info.kind,
            &cursor_info.hints.join(", ")
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
        assert_eq!(tracker.track("button", "Submit", 0), 0);
        assert_eq!(tracker.track("button", "Submit", 1), 1);
        assert_eq!(tracker.track("button", "Cancel", 2), 0);

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

    // -----------------------------------------------------------------------
    // should_assign_ref
    // Guards the logic that turns AX tree nodes into snapshot refs, including
    // the cursor-interactive path that covers onclick/<div> game buttons inside
    // cross-origin iframes.
    // -----------------------------------------------------------------------

    fn clickable_cursor_info(text: &str) -> CursorElementInfo {
        CursorElementInfo {
            kind: "clickable".to_string(),
            hints: vec!["cursor:pointer".to_string()],
            text: text.to_string(),
            hidden_input_kind: None,
            hidden_input_checked: None,
        }
    }

    #[test]
    fn test_should_assign_ref_interactive_roles_always_get_ref() {
        let cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        for role in INTERACTIVE_ROLES {
            assert!(
                should_assign_ref(role, "", None, &cursor_elements),
                "interactive role '{role}' should always get a ref"
            );
        }
    }

    #[test]
    fn test_should_assign_ref_content_roles_require_non_empty_name() {
        let cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        assert!(should_assign_ref("heading", "Title", None, &cursor_elements));
        assert!(!should_assign_ref("heading", "", None, &cursor_elements));
        assert!(should_assign_ref("cell", "Value", None, &cursor_elements));
        assert!(!should_assign_ref("cell", "", None, &cursor_elements));
    }

    #[test]
    fn test_should_assign_ref_generic_without_cursor_info_skipped() {
        // A plain <div> with no cursor-interactive match must NOT get a ref —
        // otherwise every structural wrapper would pollute the snapshot.
        let cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        assert!(!should_assign_ref("generic", "", None, &cursor_elements));
        assert!(!should_assign_ref("generic", "Some text", None, &cursor_elements));
        assert!(!should_assign_ref("generic", "", Some(42), &cursor_elements));
    }

    #[test]
    fn test_should_assign_ref_generic_with_cursor_info_gets_ref() {
        // A <div> detected as cursor-interactive (onclick / cursor:pointer)
        // MUST get a ref so agents can interact with it.
        // This is the exact case for game number-buttons (e.g. Sudoku 1–9) and
        // action buttons (SLET / HJÆLP) that live inside cross-origin iframes
        // and carry no ARIA role.
        let bid: i64 = 42;
        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(bid, clickable_cursor_info("SLET"));

        // Node with a different backendNodeId → no ref
        assert!(!should_assign_ref("generic", "", Some(999), &cursor_elements));
        // No backendNodeId at all → no ref
        assert!(!should_assign_ref("generic", "", None, &cursor_elements));

        // Node whose backendNodeId appears in cursor_elements → ref assigned
        assert!(should_assign_ref("generic", "", Some(bid), &cursor_elements));
        assert!(should_assign_ref("generic", "SLET", Some(bid), &cursor_elements));
    }

    #[test]
    fn test_should_assign_ref_structural_roles_without_cursor_info_skipped() {
        let cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        for role in STRUCTURAL_ROLES {
            assert!(
                !should_assign_ref(role, "anything", None, &cursor_elements),
                "structural role '{role}' without cursor info must not get a ref"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Cross-origin iframe session regression guard
    //
    // When take_snapshot recurses into a cross-origin iframe it computes
    // effective_session_id via resolve_ax_session.  find_cursor_interactive_elements
    // MUST be called with effective_session_id (not session_id) so that the JS
    // evaluation runs inside the iframe document.
    //
    // If the parent session were used instead, document.body.querySelectorAll('*')
    // would scan the main frame and find none of the iframe's elements, leaving
    // every clickable <div> inside the iframe as plain StaticText with no ref.
    // -----------------------------------------------------------------------

    #[test]
    fn test_cross_origin_iframe_effective_session_differs_from_parent() {
        let parent_session = "parent-session-id";
        let cross_origin_frame_id = "cross-origin-frame-id";
        let iframe_session = "iframe-dedicated-session-id";

        let mut iframe_sessions = HashMap::new();
        iframe_sessions.insert(cross_origin_frame_id.to_string(), iframe_session.to_string());

        let (_params, effective_session) =
            resolve_ax_session(Some(cross_origin_frame_id), parent_session, &iframe_sessions);

        assert_ne!(
            effective_session, parent_session,
            "cross-origin iframe: effective_session_id must differ from parent session; \
             passing parent session to find_cursor_interactive_elements scans the wrong document"
        );
        assert_eq!(
            effective_session, iframe_session,
            "cross-origin iframe: effective_session_id must be the dedicated iframe CDP session"
        );
    }

    #[test]
    fn test_main_frame_effective_session_equals_parent() {
        // For main-frame snapshots effective_session_id == session_id, so switching
        // find_cursor_interactive_elements from session_id to effective_session_id
        // is a no-op and cannot break existing behaviour.
        let parent_session = "parent-session-id";
        let iframe_sessions: HashMap<String, String> = HashMap::new();

        let (_params, effective_session) =
            resolve_ax_session(None, parent_session, &iframe_sessions);

        assert_eq!(
            effective_session, parent_session,
            "main frame: effective_session_id must equal parent session"
        );
    }

    #[test]
    fn test_same_origin_iframe_effective_session_equals_parent() {
        // Same-origin iframes share the parent CDP session (no separate entry in
        // iframe_sessions), so effective_session_id == session_id.  The cursor
        // element fix provides no improvement here (same-origin requires
        // execution-context scoping), but it must not break anything either.
        let parent_session = "parent-session-id";
        let same_origin_frame_id = "same-origin-frame-id";
        let iframe_sessions: HashMap<String, String> = HashMap::new(); // no entry

        let (_params, effective_session) =
            resolve_ax_session(Some(same_origin_frame_id), parent_session, &iframe_sessions);

        assert_eq!(
            effective_session, parent_session,
            "same-origin iframe: effective_session_id falls back to parent session"
        );
    }

    // -----------------------------------------------------------------------
    // promote_cursor_matched_ignored
    //
    // Chrome marks semantically-empty divs (e.g. CSS grid cells with no ARIA
    // role) as "ignored" in the AX tree.  build_tree therefore creates empty
    // TreeNodes (role="") for them — but now preserves their backendDOMNodeId.
    // promote_cursor_matched_ignored restores "generic" role + textContent name
    // for any such node whose backendNodeId appears in cursor_elements.
    // -----------------------------------------------------------------------

    fn make_ignored_node(backend_node_id: Option<i64>) -> TreeNode {
        // Simulates what build_tree produces for an AX-ignored div:
        // role="" (empty), backendNodeId optionally set.
        let mut node = TreeNode::empty();
        node.backend_node_id = backend_node_id;
        node
    }

    #[test]
    fn test_promote_ignored_node_with_cursor_match() {
        // An ignored div (<div class="cell">) that was found by cursor detection
        // must be promoted to role="generic" so it can receive a ref.
        let bid: i64 = 101;
        let mut nodes = vec![make_ignored_node(Some(bid))];
        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(bid, clickable_cursor_info("8"));

        promote_cursor_matched_ignored(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "generic", "ignored node must be promoted to generic");
        assert_eq!(nodes[0].name, "8", "name must come from cursor_info.text");
    }

    #[test]
    fn test_promote_ignored_empty_cell_no_text() {
        // An empty cell div (no text content) is still promoted to generic even
        // though cursor_info.text is empty — so it can receive a ref and appear
        // in the snapshot as an interactable empty cell.
        let bid: i64 = 202;
        let mut nodes = vec![make_ignored_node(Some(bid))];
        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(bid, clickable_cursor_info("")); // empty textContent

        promote_cursor_matched_ignored(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "generic");
        assert_eq!(nodes[0].name, "", "empty cell: name stays empty (no textContent)");
    }

    #[test]
    fn test_promote_ignored_node_without_cursor_match_unchanged() {
        // A node that is ignored AND not in cursor_elements must not be touched.
        let bid: i64 = 303;
        let mut nodes = vec![make_ignored_node(Some(bid))];
        let cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new(); // empty

        promote_cursor_matched_ignored(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "", "no cursor match: role stays empty");
    }

    #[test]
    fn test_promote_ignored_node_without_backend_node_id_unchanged() {
        // Nodes without a backendNodeId cannot be matched regardless.
        let mut nodes = vec![make_ignored_node(None)];
        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(999, clickable_cursor_info("x"));

        promote_cursor_matched_ignored(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "");
    }

    #[test]
    fn test_promote_deduplicates_matching_static_text_child() {
        // After promotion the node's name = "8".  If it has a single StaticText
        // child also named "8", that child must be cleared to avoid duplicates.
        let parent_bid: i64 = 404;
        let mut parent = make_ignored_node(Some(parent_bid));
        parent.children = vec![1]; // child at index 1

        let mut child = TreeNode::empty();
        child.role = "StaticText".to_string();
        child.name = "8".to_string();

        let mut nodes = vec![parent, child];
        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(parent_bid, clickable_cursor_info("8"));

        promote_cursor_matched_ignored(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "generic");
        assert_eq!(nodes[0].name, "8");
        assert_eq!(nodes[1].role, "", "matching StaticText child must be cleared");
    }

    #[test]
    fn test_promote_does_not_clear_non_matching_static_text_child() {
        // If the StaticText child has different text, it must be preserved.
        let parent_bid: i64 = 505;
        let mut parent = make_ignored_node(Some(parent_bid));
        parent.children = vec![1];

        let mut child = TreeNode::empty();
        child.role = "StaticText".to_string();
        child.name = "9".to_string(); // different from promoted name "8"

        let mut nodes = vec![parent, child];
        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(parent_bid, clickable_cursor_info("8"));

        promote_cursor_matched_ignored(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].name, "8");
        assert_eq!(nodes[1].role, "StaticText", "non-matching child must be preserved");
        assert_eq!(nodes[1].name, "9");
    }

    #[test]
    fn test_promote_does_not_touch_proper_ax_nodes() {
        // Nodes with a non-empty role (already proper AX nodes) must be left alone.
        let bid: i64 = 606;
        let mut node = make_node("generic", "some name", Some(bid));
        let mut nodes = vec![node];
        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(bid, clickable_cursor_info("other"));

        promote_cursor_matched_ignored(&mut nodes, &cursor_elements);

        assert_eq!(nodes[0].role, "generic");
        assert_eq!(nodes[0].name, "some name", "proper AX node must not be overwritten");
    }

    // -----------------------------------------------------------------------
    // inject_cursor_orphans
    // -----------------------------------------------------------------------

    /// Build a minimal parent + optional StaticText child tree, then call
    /// `inject_cursor_orphans` with an orphan cursor element.
    fn run_inject_orphan(
        parent_bid: i64,
        static_text_name: Option<&str>,
        orphan_cursor_bid: i64,
        orphan_text: &str,
    ) -> Vec<TreeNode> {
        // Index 0 = parent node (e.g. a .sudoku-grid div in the AX tree as ignored)
        let mut parent = TreeNode::empty();
        parent.backend_node_id = Some(parent_bid);

        let mut nodes: Vec<TreeNode> = vec![parent];

        if let Some(st_name) = static_text_name {
            let mut st = TreeNode::empty();
            st.role = "StaticText".to_string();
            st.name = st_name.to_string();
            nodes[0].children.push(1); // child at index 1
            nodes.push(st);
        }

        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(orphan_cursor_bid, clickable_cursor_info(orphan_text));

        let mut cursor_parent_bid_map: HashMap<i64, Option<i64>> = HashMap::new();
        cursor_parent_bid_map.insert(orphan_cursor_bid, Some(parent_bid));

        inject_cursor_orphans(&mut nodes, &cursor_elements, &cursor_parent_bid_map);
        nodes
    }

    #[test]
    fn test_inject_orphan_adds_generic_child_to_parent() {
        // A cursor element absent from the AX tree must be injected as a "generic"
        // child of its parent AX node so the snapshot shows it with a ref.
        let nodes = run_inject_orphan(10, None, 99, "8");

        // A new node must have been appended.
        assert_eq!(nodes.len(), 2, "one synthetic node should be appended");
        let injected = &nodes[1];
        assert_eq!(injected.role, "generic");
        assert_eq!(injected.name, "8");
        assert_eq!(injected.backend_node_id, Some(99));
        assert_eq!(injected.parent_idx, Some(0));
        // Parent must list the new node as a child.
        assert!(nodes[0].children.contains(&1), "parent must have injected node as child");
    }

    #[test]
    fn test_inject_orphan_empty_text_cell() {
        // An empty cell (no text) is still injected as a named-empty generic.
        let nodes = run_inject_orphan(10, None, 99, "");

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].role, "generic");
        assert_eq!(nodes[1].name, "");
    }

    #[test]
    fn test_inject_orphan_clears_hoisted_static_text() {
        // Chrome hoists text from role=none elements to the nearest non-pruned ancestor.
        // inject_cursor_orphans must clear that StaticText sibling to avoid duplicates.
        //
        // Tree before:
        //   [0] parent (bid=10)
        //     [1] StaticText "8"        ← Chrome hoisted this from the cell div
        //
        // After inject for orphan cursor bid=99 (text="8"), parent=10:
        //   [0] parent (bid=10)
        //     [2] generic "8" (bid=99)  ← injected
        //   [1] StaticText "8"          ← cleared (role="" name="" so snapshot skips it)
        let nodes = run_inject_orphan(10, Some("8"), 99, "8");

        // Three nodes total: parent + original StaticText + injected generic.
        assert_eq!(nodes.len(), 3);

        let injected = &nodes[2];
        assert_eq!(injected.role, "generic");
        assert_eq!(injected.name, "8");
        assert_eq!(injected.backend_node_id, Some(99));

        // The hoisted StaticText must have been cleared.
        let static_text = &nodes[1];
        assert_eq!(static_text.role, "", "hoisted StaticText must be cleared");
        assert_eq!(static_text.name, "", "hoisted StaticText name must be cleared");
    }

    #[test]
    fn test_inject_orphan_preserves_non_matching_static_text() {
        // A StaticText with DIFFERENT text (e.g. a different cell's text hoisted to
        // the same container) must NOT be cleared.
        let nodes = run_inject_orphan(10, Some("9"), 99, "8");

        // Three nodes total.
        assert_eq!(nodes.len(), 3);
        // StaticText "9" must be untouched.
        let static_text = &nodes[1];
        assert_eq!(static_text.role, "StaticText");
        assert_eq!(static_text.name, "9");
    }

    #[test]
    fn test_inject_orphan_skips_already_in_tree() {
        // If a cursor element's backendNodeId already appears in the tree (e.g. it was
        // matched or promoted by earlier passes), inject_cursor_orphans must skip it.
        let parent_bid: i64 = 10;
        let cursor_bid: i64 = 20;

        // Existing tree: parent → existing-generic (bid=cursor_bid already in tree)
        let mut parent = TreeNode::empty();
        parent.backend_node_id = Some(parent_bid);
        parent.children = vec![1];

        let mut existing = TreeNode::empty();
        existing.role = "generic".to_string();
        existing.name = "already there".to_string();
        existing.backend_node_id = Some(cursor_bid);

        let mut nodes = vec![parent, existing];

        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(cursor_bid, clickable_cursor_info("already there"));

        let mut cursor_parent_bid_map: HashMap<i64, Option<i64>> = HashMap::new();
        cursor_parent_bid_map.insert(cursor_bid, Some(parent_bid));

        inject_cursor_orphans(&mut nodes, &cursor_elements, &cursor_parent_bid_map);

        // No new node should be appended since cursor_bid is already in the tree.
        assert_eq!(nodes.len(), 2, "already-present node must not be duplicated");
    }

    #[test]
    fn test_inject_orphan_skips_when_no_parent_bid() {
        // If cursor_parent_bid_map has None for the cursor element, skip injection.
        let cursor_bid: i64 = 99;

        let mut nodes: Vec<TreeNode> = vec![TreeNode::empty()]; // single root

        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(cursor_bid, clickable_cursor_info("x"));

        let mut cursor_parent_bid_map: HashMap<i64, Option<i64>> = HashMap::new();
        cursor_parent_bid_map.insert(cursor_bid, None); // no parent resolved

        inject_cursor_orphans(&mut nodes, &cursor_elements, &cursor_parent_bid_map);

        // Nothing should be injected.
        assert_eq!(nodes.len(), 1, "no injection when parent bid is None");
    }

    #[test]
    fn test_inject_orphan_skips_when_parent_not_in_tree() {
        // If the parent backendNodeId doesn't appear in any tree node, skip.
        let cursor_bid: i64 = 99;
        let orphan_parent_bid: i64 = 999; // not in tree

        let mut nodes = vec![{
            let mut n = TreeNode::empty();
            n.backend_node_id = Some(10); // different bid
            n
        }];

        let mut cursor_elements: HashMap<i64, CursorElementInfo> = HashMap::new();
        cursor_elements.insert(cursor_bid, clickable_cursor_info("x"));

        let mut cursor_parent_bid_map: HashMap<i64, Option<i64>> = HashMap::new();
        cursor_parent_bid_map.insert(cursor_bid, Some(orphan_parent_bid));

        inject_cursor_orphans(&mut nodes, &cursor_elements, &cursor_parent_bid_map);

        assert_eq!(nodes.len(), 1, "no injection when parent is not in tree");
    }
}
