//! Accessibility snapshots for WebDriver sessions, built by walking each frame's DOM in the page.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::backend::{ElementHandle, WebDriverBackend};
use super::client::ELEMENT_KEY;
use crate::native::cdp::types::{AXNode, AXProperty, AXValue};
use crate::native::element::RefMap;
use crate::native::snapshot::{
    cursor_info_from_value, snapshot_from_ax_nodes, SnapshotOptions, SnapshotRefContext,
    CONTENT_ROLES, HINT_FUNCTION, INTERACTIVE_ROLES, MAX_IFRAME_SNAPSHOT_DEPTH, TABLE_CELL_ROLES,
};

/// Walks the composed tree of the current frame, including open shadow roots, and returns
/// `{nodes, doc}`. Each node is `[parent, role, name, element, properties, value, cursor, href]`
/// in Chrome accessibility-tree terms. `element` is set only on nodes that can take a ref, so the
/// browser returns its own element reference for them. Iframes are returned as leaves.
/// Arguments: selector or null, whether to collect link URLs, and the roles that take refs.
const WALKER_JS: &str = r#"
const [selector, withUrls, refRoles, contentRoles] = arguments;
const REF = new Set(refRoles), CONTENT = new Set(contentRoles);
const FROM_CONTENT = new Set(['button', 'link', 'heading', 'cell', 'gridcell', 'columnheader', 'rowheader', 'option', 'tab', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'checkbox', 'radio', 'switch', 'treeitem', 'DisclosureTriangle', 'tooltip']);
const SKIP = new Set(['script', 'style', 'noscript', 'template', 'head', 'meta', 'link', 'title', 'base']);
const LEAF = new Set(['input', 'textarea', 'select', 'iframe', 'frame', 'img', 'svg']);
const NATIVE = new Set(['a', 'button', 'input', 'select', 'textarea', 'details', 'summary']);
const ARIA_INTERACTIVE = new Set(['button', 'link', 'textbox', 'checkbox', 'radio', 'combobox', 'listbox', 'menuitem', 'menuitemcheckbox', 'menuitemradio', 'option', 'searchbox', 'slider', 'spinbutton', 'switch', 'tab', 'treeitem']);
const MARKERS = { disc: '• ', circle: '◦ ', square: '▪ ' };
const styles = new Map();
const style = (el) => { let s = styles.get(el); if (!s) { s = getComputedStyle(el); styles.set(el, s); } return s; };
const ws = (s) => (s || '').replace(/\s+/g, ' ').trim();
const kids = (n) => {
  if (n.shadowRoot) return [...n.shadowRoot.childNodes];
  if (n.localName === 'slot') { const a = n.assignedNodes({ flatten: true }); if (a.length) return a; }
  return [...n.childNodes];
};
const hidden = (el) => el.hidden || el.getAttribute('aria-hidden') === 'true' || style(el).display === 'none';
const text = (n, skip) => {
  let out = '';
  for (const c of kids(n)) {
    if (c === skip) continue;
    if (c.nodeType === 3) { out += c.data; continue; }
    if (c.nodeType !== 1 || SKIP.has(c.localName) || hidden(c)) continue;
    const t = c.localName;
    if (t === 'textarea' || (t === 'input' && !['checkbox', 'radio', 'image'].includes(c.type))) { if (!['hidden', 'password'].includes(c.type)) out += c.value; continue; }
    if (t === 'select') { out += (c.selectedOptions[0] || {}).text || ''; continue; }
    const label = c.getAttribute('aria-label');
    if (label && label.trim()) { out += ' ' + label + ' '; continue; }
    if (t === 'img' || t === 'input') { out += c.getAttribute('alt') || ''; continue; }
    if (t === 'br') { out += ' '; continue; }
    const inner = text(c, skip);
    out += /^inline/.test(style(c).display) ? inner : ' ' + inner + ' ';
  }
  return out;
};
const byIds = (el, ids) => ids.split(/\s+/)
  .map((id) => (el.getRootNode().getElementById ? el.getRootNode().getElementById(id) : null) || document.getElementById(id))
  .filter(Boolean)
  .map((r) => r.getAttribute('aria-label') || text(r))
  .join(' ');
