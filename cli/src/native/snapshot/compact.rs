//! Budgeted compact snapshots (`snapshot -c`).
//!
//! The renderer emits one [`SnapshotLine`] per tree line together with the
//! accessibility facts that compaction ranks by. Compaction then:
//!
//! 1. drops structural lines with no content below them;
//! 2. collapses long runs of adjacent siblings with the same repeated role
//!    (rows, list items, options, links), keeping the first and last few items
//!    plus any item that holds a focused, selected, or checked element, and
//!    items with interactive content when at most a quarter of the run has any;
//! 3. ranks the remaining content lines and keeps the best ones that fit the
//!    token budget, together with their ancestors so the output stays a tree
//!    and with the line that says where they are (a table row's identifying
//!    cell, or the heading above a lead paragraph);
//! 4. marks omitted content: each run of collapsed items gets a marker where
//!    it was (`- … 493 more rows (500 total)`), and the other omitted children
//!    of a parent share one marker after its last printed child
//!    (`- … 12 more links`).
//!
//! Compaction only hides lines. Refs are allocated before it runs, so a ref
//! printed by a compact snapshot is the same ref the full snapshot prints, and
//! refs for hidden lines keep working with actions.

/// Default token budget for `snapshot -c`. Picked from measurements: it keeps
/// every control on typical forms, menus, dialogs, and article pages, and cuts
/// large tables, feeds, and documentation pages to a size that fits easily in
/// an agent's context.
pub const DEFAULT_MAX_TOKENS: usize = 2000;

/// Non-interactive names and values longer than this many characters are
/// truncated with an ellipsis. Interactive names are never truncated.
pub(super) const TEXT_LIMIT: usize = 100;

/// Sibling runs with more items than this are collapsed.
const COLLAPSE_MIN_ITEMS: usize = 12;
/// Items kept at the start of a collapsed run.
const COLLAPSE_HEAD: usize = 5;
/// Items kept at the end of a collapsed run.
const COLLAPSE_TAIL: usize = 2;

/// Estimated cost of the summary line printed when lines were omitted.
const SUMMARY_TOKENS: usize = 32;

/// Roles whose sibling runs are treated as repeated structure.
/// Cells form long runs only in very wide rows.
const REPEATED_ROLES: &[&str] = &[
    "row",
    "listitem",
    "option",
    "treeitem",
    "article",
    "link",
    "term",
    "definition",
    "cell",
    "gridcell",
];

const HEADING_LANDMARK_ROLES: &[&str] = &[
    "heading",
    "main",
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "search",
    "form",
    "region",
    "dialog",
    "alertdialog",
    "alert",
    "status",
    "tabpanel",
];

const LABEL_ROLES: &[&str] = &[
    "LabelText",
    "legend",
    "caption",
    "columnheader",
    "rowheader",
    "group",
    "radiogroup",
    "tooltip",
];

/// Roles that can be the lead block under a heading.
const LEAD_ROLES: &[&str] = &["paragraph", "list", "DescriptionList"];
/// How many siblings after a heading are searched for its lead block.
const LEAD_LOOKAHEAD: usize = 3;

/// Roles whose names are decoration rather than content.
const DECORATIVE_ROLES: &[&str] = &["ListMarker"];

/// Where a node sits relative to the viewport when the snapshot was taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Placement {
    /// No layout information (iframe content, nodes without a DOM node).
    #[default]
    Unknown,
    InViewport,
    Offscreen,
    /// Not rendered, or rendered with an empty box (for example visually
    /// hidden skip links).
    Hidden,
}

/// One rendered snapshot line and the node facts compaction ranks by.
#[derive(Clone, Debug, Default)]
pub(super) struct SnapshotLine {
    /// The rendered line, including indentation.
    pub text: String,
    pub depth: usize,
    pub role: String,
    /// Interactive role or cursor-interactive element.
    pub interactive: bool,
    pub has_ref: bool,
    /// The line shows a name.
    pub named: bool,
    /// The full display name, before truncation.
    pub name: String,
    /// The line shows a value (`: value`).
    pub has_value: bool,
    pub focused: bool,
    /// Dialog that is modal (`aria-modal` or `showModal()`).
    pub modal: bool,
    pub expanded: Option<bool>,
    /// Selected, checked, or pressed.
    pub active: bool,
    pub placement: Placement,
}

impl SnapshotLine {
    fn is_content(&self, interactive_only: bool) -> bool {
        self.has_ref
            || self.has_value
            || (!interactive_only && self.named && !DECORATIVE_ROLES.contains(&self.role.as_str()))
    }

    fn is_dialog(&self) -> bool {
        self.modal || matches!(self.role.as_str(), "dialog" | "alertdialog")
    }
}

/// Shorten `text` to [`TEXT_LIMIT`] characters, ending with an ellipsis.
pub(super) fn truncate_text(text: &str) -> std::borrow::Cow<'_, str> {
    match text.char_indices().nth(TEXT_LIMIT - 1) {
        Some((cut, _)) if text[cut..].chars().nth(1).is_some() => {
            format!("{}…", text[..cut].trim_end()).into()
        }
        _ => text.into(),
    }
}

/// Approximate o200k token count of one output line, newline included.
/// Indentation tokenizes as one or two tokens, the rest at about 3 bytes per
/// token. On the benchmark pages this estimates compact output at 0.97 to
/// 1.28 times its real token count.
pub(super) fn estimate_tokens(text: &str) -> usize {
    text.trim_start().len() / 3 + 2
}

/// Tree links between lines, derived from line depth.
struct Tree {
    parent: Vec<Option<usize>>,
    children: Vec<Vec<usize>>,
    roots: Vec<usize>,
    /// One past the last line of each subtree (lines are in preorder).
    end: Vec<usize>,
}

