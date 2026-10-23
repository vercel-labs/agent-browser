use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::rdp::RdpClient;
use super::session::FirefoxSession;
use super::target::find_element;
use crate::native::actions::{self as chrome, DaemonState};
use crate::native::cdp::types::{AXNode, AXProperty, AXValue};
use crate::native::element::RefMap;
use crate::native::snapshot::{snapshot_from_ax_nodes, SnapshotOptions};

const FRAME_TARGET_IDLE: Duration = Duration::from_millis(200);
const PAINT_WAIT: Duration = Duration::from_millis(500);

/// Waits two animation frames so Firefox has applied pending DOM changes to its accessibility tree.
const SETTLE_JS: &str =
    "() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(0))))";

/// Batch-resolves a pruned `[selector, role, children]` tree to `[node, contentWindow, inShadowRoot]` triples in document order.
/// Children of an open shadow host resolve inside its shadow root first.
const RESOLVE_JS: &str = r#"(tree) => {
  const out = [];
  const find = (roots, sel) => {
    for (const r of roots) {
      try { const e = r.querySelector(sel); if (e) return e; } catch (_) {}
    }
    return null;
  };
  const walk = ([sel, role, kids], roots) => {
    const el = role === 'document' ? roots[0] : sel && !sel.endsWith('#text') ? find(roots, sel) : null;
    out.push([el, role === 'internal frame' && el ? el.contentWindow : null, !!el && el.getRootNode() instanceof ShadowRoot]);
    const next = role !== 'document' && el && el.shadowRoot ? [el.shadowRoot, ...roots] : roots;
    for (const k of kids) walk(k, next);
  };
  walk(JSON.parse(tree), [document]);
  return out;
}"#;

/// Converts a Firefox DevTools `accessible.snapshot` tree into Chrome-shaped accessibility nodes.
/// `durable_id` supplies the stable integer stored as `backend_d_o_m_node_id` for each node.
pub fn ax_nodes_from_snapshot(
    root: &Value,
    durable_id: &mut dyn FnMut(&Value) -> Option<i64>,
) -> Vec<AXNode> {
    let mut nodes = Vec::new();
    push_node(root, durable_id, &mut nodes);
    nodes
}

fn ax_value(kind: &str, value: Value) -> AXValue {
    AXValue {
        value_type: kind.to_string(),
        value: Some(value),
    }
}

fn property(name: &str, value: bool) -> AXProperty {
    AXProperty {
        name: name.to_string(),
        value: ax_value("boolean", Value::Bool(value)),
    }
}

fn push_node(
    node: &Value,
    durable_id: &mut dyn FnMut(&Value) -> Option<i64>,
    nodes: &mut Vec<AXNode>,
) -> String {
    let id = nodes.len().to_string();
    let states: Vec<&str> = node["states"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let role = match node["role"].as_str().unwrap_or("") {
        "document" => "RootWebArea",
        "text leaf" => "StaticText",
        "internal frame" => "Iframe",
        "entry" | "password text" => "textbox",
        "search" if states.contains(&"editable") => "searchbox",
        "spin button" => "spinbutton",
        other => other,
    };
    let mut properties: Vec<AXProperty> = [
        ("checked", "checked"),
        ("expanded", "expanded"),
        ("selected", "selected"),
        ("unavailable", "disabled"),
        ("required", "required"),
    ]
    .iter()
    .filter(|(state, _)| states.contains(state))
    .map(|(_, name)| property(name, true))
    .collect();
    let level = node["level"]
        .as_str()
        .or_else(|| node["attributes"]["level"].as_str())
        .and_then(|l| l.parse::<i64>().ok());
    if let Some(level) = level {
        properties.push(AXProperty {
            name: "level".to_string(),
            value: ax_value("integer", json!(level)),
        });
    }
    let text = |key: &str| {
        node[key]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| ax_value("string", json!(s)))
    };
    nodes.push(AXNode {
        node_id: id.clone(),
        role: Some(ax_value("role", json!(role))),
        name: text("name"),
        value: if role == "link" { None } else { text("value") },
        description: None,
        properties: Some(properties),
        child_ids: None,
        backend_d_o_m_node_id: durable_id(node),
        ignored: None,
    });
    let child_ids: Vec<String> = node["children"]
        .as_array()
        .map(|c| c.iter().map(|c| push_node(c, durable_id, nodes)).collect())
        .unwrap_or_default();
    let index = id.parse::<usize>().unwrap_or(0);
    nodes[index].child_ids = Some(child_ids);
    id
}