const role = (el) => {
  const explicit = (el.getAttribute('role') || '').trim().split(/\s+/)[0].toLowerCase();
  if (explicit) return explicit === 'presentation' || explicit === 'none' ? null : explicit;
  const t = el.localName;
  switch (t) {
    case 'a': case 'area': return el.hasAttribute('href') ? 'link' : 'generic';
    case 'button': return 'button';
    case 'input': {
      const type = (el.getAttribute('type') || 'text').toLowerCase();
      if (type === 'hidden') return 'hidden';
      if (['button', 'submit', 'reset', 'image', 'file'].includes(type)) return 'button';
      if (type === 'checkbox' || type === 'radio') return type;
      if (type === 'range') return 'slider';
      if (type === 'number') return 'spinbutton';
      if (type === 'color') return 'ColorWell';
      if (el.list) return 'combobox';
      return type === 'search' ? 'searchbox' : 'textbox';
    }
    case 'textarea': return 'textbox';
    case 'select': return el.multiple || el.size > 1 ? 'listbox' : 'combobox';
    case 'h1': case 'h2': case 'h3': case 'h4': case 'h5': case 'h6': return 'heading';
    case 'p': return 'paragraph';
    case 'ul': case 'ol': case 'menu': return 'list';
    case 'li': return 'listitem';
    case 'nav': return 'navigation';
    case 'main': return 'main';
    case 'aside': return 'complementary';
    case 'article': return 'article';
    case 'section': return el.hasAttribute('aria-label') || el.hasAttribute('aria-labelledby') ? 'region' : 'generic';
    case 'header': return el.closest('article, aside, main, nav, section') ? 'generic' : 'banner';
    case 'footer': return el.closest('article, aside, main, nav, section') ? 'generic' : 'contentinfo';
    case 'form': return 'form';
    case 'fieldset': case 'details': case 'optgroup': return 'group';
    case 'legend': return 'Legend';
    case 'label': return 'LabelText';
    case 'summary': return el.parentElement && el.parentElement.localName === 'details' && el.parentElement.querySelector(':scope > summary') === el ? 'DisclosureTriangle' : 'generic';
    case 'table': return 'table';
    case 'tr': return 'row';
    case 'thead': case 'tfoot': return 'rowgroup';
    case 'tbody': return null;
    case 'td': return el.closest('table') && el.closest('table').getAttribute('role') === 'grid' ? 'gridcell' : 'cell';
    case 'th': return el.getAttribute('scope') === 'row' ? 'rowheader' : 'columnheader';
    case 'img': return el.getAttribute('alt') === '' ? null : 'image';
    case 'svg': return 'image';
    case 'iframe': case 'frame': return 'Iframe';
    case 'dialog': return 'dialog';
    case 'hr': return 'separator';
    case 'progress': return 'progressbar';
    case 'meter': return 'meter';
    case 'output': return 'status';
    case 'blockquote': return 'blockquote';
    case 'figure': return 'figure';
    case 'code': return 'code';
    case 'strong': return 'strong';
    case 'em': return 'emphasis';
    case 'dl': return 'DescriptionList';
    case 'dt': return 'term';
    case 'dd': return 'definition';
    default: return 'generic';
  }
};
const name = (el, r) => {
  const labelledby = el.getAttribute('aria-labelledby');
  if (labelledby) { const v = ws(byIds(el, labelledby)); if (v) return v; }
  const label = el.getAttribute('aria-label');
  if (label && label.trim()) return ws(label);
  const t = el.localName;
  if (t === 'input' && ['button', 'submit', 'reset'].includes(el.type)) return ws(el.value || { submit: 'Submit', reset: 'Reset' }[el.type] || '');
  if (t === 'img' || t === 'area' || (t === 'input' && el.type === 'image')) return ws(el.getAttribute('alt') || el.title);
  if (el.labels && el.labels.length) { const v = ws([...el.labels].map((l) => text(l, el)).join(' ')); if (v) return v; }
  if (t === 'fieldset') { const legend = [...el.children].find((c) => c.localName === 'legend'); if (legend) return ws(text(legend)); }
  if (t === 'table' && el.caption) return ws(text(el.caption));
  if (t === 'figure') { const caption = [...el.children].find((c) => c.localName === 'figcaption'); if (caption) return ws(text(caption)); }
  if (t === 'iframe' || t === 'frame') return ws(el.title);
  if (t === 'svg') { const title = el.querySelector(':scope > title'); if (title) return ws(title.textContent); }
  if (FROM_CONTENT.has(r)) { const v = ws(text(el)); if (v) return v; }
  if ((t === 'input' || t === 'textarea') && el.placeholder) return ws(el.placeholder);
  return ws(el.title);
};
const properties = (el, r, depth) => {
  const p = {};
  if (r === 'heading') p.level = +el.getAttribute('aria-level') || +el.localName.slice(1) || 2;
  if (r === 'listitem') p.level = depth;
  if (['checkbox', 'radio', 'switch', 'menuitemcheckbox', 'menuitemradio'].includes(r)) {
    p.checked = el.getAttribute('aria-checked') || (el.indeterminate ? 'mixed' : String(!!el.checked));
  }
  const expanded = el.getAttribute('aria-expanded');
  if (expanded === 'true' || expanded === 'false') p.expanded = expanded === 'true';
  else if (r === 'combobox' && el.localName === 'select') p.expanded = false;
  else if (r === 'DisclosureTriangle') p.expanded = el.parentElement.open;
  if (r === 'option') p.selected = el.localName === 'option' ? el.selected : el.getAttribute('aria-selected') === 'true';
  else if (el.getAttribute('aria-selected') === 'true') p.selected = true;
  if (el.matches(':disabled') || el.getAttribute('aria-disabled') === 'true') p.disabled = true;
  if (el.required || el.getAttribute('aria-required') === 'true') p.required = true;
  return p;
};
const value = (el, r) => {
  if (el.localName === 'select') return r === 'combobox' ? ((el.selectedOptions[0] || {}).text || null) : null;
  if (['textbox', 'searchbox', 'spinbutton', 'slider', 'combobox'].includes(r) && 'value' in el && el.type !== 'password') return el.value || null;
  return null;
};
const cursor = (el, s) => {
  if (NATIVE.has(el.localName) || ARIA_INTERACTIVE.has((el.getAttribute('role') || '').toLowerCase())) return null;
  const hasCursorPointer = s.cursor === 'pointer';
  const hasOnClick = el.hasAttribute('onclick') || el.onclick !== null;
  const tabIndex = el.getAttribute('tabindex');
  const hasTabIndex = tabIndex !== null && tabIndex !== '-1';
  const ce = el.getAttribute('contenteditable');
  const isEditable = ce === '' || ce === 'true';
  if (!hasCursorPointer && !hasOnClick && !hasTabIndex && !isEditable) return null;
  if (hasCursorPointer && !hasOnClick && !hasTabIndex && !isEditable && el.parentElement && style(el.parentElement).cursor === 'pointer') return null;
  const rect = el.getBoundingClientRect();
  if (rect.width === 0 || rect.height === 0) return null;
  let hiddenInputType = null, hiddenInputChecked = null;
  const input = el.querySelector('input[type="radio"], input[type="checkbox"]');
  if (input) {
    const is = style(input);
    if (is.display === 'none' || is.visibility === 'hidden' || input.hidden) {
      hiddenInputType = input.type;
      hiddenInputChecked = input.indeterminate ? 'mixed' : String(input.checked);
    }
  }
  return { text: (el.textContent || '').trim().slice(0, 100), tagName: el.localName, hasOnClick, hasCursorPointer, hasTabIndex, isEditable, hiddenInputType, hiddenInputChecked };
};
const nodes = [];
const push = (...node) => {
  if (node[4] && !Object.keys(node[4]).length) node[4] = null;
  while (node[node.length - 1] == null) node.pop();
  nodes.push(node);
  return nodes.length - 1;
};
const inline = (n) => !!n && (n.nodeType === 3 ? /\S/.test(n.data) : n.nodeType === 1 && !SKIP.has(n.localName) && /^inline/.test(style(n).display));
const walk = (n, parent, depth) => {
  const list = kids(n);
  const shown = n.nodeType !== 1 || !/^(hidden|collapse)$/.test(style(n).visibility);
  list.forEach((c, i) => {
    if (c.nodeType === 1 && !SKIP.has(c.localName)) return visit(c, parent, depth);
    if (c.nodeType !== 3 || !shown) return;
    let t = c.data.replace(/\s+/g, ' ');
    if (!inline(list[i - 1])) t = t.replace(/^ /, '');
    if (!inline(list[i + 1])) t = t.replace(/ $/, '');
    if (t) push(parent, 'StaticText', t, null, null, null, null, null);
  });
};
const visit = (el, parent, depth) => {
  const s = style(el);
  if (el.hidden || el.getAttribute('aria-hidden') === 'true' || s.display === 'none') return;
  let r = role(el);
  if (r === 'hidden') return;
  if (r === 'LabelText' && el.control && el.contains(el.control) && ['checkbox', 'radio'].includes(el.control.type)) { visit(el.control, parent, depth); return; }
  const visible = s.visibility !== 'hidden' && s.visibility !== 'collapse';
  const c = visible ? cursor(el, s) : null;
  const n = r && visible ? name(el, r) : '';
  if (!visible || (r === 'generic' && !n && !c && /^(inline|contents)$/.test(s.display))) r = null;
  if (!r) { if (!LEAF.has(el.localName)) walk(el, parent, depth); return; }
  const wantsRef = REF.has(r) || !!c || (CONTENT.has(r) && !!n);
  const href = withUrls && r === 'link' ? el.href || null : null;
  const idx = push(parent, r, n, wantsRef ? el : null, properties(el, r, depth), value(el, r), c, href);
  if (r === 'listitem') {
    const type = s.listStyleType;
    const marker = MARKERS[type] || (type === 'decimal' ? ([...el.parentElement.children].filter((k) => k.localName === 'li').indexOf(el) + (el.parentElement.start || 1)) + '. ' : null);
    if (marker) push(idx, 'ListMarker', marker, null, null, null, null, null);
  }
  if (el.localName === 'select') {
    const holder = r === 'combobox' ? push(idx, 'MenuListPopup', '', null, null, null, null, null) : idx;
    for (const o of el.options) push(holder, 'option', ws(o.label || o.text), o, { selected: o.selected, ...(o.disabled ? { disabled: true } : {}) }, null, null, null);
    return;
  }
  if (LEAF.has(el.localName)) return;
  if (el.localName === 'details' && !el.open) {
    const summary = el.querySelector(':scope > summary');
    if (summary) visit(summary, idx, depth);
    return;
  }
  walk(el, idx, r === 'list' ? depth + 1 : depth);
};
const root = push(-1, 'RootWebArea', ws(document.title), document.documentElement, null, null, null, null);
if (selector) {
  const all = [...document.querySelectorAll(selector)];
  const outer = all.filter((m) => !all.some((o) => o !== m && o.contains(m)));
  if (!outer.length) return null;
  for (const m of outer) visit(m, root, 0);
} else {
  walk(document.body || document.documentElement, root, 0);
}
return { nodes, doc: document.documentElement };
"#;