impl Tree {
    fn new(lines: &[SnapshotLine]) -> Self {
        let n = lines.len();
        let mut parent = vec![None; n];
        let mut children = vec![Vec::new(); n];
        let mut roots = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            while stack
                .last()
                .is_some_and(|&top| lines[top].depth >= line.depth)
            {
                stack.pop();
            }
            parent[i] = stack.last().copied();
            match parent[i] {
                Some(p) => children[p].push(i),
                None => roots.push(i),
            }
            stack.push(i);
        }
        let mut end = vec![0; n];
        for i in (0..n).rev() {
            end[i] = children[i].last().map_or(i + 1, |&c| end[c]);
        }
        Self {
            parent,
            children,
            roots,
            end,
        }
    }

    fn children_of(&self, parent: Option<usize>) -> &[usize] {
        match parent {
            Some(p) => &self.children[p],
            None => &self.roots,
        }
    }

    fn subtree(&self, i: usize) -> std::ops::Range<usize> {
        i..self.end[i]
    }
}

/// A collapsed run of repeated siblings.
struct CollapseGroup {
    total: usize,
    /// The last item hidden by collapsing, which carries the total in its marker.
    last_hidden: usize,
}

/// Compact `lines` into at most about `max_tokens` tokens of output.
/// `usize::MAX` disables the cap but still collapses and truncates.
pub(super) fn compact_lines(
    lines: &[SnapshotLine],
    interactive_only: bool,
    max_tokens: usize,
) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let tree = Tree::new(lines);
    let n = lines.len();

    let content: Vec<bool> = (0..n)
        .map(|i| lines[i].is_content(interactive_only) && !repeats_nearby_name(lines, &tree, i))
        .collect();
    // A line is live when it, or something below it, is content. Children
    // follow their parents, so one reverse pass propagates liveness upward.
    let mut live = content.clone();
    for i in (0..n).rev() {
        if live[i] {
            if let Some(p) = tree.parent[i] {
                live[p] = true;
            }
        }
    }

    let lead = lead_blocks(lines, &tree);
    let scores = score_lines(lines, &tree, &content, &lead);

    // Collapse repeated structure.
    let mut collapsed = vec![false; n];
    let mut group_of: Vec<Option<usize>> = vec![None; n];
    let mut groups: Vec<CollapseGroup> = Vec::new();
    for parent in std::iter::once(None).chain((0..n).map(Some)) {
        if parent.is_some_and(|p| !live[p]) {
            continue;
        }
        let kids: Vec<usize> = tree
            .children_of(parent)
            .iter()
            .copied()
            .filter(|&c| live[c])
            .collect();
        if kids.len() <= COLLAPSE_MIN_ITEMS {
            continue;
        }
        // Runs of adjacent siblings with the same repeated role.
        let runs = kids
            .chunk_by(|&a, &b| lines[a].role == lines[b].role)
            .filter(|run| REPEATED_ROLES.contains(&lines[run[0]].role.as_str()));
        let has = |item: usize, pred: &dyn Fn(&SnapshotLine) -> bool| {
            tree.subtree(item).any(|j| pred(&lines[j]))
        };
        for run in runs {
            // Header rows always stay and are not counted as repeated items.
            let items: Vec<usize> = run
                .iter()
                .copied()
                .filter(|&item| !has(item, &|l| l.role == "columnheader"))
                .collect();
            if items.len() <= COLLAPSE_MIN_ITEMS {
                continue;
            }
            let interactive_items = items
                .iter()
                .filter(|&&item| has(item, &|l| l.interactive))
                .count();
            // Interactive content marks an item only when few items have it;
            // otherwise interactivity is part of the repeated structure.
            let pin_interactive = interactive_items * 4 <= items.len();
            let gid = groups.len();
            let mut last_hidden = None;
            for (k, &item) in items.iter().enumerate() {
                let keep = k < COLLAPSE_HEAD
                    || k >= items.len() - COLLAPSE_TAIL
                    || has(item, &|l| l.focused || l.active)
                    || (pin_interactive && has(item, &|l| l.interactive));
                if !keep {
                    collapsed[item] = true;
                    group_of[item] = Some(gid);
                    last_hidden = Some(item);
                }
            }
            if let Some(last_hidden) = last_hidden {
                groups.push(CollapseGroup {
                    total: items.len(),
                    last_hidden,
                });
            }
        }
    }

    // Lines inside a collapsed item are hidden too.
    let mut hidden = collapsed.clone();
    for i in 0..n {
        if let Some(p) = tree.parent[i] {
            hidden[i] |= hidden[p];
        }
    }

    // The line that says where a kept line is: its row's identifying cell,
    // or the heading above its lead paragraph unless a parent with the same
    // name (such as a named region) already shows it.
    let identity = row_identity(lines, &tree, &content);
    let context: Vec<Option<usize>> = (0..n)
        .map(|i| {
            identity[i].or_else(|| {
                lead[i].filter(|&h| {
                    !tree.parent[h].is_some_and(|p| lines[p].name.trim() == lines[h].name.trim())
                })
            })
        })
        .collect();
    let costs: Vec<usize> = lines.iter().map(|l| estimate_tokens(&l.text)).collect();
    let mut candidates: Vec<usize> = (0..n).filter(|&i| content[i] && !hidden[i]).collect();
    candidates.sort_by_key(|&i| (std::cmp::Reverse(scores[i]), i));
    let total_live = (0..n).filter(|&i| live[i]).count();

    // Reserve room for markers and the summary line.
    let mut line_budget = if max_tokens == usize::MAX {
        usize::MAX
    } else {
        max_tokens.saturating_sub(max_tokens / 10 + SUMMARY_TOKENS)
    };
    loop {
        let selected = select_lines(lines, &tree, &candidates, &context, &costs, line_budget);
        let output = render(
            lines, &tree, &live, &selected, &collapsed, &hidden, &group_of, &groups, total_live,
        );
        if max_tokens == usize::MAX || line_budget < 16 {
            return output;
        }
        // Markers can take more than the reserve. Shrink by the overshoot
        // (at least 5%) so the output ends up close to the budget.
        let used: usize = output.lines().map(estimate_tokens).sum();
        if used <= max_tokens {
            return output;
        }
        line_budget = line_budget.saturating_sub((used - max_tokens).max(line_budget / 20));
    }
}