/// Fetches the DevTools accessibility snapshot of the document behind an accessibility actor.
async fn document_snapshot(rdp: &RdpClient, accessibility: &str) -> Result<Value, String> {
    let walker = rdp.request(accessibility, "getWalker", json!({})).await?;
    let walker = walker["walker"]["actor"]
        .as_str()
        .ok_or("Firefox DevTools returned no accessibility walker")?;
    let children = rdp.request(walker, "children", json!({})).await?;
    let document = children["children"][0]["actor"]
        .as_str()
        .ok_or("Firefox DevTools returned no document accessible")?;
    let snapshot = rdp.request(document, "snapshot", json!({})).await?;
    Ok(snapshot.get("snapshot").cloned().unwrap_or(snapshot))
}

/// Frame targets seen through the tab watchers: browsing context id to accessibility actor.
/// A watcher only announces existing frames the first time it watches, so the events are kept for later snapshots.
pub struct FrameWatch {
    events: broadcast::Receiver<Value>,
    actors: HashMap<String, String>,
    tabs: HashSet<String>,
}

fn record_target(actors: &mut HashMap<String, String>, event: &Value) {
    if let (Some(id), Some(actor)) = (
        event["target"]["browsingContextID"].as_u64(),
        event["target"]["accessibilityActor"].as_str(),
    ) {
        actors.insert(id.to_string(), actor.to_string());
    }
}

/// Returns the accessibility actor of the cross-process frame whose BiDi context is `window`.
async fn frame_actor(
    ff: &mut FirefoxSession,
    tab: &str,
    window: &str,
) -> Result<Option<String>, String> {
    let rdp = ff.rdp.clone();
    let watch = ff.frames.get_or_insert_with(|| FrameWatch {
        events: rdp.subscribe(),
        actors: HashMap::new(),
        tabs: HashSet::new(),
    });
    if watch.tabs.insert(tab.to_string()) {
        let watcher = rdp
            .request(
                tab,
                "getWatcher",
                json!({"isServerTargetSwitchingEnabled": true}),
            )
            .await?;
        let watcher = watcher["actor"]
            .as_str()
            .ok_or("Firefox DevTools returned no watcher")?;
        rdp.request(watcher, "watchTargets", json!({"targetType": "frame"}))
            .await?;
    }
    loop {
        while let Ok(e) = watch.events.try_recv() {
            record_target(&mut watch.actors, &e);
        }
        if let Some(actor) = watch.actors.get(window) {
            return Ok(Some(actor.clone()));
        }
        match tokio::time::timeout(FRAME_TARGET_IDLE, watch.events.recv()).await {
            Ok(Ok(e)) => record_target(&mut watch.actors, &e),
            _ => return Ok(None),
        }
    }
}

fn shape(node: &Value) -> Value {
    let role = node["role"].as_str().unwrap_or("");
    let kids: Vec<Value> = match (role, node["children"].as_array()) {
        ("internal frame", _) | (_, None) => Vec::new(),
        (_, Some(c)) => c.iter().map(shape).collect(),
    };
    json!([node["nodeCssSelector"].as_str().unwrap_or(""), role, kids])
}

fn apply(
    node: &mut Value,
    results: &mut std::vec::IntoIter<Value>,
    context: &str,
    path: &mut Vec<usize>,
    frames: &mut Vec<(Vec<usize>, String)>,
) {
    let result = results.next().unwrap_or(Value::Null);
    if let Some(shared_id) = result[0]["sharedId"].as_str() {
        node["abSid"] = json!(shared_id);
        node["abCtx"] = json!(context);
        node["abShadow"] = json!(result[2] == true);
    }
    if let Some(window) = result[1].as_str() {
        frames.push((path.clone(), window.to_string()));
    }
    if node["role"] == "internal frame" {
        return;
    }
    if let Some(children) = node["children"].as_array_mut() {
        for (i, child) in children.iter_mut().enumerate() {
            path.push(i);
            apply(child, results, context, path, frames);
            path.pop();
        }
    }
}

