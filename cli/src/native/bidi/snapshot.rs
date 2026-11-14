use std::collections::{HashMap, HashSet};
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use super::client::SANDBOX;
use super::firefox_devtools::RdpClient;
use super::session::FirefoxSession;
use super::target::find_element;
use crate::native::actions::{self as chrome, DaemonState};
use crate::native::cdp::types::{AXNode, AXProperty, AXValue};
use crate::native::element::RefMap;
use crate::native::snapshot::{
    cursor_info_from_value, snapshot_from_ax_nodes, SnapshotOptions, SnapshotRefContext,
    CURSOR_DETECTION_JS,
};

const PAINT_WAIT: Duration = Duration::from_millis(500);
const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(30);

/// Waits two animation frames so Firefox has applied pending DOM changes to its accessibility tree.
const SETTLE_JS: &str =
    "() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(() => r(0))))";

/// Resolves selectors across all open roots, marking collisions for exact accessible-actor lookup.
/// A text leaf resolves to the element that holds the text when that element has no node of its own.
/// It returns frame windows, so it must run in the sandbox, where a page cannot replace `contentWindow`.
const RESOLVE_JS: &str = r#"(tree) => {
  const roots = [document];
  for (let i = 0; i < roots.length; i++) {
    for (const el of roots[i].querySelectorAll('*')) {
      if (el.shadowRoot) roots.push(el.shadowRoot);
    }
  }
  const out = [];
  const walk = ([sel, role, kids]) => {
    let el = role === 'document' ? document : null;
    let ambiguous = false;
    if (role !== 'document' && sel && !sel.endsWith('#text')) {
      for (const root of roots) {
        try {
          const hit = root.querySelector(sel);
          if (hit) {
            if (el) { el = null; ambiguous = true; break; }
            el = hit;
          }
        } catch (_) {}
      }
    }
    const text = !!sel && sel.endsWith('#text');
    out.push([el, role === 'internal frame' && el ? el.contentWindow : null, !!el && el.getRootNode() instanceof ShadowRoot, ambiguous, text ? sel.slice(0, -5) : null]);
    for (const k of kids) walk(k);
  };
  walk(JSON.parse(tree));
  const resolved = new Set(out.map(entry => entry[0]).filter(Boolean));
  for (const entry of out) {
    if (!entry[4]) continue;
    const hits = roots.flatMap(root => {
      try { return [...root.querySelectorAll(entry[4])].slice(0, 2); } catch (_) { return []; }
    });
    if (hits.length !== 1 || resolved.has(hits[0])) continue;
    resolved.add(hits[0]);
    entry[0] = hits[0];
  }
  return out;
}"#;

