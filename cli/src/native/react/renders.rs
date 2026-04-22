//! React fiber render profiler report formatter.
//!
//! Default output is the
//! full agent-readable report (summary, FPS, component table, per-component
//! "change details (prev -> next)"). `--json` emits the raw structured data
//! instead.

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct RendersData {
    pub elapsed: f64,
    pub fps: FpsStats,
    #[serde(rename = "totalRenders")]
    pub total_renders: i64,
    #[serde(rename = "totalMounts")]
    pub total_mounts: i64,
    #[serde(rename = "totalReRenders")]
    pub total_re_renders: i64,
    #[serde(rename = "totalComponents")]
    pub total_components: i64,
    pub components: Vec<Component>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FpsStats {
    pub avg: i64,
    pub min: i64,
    pub max: i64,
    pub drops: i64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Component {
    pub name: String,
    pub count: i64,
    pub mounts: i64,
    #[serde(rename = "reRenders")]
    pub re_renders: i64,
    #[serde(rename = "instanceCount")]
    pub instance_count: i64,
    #[serde(rename = "totalTime")]
    pub total_time: f64,
    #[serde(rename = "selfTime")]
    pub self_time: f64,
    #[serde(rename = "domMutations")]
    pub dom_mutations: i64,
    pub changes: Vec<Change>,
    #[serde(rename = "changeSummary")]
    pub change_summary: std::collections::HashMap<String, i64>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Change {
    #[serde(rename = "type")]
    pub change_type: String,
    pub name: Option<String>,
    pub prev: Option<String>,
    pub next: Option<String>,
}

pub fn format_renders_report(d: &RendersData) -> String {
    if d.components.is_empty() {
        return "(no renders captured)".to_string();
    }

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("# Render Profile - {}s recording", d.elapsed));
    lines.push(format!(
        "# {} renders ({} mounts + {} re-renders) across {} components",
        d.total_renders, d.total_mounts, d.total_re_renders, d.total_components
    ));
    lines.push(format!(
        "# FPS: avg {}, min {}, max {}, drops (<30fps): {}",
        d.fps.avg, d.fps.min, d.fps.max, d.fps.drops
    ));
    lines.push(String::new());
    lines.push("## Components by total render time".to_string());

    let top: Vec<&Component> = d.components.iter().take(50).collect();
    let name_w = top.iter().map(|c| c.name.len()).max().unwrap_or(9).max(9);

    lines.push(format!(
        "| {:<name_w$} | Insts | Mounts | Re-renders | Total    | Self     | DOM   | Top change reason          |",
        "Component",
        name_w = name_w
    ));
    lines.push(format!(
        "| {:-<name_w$} | ----- | ------ | ---------- | -------- | -------- | ----- | -------------------------- |",
        "",
        name_w = name_w
    ));
    for c in &top {
        let total = if c.total_time > 0.0 {
            format!("{}ms", c.total_time)
        } else {
            "-".to_string()
        };
        let self_time = if c.self_time > 0.0 {
            format!("{}ms", c.self_time)
        } else {
            "-".to_string()
        };
        let dom = format!("{}/{}", c.dom_mutations, c.count);
        let top_change = c
            .change_summary
            .iter()
            .max_by_key(|(_, v)| *v)
            .map(|(k, _)| k.as_str())
            .unwrap_or("-");
        lines.push(format!(
            "| {:<name_w$} | {:>5} | {:>6} | {:>10} | {:>8} | {:>8} | {:>5} | {:<26} |",
            c.name,
            c.instance_count,
            c.mounts,
            c.re_renders,
            total,
            self_time,
            dom,
            top_change,
            name_w = name_w
        ));
    }
    if d.components.len() > 50 {
        lines.push(format!("... and {} more", d.components.len() - 50));
    }

    let detailed: Vec<&Component> = d
        .components
        .iter()
        .filter(|c| {
            c.changes
                .iter()
                .any(|ch| ch.change_type != "mount" && ch.change_type != "parent")
        })
        .take(15)
        .collect();
    if !detailed.is_empty() {
        lines.push(String::new());
        lines.push("## Change details (prev -> next)".to_string());
        for c in &detailed {
            lines.push(format!("  {}", c.name));
            let mut seen = std::collections::HashSet::new();
            for ch in &c.changes {
                if ch.change_type == "mount" || ch.change_type == "parent" {
                    continue;
                }
                let name = ch.name.clone().unwrap_or_default();
                let key = format!("{}:{}", ch.change_type, name);
                if !seen.insert(key) {
                    continue;
                }
                let label = match ch.change_type.as_str() {
                    "props" => format!("props.{}", name),
                    "state" => format!("state ({})", name),
                    _ => format!("context ({})", name),
                };
                lines.push(format!(
                    "    {}: {} -> {}",
                    label,
                    ch.prev.clone().unwrap_or_else(|| "?".into()),
                    ch.next.clone().unwrap_or_else(|| "?".into())
                ));
            }
        }
    }

    lines.join("\n")
}