/// Priority score per line; higher is kept first.
fn score_lines(
    lines: &[SnapshotLine],
    tree: &Tree,
    content: &[bool],
    lead: &[Option<usize>],
) -> Vec<i32> {
    let n = lines.len();
    let mut in_dialog = vec![false; n];
    let mut under_collapsed = vec![false; n];
    for i in 0..n {
        let parent = tree.parent[i];
        in_dialog[i] = lines[i].is_dialog() || parent.is_some_and(|p| in_dialog[p]);
        under_collapsed[i] =
            parent.is_some_and(|p| under_collapsed[p] || lines[p].expanded == Some(false));
    }
    (0..n)
        .map(|i| {
            if !content[i] {
                return 0;
            }
            let line = &lines[i];
            let role = line.role.as_str();
            let near_interactive = tree.parent[i].is_some_and(|p| {
                lines[p].role == "LabelText"
                    || tree.children[p]
                        .iter()
                        .any(|&s| s != i && lines[s].interactive)
            });
            // Links inside running text read as part of that text, so they
            // rank just below headings rather than with standalone controls.
            // In the viewport or in the lead paragraph under a heading they
            // are what a reader sees first, so they rank as controls there.
            let inline_link = role == "link"
                && lead[i].is_none()
                && line.placement != Placement::InViewport
                && tree.parent[i].is_some_and(|p| {
                    tree.children[p]
                        .iter()
                        .any(|&s| lines[s].role == "StaticText")
                });
            // A link that only repeats its heading (a section permalink, or a
            // post title on an index page) ranks with headings: above text
            // and in-text links, below standalone controls.
            let heading_link = role == "link"
                && tree.parent[i].is_some_and(|p| {
                    lines[p].role == "heading" && lines[p].name.trim() == line.name.trim()
                });
            let mut score = if inline_link {
                250
            } else if line.interactive && !heading_link {
                400
            } else if HEADING_LANDMARK_ROLES.contains(&role) || heading_link {
                300
            } else if LABEL_ROLES.contains(&role) || near_interactive {
                200
            } else {
                100
            };
            if line.focused {
                score += 10_000;
            }
            if in_dialog[i] {
                score += 300;
            }
            if line.active {
                score += 20;
            }
            if under_collapsed[i] {
                score -= 150;
            }
            score += match line.placement {
                Placement::InViewport => 50,
                Placement::Offscreen => -50,
                Placement::Hidden => -200,
                Placement::Unknown => 0,
            };
            score
        })
        .collect()
}

/// For each line inside a table row, the line that says which row it is:
/// the row's first row header, or else its first named cell. Unnamed rows
/// only; a named row already identifies itself.
fn row_identity(lines: &[SnapshotLine], tree: &Tree, content: &[bool]) -> Vec<Option<usize>> {
    let n = lines.len();
    let named_cell = |c: usize, roles: &[&str]| {
        content[c] && lines[c].named && roles.contains(&lines[c].role.as_str())
    };
    // The identifying cell of each unnamed row line.
    let mut of_row = vec![None; n];
    let mut identity = vec![None; n];
    for i in 0..n {
        if lines[i].role == "row" && !lines[i].named {
            let cells = &tree.children[i];
            of_row[i] = cells
                .iter()
                .copied()
                .find(|&c| named_cell(c, &["rowheader"]))
                .or_else(|| {
                    cells
                        .iter()
                        .copied()
                        .find(|&c| named_cell(c, &["cell", "gridcell"]))
                });
        }
        identity[i] = tree.parent[i].and_then(|p| {
            if lines[p].role == "row" {
                of_row[p]
            } else {
                identity[p]
            }
        });
    }
    identity
}

/// For lines inside the lead block of a heading, that heading. The lead
/// block is the first paragraph, list, or description list among the next
/// few siblings after the heading, before any other heading.
fn lead_blocks(lines: &[SnapshotLine], tree: &Tree) -> Vec<Option<usize>> {
    let mut lead_of = vec![None; lines.len()];
    for parent in std::iter::once(None).chain((0..lines.len()).map(Some)) {
        let siblings = tree.children_of(parent);
        for (k, &h) in siblings.iter().enumerate() {
            if lines[h].role != "heading" {
                continue;
            }
            let lead = siblings[k + 1..]
                .iter()
                .take(LEAD_LOOKAHEAD)
                .take_while(|&&s| lines[s].role != "heading")
                .find(|&&s| LEAD_ROLES.contains(&lines[s].role.as_str()));
            if let Some(&block) = lead {
                lead_of[tree.subtree(block)].fill(Some(h));
            }
        }
    }
    lead_of
}

