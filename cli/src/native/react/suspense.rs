//! React Suspense boundary introspection: walker data types, classifier, and
//! human-readable report.
//!
//! The classifier labels and recommendations are React-Suspense-general —
//! they describe what kind of thing is making a boundary suspend (`client-hook`,
//! `request-api`, `server-fetch`, `cache`, `stream`, `framework`, `unknown`)
//! and a high-level direction for fixing it. Framework-specific reasoning
//! (e.g. Next.js PPR push vs goto semantics) is left to the caller.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type StackFrame = (String, String, i64, i64);

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Boundary {
    pub id: i64,
    #[serde(rename = "parentID")]
    pub parent_id: i64,
    pub name: Option<String>,
    #[serde(rename = "isSuspended")]
    pub is_suspended: bool,
    pub environments: Vec<String>,
    #[serde(rename = "suspendedBy")]
    pub suspended_by: Vec<Suspender>,
    #[serde(rename = "unknownSuspenders")]
    pub unknown_suspenders: Option<String>,
    pub owners: Vec<Owner>,
    #[serde(rename = "jsxSource")]
    pub jsx_source: Option<(String, i64, i64)>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Owner {
    pub name: String,
    pub env: Option<String>,
    pub source: Option<(String, i64, i64)>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Suspender {
    pub name: String,
    pub description: String,
    pub duration: i64,
    pub env: Option<String>,
    #[serde(rename = "ownerName")]
    pub owner_name: Option<String>,
    #[serde(rename = "ownerStack")]
    pub owner_stack: Option<Vec<StackFrame>>,
    #[serde(rename = "awaiterName")]
    pub awaiter_name: Option<String>,
    #[serde(rename = "awaiterStack")]
    pub awaiter_stack: Option<Vec<StackFrame>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockerKind {
    ClientHook,
    RequestApi,
    ServerFetch,
    Stream,
    Cache,
    Framework,
    Unknown,
}

impl BlockerKind {
    fn label(self) -> &'static str {
        match self {
            Self::ClientHook => "client-hook",
            Self::RequestApi => "request-api",
            Self::ServerFetch => "server-fetch",
            Self::Stream => "stream",
            Self::Cache => "cache",
            Self::Framework => "framework",
            Self::Unknown => "unknown",
        }
    }

    fn weight(self) -> i32 {
        match self {
            Self::ClientHook => 7,
            Self::RequestApi => 6,
            Self::ServerFetch => 5,
            Self::Cache => 4,
            Self::Stream => 3,
            Self::Unknown => 2,
            Self::Framework => 1,
        }
    }

    fn actionability(self) -> i32 {
        match self {
            Self::ClientHook => 90,
            Self::RequestApi => 88,
            Self::ServerFetch => 82,
            Self::Cache => 74,
            Self::Stream => 60,
            Self::Unknown => 35,
            Self::Framework => 18,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryKind {
    RouteSegment,
    ExplicitSuspense,
    Component,
}

impl BoundaryKind {
    fn label(self) -> &'static str {
        match self {
            Self::RouteSegment => "route-segment",
            Self::ExplicitSuspense => "explicit-suspense",
            Self::Component => "component",
        }
    }

    fn weight(self) -> i32 {
        match self {
            Self::RouteSegment => 3,
            Self::ExplicitSuspense => 2,
            Self::Component => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ActionableBlocker {
    pub key: String,
    pub name: String,
    pub kind: BlockerKind,
    pub env: Option<String>,
    pub description: String,
    pub owner_name: Option<String>,
    pub awaiter_name: Option<String>,
    pub source_frame: Option<StackFrame>,
    pub owner_frame: Option<StackFrame>,
    pub awaiter_frame: Option<StackFrame>,
    pub actionability: i32,
    pub suggestion: String,
}

#[derive(Debug, Clone)]
pub struct BoundaryInsight {
    pub id: i64,
    pub name: Option<String>,
    pub boundary_kind: BoundaryKind,
    pub environments: Vec<String>,
    pub source: Option<(String, i64, i64)>,
    pub rendered_by: Vec<Owner>,
    pub primary_blocker: Option<ActionableBlocker>,
    pub blockers: Vec<ActionableBlocker>,
    pub unknown_suspenders: Option<String>,
    pub actionability: i32,
    pub recommendation: String,
}

#[derive(Debug, Clone)]
pub struct RootCauseGroup {
    pub kind: BlockerKind,
    pub name: String,
    pub source_frame: Option<StackFrame>,
    pub boundary_names: Vec<String>,
    pub count: usize,
    pub actionability: i32,
    pub suggestion: String,
}

pub struct AnalysisReport {
    pub total_boundaries: usize,
    pub dynamic_hole_count: usize,
    pub static_count: usize,
    pub holes: Vec<BoundaryInsight>,
    pub statics: Vec<StaticBoundarySummary>,
    pub root_causes: Vec<RootCauseGroup>,
    pub files_to_read: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StaticBoundarySummary {
    pub name: Option<String>,
    pub source: Option<(String, i64, i64)>,
    pub rendered_by: Vec<Owner>,
}

pub fn format_suspense_report(boundaries: &[Boundary], only_dynamic: bool) -> String {
    let report = analyze_boundaries(boundaries);
    format_report(&report, only_dynamic)
}

fn analyze_boundaries(boundaries: &[Boundary]) -> AnalysisReport {
    let mut holes: Vec<&Boundary> = Vec::new();
    let mut statics_raw: Vec<&Boundary> = Vec::new();

    for b in boundaries {
        if b.parent_id == 0 {
            continue;
        }
        let has_blocker = !b.suspended_by.is_empty() || b.unknown_suspenders.is_some();
        if b.is_suspended || has_blocker {
            holes.push(b);
        } else {
            statics_raw.push(b);
        }
    }

    let mut hole_insights: Vec<BoundaryInsight> = holes.iter().map(|b| build_insight(b)).collect();
    hole_insights.sort_by(|a, b| {
        b.actionability.cmp(&a.actionability).then_with(|| {
            b.boundary_kind
                .weight()
                .cmp(&a.boundary_kind.weight())
                .then_with(|| b.blockers.len().cmp(&a.blockers.len()))
                .then_with(|| {
                    a.name
                        .as_deref()
                        .unwrap_or("")
                        .cmp(b.name.as_deref().unwrap_or(""))
                })
        })
    });

    let static_summaries: Vec<StaticBoundarySummary> = statics_raw
        .iter()
        .map(|b| StaticBoundarySummary {
            name: b.name.clone(),
            source: b.jsx_source.clone(),
            rendered_by: b.owners.clone(),
        })
        .collect();

    let root_causes = build_root_causes(&hole_insights);
    let files_to_read = collect_files_to_read(&hole_insights, &root_causes);

    AnalysisReport {
        total_boundaries: hole_insights.len() + static_summaries.len(),
        dynamic_hole_count: hole_insights.len(),
        static_count: static_summaries.len(),
        holes: hole_insights,
        statics: static_summaries,
        root_causes,
        files_to_read,
    }
}

fn build_insight(b: &Boundary) -> BoundaryInsight {
    let boundary_kind = infer_boundary_kind(b);
    let mut blockers: Vec<ActionableBlocker> = b
        .suspended_by
        .iter()
        .map(build_actionable_blocker)
        .collect();
    blockers.sort_by(|a, b| {
        b.actionability.cmp(&a.actionability).then_with(|| {
            b.kind
                .weight()
                .cmp(&a.kind.weight())
                .then_with(|| a.name.cmp(&b.name))
        })
    });
    let primary = blockers.first().cloned();
    let recommendation = recommend_fix(
        boundary_kind,
        primary.as_ref(),
        b.unknown_suspenders.as_deref(),
    );
    let primary_action = primary.as_ref().map(|p| p.actionability).unwrap_or(0);
    let base_action = if boundary_kind == BoundaryKind::RouteSegment {
        55
    } else {
        0
    };

    BoundaryInsight {
        id: b.id,
        name: b.name.clone(),
        boundary_kind,
        environments: b.environments.clone(),
        source: b.jsx_source.clone(),
        rendered_by: b.owners.clone(),
        primary_blocker: primary,
        blockers,
        unknown_suspenders: b.unknown_suspenders.clone(),
        actionability: primary_action.max(base_action),
        recommendation,
    }
}

fn build_actionable_blocker(s: &Suspender) -> ActionableBlocker {
    let owner_frame = pick_preferred_frame(s.owner_stack.as_deref());
    let awaiter_frame = pick_preferred_frame(s.awaiter_stack.as_deref());
    let source_frame = owner_frame.clone().or_else(|| awaiter_frame.clone());
    let kind = classify_blocker(s, source_frame.as_ref());
    let suggestion = suggest_blocker_fix(kind);
    let mut actionability = kind.actionability();
    if let Some(ref frame) = source_frame {
        if !is_frameworkish_path(&frame.1) {
            actionability += 8;
        }
    }
    if s.owner_name.is_some() || s.awaiter_name.is_some() {
        actionability += 4;
    }
    if actionability > 100 {
        actionability = 100;
    }
    let key = build_blocker_key(&s.name, kind, source_frame.as_ref());

    ActionableBlocker {
        key,
        name: s.name.clone(),
        kind,
        env: s.env.clone(),
        description: s.description.clone(),
        owner_name: s.owner_name.clone(),
        awaiter_name: s.awaiter_name.clone(),
        source_frame,
        owner_frame,
        awaiter_frame,
        actionability,
        suggestion,
    }
}

fn infer_boundary_kind(b: &Boundary) -> BoundaryKind {
    let owner_names: Vec<&str> = b.owners.iter().map(|o| o.name.as_str()).collect();
    let name_ends_slash = b.name.as_ref().is_some_and(|n| n.ends_with('/'));
    if name_ends_slash
        || owner_names.contains(&"LoadingBoundary")
        || owner_names.contains(&"OuterLayoutRouter")
    {
        return BoundaryKind::RouteSegment;
    }
    let name_has_suspense = b.name.as_ref().is_some_and(|n| n.contains("Suspense"));
    if name_has_suspense || owner_names.iter().any(|n| n.contains("Suspense")) {
        return BoundaryKind::ExplicitSuspense;
    }
    BoundaryKind::Component
}

fn classify_blocker(s: &Suspender, source_frame: Option<&StackFrame>) -> BlockerKind {
    let name = s.name.to_lowercase();
    match name.as_str() {
        "usepathname"
        | "useparams"
        | "usesearchparams"
        | "useselectedlayoutsegments"
        | "useselectedlayoutsegment"
        | "userouter" => return BlockerKind::ClientHook,
        "cookies" | "headers" | "connection" | "params" | "searchparams" | "draftmode" => {
            return BlockerKind::RequestApi
        }
        _ => {}
    }
    if name == "rsc stream" {
        return BlockerKind::Stream;
    }
    if name.contains("fetch") {
        return BlockerKind::ServerFetch;
    }
    if name.contains("cache") || s.description.to_lowercase().contains("cache") {
        return BlockerKind::Cache;
    }
    if name.starts_with("use") {
        return BlockerKind::ClientHook;
    }
    if let Some(frame) = source_frame {
        if is_frameworkish_path(&frame.1) {
            return BlockerKind::Framework;
        }
    }
    BlockerKind::Unknown
}

fn suggest_blocker_fix(kind: BlockerKind) -> String {
    match kind {
        BlockerKind::ClientHook => "Move route hooks behind a smaller client Suspense or provide a real non-null loading fallback for this segment.",
        BlockerKind::RequestApi => "Push request-bound reads to a smaller server leaf, or cache around them so the parent shell can stay static.",
        BlockerKind::ServerFetch => "Split static shell content from data widgets, then push the fetch into smaller Suspense leaves or cache it.",
        BlockerKind::Cache => "This looks cache-related; check whether \"use cache\" or runtime prefetch can eliminate the suspension.",
        BlockerKind::Stream => "A stream is still pending here; extract static siblings outside the boundary and push the stream consumer deeper.",
        BlockerKind::Framework => "This currently looks framework-driven; find the nearest user-owned caller above it before changing code.",
        BlockerKind::Unknown => "Inspect the nearest user-owned owner/awaiter frame and verify whether this suspender really belongs at this boundary.",
    }.to_string()
}

fn recommend_fix(
    boundary_kind: BoundaryKind,
    primary: Option<&ActionableBlocker>,
    unknown_suspenders: Option<&str>,
) -> String {
    if boundary_kind == BoundaryKind::RouteSegment
        && primary.is_some_and(|p| p.kind == BlockerKind::ClientHook)
    {
        return "This route segment is suspending on client hooks. Check loading.tsx first; if it is null or visually empty, fix the fallback before chasing deeper push-down work.".to_string();
    }
    if let Some(p) = primary {
        match p.kind {
            BlockerKind::ClientHook => {
                return "Push the hook-using client UI behind a smaller local Suspense boundary so the parent shell can prerender.".to_string();
            }
            BlockerKind::RequestApi | BlockerKind::ServerFetch => {
                return "Push the request-bound async work into a smaller leaf or split static siblings out of this boundary.".to_string();
            }
            BlockerKind::Cache => {
                return "Check whether caching or runtime prefetch can move this personalized content into the shell.".to_string();
            }
            BlockerKind::Stream => {
                return "Keep the stream behind Suspense, but extract any static shell content outside the boundary.".to_string();
            }
            BlockerKind::Framework => {
                return "The top blocker still looks framework-heavy. Find the nearest user-owned caller before changing boundary placement.".to_string();
            }
            _ => {}
        }
    }
    if let Some(reason) = unknown_suspenders {
        return format!(
            "React could not identify the suspender ({}). Investigate the nearest user-owned owner or awaiter frame.",
            reason
        );
    }
    "No primary blocker was identified. Inspect the boundary source and owner chain directly."
        .to_string()
}

fn pick_preferred_frame(stack: Option<&[StackFrame]>) -> Option<StackFrame> {
    let s = stack?;
    if s.is_empty() {
        return None;
    }
    s.iter()
        .find(|f| !is_frameworkish_path(&f.1))
        .cloned()
        .or_else(|| s.first().cloned())
}

fn is_frameworkish_path(file: &str) -> bool {
    file.contains("/node_modules/")
}

fn build_blocker_key(name: &str, kind: BlockerKind, source_frame: Option<&StackFrame>) -> String {
    match source_frame {
        None => format!("{}:{}:unknown", kind.label(), name),
        Some(f) => format!("{}:{}:{}:{}", kind.label(), name, f.1, f.2),
    }
}

fn build_root_causes(holes: &[BoundaryInsight]) -> Vec<RootCauseGroup> {
    let mut groups: HashMap<String, RootCauseGroup> = HashMap::new();
    for hole in holes {
        let Some(blocker) = &hole.primary_blocker else {
            continue;
        };
        let display_name = hole
            .name
            .clone()
            .unwrap_or_else(|| format!("boundary-{}", hole.id));
        groups
            .entry(blocker.key.clone())
            .and_modify(|existing| {
                existing.boundary_names.push(display_name.clone());
                existing.count += 1;
                if blocker.actionability > existing.actionability {
                    existing.actionability = blocker.actionability;
                }
            })
            .or_insert_with(|| RootCauseGroup {
                kind: blocker.kind,
                name: blocker.name.clone(),
                source_frame: blocker.source_frame.clone(),
                boundary_names: vec![display_name],
                count: 1,
                actionability: blocker.actionability,
                suggestion: blocker.suggestion.clone(),
            });
    }
    let mut out: Vec<RootCauseGroup> = groups.into_values().collect();
    out.sort_by(|a, b| {
        let score_a = (a.count as i32) * a.actionability;
        let score_b = (b.count as i32) * b.actionability;
        score_b.cmp(&score_a).then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn collect_files_to_read(holes: &[BoundaryInsight], root_causes: &[RootCauseGroup]) -> Vec<String> {
    let mut counts: HashMap<String, i32> = HashMap::new();
    let mut add = |f: Option<&str>| {
        if let Some(path) = f {
            if !path.is_empty() {
                *counts.entry(path.to_string()).or_insert(0) += 1;
            }
        }
    };
    for hole in holes {
        add(hole.source.as_ref().map(|s| s.0.as_str()));
        if let Some(pb) = &hole.primary_blocker {
            add(pb.source_frame.as_ref().map(|f| f.1.as_str()));
        }
        for owner in &hole.rendered_by {
            add(owner.source.as_ref().map(|s| s.0.as_str()));
        }
    }
    for cause in root_causes {
        add(cause.source_frame.as_ref().map(|f| f.1.as_str()));
    }

    let mut entries: Vec<(String, i32)> = counts.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries.into_iter().take(12).map(|(f, _)| f).collect()
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn format_report(report: &AnalysisReport, only_dynamic: bool) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push("# Suspense Boundary Analysis".to_string());
    if only_dynamic {
        lines.push(format!(
            "# {} dynamic holes (static boundaries hidden; pass without --only-dynamic to see them)",
            report.dynamic_hole_count
        ));
    } else {
        lines.push(format!(
            "# {} boundaries: {} dynamic holes, {} static",
            report.total_boundaries, report.dynamic_hole_count, report.static_count
        ));
    }
    lines.push(String::new());

    if !report.holes.is_empty() {
        lines.push("## Summary".to_string());
        if let Some(top) = report.holes.first() {
            if let Some(blocker) = &top.primary_blocker {
                lines.push(format!(
                    "- Top actionable hole: {} - {} ({})",
                    top.name.clone().unwrap_or_else(|| "(unnamed)".into()),
                    blocker.name,
                    blocker.kind.label()
                ));
                lines.push(format!("- Suggested next step: {}", top.recommendation));
            }
        }
        if let Some(root) = report.root_causes.first() {
            lines.push(format!(
                "- Most common root cause: {} ({}) affecting {} boundar{}",
                root.name,
                root.kind.label(),
                root.count,
                if root.count == 1 { "y" } else { "ies" }
            ));
        }
        lines.push(String::new());

        lines.push("## Quick Reference".to_string());
        lines.push(
            "| Boundary | Type | Primary blocker | Source | Suggested next step |".to_string(),
        );
        lines.push("| --- | --- | --- | --- | --- |".to_string());
        for hole in &report.holes {
            let blocker = &hole.primary_blocker;
            let source = match blocker.as_ref().and_then(|b| b.source_frame.as_ref()) {
                Some(f) => format!("{}:{}", f.1, f.2),
                None => match &hole.source {
                    Some((f, l, _)) => format!("{}:{}", f, l),
                    None => "unknown".to_string(),
                },
            };
            let blocker_text = match blocker {
                Some(b) => format!("{} ({})", b.name, b.kind.label()),
                None => "unknown".to_string(),
            };
            lines.push(format!(
                "| {} | {} | {} | {} | {} |",
                escape_cell(hole.name.as_deref().unwrap_or("(unnamed)")),
                hole.boundary_kind.label(),
                escape_cell(&blocker_text),
                escape_cell(&source),
                escape_cell(&hole.recommendation),
            ));
        }
        lines.push(String::new());

        if !report.files_to_read.is_empty() {
            lines.push("## Files to Read".to_string());
            for file in &report.files_to_read {
                lines.push(format!("- {}", file));
            }
            lines.push(String::new());
        }

        if !report.root_causes.is_empty() {
            lines.push("## Root Causes".to_string());
            for cause in &report.root_causes {
                let source = match &cause.source_frame {
                    Some(f) => format!("{}:{}", f.1, f.2),
                    None => "unknown".to_string(),
                };
                lines.push(format!(
                    "- {} ({}) at {} - affects {} boundar{}",
                    cause.name,
                    cause.kind.label(),
                    source,
                    cause.count,
                    if cause.count == 1 { "y" } else { "ies" }
                ));
                lines.push(format!("  next step: {}", cause.suggestion));
                lines.push(format!("  boundaries: {}", cause.boundary_names.join(", ")));
            }
            lines.push(String::new());
        }
    }

    if !only_dynamic && !report.statics.is_empty() {
        lines.push("## Static (not suspended)".to_string());
        for b in &report.statics {
            let name = b.name.clone().unwrap_or_else(|| "(unnamed)".into());
            let src = match &b.source {
                Some(s) => format!(" at {}:{}:{}", s.0, s.1, s.2),
                None => String::new(),
            };
            lines.push(format!("  {}{}", name, src));
        }
    }

    lines.join("\n")
}