fn node_at<'a>(node: &'a mut Value, path: &[usize]) -> &'a mut Value {
    path.iter().fold(node, |n, i| &mut n["children"][*i])
}

fn find_node<'a>(node: &'a Value, context: &str, shared_id: &str) -> Option<&'a Value> {
    if node["abSid"] == shared_id && node["abCtx"] == context {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|c| find_node(c, context, shared_id))
}

fn find_context_root<'a>(node: &'a Value, context: &str) -> Option<&'a Value> {
    if node["abCtx"] == context {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|c| find_context_root(c, context))
}

/// Tags every node of `doc` with its BiDi `abSid`/`abCtx`, then does the same inside each frame's own context.
async fn annotate(
    ff: &mut FirefoxSession,
    context: &str,
    doc: &mut Value,
    tab: &str,
) -> Result<(), String> {
    let arg = json!({"type": "string", "value": shape(doc).to_string()});
    let resolved = ff
        .bidi
        .call_function(context, RESOLVE_JS, None, vec![arg])
        .await?;
    let results = resolved.as_array().cloned().unwrap_or_default();
    let mut frames = Vec::new();
    apply(
        doc,
        &mut results.into_iter(),
        context,
        &mut Vec::new(),
        &mut frames,
    );
    if let Some(sid) = doc["abSid"].as_str() {
        if ff
            .documents
            .insert(context.to_string(), sid.to_string())
            .as_deref()
            != Some(sid)
        {
            ff.refs.retain(|(c, _), _| c != context);
        }
    }
    for (path, window) in frames {
        if node_at(doc, &path)["useChildTargetToFetchChildren"] == true {
            let Some(actor) = frame_actor(ff, tab, &window).await? else {
                continue;
            };
            let frame = node_at(doc, &path);
            frame["children"] = json!([document_snapshot(&ff.rdp, &actor).await?]);
        }
        if let Some(child) = node_at(doc, &path)["children"].get_mut(0) {
            Box::pin(annotate(ff, &window, child, tab)).await?;
        }
    }
    Ok(())
}

/// Renders the active tab as a Chrome-format snapshot, including iframes and open shadow roots.
pub async fn take_snapshot(
    ff: &mut FirefoxSession,
    options: &SnapshotOptions,
    ref_map: &mut RefMap,
) -> Result<String, String> {
    let top = ff.top_context.clone();
    let context = ff.context.clone();
    ff.bidi
        .send("browsingContext.activate", json!({"context": top}))
        .await?;
    let _ = tokio::time::timeout(
        PAINT_WAIT,
        ff.bidi.call_function(&top, SETTLE_JS, None, vec![]),
    )
    .await;
    let tabs = ff.rdp.request("root", "listTabs", json!({})).await?;
    let tab = tabs["tabs"]
        .as_array()
        .and_then(|t| t.iter().find(|t| t["selected"] == true))
        .and_then(|t| t["actor"].as_str())
        .ok_or("Firefox DevTools reported no selected tab")?
        .to_string();
    let target = ff.rdp.request(&tab, "getTarget", json!({})).await?;
    let accessibility = target["frame"]["accessibilityActor"]
        .as_str()
        .ok_or("Firefox DevTools returned no accessibility actor for the tab")?;
    let mut doc = document_snapshot(&ff.rdp, accessibility).await?;
    Box::pin(annotate(ff, &top, &mut doc, &tab)).await?;

    let frame_root = if context == top {
        &doc
    } else {
        find_context_root(&doc, &context)
            .ok_or("The selected frame is no longer in the page. Run frame again.")?
    };
    let root = match &options.selector {
        None => frame_root,
        Some(selector) => {
            let shared_id = find_element(ff, &context, selector)
                .await
                .map_err(|e| format!("Invalid selector '{selector}': {e}"))?
                .ok_or_else(|| format!("Selector '{selector}' did not match any element"))?;
            find_node(frame_root, &context, &shared_id)
                .ok_or_else(|| format!("No accessibility node found for selector '{selector}'"))?
        }
    };
    let mut urls = HashMap::new();
    let FirefoxSession {
        refs,
        next_node_id,
        shadow_ids,
        ..
    } = ff;
    let mut durable_id = |n: &Value| {
        let key = (
            n["abCtx"].as_str()?.to_string(),
            n["abSid"].as_str()?.to_string(),
        );
        let id = *refs.entry(key).or_insert_with(|| {
            *next_node_id += 1;
            *next_node_id
        });
        if n["abShadow"] == true {
            shadow_ids.insert(id);
        }
        if let Some(href) = n["value"]
            .as_str()
            .filter(|h| n["role"] == "link" && !h.is_empty())
        {
            urls.insert(id, href.to_string());
        }
        Some(id)
    };
    let nodes = ax_nodes_from_snapshot(root, &mut durable_id);
    let known = ref_map.observe_document(&context, None, &context, frame_root["abSid"].as_str());
    Ok(snapshot_from_ax_nodes(
        &nodes, &urls, options, ref_map, &context, None, known,
    ))
}

