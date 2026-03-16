use std::collections::HashMap;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::{
    AXNode, AXProperty, AXValue, EvaluateParams, EvaluateResult, GetFullAXTreeResult,
};
use super::element::RefMap;

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

#[derive(Default)]
pub struct SnapshotOptions {
    pub selector: Option<String>,
    pub interactive: bool,
    pub compact: bool,
    pub depth: Option<usize>,
    pub cursor: bool,
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
    /// Cursor-interactive information (only set when options.cursor is true)
    cursor_info: Option<CursorElementInfo>,
}

/// Information about a cursor-interactive element (elements with cursor:pointer, onclick, tabindex, etc.)
#[derive(Clone)]
struct CursorElementInfo {
    kind: String, // "clickable", "focusable", "editable"
    hints: Vec<String>,
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

    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed(
            "Accessibility.getFullAXTree",
            &serde_json::json!({}),
            Some(session_id),
        )
        .await?;

    let (tree_nodes, root_indices) = build_tree(&ax_tree.nodes);

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

    // When cursor mode is enabled, pre-collect cursor-interactive elements
    // so we can mark them with refs during tree building
    let cursor_elements: HashMap<i64, CursorElementInfo> = if options.cursor {
        find_cursor_interactive_elements(client, session_id)
            .await
            .unwrap_or_default()
    } else {
        HashMap::new()
    };

    for (idx, node) in tree_nodes.iter().enumerate() {
        let role = node.role.as_str();
        let should_ref = if INTERACTIVE_ROLES.contains(&role) {
            true
        } else if CONTENT_ROLES.contains(&role) {
            !node.name.is_empty()
        } else if options.cursor {
            // In cursor mode, also ref elements that are cursor-interactive
            node.backend_node_id
                .is_some_and(|bid| cursor_elements.contains_key(&bid))
        } else {
            false
        };

        if should_ref {
            let nth = tracker.track(role, &node.name, idx);
            nodes_with_refs.push((idx, nth));
        }
    }

    let duplicates = tracker.get_duplicates();

    let mut tree_nodes = tree_nodes;
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

        ref_map.add(
            ref_id.clone(),
            tree_nodes[*idx].backend_node_id,
            &tree_nodes[*idx].role,
            &tree_nodes[*idx].name,
            actual_nth,
        );

        tree_nodes[*idx].has_ref = true;
        tree_nodes[*idx].ref_id = Some(ref_id);
    }

    // Populate cursor_info for ref-bearing nodes when cursor mode is enabled
    if options.cursor {
        for (idx, _) in &nodes_with_refs {
            if let Some(bid) = tree_nodes[*idx].backend_node_id {
                if let Some(cursor_info) = cursor_elements.get(&bid) {
                    tree_nodes[*idx].cursor_info = Some((*cursor_info).clone());
                }
            }
        }
    }

    ref_map.set_next_ref_num(next_ref);

    let mut output = String::new();
    for &root_idx in &effective_roots {
        render_tree(&tree_nodes, root_idx, 0, &mut output, options);
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
    let js = r#"
(function() {
    var results = [];
    if (!document.body) return results;

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

        el.setAttribute('data-__ab-ci', String(results.length));
        results.push({
            text: text,
            tagName: tagName,
            hasOnClick: hasOnClick,
            hasCursorPointer: hasCursorPointer,
            hasTabIndex: hasTabIndex,
            isEditable: isEditable
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
        let backend_node_id = idx_to_backend.get(&i).copied();

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

        if let Some(bid) = backend_node_id {
            map.insert(
                bid,
                CursorElementInfo {
                    kind: kind.to_string(),
                    hints,
                },
            );
        }
    }

    Ok(map)
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

        if node.ignored.unwrap_or(false) && role != "RootWebArea" {
            tree_nodes.push(TreeNode {
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
            });
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

    if node.role.is_empty() {
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

    if !node.name.is_empty() {
        line.push_str(&format!(" \"{}\"", node.name));
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
        if line.contains("[ref=") || line.contains(": ") {
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
}
