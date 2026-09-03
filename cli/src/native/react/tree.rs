//! React component tree snapshot and formatter.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct TreeNode {
    pub id: i64,
    #[serde(rename = "type")]
    pub node_type: i64,
    pub name: Option<String>,
    pub key: Option<String>,
    pub parent: i64,
}

const HEADER: &str = "# React component tree\n# Columns: depth id parent name [key=...]\n# Use `react inspect <id>` for props/hooks/state. IDs valid until next navigation.";

pub fn format_tree(nodes: &[TreeNode]) -> String {
    use std::collections::HashMap;
    let mut children: HashMap<i64, Vec<&TreeNode>> = HashMap::new();
    for n in nodes {
        children.entry(n.parent).or_default().push(n);
    }

    let mut lines: Vec<String> = vec![HEADER.to_string()];
    if let Some(roots) = children.get(&0) {
        for root in roots {
            walk(root, 0, &children, &mut lines);
        }
    }
    lines.join("\n")
}

fn walk<'a>(
    node: &'a TreeNode,
    depth: usize,
    children: &std::collections::HashMap<i64, Vec<&'a TreeNode>>,
    lines: &mut Vec<String>,
) {
    let name = node
        .name
        .clone()
        .unwrap_or_else(|| type_name(node.node_type));
    let key = match &node.key {
        Some(k) => format!(" key={:?}", k),
        None => String::new(),
    };
    let parent = if node.parent == 0 {
        "-".to_string()
    } else {
        node.parent.to_string()
    };
    lines.push(format!("{} {} {} {}{}", depth, node.id, parent, name, key));
    if let Some(cs) = children.get(&node.id) {
        for c in cs {
            walk(c, depth + 1, children, lines);
        }
    }
}

fn type_name(t: i64) -> String {
    match t {
        11 => "Root".to_string(),
        12 => "Suspense".to_string(),
        13 => "SuspenseList".to_string(),
        _ => format!("({})", t),
    }
}