/// One walked node, with the element it came from when it can take a ref.
struct WalkedNode {
    parent: Option<usize>,
    role: String,
    name: String,
    element: Option<ElementHandle>,
    properties: Value,
    value: Option<String>,
    cursor: Value,
    href: Option<String>,
}

impl WebDriverBackend {
    /// Renders the page as a Chrome-format snapshot, descending into iframes and open shadow
    /// roots, and records the refs it hands out in `ref_map`.
    pub async fn snapshot(
        &mut self,
        options: &SnapshotOptions,
        ref_map: &mut RefMap,
    ) -> Result<String, String> {
        let (mut walked, document) = self.walk_frame(&[], options).await?;
        let mut frames = 0;
        let result = self
            .walk_child_frames(&mut walked, 0, 0, &mut frames, options)
            .await;
        if frames > 0 {
            let _ = self.client.switch_to_frame(None).await;
        }
        result?;

        let session = self.session_key()?;
        let mut urls = HashMap::new();
        let mut cursor_elements = HashMap::new();
        let mut node_contexts = HashMap::new();
        let mut ids = Vec::with_capacity(walked.len());
        for node in &walked {
            let id = node.element.as_ref().map(|handle| self.durable_id(handle));
            if let (Some(id), Some(handle)) = (id, &node.element) {
                if !handle.frames.is_empty() {
                    node_contexts.insert(id, handle.frames.join("/"));
                }
                if node.cursor.is_object() {
                    cursor_elements.insert(id, cursor_info_from_value(&node.cursor));
                }
                if let Some(href) = &node.href {
                    urls.insert(id, href.clone());
                }
            }
            ids.push(id);
        }
        let nodes = ax_nodes(&walked, &ids);
        let known = ref_map.observe_document(&session, None, &session, Some(&document));
        let elements = &self.elements;
        let client = &self.client;
        Ok(snapshot_from_ax_nodes(
            &nodes,
            &urls,
            &cursor_elements,
            options,
            ref_map,
            SnapshotRefContext {
                node_contexts: &node_contexts,
                session_id: &session,
                frame_id: None,
                document_is_known: known,
            },
            |targets| async move {
                let mut by_frame: HashMap<&[String], Vec<(usize, &str)>> = HashMap::new();
                for (idx, id) in targets {
                    if let Some(handle) = elements.iter().find(|(_, v)| **v == id).map(|(h, _)| h) {
                        by_frame
                            .entry(handle.frames.as_slice())
                            .or_default()
                            .push((idx, handle.id.as_str()));
                    }
                }
                let script = format!(
                    "return arguments[0].map((e) => ({}).call(e));",
                    HINT_FUNCTION
                );
                let mut hints = Vec::new();
                for (frames, targets) in by_frame {
                    let args = targets
                        .iter()
                        .map(|(_, id)| json!({ ELEMENT_KEY: id }))
                        .collect();
                    if WebDriverBackend::enter(client, frames).await.is_ok() {
                        if let Ok(Value::Array(values)) = client
                            .execute_script(&script, vec![Value::Array(args)])
                            .await
                        {
                            for ((idx, _), hint) in targets.iter().zip(values) {
                                if let Some(hint) = hint.as_str().filter(|h| !h.is_empty()) {
                                    hints.push((*idx, hint.to_string()));
                                }
                            }
                        }
                    }
                    if !frames.is_empty() {
                        let _ = client.switch_to_frame(None).await;
                    }
                }
                hints
            },
        )
        .await)
    }