/// Greedily keep the best candidates, with their ancestors, within `budget`
/// tokens. Each line brings its `context` line along (the row's
/// identifying cell, or the heading above a lead paragraph), so a kept
/// control still says which row or section it belongs to. Focused lines are
/// always kept.
fn select_lines(
    lines: &[SnapshotLine],
    tree: &Tree,
    candidates: &[usize],
    context: &[Option<usize>],
    costs: &[usize],
    budget: usize,
) -> Vec<bool> {
    let mut selected = vec![false; lines.len()];
    let mut used = 0usize;
    let mut take: Vec<usize> = Vec::new();
    for &i in candidates {
        take.clear();
        for start in [Some(i), context[i]] {
            let mut node = start;
            while let Some(j) = node {
                if selected[j] || take.contains(&j) {
                    break;
                }
                take.push(j);
                node = tree.parent[j];
            }
        }
        let cost: usize = take.iter().map(|&j| costs[j]).sum();
        if cost == 0 {
            continue;
        }
        if !lines[i].focused && used.saturating_add(cost) > budget {
            continue;
        }
        used += cost;
        for &j in &take {
            selected[j] = true;
        }
    }
    selected
}

/// Whether a plain line only repeats a name already shown next to it: label
/// text matching its control, a legend or caption matching its group or
/// table, or an image or text node matching its parent.
fn repeats_nearby_name(lines: &[SnapshotLine], tree: &Tree, i: usize) -> bool {
    let line = &lines[i];
    if line.has_ref || line.has_value || line.interactive || !line.named {
        return false;
    }
    let name = line.name.trim();
    let same = |j: usize| lines[j].named && lines[j].name.trim() == name;
    let Some(parent) = tree.parent[i] else {
        return false;
    };
    same(parent)
        || (!lines[parent].named && tree.parent[parent].is_some_and(same))
        || tree.children[parent]
            .iter()
            .any(|&s| s != i && (lines[s].interactive || lines[s].has_ref) && same(s))
}

#[allow(clippy::too_many_arguments)]
fn render(
    lines: &[SnapshotLine],
    tree: &Tree,
    live: &[bool],
    selected: &[bool],
    collapsed: &[bool],
    hidden: &[bool],
    group_of: &[Option<usize>],
    groups: &[CollapseGroup],
    total_live: usize,
) -> String {
    let ctx = RenderContext {
        lines,
        tree,
        live,
        selected,
        collapsed,
        group_of,
        groups,
    };
    let mut out = String::new();
    let shown = ctx.render_children(None, &mut out);
    if shown < total_live {
        // Collapsing and truncation happen at any budget, so only dropping
        // -c brings collapsed items back. A larger budget or a narrower
        // scope only helps when the budget cut lines.
        let budget_cut = (0..lines.len()).any(|i| live[i] && !hidden[i] && !selected[i]);
        let collapsed_any = (0..lines.len()).any(|i| live[i] && collapsed[i]);
        let advice = match (budget_cut, collapsed_any) {
            (true, false) => "use --max-tokens <n>, -s <selector>, or no -c for more",
            (true, true) => {
                "use --max-tokens <n> or -s <selector> for more, or drop -c to see collapsed items"
            }
            _ => "drop -c to see collapsed items",
        };
        out.push_str(&format!(
            "(compact: showing {} of {} lines; {})\n",
            shown, total_live, advice
        ));
    }
    out
}

struct RenderContext<'a> {
    lines: &'a [SnapshotLine],
    tree: &'a Tree,
    live: &'a [bool],
    selected: &'a [bool],
    collapsed: &'a [bool],
    group_of: &'a [Option<usize>],
    groups: &'a [CollapseGroup],
}

impl RenderContext<'_> {
    /// Print the selected children of `parent` and return how many
    /// lines were printed. Items hidden by collapsing get a marker where they
    /// were; everything else omitted under one parent shares a single marker
    /// after its last printed child.
    fn render_children(&self, parent: Option<usize>, out: &mut String) -> usize {
        let mut shown = 0;
        let mut collapsed_run: Vec<usize> = Vec::new();
        let mut omitted: Vec<usize> = Vec::new();
        for &c in self.tree.children_of(parent) {
            if !self.live[c] {
                continue;
            }
            if self.collapsed[c] {
                collapsed_run.push(c);
                continue;
            }
            if !self.selected[c] {
                omitted.push(c);
                continue;
            }
            self.marker(&mut collapsed_run, out);
            out.push_str(&self.lines[c].text);
            out.push('\n');
            shown += 1 + self.render_children(Some(c), out);
        }
        self.marker(&mut collapsed_run, out);
        self.marker(&mut omitted, out);
        shown
    }

    fn marker(&self, run: &mut Vec<usize>, out: &mut String) {
        let Some(&first) = run.first() else {
            return;
        };
        let lines = self.lines;
        let role = lines[first].role.as_str();
        let noun = if run.iter().all(|&i| lines[i].role == role) {
            plural_role(role, run.len())
        } else {
            if run.len() == 1 { "node" } else { "nodes" }.to_string()
        };
        out.push_str(&format!(
            "{}- … {} more {}",
            "  ".repeat(lines[first].depth),
            run.len(),
            noun
        ));
        if let Some(group) = run.iter().find_map(|&i| {
            self.group_of[i]
                .map(|g| &self.groups[g])
                .filter(|g| g.last_hidden == i)
        }) {
            out.push_str(&format!(" ({} total)", group.total));
        }
        out.push('\n');
        run.clear();
    }
}

