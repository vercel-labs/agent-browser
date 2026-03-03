use std::collections::HashMap;

use serde_json::Value;

use super::cdp::client::CdpClient;
use super::cdp::types::{
    AXNode, AXProperty, AXValue, CallFunctionOnParams, EvaluateParams, EvaluateResult,
    GetFullAXTreeResult,
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

pub struct SnapshotOptions {
    pub selector: Option<String>,
    pub interactive: bool,
    pub compact: bool,
    pub depth: Option<usize>,
    pub cursor: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            selector: None,
            interactive: false,
            compact: false,
            depth: None,
            cursor: false,
        }
    }
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
    has_ref: bool,
    ref_id: Option<String>,
    depth: usize,
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

    let ax_tree: GetFullAXTreeResult = client
        .send_command_typed(
            "Accessibility.getFullAXTree",
            &serde_json::json!({}),
            Some(session_id),
        )
        .await?;

    let (tree_nodes, root_indices) = build_tree(&ax_tree.nodes);

    let mut tracker = RoleNameTracker::new();
    let mut next_ref: usize = ref_map.next_ref_num();

    let mut nodes_with_refs: Vec<(usize, usize)> = Vec::new();

    for (idx, node) in tree_nodes.iter().enumerate() {
        let role = node.role.as_str();
        let should_ref = if INTERACTIVE_ROLES.contains(&role) {
            true
        } else if CONTENT_ROLES.contains(&role) {
            !node.name.is_empty()
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

    ref_map.set_next_ref_num(next_ref);

    let mut output = String::new();
    for &root_idx in &root_indices {
        render_tree(&tree_nodes, root_idx, 0, &mut output, options);
    }

    if options.compact {
        output = compact_tree(&output, options.interactive);
    }

    let mut trimmed = output.trim().to_string();
    if trimmed.is_empty() {
        if options.interactive {
            return Ok("(no interactive elements)".to_string());
        }
        return Ok("(empty page)".to_string());
    }

    if options.cursor {
        let cursor_section = find_cursor_interactive_elements(client, session_id, ref_map).await?;
        if !cursor_section.is_empty() {
            trimmed.push_str("\n# Cursor-interactive elements:\n");
            trimmed.push_str(&cursor_section);
        }
    }

    Ok(trimmed)
}

async fn find_cursor_interactive_elements(
    client: &CdpClient,
    session_id: &str,
    ref_map: &mut RefMap,
) -> Result<String, String> {
    let js = r#"
(function() {
    const elements = [];
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT);
    let node;
    while (node = walker.nextNode()) {
        if (node.closest && node.closest('[hidden], [aria-hidden="true"]')) continue;
        const explicitRole = node.getAttribute ? node.getAttribute('role') : null;
        if (explicitRole) continue;
        const tag = node.tagName ? node.tagName.toLowerCase() : '';
        const hasClick = node.onclick || (node.attributes && node.attributes.getNamedItem('onclick'));
        const tabindex = node.getAttribute ? node.getAttribute('tabindex') : null;
        const contentEditable = node.getAttribute ? node.getAttribute('contenteditable') : null;
        const isInherentlyClickable =
            (tag === 'a' && node.href) || tag === 'button' ||
            (tag === 'input' && ['submit','button','image','reset'].indexOf((node.type||'').toLowerCase()) >= 0) ||
            tag === 'summary';
        const isFocusable = tabindex !== null && parseInt(tabindex, 10) >= 0;
        const isEditable = contentEditable === '' || contentEditable === 'true';
        if (hasClick || isInherentlyClickable || isFocusable || isEditable) {
            elements.push(node);
        }
    }
    return elements;
})()
"#;

    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &EvaluateParams {
                expression: js.to_string(),
                return_by_value: Some(false),
                await_promise: Some(false),
            },
            Some(session_id),
        )
        .await?;

    let array_object_id = match result.result.object_id {
        Some(id) => id,
        None => return Ok(String::new()),
    };

    let props_result: Value = client
        .send_command(
            "Runtime.getProperties",
            Some(serde_json::json!({ "objectId": array_object_id })),
            Some(session_id),
        )
        .await?;

    let empty: Vec<Value> = Vec::new();
    let result_array = props_result
        .get("result")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);

    let mut indexed: Vec<(usize, String)> = Vec::new();
    for prop in result_array {
        let name = prop.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if let Ok(idx) = name.parse::<usize>() {
            if let Some(obj_id) = prop
                .get("value")
                .and_then(|v| v.get("objectId"))
                .and_then(|v| v.as_str())
            {
                indexed.push((idx, obj_id.to_string()));
            }
        }
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    let element_object_ids: Vec<String> = indexed.into_iter().map(|(_, id)| id).collect();

    let mut next_ref = ref_map.next_ref_num();
    let mut lines: Vec<String> = Vec::new();
    let get_text_js =
        r#"function(){ return (this.innerText || this.textContent || '').trim().slice(0, 100) }"#;

    for object_id in &element_object_ids {
        let describe: Value = client
            .send_command(
                "DOM.describeNode",
                Some(serde_json::json!({ "objectId": object_id })),
                Some(session_id),
            )
            .await?;

        let backend_node_id = describe
            .get("node")
            .and_then(|n| n.get("backendNodeId"))
            .and_then(|v| v.as_i64());

        let text_result: EvaluateResult = client
            .send_command_typed(
                "Runtime.callFunctionOn",
                &CallFunctionOnParams {
                    function_declaration: get_text_js.to_string(),
                    object_id: Some(object_id.clone()),
                    arguments: None,
                    return_by_value: Some(true),
                    await_promise: Some(false),
                },
                Some(session_id),
            )
            .await?;

        let text = text_result
            .result
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let kind = "clickable";
        let ref_id = format!("e{}", next_ref);
        next_ref += 1;

        ref_map.add(ref_id.clone(), backend_node_id, kind, &text, None);

        let escaped = text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
            .replace('\r', " ");
        lines.push(format!("[ref={}] ({}) \"{}\"", ref_id, kind, escaped));
    }

    ref_map.set_next_ref_num(next_ref);

    Ok(lines.join("\n"))
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
                has_ref: false,
                ref_id: None,
                depth: 0,
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
            has_ref: false,
            ref_id: None,
            depth: 0,
        });
        id_to_idx.insert(node.node_id.clone(), i);
    }

    // Build parent-child relationships
    for (i, node) in nodes.iter().enumerate() {
        if let Some(ref child_ids) = node.child_ids {
            for cid in child_ids {
                if let Some(&child_idx) = id_to_idx.get(cid) {
                    tree_nodes[i].children.push(child_idx);
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
}