    /// Runs the walker in the frame the session is switched to and returns its nodes and the
    /// element id of its document root.
    async fn walk_frame(
        &self,
        frames: &[String],
        options: &SnapshotOptions,
    ) -> Result<(Vec<WalkedNode>, String), String> {
        let selector = if frames.is_empty() {
            options.selector.as_deref()
        } else {
            None
        };
        let result = self
            .client
            .execute_script(
                WALKER_JS,
                vec![
                    json!(selector),
                    json!(options.urls),
                    json!([INTERACTIVE_ROLES, TABLE_CELL_ROLES].concat()),
                    json!(CONTENT_ROLES),
                ],
            )
            .await?;
        if result.is_null() {
            return Err(format!(
                "Selector '{}' did not match any element",
                selector.unwrap_or_default()
            ));
        }
        let element_id = |v: &Value| v[ELEMENT_KEY].as_str().map(String::from);
        let document = element_id(&result["doc"]).ok_or("The snapshot returned no document")?;
        let nodes = result["nodes"]
            .as_array()
            .ok_or("The snapshot returned no nodes")?
            .iter()
            .map(|n| WalkedNode {
                parent: n[0].as_u64().map(|p| p as usize),
                role: n[1].as_str().unwrap_or_default().to_string(),
                name: n[2].as_str().unwrap_or_default().to_string(),
                element: element_id(&n[3]).map(|id| ElementHandle {
                    frames: frames.to_vec(),
                    id,
                }),
                properties: n[4].clone(),
                value: n[5].as_str().map(String::from),
                cursor: n[6].clone(),
                href: n[7].as_str().map(String::from),
            })
            .collect();
        Ok((nodes, document))
    }