fn plural_role(role: &str, count: usize) -> String {
    let (one, many) = match role {
        "StaticText" => ("text node", "text nodes"),
        "listitem" => ("list item", "list items"),
        "treeitem" => ("tree item", "tree items"),
        "menuitem" => ("menu item", "menu items"),
        "columnheader" => ("column header", "column headers"),
        "rowheader" => ("row header", "row headers"),
        "LabelText" => ("label", "labels"),
        "generic" | "group" | "none" | "presentation" => ("section", "sections"),
        "DescriptionList" => ("description list", "description lists"),
        "Iframe" => ("iframe", "iframes"),
        _ => {
            let lower = role.to_lowercase();
            return if count == 1 {
                lower
            } else {
                format!("{}s", lower)
            };
        }
    };
    if count == 1 { one } else { many }.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build lines from `(depth, role, name, ref)` tuples. Roles listed as
    /// interactive by the snapshot renderer are marked interactive.
    fn line(depth: usize, role: &str, name: &str, ref_id: Option<&str>) -> SnapshotLine {
        let mut text = format!("{}- {}", "  ".repeat(depth), role);
        if !name.is_empty() {
            text.push_str(&format!(" {:?}", name));
        }
        if let Some(r) = ref_id {
            text.push_str(&format!(" [ref={}]", r));
        }
        SnapshotLine {
            text,
            depth,
            role: role.to_string(),
            interactive: super::super::INTERACTIVE_ROLES.contains(&role),
            has_ref: ref_id.is_some(),
            named: !name.is_empty(),
            name: name.to_string(),
            ..SnapshotLine::default()
        }
    }

    fn table(rows: usize) -> Vec<SnapshotLine> {
        let mut lines = vec![
            line(0, "main", "", None),
            line(1, "heading", "Customers", Some("e1")),
            line(1, "table", "", None),
            line(2, "rowgroup", "", None),
            line(3, "row", "", None),
            line(4, "columnheader", "Name", Some("e2")),
        ];
        for r in 0..rows {
            lines.push(line(2, "row", "", None));
            lines.push(line(
                3,
                "cell",
                &format!("Customer {r}"),
                Some(&format!("c{r}")),
            ));
            lines.push(line(4, "button", "Edit", Some(&format!("b{r}"))));
        }
        lines.push(line(1, "link", "Next page", Some("e3")));
        lines
    }

    #[test]
    fn collapses_long_tables_and_states_total() {
        let out = compact_lines(&table(500), false, usize::MAX);
        assert!(out.contains("cell \"Customer 0\""));
        assert!(out.contains("cell \"Customer 4\""));
        assert!(!out.contains("cell \"Customer 5\""));
        assert!(out.contains("cell \"Customer 498\""));
        assert!(out.contains("cell \"Customer 499\""));
        assert!(out.contains("    - … 493 more rows (500 total)\n"), "{out}");
        assert!(out.contains("link \"Next page\""));
        assert!(out.contains("columnheader \"Name\""));
    }

    /// Lines as `-i -c` renders a table: row lines without the rowgroup and
    /// table wrappers, plain-text cells (cells always carry refs), and an
    /// actions cell holding Edit and Delete buttons.
    fn interactive_table(rows: usize) -> Vec<SnapshotLine> {
        let mut lines = vec![line(0, "heading", "Users", Some("h1"))];
        lines.push(line(0, "row", "", None));
        for column in ["ID", "Name", "Actions"] {
            lines.push(line(
                1,
                "columnheader",
                column,
                Some(&format!("ch{column}")),
            ));
        }
        for r in 0..rows {
            lines.push(line(0, "row", "", None));
            lines.push(line(1, "cell", &format!("U-{r}"), Some(&format!("id{r}"))));
            lines.push(line(
                1,
                "cell",
                &format!("User {r}"),
                Some(&format!("n{r}")),
            ));
            lines.push(line(1, "cell", "Edit Delete", Some(&format!("a{r}"))));
            lines.push(line(2, "button", "Edit", Some(&format!("e{r}"))));
            lines.push(line(2, "button", "Delete", Some(&format!("d{r}"))));
        }
        lines
    }

    /// For every printed button, whether its row also prints the row's ID cell.
    fn buttons_with_row_identity(out: &str) -> (usize, usize) {
        let lines: Vec<&str> = out.lines().collect();
        let (mut buttons, mut identified) = (0, 0);
        for (i, text) in lines.iter().enumerate() {
            if !text.trim_start().starts_with("- button") {
                continue;
            }
            buttons += 1;
            let Some(row) = (0..i).rev().find(|&r| lines[r].trim() == "- row") else {
                continue;
            };
            let row_depth = lines[row].len() - lines[row].trim_start().len();
            if lines[row + 1..]
                .iter()
                .take_while(|l| l.len() - l.trim_start().len() > row_depth)
                .any(|l| l.trim_start().starts_with("- cell \"U-"))
            {
                identified += 1;
            }
        }
        (buttons, identified)
    }

    #[test]
    fn interactive_compact_collapses_whole_rows_and_keeps_row_identity() {
        let out = compact_lines(&interactive_table(500), true, DEFAULT_MAX_TOKENS);
        // The header row is kept and is not counted as one of the rows.
        assert!(out.contains("- … 493 more rows (500 total)\n"), "{out}");
        assert!(out.contains("columnheader \"Actions\""), "{out}");
        let (buttons, identified) = buttons_with_row_identity(&out);
        assert_eq!(buttons, 14, "{out}");
        assert_eq!(identified, buttons, "{out}");
        assert!(out.contains("cell \"U-499\""), "{out}");
    }

    #[test]
    fn tight_budget_keeps_each_buttons_row_identity() {
        let mut lines = interactive_table(500);
        let focused = lines
            .iter()
            .position(|l| l.text.contains("[ref=e250]"))
            .unwrap();
        lines[focused].focused = true;
        for budget in [60, 120, 250] {
            let out = compact_lines(&lines, true, budget);
            let (buttons, identified) = buttons_with_row_identity(&out);
            assert!(buttons > 0, "{out}");
            assert_eq!(identified, buttons, "budget {budget}:\n{out}");
            assert!(
                out.contains("[ref=e250]") && out.contains("cell \"U-250\""),
                "{out}"
            );
        }
    }

    #[test]
    fn row_identity_prefers_row_headers() {
        let mut lines = vec![line(0, "table", "", None)];
        for r in 0..20 {
            lines.push(line(1, "row", "", None));
            lines.push(line(
                2,
                "cell",
                &format!("Note {r}"),
                Some(&format!("c{r}")),
            ));
            lines.push(line(
                2,
                "rowheader",
                &format!("Order {r}"),
                Some(&format!("h{r}")),
            ));
            lines.push(line(2, "cell", "", Some(&format!("a{r}"))));
            lines.push(line(3, "button", "Refund", Some(&format!("b{r}"))));
        }
        let out = compact_lines(&lines, true, 150);
        assert!(out.contains("rowheader \"Order 1\""), "{out}");
        assert!(out.contains("button \"Refund\" [ref=b1]"), "{out}");
        assert!(!out.contains("cell \"Note 1\""), "{out}");
    }

    #[test]
    fn short_runs_are_not_collapsed() {
        let out = compact_lines(&table(COLLAPSE_MIN_ITEMS), false, usize::MAX);
        assert!(!out.contains('…'), "{out}");
        assert!(!out.contains("(compact:"));
    }

    #[test]
    fn collapse_only_counts_adjacent_siblings() {
        let mut lines = vec![line(0, "navigation", "", None)];
        for section in 0..3 {
            lines.push(line(
                1,
                "heading",
                &format!("Section {section}"),
                Some(&format!("h{section}")),
            ));
            for i in 0..10 {
                lines.push(line(
                    1,
                    "link",
                    &format!("Link {section}.{i}"),
                    Some(&format!("l{section}{i}")),
                ));
            }
        }
        let out = compact_lines(&lines, true, usize::MAX);
        assert!(!out.contains('…'), "{out}");
        assert_eq!(out.matches("- link").count(), 30);
    }

    #[test]
    fn collapse_keeps_focused_and_selected_rows() {
        let mut lines = table(100);
        let focused = lines
            .iter()
            .position(|l| l.text.contains("[ref=b50]"))
            .unwrap();
        lines[focused].focused = true;
        let selected = lines
            .iter()
            .position(|l| l.text.contains("[ref=c70]"))
            .unwrap();
        lines[selected].active = true;
        let out = compact_lines(&lines, false, usize::MAX);
        assert!(out.contains("[ref=b50]"));
        assert!(out.contains("[ref=c70]"));
        assert!(out.contains("- … 45 more rows\n"), "{out}");
        assert!(out.contains("- … 19 more rows\n"), "{out}");
        assert!(out.contains("- … 27 more rows (100 total)\n"), "{out}");
    }

    #[test]
    fn collapse_keeps_rows_whose_interactive_content_is_the_exception() {
        let mut lines = vec![line(0, "list", "", None)];
        for i in 0..40 {
            lines.push(line(1, "listitem", "", None));
            lines.push(line(2, "StaticText", &format!("Item {i}"), None));
            if i == 20 {
                lines.push(line(2, "button", "Restore", Some("e1")));
            }
        }
        let out = compact_lines(&lines, false, usize::MAX);
        assert!(out.contains("button \"Restore\" [ref=e1]"), "{out}");
        assert!(out.contains("StaticText \"Item 20\""));
        assert!(!out.contains("StaticText \"Item 19\""));
    }

    #[test]
    fn cap_keeps_interactive_before_text_and_preserves_ancestors() {
        let mut lines = vec![line(0, "main", "", None)];
        for i in 0..200 {
            lines.push(line(1, "paragraph", "", None));
            lines.push(line(2, "StaticText", &format!("Paragraph {i} text"), None));
        }
        lines.push(line(1, "form", "Search", None));
        lines.push(line(2, "searchbox", "Query", Some("e1")));
        lines.push(line(2, "button", "Go", Some("e2")));
        let out = compact_lines(&lines, false, 300);
        assert!(out.contains("- main\n"));
        assert!(out.contains("  - form \"Search\"\n    - searchbox \"Query\" [ref=e1]\n    - button \"Go\" [ref=e2]\n"), "{out}");
        assert!(out.contains("more paragraphs"), "{out}");
        assert!(out.contains("(compact: showing "));
        let tokens: usize = out.lines().map(estimate_tokens).sum();
        assert!(tokens <= 300, "{tokens} tokens:\n{out}");
    }

    #[test]
    fn cap_ranks_headings_above_text_and_labels_above_plain_text() {
        let mut lines = vec![line(0, "main", "", None)];
        for i in 0..60 {
            lines.push(line(
                1,
                "StaticText",
                &format!("Plain text number {i}"),
                None,
            ));
        }
        lines.push(line(1, "heading", "Title", None));
        lines.push(line(1, "LabelText", "", None));
        lines.push(line(2, "StaticText", "Work email", None));
        lines.push(line(2, "textbox", "Email", Some("e1")));
        let out = compact_lines(&lines, false, 120);
        assert!(out.contains("  - heading \"Title\"\n"), "{out}");
        assert!(
            out.contains("  - LabelText\n    - StaticText \"Work email\"\n    - textbox \"Email\" [ref=e1]\n"),
            "{out}"
        );
        let plain = out.matches("Plain text number").count();
        assert!(plain < 60, "{out}");
        assert!(out.contains("more text nodes"), "{out}");
    }

    #[test]
    fn cap_boosts_viewport_and_dialogs_and_demotes_hidden_and_collapsed() {
        let mut lines = vec![
            line(0, "main", "", None),
            line(1, "link", "Skip to content", Some("s1")),
        ];
        lines[1].placement = Placement::Hidden;
        for i in 0..40 {
            lines.push(line(
                1,
                "button",
                &format!("Filler {i}"),
                Some(&format!("f{i}")),
            ));
        }
        let mut menu = line(1, "button", "Menu", Some("m1"));
        menu.expanded = Some(false);
        lines.push(menu);
        lines.push(line(2, "menuitem", "Collapsed child", Some("m2")));
        let mut offscreen = line(1, "button", "Offscreen", Some("o1"));
        offscreen.placement = Placement::Offscreen;
        lines.push(offscreen);
        let mut visible = line(1, "button", "Visible", Some("v1"));
        visible.placement = Placement::InViewport;
        lines.push(visible);
        lines.push(line(0, "dialog", "Confirm", None));
        lines.push(line(1, "button", "OK", Some("d1")));

        let out = compact_lines(&lines, false, 150);
        assert!(
            out.contains("- dialog \"Confirm\"\n  - button \"OK\" [ref=d1]\n"),
            "{out}"
        );
        assert!(out.contains("button \"Visible\""), "{out}");
        assert!(out.contains("button \"Filler 0\""), "{out}");
        assert!(!out.contains("button \"Filler 39\""), "{out}");
        assert!(!out.contains("Offscreen"), "{out}");
        assert!(!out.contains("Skip to content"), "{out}");
        assert!(!out.contains("Collapsed child"), "{out}");
    }

    /// A docs section: a heading with its permalink, a lead paragraph with
    /// an in-text link, and later paragraphs with more in-text links.
    fn docs_section(lines: &mut Vec<SnapshotLine>, depth: usize, s: usize) {
        let heading = format!("Section {s}");
        lines.push(line(depth, "heading", &heading, Some(&format!("h{s}"))));
        lines.push(line(depth + 1, "link", &heading, Some(&format!("p{s}"))));
        for (k, name) in ["lead", "later", "last"].iter().enumerate() {
            lines.push(line(depth, "paragraph", "", None));
            lines.push(line(depth + 1, "StaticText", "Read about ", None));
            lines.push(line(
                depth + 1,
                "link",
                &format!("{name} {s}"),
                Some(&format!("l{s}{k}")),
            ));
            lines.push(line(depth + 1, "StaticText", " for details.", None));
        }
    }

    #[test]
    fn cap_keeps_lead_paragraph_links_with_their_heading() {
        let mut lines = vec![line(0, "main", "", None)];
        for s in 0..12 {
            docs_section(&mut lines, 1, s);
        }
        let out = compact_lines(&lines, false, 400);
        for s in 0..6 {
            let heading = format!("  - heading \"Section {s}\" [ref=h{s}]\n");
            let lead = format!("  - paragraph\n    - link \"lead {s}\" [ref=l{s}0]\n");
            let at = out.find(&heading).expect(&out);
            assert!(out[at..].contains(&lead), "{out}");
        }
        // Links further down a section and heading permalinks rank lower.
        assert!(!out.contains("link \"later 0\""), "{out}");
        assert!(!out.contains("link \"Section 0\""), "{out}");
        let tokens: usize = out.lines().map(estimate_tokens).sum();
        assert!(tokens <= 400, "{tokens} tokens:\n{out}");
    }

    #[test]
    fn heading_links_rank_with_headings_above_text() {
        let mut lines = vec![line(0, "main", "", None)];
        for i in 0..40 {
            lines.push(line(1, "paragraph", "", None));
            lines.push(line(2, "StaticText", &format!("Teaser text {i} "), None));
            lines.push(line(
                2,
                "link",
                &format!("read more {i}"),
                Some(&format!("m{i}")),
            ));
        }
        for i in 0..5 {
            lines.push(line(1, "heading", &format!("Post {i}"), None));
            lines.push(line(
                2,
                "link",
                &format!("Post {i}"),
                Some(&format!("p{i}")),
            ));
        }
        lines.push(line(1, "button", "Load more", Some("b1")));
        let out = compact_lines(&lines, false, 200);
        assert!(out.contains("button \"Load more\""), "{out}");
        for i in 0..5 {
            assert!(
                out.contains(&format!("link \"Post {i}\" [ref=p{i}]")),
                "{out}"
            );
        }
        assert!(!out.contains("read more 39"), "{out}");
    }

    #[test]
    fn named_region_stands_in_for_its_heading() {
        let mut lines = vec![line(0, "main", "", None)];
        for s in 0..12 {
            lines.push(line(
                1,
                "region",
                &format!("Section {s}"),
                Some(&format!("r{s}")),
            ));
            docs_section(&mut lines, 2, s);
        }
        let out = compact_lines(&lines, false, 300);
        // The region already names the section, so a lead link does not
        // bring its heading along.
        assert!(
            out.contains(
                "  - region \"Section 2\" [ref=r2]\n    - paragraph\n      - link \"lead 2\""
            ),
            "{out}"
        );
        assert!(!out.contains("heading \"Section 2\""), "{out}");
    }

    #[test]
    fn in_viewport_text_links_rank_as_controls() {
        let mut lines = vec![line(0, "main", "", None)];
        for i in 0..30 {
            lines.push(line(
                1,
                "button",
                &format!("Tool {i}"),
                Some(&format!("t{i}")),
            ));
        }
        lines.push(line(1, "paragraph", "", None));
        lines.push(line(
            2,
            "StaticText",
            "Long running text before the link.",
            None,
        ));
        let mut seen = line(2, "link", "visible link", Some("v1"));
        seen.placement = Placement::InViewport;
        lines.push(seen);
        lines.push(line(2, "link", "other link", Some("o1")));
        let out = compact_lines(&lines, false, 150);
        assert!(out.contains("link \"visible link\""), "{out}");
        assert!(!out.contains("link \"other link\""), "{out}");
    }

    /// The trailing hint line, if any.
    fn hint(out: &str) -> Option<&str> {
        out.lines().last().filter(|l| l.starts_with("(compact: "))
    }

    #[test]
    fn hint_says_drop_c_when_only_collapsing_hid_lines() {
        // Collapsing happens at any budget, so a larger budget brings
        // nothing back; only dropping -c does.
        for budget in [DEFAULT_MAX_TOKENS, usize::MAX] {
            let out = compact_lines(&table(40), false, budget);
            assert!(out.contains("- … 33 more rows (40 total)\n"), "{out}");
            let hint = hint(&out).expect(&out);
            assert!(hint.ends_with("; drop -c to see collapsed items)"), "{out}");
            assert!(!hint.contains("--max-tokens"), "{out}");
            assert!(!hint.contains("-s <selector>"), "{out}");
        }
    }

    #[test]
    fn hint_suggests_a_larger_budget_only_when_the_budget_cut_lines() {
        // Budget cut, nothing collapsed.
        let mut lines = vec![line(0, "main", "", None)];
        for i in 0..10 {
            lines.push(line(1, "paragraph", "", None));
            lines.push(line(2, "StaticText", &format!("Paragraph {i} text"), None));
        }
        let out = compact_lines(&lines, false, 60);
        assert!(
            hint(&out)
                .expect(&out)
                .ends_with("; use --max-tokens <n>, -s <selector>, or no -c for more)"),
            "{out}"
        );
        // Budget cut and rows collapsed.
        let out = compact_lines(&table(500), false, 120);
        assert!(out.contains("more rows (500 total)"), "{out}");
        assert!(
            hint(&out).expect(&out).ends_with(
                "; use --max-tokens <n> or -s <selector> for more, or drop -c to see collapsed items)"
            ),
            "{out}"
        );
        // Nothing hidden: no hint.
        let out = compact_lines(&table(3), false, usize::MAX);
        assert_eq!(hint(&out), None, "{out}");
    }

    #[test]
    fn hint_explains_output_when_nothing_fits_the_budget() {
        let mut lines = vec![line(0, "main", "", None)];
        for i in 0..10 {
            lines.push(line(1, "paragraph", "", None));
            lines.push(line(2, "StaticText", &format!("Paragraph {i} text"), None));
        }
        let out = compact_lines(&lines, false, 30);
        assert!(out.starts_with("- … 1 more main\n"), "{out}");
        assert!(
            hint(&out)
                .expect(&out)
                .starts_with("(compact: showing 0 of 21 lines; use --max-tokens"),
            "{out}"
        );
    }

    #[test]
    fn focused_line_survives_any_budget() {
        let mut lines = table(50);
        let focused = lines
            .iter()
            .position(|l| l.text.contains("[ref=b30]"))
            .unwrap();
        lines[focused].focused = true;
        let out = compact_lines(&lines, false, 1);
        assert!(out.contains("[ref=b30]"), "{out}");
        assert!(out.starts_with("- main\n"), "{out}");
    }

    #[test]
    fn interactive_only_ignores_unreferenced_names() {
        let lines = vec![
            line(0, "navigation", "", None),
            line(1, "StaticText", "Menu", None),
            line(1, "link", "Home", Some("e1")),
        ];
        assert_eq!(
            compact_lines(&lines, true, usize::MAX),
            "- navigation\n  - link \"Home\" [ref=e1]\n"
        );
    }

    #[test]
    fn list_markers_are_not_content() {
        let lines = vec![
            line(0, "list", "", None),
            line(1, "listitem", "", None),
            line(2, "ListMarker", "• ", None),
            line(2, "link", "Docs", Some("e1")),
            line(1, "listitem", "", None),
            line(2, "ListMarker", "• ", None),
        ];
        assert_eq!(
            compact_lines(&lines, false, usize::MAX),
            "- list\n  - listitem\n    - link \"Docs\" [ref=e1]\n"
        );
    }

    #[test]
    fn text_repeating_a_nearby_name_is_dropped() {
        let lines = vec![
            line(0, "group", "Contact", None),
            line(1, "Legend", "", None),
            line(2, "StaticText", "Contact", None),
            line(1, "LabelText", "", None),
            line(2, "StaticText", "Email ", None),
            line(2, "textbox", "Email", Some("e1")),
            line(1, "link", "Home", Some("e2")),
            line(2, "image", "Home", None),
            line(1, "StaticText", "Contact us any time", None),
        ];
        assert_eq!(
            compact_lines(&lines, false, usize::MAX),
            "- group \"Contact\"\n  - LabelText\n    - textbox \"Email\" [ref=e1]\n  - link \"Home\" [ref=e2]\n  - StaticText \"Contact us any time\"\n"
        );
    }

    #[test]
    fn truncate_text_keeps_short_text_and_shortens_long_text() {
        assert_eq!(truncate_text("short"), "short");
        let exact = "a".repeat(TEXT_LIMIT);
        assert_eq!(truncate_text(&exact), exact);
        let long = "é".repeat(TEXT_LIMIT + 20);
        let cut = truncate_text(&long);
        assert_eq!(cut.chars().count(), TEXT_LIMIT);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn marker_names_mixed_runs_generically() {
        let mut lines = vec![line(0, "main", "", None)];
        lines.push(line(1, "button", "Keep", Some("e1")));
        for i in 0..20 {
            lines.push(line(
                1,
                "StaticText",
                &format!("Some text that is long enough {i}"),
                None,
            ));
            lines.push(line(
                1,
                "paragraph",
                &format!("A paragraph that is long enough {i}"),
                None,
            ));
        }
        let out = compact_lines(&lines, false, 60);
        assert!(
            out.contains("  - button \"Keep\" [ref=e1]\n  - … 40 more nodes\n"),
            "{out}"
        );
    }
}