/// Resolves an exact sequence of root-local selectors through shadow hosts.
/// It returns a frame window, so it must run in the sandbox like `RESOLVE_JS`.
const RESOLVE_CHAIN_JS: &str = r#"(input) => {
  const [chain, role] = JSON.parse(input);
  let root = document;
  let el = null;
  for (let i = 0; i < chain.length; i++) {
    el = root.querySelector(chain[i]);
    if (!el) throw new Error('Snapshot DOM node disappeared');
    if (i + 1 < chain.length) {
      root = el.shadowRoot;
      if (!root) throw new Error('Snapshot shadow root disappeared');
    }
  }
  return [el, role === 'internal frame' ? el.contentWindow : null, el.getRootNode() instanceof ShadowRoot];
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
        "label" => "LabelText",
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
    // Feed Firefox's durable BiDi id into the shared backend-node ref allocator.
    let backend_node_id = durable_id(node);
    nodes.push(AXNode {
        node_id: id.clone(),
        role: Some(ax_value("role", json!(role))),
        name: text("name"),
        value: if role == "link" { None } else { text("value") },
        description: None,
        properties: Some(properties),
        child_ids: None,
        backend_d_o_m_node_id: backend_node_id,
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

#[derive(Clone)]
struct AccessibleSource {
    actor: String,
    inspector: String,
    path: Vec<usize>,
}

/// Fetches a document snapshot and retains its actor for exact DOM-node lookup.
async fn document_snapshot(
    rdp: &RdpClient,
    target: &Value,
) -> Result<(Value, AccessibleSource), String> {
    let accessibility = target["accessibilityActor"]
        .as_str()
        .ok_or("Firefox DevTools returned no accessibility actor")?;
    let walker = rdp.request(accessibility, "getWalker", json!({})).await?;
    let walker = walker["walker"]["actor"]
        .as_str()
        .ok_or("Firefox DevTools returned no accessibility walker")?;
    let children = rdp.request(walker, "children", json!({})).await?;
    let document = children["children"][0]["actor"]
        .as_str()
        .ok_or("Firefox DevTools returned no document accessible")?;
    let snapshot = rdp.request(document, "snapshot", json!({})).await?;
    Ok((
        snapshot.get("snapshot").cloned().unwrap_or(snapshot),
        AccessibleSource {
            actor: document.to_string(),
            inspector: target["inspectorActor"].as_str().unwrap_or("").to_string(),
            path: Vec::new(),
        },
    ))
}

/// Frame targets seen through the tab watchers, indexed by browsing context id.
/// A watcher only announces existing frames the first time it watches, so the events are kept for later snapshots.
pub struct FrameWatch {
    events: broadcast::Receiver<Value>,
    actors: HashMap<String, Value>,
    tabs: HashSet<String>,
}

fn record_target(actors: &mut HashMap<String, Value>, event: &Value) {
    if let (Some(id), Some(_)) = (
        event["target"]["browsingContextID"].as_u64(),
        event["target"]["accessibilityActor"].as_str(),
    ) {
        actors.insert(id.to_string(), event["target"].clone());
    }
}

/// Returns the target of the cross-process frame whose BiDi context is `window`.
async fn frame_actor(
    ff: &mut FirefoxSession,
    tab: &str,
    window: &str,
    deadline: tokio::time::Instant,
) -> Result<Value, String> {
    let rdp = ff.rdp.clone();
    let watch = ff.frames.get_or_insert_with(|| FrameWatch {
        events: rdp.subscribe(),
        actors: HashMap::new(),
        tabs: HashSet::new(),
    });
    if !watch.tabs.contains(tab) {
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
        watch.tabs.insert(tab.to_string());
    }
    loop {
        while let Ok(e) = watch.events.try_recv() {
            record_target(&mut watch.actors, &e);
        }
        if let Some(actor) = watch.actors.get(window) {
            return Ok(actor.clone());
        }
        match tokio::time::timeout_at(deadline, watch.events.recv()).await {
            Ok(Ok(e)) => record_target(&mut watch.actors, &e),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(broadcast::error::RecvError::Closed)) => {
                return Err(format!(
                    "Firefox did not announce frame target {window}: event stream closed"
                ));
            }
            Err(_) => {
                return Err(format!(
                    "Firefox did not announce frame target {window} before the snapshot deadline"
                ));
            }
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

fn resolution_paths(node: &Value, path: &mut Vec<usize>, paths: &mut Vec<Vec<usize>>) {
    paths.push(path.clone());
    if node["role"] != "internal frame" {
        if let Some(children) = node["children"].as_array() {
            for (i, child) in children.iter().enumerate() {
                path.push(i);
                resolution_paths(child, path, paths);
                path.pop();
            }
        }
    }
}

async fn accessible_at(
    rdp: &RdpClient,
    source: &AccessibleSource,
    path: &[usize],
    children: &mut HashMap<String, Vec<Value>>,
) -> Result<String, String> {
    let mut actor = source.actor.clone();
    for &index in source.path.iter().chain(path) {
        if !children.contains_key(&actor) {
            let response = rdp.request(&actor, "children", json!({})).await?;
            children.insert(
                actor.clone(),
                response["children"].as_array().cloned().unwrap_or_default(),
            );
        }
        actor = children[&actor]
            .get(index)
            .and_then(|node| node["actor"].as_str())
            .ok_or("Firefox accessibility tree changed during snapshot")?
            .to_string();
    }
    Ok(actor)
}

async fn attached_node(
    rdp: &RdpClient,
    walker: &str,
    actor: &str,
    path: Value,
    nodes: &mut HashMap<String, Value>,
) -> Result<Value, String> {
    let response = rdp
        .request(
            walker,
            "getNodeFromActor",
            json!({"actorID": actor, "path": path}),
        )
        .await?;
    let attached = &response["node"];
    for node in attached["newParents"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(std::iter::once(&attached["node"]))
    {
        if let Some(actor) = node["actor"].as_str() {
            nodes.insert(actor.to_string(), node.clone());
        }
    }
    if attached["node"]["actor"].as_str().is_none() {
        return Err("Firefox could not locate the snapshot DOM node".to_string());
    }
    Ok(attached["node"].clone())
}

async fn selector_chain(
    rdp: &RdpClient,
    walker: &str,
    accessible: &str,
    nodes: &mut HashMap<String, Value>,
) -> Result<Vec<String>, String> {
    let mut node = attached_node(
        rdp,
        walker,
        accessible,
        json!(["rawAccessible", "DOMNode"]),
        nodes,
    )
    .await?;
    let actor = node["actor"].as_str().ok_or("Missing DOM actor")?;
    let selector = rdp.request(actor, "getUniqueSelector", json!({})).await?;
    let mut chain = vec![selector["value"]
        .as_str()
        .ok_or("Firefox returned no unique selector")?
        .to_string()];
    let mut visited = HashSet::new();
    while node["nodeType"] != 9 {
        let parent = if node["isShadowRoot"] == true {
            node["host"].as_str()
        } else {
            node["parent"].as_str()
        }
        .ok_or("Firefox returned an incomplete DOM ancestry")?
        .to_string();
        if !visited.insert(parent.clone()) {
            return Err("Firefox returned a cyclic DOM ancestry".to_string());
        }
        let host = node["isShadowRoot"] == true;
        node = match nodes.get(&parent) {
            Some(node) => node.clone(),
            None => attached_node(rdp, walker, &parent, json!(["rawNode"]), nodes).await?,
        };
        if host {
            let selector = rdp.request(&parent, "getUniqueSelector", json!({})).await?;
            chain.push(
                selector["value"]
                    .as_str()
                    .ok_or("Firefox returned no shadow-host selector")?
                    .to_string(),
            );
        }
    }
    chain.reverse();
    Ok(chain)
}

async fn resolve_collisions(
    ff: &FirefoxSession,
    context: &str,
    doc: &mut Value,
    source: &AccessibleSource,
    results: &mut [Value],
) -> Result<(), String> {
    if !results.iter().any(|result| result[3] == true) {
        return Ok(());
    }
    let walker = ff
        .rdp
        .request(&source.inspector, "getWalker", json!({}))
        .await?;
    let walker = walker["walker"]["actor"]
        .as_str()
        .ok_or("Firefox DevTools returned no inspector walker")?;
    let mut paths = Vec::new();
    resolution_paths(doc, &mut Vec::new(), &mut paths);
    let mut children = HashMap::new();
    let mut nodes = HashMap::new();
    for (path, result) in paths.iter().zip(results.iter_mut()) {
        if result[3] != true {
            continue;
        }
        let actor = accessible_at(&ff.rdp, source, path, &mut children).await?;
        let chain = selector_chain(&ff.rdp, walker, &actor, &mut nodes).await?;
        let role = node_at(doc, path)["role"].clone();
        *result = ff
            .bidi
            .call_function_in_sandbox(
                context,
                SANDBOX,
                RESOLVE_CHAIN_JS,
                vec![json!({"type": "string", "value": json!([chain, role]).to_string()})],
            )
            .await?;
    }
    Ok(())
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

/// Runs the shared cursor detection over the document and its open shadow roots, keyed by BiDi `sharedId`.
async fn cursor_detections(ff: &FirefoxSession, context: &str) -> HashMap<String, Value> {
    let js = format!(
        r#"() => {{
      const scopes = document.body ? [document.body] : [];
      for (let i = 0; i < scopes.length; i++)
        for (const el of scopes[i].querySelectorAll('*')) if (el.shadowRoot) scopes.push(el.shadowRoot);
      return scopes.flatMap(scope => ({CURSOR_DETECTION_JS})(scope));
    }}"#
    );
    ff.bidi
        .call_function(context, &js, None, vec![])
        .await
        .ok()
        .and_then(|found| found.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|item| Some((item["element"]["sharedId"].as_str()?.to_string(), item)))
        .collect()
}

/// Copies each detected element's cursor info onto the node that resolved to it.
fn attach_cursor(node: &mut Value, detected: &HashMap<String, Value>) {
    if let Some(info) = node["abSid"].as_str().and_then(|sid| detected.get(sid)) {
        node["abCursor"] = info.clone();
    }
    if let Some(children) = node["children"].as_array_mut() {
        for child in children {
            attach_cursor(child, detected);
        }
    }
}

/// Tags every node of `doc` with its BiDi `abSid`/`abCtx`, then does the same inside each frame's own context.
async fn annotate(
    ff: &mut FirefoxSession,
    context: &str,
    doc: &mut Value,
    source: &AccessibleSource,
    tab: &str,
    deadline: tokio::time::Instant,
) -> Result<(), String> {
    let arg = json!({"type": "string", "value": shape(doc).to_string()});
    let resolved = ff
        .bidi
        .call_function_in_sandbox(context, SANDBOX, RESOLVE_JS, vec![arg])
        .await?;
    let mut results = resolved.as_array().cloned().unwrap_or_default();
    resolve_collisions(ff, context, doc, source, &mut results).await?;
    let mut frames = Vec::new();
    apply(
        doc,
        &mut results.into_iter(),
        context,
        &mut Vec::new(),
        &mut frames,
    );
    attach_cursor(doc, &cursor_detections(ff, context).await);
    if let Some(sid) = doc["abSid"].as_str() {
        if ff
            .documents
            .insert(context.to_string(), sid.to_string())
            .as_deref()
            != Some(sid)
        {
            let stale_ids: HashSet<i64> = ff
                .refs
                .iter()
                .filter(|((c, _), _)| c == context)
                .map(|(_, id)| *id)
                .collect();
            ff.refs.retain(|(c, _), _| c != context);
            ff.shadow_ids.retain(|id| !stale_ids.contains(id));
        }
    }
    if !frames.is_empty() {
        ff.refresh_frame_tree().await?;
    }
    for (path, window) in frames {
        if !ff.contexts.is_child_of(&window, context) {
            return Err(
                "The page's frames changed during the snapshot. Run snapshot again.".into(),
            );
        }
        let child_source = if node_at(doc, &path)["useChildTargetToFetchChildren"] == true {
            let target = frame_actor(ff, tab, &window, deadline).await?;
            let (snapshot, source) = document_snapshot(&ff.rdp, &target).await?;
            node_at(doc, &path)["children"] = json!([snapshot]);
            source
        } else {
            let mut child_source = source.clone();
            child_source.path.extend_from_slice(&path);
            child_source.path.push(0);
            child_source
        };
        if let Some(child) = node_at(doc, &path)["children"].get_mut(0) {
            Box::pin(annotate(ff, &window, child, &child_source, tab, deadline)).await?;
        }
    }
    Ok(())
}

/// Per-context ref and document tracking captured before a snapshot so it can be committed or rolled back.
struct SnapshotCheckpoint {
    documents: HashMap<String, String>,
    refs: HashMap<(String, String), i64>,
    next_node_id: i64,
    shadow_ids: HashSet<i64>,
}

impl SnapshotCheckpoint {
    fn capture(ff: &FirefoxSession) -> Self {
        Self {
            documents: ff.documents.clone(),
            refs: ff.refs.clone(),
            next_node_id: ff.next_node_id,
            shadow_ids: ff.shadow_ids.clone(),
        }
    }

    /// Restores tracking when no document changed, otherwise drops the refs of every changed document.
    /// Returns whether a document change invalidated old refs.
    fn restore(self, ff: &mut FirefoxSession) -> bool {
        let changed: HashSet<String> = ff
            .documents
            .iter()
            .filter(|(context, document)| self.documents.get(*context) != Some(*document))
            .map(|(context, _)| context.clone())
            .collect();
        if changed.is_empty() {
            ff.documents = self.documents;
            ff.refs = self.refs;
            ff.next_node_id = self.next_node_id;
            ff.shadow_ids = self.shadow_ids;
            return false;
        }
        let stale: HashSet<i64> = self
            .refs
            .iter()
            .filter(|((context, _), _)| changed.contains(context))
            .map(|(_, id)| *id)
            .collect();
        ff.shadow_ids.retain(|id| !stale.contains(id));
        ff.refs.retain(|(context, _), _| !changed.contains(context));
        true
    }
}

fn recover_failed_capture(
    ff: &mut FirefoxSession,
    ref_map: &mut RefMap,
    checkpoint: SnapshotCheckpoint,
) {
    if checkpoint.restore(ff) {
        ref_map.invalidate_all_documents();
    }
}

pub(crate) async fn capture_tree(ff: &mut FirefoxSession, context: &str) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + SNAPSHOT_DEADLINE;
    let top = ff.contexts.active_tab().to_string();
    ff.bidi
        .send("browsingContext.activate", json!({"context": top}))
        .await?;
    let _ = tokio::time::timeout(
        PAINT_WAIT,
        ff.bidi.call_function(&top, SETTLE_JS, None, vec![]),
    )
    .await;
    let tabs = ff.rdp.request("root", "listTabs", json!({})).await?;
    let (tab, target) = active_tab_target(ff, &top, &tabs, deadline).await?;
    let (mut doc, source) = document_snapshot(&ff.rdp, &target["frame"]).await?;
    Box::pin(annotate(ff, &top, &mut doc, &source, &tab, deadline)).await?;
    if context == top {
        return Ok(doc);
    }
    find_context_root(&doc, context)
        .cloned()
        .ok_or_else(|| "The selected frame is no longer in the page. Run frame again.".to_string())
}

/// Renders the active tab as a Chrome-format snapshot, including iframes and open shadow roots.
pub async fn take_snapshot(
    ff: &mut FirefoxSession,
    options: &SnapshotOptions,
    ref_map: &mut RefMap,
) -> Result<String, String> {
    let context = ff.contexts.active().to_string();
    let frame_root = capture_tree(ff, &context).await?;
    let root = match &options.selector {
        None => &frame_root,
        Some(selector) => {
            let shared_id = find_element(ff, &context, selector)
                .await
                .map_err(|e| format!("Invalid selector '{selector}': {e}"))?
                .ok_or_else(|| format!("Selector '{selector}' did not match any element"))?;
            find_node(&frame_root, &context, &shared_id)
                .ok_or_else(|| format!("No accessibility node found for selector '{selector}'"))?
        }
    };
    let mut urls = HashMap::new();
    let mut cursor_elements = HashMap::new();
    let mut node_contexts = HashMap::new();
    let FirefoxSession {
        refs,
        next_node_id,
        shadow_ids,
        ..
    } = ff;
    let mut durable_id = |n: &Value| {
        let context = n["abCtx"].as_str()?.to_string();
        let key = (context.clone(), n["abSid"].as_str()?.to_string());
        let id = *refs.entry(key).or_insert_with(|| {
            *next_node_id += 1;
            *next_node_id
        });
        node_contexts.insert(id, context);
        if n["abShadow"] == true {
            shadow_ids.insert(id);
        }
        if n["abCursor"].is_object() {
            cursor_elements.insert(id, cursor_info_from_value(&n["abCursor"]));
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
    // Firefox has no CDP loaderId; the document root's sharedId scopes durable refs.
    let document_id = frame_root["abSid"].as_str();
    let known = ref_map.observe_document(&context, None, &context, document_id);
    ff.ref_documents = ff.documents.clone();
    Ok(snapshot_from_ax_nodes(
        &nodes,
        &urls,
        &cursor_elements,
        options,
        ref_map,
        SnapshotRefContext {
            node_contexts: &node_contexts,
            session_id: &context,
            frame_id: None,
            document_is_known: known,
        },
    ))
}

/// Handles the `snapshot` action.
pub async fn snapshot(cmd: &Value, state: &mut DaemonState) -> Result<Value, String> {
    let options = chrome::snapshot_options(cmd);
    let ff = state.firefox.as_mut().ok_or("Browser not launched")?;
    let previous_refs = state.ref_map.ref_ids();
    let mut staged_ref_map = state.ref_map.clone();
    staged_ref_map.begin_snapshot();
    let checkpoint = SnapshotCheckpoint::capture(ff);
    let tree = match Box::pin(take_snapshot(ff, &options, &mut staged_ref_map)).await {
        Ok(tree) => tree,
        Err(error) => {
            recover_failed_capture(ff, &mut state.ref_map, checkpoint);
            return Err(error);
        }
    };
    state.ref_map = staged_ref_map;
    let url = ff.url().await.unwrap_or_default();
    let session_id = ff.contexts.active_tab().to_string();
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
    let checkpoint = SnapshotCheckpoint::capture(ff);
    let mut current_ref_map = state.ref_map.clone();
    current_ref_map.begin_snapshot();
    let current = match Box::pin(take_snapshot(ff, &options, &mut current_ref_map)).await {
        Ok(current) => current,
        Err(error) => {
            recover_failed_capture(ff, &mut state.ref_map, checkpoint);
            return Err(error);
        }
    };
    let result = chrome::diff_snapshot_response(cmd, state, current, current_ref_map);
    if result.is_err() {
        if let Some(ff) = state.firefox.as_mut() {
            recover_failed_capture(ff, &mut state.ref_map, checkpoint);
        }
    }
    result
}

/// Returns the tab's target when the page behind it holds `secret` under `marker`, or `None` when it does not.
/// A page decides what its own globals return, so only a secret no other page can read identifies it.
async fn probe_tab(
    rdp: &RdpClient,
    actor: &str,
    marker: &str,
    secret: &str,
) -> Result<Option<Value>, String> {
    let target = rdp.request(actor, "getTarget", json!({})).await?;
    let console = target["frame"]["consoleActor"]
        .as_str()
        .ok_or("Firefox DevTools returned no console actor for the tab")?
        .to_string();
    let text = format!("window[{}]", json!(marker));
    let mut events = rdp.subscribe();
    let pending = rdp
        .request(&console, "evaluateJSAsync", json!({"text": text}))
        .await?;
    let result_id = pending["resultID"]
        .as_str()
        .ok_or("Firefox DevTools returned no evaluation id")?;
    loop {
        let event = events.recv().await.map_err(|e| e.to_string())?;
        if event["from"] == console.as_str()
            && event["type"] == "evaluationResult"
            && event["resultID"] == result_id
        {
            if !event["exception"].is_null() {
                return Err("Firefox DevTools could not inspect the tab".to_string());
            }
            return Ok((event["result"] == secret).then_some(target));
        }
    }
}

async fn active_tab_target(
    ff: &FirefoxSession,
    top: &str,
    tabs: &Value,
    deadline: tokio::time::Instant,
) -> Result<(String, Value), String> {
    let marker = format!("agent-browser-{}", uuid::Uuid::new_v4());
    let secret = uuid::Uuid::new_v4().to_string();
    ff.bidi.call_function(
        top,
        "function(key, secret) { Object.defineProperty(window, key, { value: secret, configurable: true }); }",
        None,
        vec![json!(marker), json!(secret)],
    ).await?;
    let result = tokio::time::timeout_at(deadline, async {
        let tabs = tabs["tabs"]
            .as_array()
            .ok_or("Firefox DevTools reported no tabs")?;
        let (marker, secret) = (marker.as_str(), secret.as_str());
        let mut probes: FuturesUnordered<_> = tabs
            .iter()
            .filter_map(|tab| tab["actor"].as_str())
            .map(|actor| async move { (actor, probe_tab(&ff.rdp, actor, marker, secret).await) })
            .collect();
        let mut failure = None;
        while let Some((actor, probed)) = probes.next().await {
            match probed {
                Ok(Some(target)) => return Ok((actor.to_string(), target)),
                Ok(None) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        Err(failure.unwrap_or_else(|| "Firefox DevTools could not find the active tab".to_string()))
    })
    .await
    .unwrap_or_else(|_| Err("Firefox DevTools evaluation timed out".to_string()));
    let _ = ff
        .bidi
        .call_function(
            top,
            "function(key) { delete window[key]; }",
            None,
            vec![json!(marker)],
        )
        .await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::element::RefMap;
    use crate::native::snapshot::{snapshot_from_ax_nodes, SnapshotOptions, SnapshotRefContext};
    use std::collections::{HashMap, HashSet};

    fn leaf(role: &str, name: &str, sel: &str, states: &[&str]) -> Value {
        json!({"role": role, "name": name, "nodeCssSelector": sel, "states": states, "children": []})
    }

    fn page(children: Vec<Value>) -> Value {
        json!({"role": "document", "name": "", "nodeCssSelector": "", "states": [], "children": children})
    }

    #[tokio::test]
    async fn missing_frame_actor_fails_before_snapshot_returns_partial_tree() {
        let (mut ff, _, _) = crate::native::bidi::session::tests::fake_session().await;
        ff.frames = Some(FrameWatch {
            events: ff.rdp.subscribe(),
            actors: HashMap::new(),
            tabs: HashSet::from(["tab".to_string()]),
        });

        let error = frame_actor(
            &mut ff,
            "tab",
            "child-context",
            tokio::time::Instant::now() + Duration::from_millis(5),
        )
        .await
        .unwrap_err();

        assert!(error.contains("child-context"), "{error}");
        assert!(error.contains("snapshot deadline"), "{error}");
    }

    #[tokio::test]
    async fn lagged_frame_events_do_not_abort_wait() {
        let (mut ff, _, _) = crate::native::bidi::session::tests::fake_session().await;
        let (tx, events) = broadcast::channel(1024);
        ff.frames = Some(FrameWatch {
            events,
            actors: HashMap::new(),
            tabs: HashSet::from(["tab".to_string()]),
        });
        let sender = tokio::spawn(async move {
            tokio::task::yield_now().await;
            for _ in 0..1024 {
                tx.send(json!({})).unwrap();
            }
            tx.send(json!({
                "target": {"browsingContextID": 42, "accessibilityActor": "frame-actor"}
            }))
            .unwrap();
        });

        let actor = frame_actor(
            &mut ff,
            "tab",
            "42",
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap();
        sender.await.unwrap();

        assert_eq!(actor["accessibilityActor"], "frame-actor");
    }

    #[tokio::test]
    async fn closed_frame_events_report_a_terminal_error() {
        let (mut ff, _, _) = crate::native::bidi::session::tests::fake_session().await;
        let (tx, events) = broadcast::channel(1024);
        drop(tx);
        ff.frames = Some(FrameWatch {
            events,
            actors: HashMap::new(),
            tabs: HashSet::from(["tab".to_string()]),
        });

        let error = frame_actor(
            &mut ff,
            "tab",
            "child-context",
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(error.contains("event stream closed"), "{error}");
    }

    /// A session whose DevTools peer lists a console per tab and lets `answer` decide what each
    /// console evaluates the marker to, given the secret the daemon placed. `None` never answers.
    async fn tab_lookup_session(
        answer: impl Fn(&str, &str) -> Option<Value> + Send + 'static,
    ) -> FirefoxSession {
        use crate::native::bidi::firefox_devtools::tests::{get, pair, put};

        let secret = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let placed = secret.clone();
        let (mut ff, _, _) =
            crate::native::bidi::session::tests::fake_session_with(move |v: &Value| {
                if let Some(value) = v["params"]["arguments"][1]["value"].as_str() {
                    *placed.lock().unwrap() = value.to_string();
                }
                json!({})
            })
            .await;
        let (rdp, mut peer) = pair().await;
        ff.rdp = rdp;
        tokio::spawn(async move {
            loop {
                let request = get(&mut peer).await;
                let to = request["to"].as_str().unwrap().to_string();
                match request["type"].as_str().unwrap() {
                    "getTarget" => {
                        let console = format!("console-{to}");
                        put(
                            &mut peer,
                            json!({"from": to, "frame": {"consoleActor": console}}),
                        )
                        .await;
                    }
                    "evaluateJSAsync" => {
                        put(&mut peer, json!({"from": to, "resultID": "result"})).await;
                        let secret = secret.lock().unwrap().clone();
                        if let Some(result) = answer(&to, &secret) {
                            put(
                                &mut peer,
                                json!({"from": to, "type": "evaluationResult", "resultID": "result", "result": result}),
                            )
                            .await;
                        }
                    }
                    other => panic!("unexpected DevTools request {other}"),
                }
            }
        });
        ff
    }

    async fn looked_up_tab(ff: &FirefoxSession, tabs: &Value) -> String {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let (tab, target) = active_tab_target(ff, "ctx-active", tabs, deadline)
            .await
            .unwrap();
        assert_eq!(target["frame"]["consoleActor"], format!("console-{tab}"));
        tab
    }

    #[tokio::test]
    async fn a_stalled_tab_does_not_block_finding_the_active_tab() {
        let ff = tab_lookup_session(|console, secret| {
            (console == "console-active").then(|| json!(secret))
        })
        .await;
        let tabs = json!({"tabs": [{"actor": "blocked"}, {"actor": "active"}]});

        assert_eq!(looked_up_tab(&ff, &tabs).await, "active");
    }

    #[tokio::test]
    async fn a_tab_that_forges_the_marker_reply_is_not_taken_for_the_active_tab() {
        let ff = tab_lookup_session(|console, secret| {
            Some(if console == "console-active" {
                json!(secret)
            } else {
                json!(true)
            })
        })
        .await;
        let tabs = json!({"tabs": [{"actor": "forged"}, {"actor": "active"}]});

        assert_eq!(looked_up_tab(&ff, &tabs).await, "active");
    }

    fn tag_snapshot_nodes(node: &mut Value, context: &str, document_id: &str, is_root: bool) {
        let shared_id = if is_root {
            document_id.to_string()
        } else {
            format!(
                "{document_id}:{}",
                node["nodeCssSelector"].as_str().unwrap_or("")
            )
        };
        node["abCtx"] = json!(context);
        node["abSid"] = json!(shared_id);
        if let Some(children) = node["children"].as_array_mut() {
            for child in children {
                tag_snapshot_nodes(child, context, document_id, false);
            }
        }
    }

    fn render_with(
        root: &Value,
        ref_map: &mut RefMap,
        ids: &mut HashMap<(String, String), i64>,
        document_id: &str,
    ) -> String {
        let mut root = root.clone();
        tag_snapshot_nodes(&mut root, "s", document_id, true);
        let mut id = |n: &Value| {
            let key = (
                n["abCtx"].as_str()?.to_string(),
                n["abSid"].as_str()?.to_string(),
            );
            let next = ids.len() as i64 + 1;
            Some(*ids.entry(key).or_insert(next))
        };
        let nodes = ax_nodes_from_snapshot(&root, &mut id);
        let options = SnapshotOptions {
            selector: None,
            interactive: false,
            compact: false,
            depth: None,
            urls: false,
        };
        let known = ref_map.observe_document("s", None, "s", Some(document_id));
        snapshot_from_ax_nodes(
            &nodes,
            &HashMap::new(),
            &HashMap::new(),
            &options,
            ref_map,
            SnapshotRefContext {
                node_contexts: &HashMap::new(),
                session_id: "s",
                frame_id: None,
                document_is_known: known,
            },
        )
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
        let out = render_with(&root, &mut RefMap::new(), &mut HashMap::new(), "loader");
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
        render_with(&root, &mut map, &mut HashMap::new(), "loader");
        assert_eq!(map.get("e1").unwrap().nth, Some(0));
        assert_eq!(map.get("e2").unwrap().nth, Some(1));
    }

    #[test]
    fn duplicate_names_in_different_frames_do_not_get_nth() {
        let mut root = page(vec![
            leaf("button", "OK", "#top", &[]),
            leaf("button", "OK", "#child", &[]),
        ]);
        tag_snapshot_nodes(&mut root, "top", "document", true);
        root["children"][1]["abCtx"] = json!("child");
        let mut contexts = HashMap::new();
        let mut next_id = 0;
        let mut durable_id = |node: &Value| {
            next_id += 1;
            contexts.insert(next_id, node["abCtx"].as_str()?.to_string());
            Some(next_id)
        };
        let nodes = ax_nodes_from_snapshot(&root, &mut durable_id);
        let mut refs = RefMap::new();

        snapshot_from_ax_nodes(
            &nodes,
            &HashMap::new(),
            &HashMap::new(),
            &SnapshotOptions::default(),
            &mut refs,
            SnapshotRefContext {
                node_contexts: &contexts,
                session_id: "top",
                frame_id: None,
                document_is_known: false,
            },
        );

        assert_eq!(refs.get("e1").unwrap().nth, None);
        assert_eq!(refs.get("e2").unwrap().nth, None);
    }

    #[test]
    fn refs_survive_insertion() {
        let ok = || leaf("button", "OK", "body > button:nth-child(2)", &[]);
        let mut map = RefMap::new();
        let mut ids = HashMap::new();
        render_with(&page(vec![ok()]), &mut map, &mut ids, "loader");
        map.begin_snapshot();
        let out = render_with(
            &page(vec![
                leaf("button", "New", "body > button:nth-child(1)", &[]),
                ok(),
            ]),
            &mut map,
            &mut ids,
            "loader",
        );
        assert!(out.contains("button \"OK\" [ref=e1]"), "{out}");
        assert!(out.contains("button \"New\" [ref=e2]"), "{out}");
    }

    #[test]
    fn repeated_snapshots_reuse_refs_for_the_same_document() {
        let root = page(vec![leaf("button", "OK", "#ok", &[])]);
        let mut map = RefMap::new();
        let mut ids = HashMap::new();

        let first = render_with(&root, &mut map, &mut ids, "loader-a");
        assert!(first.contains("button \"OK\" [ref=e1]"), "{first}");
        assert!(map.get("e1").unwrap().backend_node_id.is_some());

        map.begin_snapshot();
        let second = render_with(&root, &mut map, &mut ids, "loader-a");

        assert_eq!(second, first);
        assert!(second.contains("button \"OK\" [ref=e1]"), "{second}");
    }

    #[tokio::test]
    async fn changed_document_allocates_new_refs_and_rejects_stale_refs() {
        let root = page(vec![leaf("button", "OK", "#ok", &[])]);
        let mut map = RefMap::new();
        let mut ids = HashMap::new();

        let first = render_with(&root, &mut map, &mut ids, "loader-a");
        assert!(first.contains("button \"OK\" [ref=e1]"), "{first}");
        let stale_ref = map.entries_sorted()[0].0.clone();

        map.begin_snapshot();
        let changed = render_with(&root, &mut map, &mut ids, "loader-b");

        assert!(changed.contains("button \"OK\" [ref=e2]"), "{changed}");
        assert!(map.get(&stale_ref).is_none());
        let mut state = DaemonState::new();
        state.ref_map = map;
        let error =
            crate::native::bidi::target::resolve_target(&mut state, &format!("@{stale_ref}"))
                .await
                .unwrap_err();
        assert_eq!(error, format!("Unknown ref: {stale_ref}"));
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
            snapshot_from_ax_nodes(
                &nodes,
                &hrefs,
                &HashMap::new(),
                &options,
                &mut map,
                SnapshotRefContext {
                    node_contexts: &HashMap::new(),
                    session_id: "s",
                    frame_id: None,
                    document_is_known: false,
                },
            )
        };
        assert_eq!(render(false), "- link \"Docs\" [ref=e1]");
        assert_eq!(
            render(true),
            "- link \"Docs\" [ref=e1, url=https://x.test/docs]"
        );
    }

    #[tokio::test]
    async fn failed_capture_without_a_document_change_rolls_back_tracking() {
        let (mut ff, tx, _) = crate::native::bidi::session::tests::fake_session().await;
        std::mem::forget(tx);
        ff.documents.insert("ctx-active".into(), "loader-1".into());
        ff.refs.insert(("ctx-active".into(), "node-1".into()), 7);
        ff.next_node_id = 7;
        ff.shadow_ids.insert(7);

        let checkpoint = SnapshotCheckpoint::capture(&ff);
        ff.refs.insert(("ctx-active".into(), "node-2".into()), 8);
        ff.next_node_id = 8;
        ff.shadow_ids.insert(8);

        let mut map = RefMap::new();
        recover_failed_capture(&mut ff, &mut map, checkpoint);

        assert_eq!(ff.refs.len(), 1);
        assert_eq!(
            ff.refs[&("ctx-active".to_string(), "node-1".to_string())],
            7
        );
        assert_eq!(ff.next_node_id, 7);
        assert_eq!(ff.shadow_ids, HashSet::from([7]));
        assert_eq!(ff.documents["ctx-active"], "loader-1");
    }

    #[tokio::test]
    async fn failed_capture_after_a_document_change_invalidates_old_refs() {
        let (mut ff, tx, _) = crate::native::bidi::session::tests::fake_session().await;
        std::mem::forget(tx);
        ff.documents.insert("ctx-active".into(), "loader-1".into());
        ff.refs.insert(("ctx-active".into(), "node-1".into()), 7);
        ff.shadow_ids.insert(7);

        let checkpoint = SnapshotCheckpoint::capture(&ff);
        ff.documents.insert("ctx-active".into(), "loader-2".into());
        ff.refs.insert(("ctx-active".into(), "node-2".into()), 8);

        let mut map = RefMap::new();
        map.add("e1".into(), Some(7), "button", "OK", None);
        map.observe_document("ctx-active", None, "ctx-active", Some("loader-1"));
        recover_failed_capture(&mut ff, &mut map, checkpoint);

        assert!(ff.refs.is_empty());
        assert!(!ff.shadow_ids.contains(&7));
        assert_eq!(ff.documents["ctx-active"], "loader-2");
        assert!(map.get("e1").is_none());
    }
}