    /// Walks every iframe found from `start` on, at most `MAX_IFRAME_SNAPSHOT_DEPTH` levels down,
    /// and appends each frame's nodes under its Iframe node. The session ends in a child frame
    /// when `frames` was incremented, so the caller switches back to the top.
    async fn walk_child_frames(
        &self,
        walked: &mut Vec<WalkedNode>,
        start: usize,
        depth: usize,
        frames: &mut usize,
        options: &SnapshotOptions,
    ) -> Result<(), String> {
        if depth >= MAX_IFRAME_SNAPSHOT_DEPTH {
            return Ok(());
        }
        let end = walked.len();
        for idx in start..end {
            let Some(frame) = walked[idx]
                .element
                .clone()
                .filter(|_| walked[idx].role == "Iframe")
            else {
                continue;
            };
            *frames += 1;
            if self.client.switch_to_frame(Some(&frame.id)).await.is_err() {
                continue;
            }
            let mut path = frame.frames.clone();
            path.push(frame.id.clone());
            if let Ok((child, _)) = self.walk_frame(&path, options).await {
                let offset = walked.len();
                walked.extend(child.into_iter().map(|mut node| {
                    node.parent = Some(node.parent.map_or(idx, |p| p + offset));
                    node
                }));
                Box::pin(self.walk_child_frames(walked, offset, depth + 1, frames, options))
                    .await?;
            }
            self.client.switch_to_parent_frame().await?;
        }
        Ok(())
    }
}