/// Handles the `snapshot` action.
pub async fn snapshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let options = chrome::snapshot_options(cmd);
    let ff = state.firefox.as_mut().ok_or("Browser not launched")?;
    let previous_refs = state.ref_map.ref_ids();
    state.ref_map.begin_snapshot();
    let tree = Box::pin(take_snapshot(ff, &options, &mut state.ref_map)).await?;
    let url = ff.url().await.unwrap_or_default();
    let session_id = ff.top_context.clone();
    chrome::snapshot_response(cmd, state, session_id, &options, previous_refs, tree, url)
}

/// Handles the `diff_snapshot` action.
pub async fn diff_snapshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let options = SnapshotOptions {
        interactive: false,
        urls: false,
        ..chrome::snapshot_options(cmd)
    };
    let ff = state.firefox.as_mut().ok_or("Browser not launched")?;
    let mut current_ref_map = state.ref_map.clone();
    current_ref_map.begin_snapshot();
    let current = Box::pin(take_snapshot(ff, &options, &mut current_ref_map)).await?;
    chrome::diff_snapshot_response(cmd, state, current, current_ref_map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::element::RefMap;
    use crate::native::snapshot::{snapshot_from_ax_nodes, SnapshotOptions};
    use std::collections::HashMap;

    fn leaf(role: &str, name: &str, sel: &str, states: &[&str]) -> Value {
        json!({"role": role, "name": name, "nodeCssSelector": sel, "states": states, "children": []})
    }

    fn page(children: Vec<Value>) -> Value {
        json!({"role": "document", "name": "", "nodeCssSelector": "", "states": [], "children": children})
    }

    fn render_with(root: &Value, ref_map: &mut RefMap, ids: &mut HashMap<String, i64>) -> String {
        let mut id = |n: &Value| {
            let key = n["nodeCssSelector"].as_str()?.to_string();
            let next = ids.len() as i64 + 1;
            Some(*ids.entry(key).or_insert(next))
        };
        let nodes = ax_nodes_from_snapshot(root, &mut id);
        let options = SnapshotOptions {
            selector: None,
            interactive: false,
            compact: false,
            depth: None,
            urls: false,
        };
        ref_map.observe_document("s", None, "s", Some("loader"));
        snapshot_from_ax_nodes(&nodes, &HashMap::new(), &options, ref_map, "s", None, true)
    }

    #[test]
    fn renders_chrome_format() {
        let mut email = leaf(
            "textbox",
            "Email",
            "body > input:nth-child(5)",
            &["editable"],
        );
        email["value"] = json!("a@b.c");
        let root = page(vec![
            leaf("button", "OK", "body > button:nth-child(2)", &["focusable"]),
            email,
            leaf(
                "checkbox",
                "Agree",
                "body > input:nth-child(6)",
                &["checked", "focusable"],
            ),
            leaf(
                "button",
                "Send",
                "body > button:nth-child(7)",
                &["unavailable"],
            ),
            leaf("text leaf", "hello", "body > p", &[]),
        ]);
        let out = render_with(&root, &mut RefMap::new(), &mut HashMap::new());
        assert!(out.contains("button \"OK\" [ref=e1]"), "{out}");
        assert!(out.contains("textbox \"Email\" [ref=e2]: a@b.c"), "{out}");
        assert!(
            out.contains("checkbox \"Agree\" [checked=true, ref=e3]"),
            "{out}"
        );
        assert!(out.contains("button \"Send\" [disabled, ref=e4]"), "{out}");
        assert!(out.contains("StaticText \"hello\""), "{out}");
    }

    #[test]
    fn duplicate_buttons_get_distinct_refs_with_nth() {
        let root = page(vec![
            leaf("button", "OK", "body > button:nth-child(2)", &[]),
            leaf("button", "OK", "body > button:nth-child(3)", &[]),
        ]);
        let mut map = RefMap::new();
        render_with(&root, &mut map, &mut HashMap::new());
        assert_eq!(map.get("e1").unwrap().nth, Some(0));
        assert_eq!(map.get("e2").unwrap().nth, Some(1));
    }

    #[test]
    fn refs_survive_insertion() {
        let ok = || leaf("button", "OK", "body > button:nth-child(2)", &[]);
        let mut map = RefMap::new();
        let mut ids = HashMap::new();
        render_with(&page(vec![ok()]), &mut map, &mut ids);
        let out = render_with(
            &page(vec![
                leaf("button", "New", "body > button:nth-child(1)", &[]),
                ok(),
            ]),
            &mut map,
            &mut ids,
        );
        assert!(out.contains("button \"OK\" [ref=e1]"), "{out}");
        assert!(out.contains("button \"New\" [ref=e2]"), "{out}");
    }

    #[test]
    fn frame_contents_are_resolved_in_their_own_context() {
        let mut tree = page(vec![
            leaf("button", "Top", "body > button:nth-child(1)", &[]),
            json!({"role": "internal frame", "nodeCssSelector": "#f", "children": [
                page(vec![leaf("button", "Inner", "body > button:nth-child(1)", &[])])
            ]}),
        ]);
        assert_eq!(
            shape(&tree)[2][1][2],
            json!([]),
            "frame children are pruned"
        );
        let results = vec![
            json!([{"sharedId": "doc"}, null]),
            json!([{"sharedId": "top"}, null]),
            json!([{"sharedId": "frame"}, "child-ctx"]),
        ];
        let mut frames = Vec::new();
        apply(
            &mut tree,
            &mut results.into_iter(),
            "main",
            &mut Vec::new(),
            &mut frames,
        );
        assert_eq!(frames, vec![(vec![1], "child-ctx".to_string())]);
        assert_eq!(tree["children"][0]["abSid"], "top");
        assert_eq!(tree["children"][1]["abCtx"], "main");
        assert!(tree["children"][1]["children"][0]["children"][0]["abSid"].is_null());
        assert_eq!(node_at(&mut tree, &[1])["nodeCssSelector"], "#f");
    }

    #[test]
    fn link_targets_show_only_with_urls_and_never_as_values() {
        let mut link = leaf("link", "Docs", "body > a", &[]);
        link["value"] = json!("https://x.test/docs");
        let root = page(vec![link]);
        let render = |urls: bool| {
            let mut id = |_: &Value| Some(5);
            let nodes = ax_nodes_from_snapshot(&root, &mut id);
            let options = SnapshotOptions {
                urls,
                ..Default::default()
            };
            let hrefs = HashMap::from([(5, "https://x.test/docs".to_string())]);
            let mut map = RefMap::new();
            snapshot_from_ax_nodes(&nodes, &hrefs, &options, &mut map, "s", None, false)
        };
        assert_eq!(render(false), "- link \"Docs\" [ref=e1]");
        assert_eq!(
            render(true),
            "- link \"Docs\" [ref=e1, url=https://x.test/docs]"
        );
    }
}