/// Converts walked nodes to accessibility nodes for the shared snapshot pipeline.
fn ax_nodes(walked: &[WalkedNode], ids: &[Option<i64>]) -> Vec<AXNode> {
    let ax_value = |kind: &str, value: Value| AXValue {
        value_type: kind.to_string(),
        value: Some(value),
    };
    let mut children: Vec<Vec<String>> = vec![Vec::new(); walked.len()];
    for (idx, node) in walked.iter().enumerate() {
        if let Some(parent) = node.parent {
            children[parent].push(idx.to_string());
        }
    }
    walked
        .iter()
        .zip(ids)
        .zip(children)
        .enumerate()
        .map(|(idx, ((node, id), child_ids))| {
            let properties = node
                .properties
                .as_object()
                .map(|props| {
                    props
                        .iter()
                        .map(|(name, value)| AXProperty {
                            name: name.clone(),
                            value: ax_value("", value.clone()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            AXNode {
                node_id: idx.to_string(),
                role: Some(ax_value("role", json!(node.role))),
                name: Some(ax_value("string", json!(node.name))),
                value: node.value.as_ref().map(|v| ax_value("string", json!(v))),
                description: None,
                properties: Some(properties),
                child_ids: Some(child_ids),
                backend_d_o_m_node_id: *id,
                ignored: None,
            }
        })
        .collect()
}
