//! Accessibility auditing backed by a vendored copy of Deque's axe-core.
//!
//! `axe.min.js` is the unmodified upstream build (MPL-2.0 — see
//! LICENSE-axe-core.txt and LICENSE-axe-core-THIRD-PARTY.txt alongside it).
//! The audit captures this exact build through a private CommonJS export in an
//! isolated world for every frame. Serialized partial results are merged
//! outside axe's cross-frame messaging, so page-owned JavaScript globals remain
//! untouched.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use super::cdp::client::CdpClient;
use super::cdp::types::{EvaluateResult, RemoteObject};

/// Unmodified axe-core build, injected via `Runtime.evaluate` (which is
/// not subject to the page's CSP, unlike a CDN `<script>` tag).
pub const AXE_JS: &str = include_str!("axe.min.js");

/// Version of the vendored axe-core build. Keep this in sync with `axe.min.js`.
pub const AXE_VERSION: &str = "4.12.1";

const AUDIT_WORLD_NAME: &str = "__agent_browser_a11y_4_12_1__";

fn tag_values(tags: Option<&str>) -> Vec<&str> {
    tags.map(|tags| {
        tags.split(',')
            .map(str::trim)
            .filter(|tag| !tag.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn private_engine_setup() -> String {
    format!(
        r#"const previousAxe = Object.getOwnPropertyDescriptor(window, 'axe');
  let agentAxe;
  try {{
    // This runs in an isolated world, never the page's JavaScript world. Guard
    // the world-local descriptor anyway so axe's UMD assignment cannot invoke
    // an accessor if another audit helper reused this world name.
    if (previousAxe && !previousAxe.configurable) {{
      throw new Error('Accessibility audit world has a locked axe property');
    }}
    Object.defineProperty(window, 'axe', {{
      value: undefined,
      writable: true,
      enumerable: previousAxe ? previousAxe.enumerable : false,
      configurable: true,
    }});
    // The vendored UMD build exports through this lexical CommonJS module.
    // Hide world-local AMD loaders so evaluating axe cannot register modules
    // outside the private CommonJS export.
    const module = {{ exports: {{}} }};
    const define = undefined;
    {axe_js}
    agentAxe = module.exports;
  }} finally {{
    // axe-core also assigns window.axe in browsers. Restore this isolated world
    // exactly after capturing our private export.
    if (previousAxe) {{
      Object.defineProperty(window, 'axe', previousAxe);
    }} else {{
      delete window.axe;
    }}
  }}"#,
        axe_js = AXE_JS,
    )
}

/// Build an axe report expression. Results are trimmed to what an agent needs
/// to locate and fix each issue; full pass/inapplicable node lists stay in the
/// browser.
fn build_report_expression(
    engine_setup: &str,
    run_call: &str,
    tags: Option<&str>,
    selector: Option<&str>,
    disable_iframes: bool,
) -> String {
    // JSON-encode injected values so selectors/tags can't break out of the
    // script.
    let tags_json = json!(tag_values(tags)).to_string();
    let selector_json = json!(selector).to_string();
    let axe_version_json = json!(AXE_VERSION).to_string();
    let iframes_option = if disable_iframes {
        "options.iframes = false;"
    } else {
        ""
    };
    format!(
        r#"(() => {{
  {engine_setup}
  if (!agentAxe || agentAxe.version !== {axe_version_json} || typeof agentAxe.run !== 'function') {{
    return JSON.stringify({{ error: 'Failed to initialize vendored axe-core {axe_version}' }});
  }}
  const tags = {tags_json};
  const selector = {selector_json};
  if (selector !== null) {{
    let matchedSelector;
    try {{
      matchedSelector = document.querySelector(selector);
    }} catch (error) {{
      return JSON.stringify({{ error: 'Invalid selector: ' + selector }});
    }}
    if (!matchedSelector) {{
      return JSON.stringify({{ error: 'No element matches selector: ' + selector }});
    }}
  }}
  const options = {{ resultTypes: ['violations', 'incomplete'] }};
  {iframes_option}
  if (tags.length > 0) options.runOnly = {{ type: 'tag', values: tags }};
  const trimNodes = (nodes) => nodes.slice(0, 10).map((n) => ({{
    // Keep axe's selector path intact. Nested arrays identify shadow DOM
    // boundaries and multiple entries can identify frame boundaries.
    target: n.target,
    html: typeof n.html === 'string' ? n.html.slice(0, 300) : '',
    failureSummary: n.failureSummary || '',
  }}));
  const trim = (results) => results.map((r) => ({{
    id: r.id,
    impact: r.impact || 'unknown',
    help: r.help,
    helpUrl: r.helpUrl,
    tags: r.tags,
    nodeCount: r.nodes.length,
    nodes: trimNodes(r.nodes),
  }}));
  return {run_call}.then((r) => JSON.stringify({{
    url: r.url,
    axeVersion: r.testEngine ? r.testEngine.version : null,
    counts: {{
      violations: r.violations.length,
      incomplete: r.incomplete.length,
      passes: r.passes.length,
      inapplicable: r.inapplicable.length,
    }},
    violations: trim(r.violations),
    incomplete: trim(r.incomplete),
  }}));
}})()"#,
        axe_version = AXE_VERSION,
    )
}

/// Build a standalone `axe.run()` expression for the top document.
pub fn run_expression(tags: Option<&str>, selector: Option<&str>) -> String {
    build_report_expression(
        &private_engine_setup(),
        "agentAxe.run(selector === null ? document : selector, options)",
        tags,
        selector,
        false,
    )
}

fn partial_expression(
    tags: Option<&str>,
    selector: Option<&str>,
    frame_context: Option<&Value>,
    disable_iframes: bool,
) -> String {
    let tags_json = json!(tag_values(tags)).to_string();
    let selector_json = json!(selector).to_string();
    let frame_context_json = json!(frame_context).to_string();
    let axe_version_json = json!(AXE_VERSION).to_string();
    let iframes_option = if disable_iframes {
        "options.iframes = false;"
    } else {
        ""
    };
    format!(
        r#"(() => {{
  {engine_setup}
  if (!agentAxe || agentAxe.version !== {axe_version_json} || typeof agentAxe.runPartial !== 'function' || !agentAxe.utils || typeof agentAxe.utils.getFrameContexts !== 'function') {{
    return JSON.stringify({{ error: 'Failed to initialize vendored axe-core {axe_version}' }});
  }}
  const tags = {tags_json};
  const selector = {selector_json};
  const frameContext = {frame_context_json};
  if (frameContext === null && selector !== null) {{
    let matchedSelector;
    try {{
      matchedSelector = document.querySelector(selector);
    }} catch (error) {{
      return JSON.stringify({{ error: 'Invalid selector: ' + selector }});
    }}
    if (!matchedSelector) {{
      return JSON.stringify({{ error: 'No element matches selector: ' + selector }});
    }}
  }}
  const options = {{ resultTypes: ['violations', 'incomplete'] }};
  {iframes_option}
  if (tags.length > 0) options.runOnly = {{ type: 'tag', values: tags }};
  const context = frameContext === null
    ? (selector === null ? document : selector)
    : frameContext;
  const frameContexts = agentAxe.utils.getFrameContexts(context)
    .map(({{ frameContext: childFrameContext }}) => childFrameContext);
  return agentAxe.runPartial(context, options)
    .then((partial) => JSON.stringify({{ partial, frameContexts }}));
}})()"#,
        engine_setup = private_engine_setup(),
        axe_version = AXE_VERSION,
    )
}

fn finish_expression(partials: &[Value], tags: Option<&str>, selector: Option<&str>) -> String {
    let partials_json = json!(partials).to_string();
    build_report_expression(
        &private_engine_setup(),
        &format!("agentAxe.finishRun({}, options)", partials_json),
        tags,
        selector,
        selector.is_some(),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContextEvaluateParams<'a> {
    expression: &'a str,
    return_by_value: bool,
    await_promise: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_id: Option<i64>,
}

async fn evaluate(
    client: &CdpClient,
    session_id: &str,
    context_id: Option<i64>,
    expression: &str,
) -> Result<Value, String> {
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &ContextEvaluateParams {
                expression,
                return_by_value: true,
                await_promise: true,
                context_id,
            },
            Some(session_id),
        )
        .await?;

    if let Some(details) = result.exception_details {
        let message = details
            .exception
            .as_ref()
            .and_then(|exception| exception.description.as_deref())
            .unwrap_or(&details.text);
        return Err(format!("Evaluation error: {}", message));
    }

    Ok(result.result.value.unwrap_or(Value::Null))
}

async fn evaluate_remote(
    client: &CdpClient,
    session_id: &str,
    context_id: Option<i64>,
    expression: &str,
) -> Result<RemoteObject, String> {
    let result: EvaluateResult = client
        .send_command_typed(
            "Runtime.evaluate",
            &ContextEvaluateParams {
                expression,
                return_by_value: false,
                await_promise: false,
                context_id,
            },
            Some(session_id),
        )
        .await?;

    if let Some(details) = result.exception_details {
        let message = details
            .exception
            .as_ref()
            .and_then(|exception| exception.description.as_deref())
            .unwrap_or(&details.text);
        return Err(format!("Evaluation error: {}", message));
    }

    Ok(result.result)
}

fn frame_owner_expression(frame_spec: &Value) -> Result<String, String> {
    let selector = frame_spec
        .get("selector")
        .and_then(|value| value.as_array())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "axe frame result is missing its selector".to_string())?;
    let selector_json = json!(selector).to_string();
    Ok(format!(
        r#"(() => {{
  const selectorPath = {selector_json};
  const selector = selectorPath[selectorPath.length - 1];
  if (Array.isArray(selector)) {{
    let root = document;
    let element = null;
    for (let index = 0; index < selector.length; index += 1) {{
      element = root.querySelector(selector[index]);
      if (!element) return null;
      if (index + 1 < selector.length) {{
        root = element.shadowRoot;
        if (!root) return null;
      }}
    }}
    return element;
  }}
  return typeof selector === 'string' ? document.querySelector(selector) : null;
}})()"#,
    ))
}

async fn resolve_child_frame_id(
    client: &CdpClient,
    session_id: &str,
    context_id: Option<i64>,
    frame_spec: &Value,
) -> Result<String, String> {
    let expression = frame_owner_expression(frame_spec)?;
    let remote = evaluate_remote(client, session_id, context_id, &expression).await?;
    let object_id = remote
        .object_id
        .ok_or_else(|| "Could not resolve axe frame selector".to_string())?;
    let describe = client
        .send_command(
            "DOM.describeNode",
            Some(json!({ "objectId": object_id, "depth": 1 })),
            Some(session_id),
        )
        .await;
    let _ = client
        .send_command(
            "Runtime.releaseObject",
            Some(json!({ "objectId": object_id })),
            Some(session_id),
        )
        .await;
    let describe = describe?;

    describe
        .get("node")
        .and_then(|node| node.get("contentDocument"))
        .and_then(|document| document.get("frameId"))
        .and_then(|value| value.as_str())
        .or_else(|| {
            describe
                .get("node")
                .and_then(|node| node.get("frameId"))
                .and_then(|value| value.as_str())
        })
        .map(ToString::to_string)
        .ok_or_else(|| "Could not resolve axe frame ID".to_string())
}

#[cfg(test)]
fn collect_frame_ids(tree: &Value, frame_ids: &mut Vec<String>) {
    if let Some(frame_id) = tree
        .get("frame")
        .and_then(|frame| frame.get("id"))
        .and_then(|id| id.as_str())
    {
        frame_ids.push(frame_id.to_string());
    }
    if let Some(children) = tree.get("childFrames").and_then(|value| value.as_array()) {
        for child in children {
            collect_frame_ids(child, frame_ids);
        }
    }
}

#[derive(Debug, Clone)]
struct FrameTarget {
    frame_id: String,
    session_id: String,
    parent_id: Option<String>,
}

fn collect_frame_targets(
    tree: &Value,
    parent_session_id: &str,
    iframe_sessions: &HashMap<String, String>,
    targets: &mut HashMap<String, FrameTarget>,
) {
    let session_id = if let Some(frame) = tree.get("frame") {
        let Some(frame_id) = frame.get("id").and_then(|id| id.as_str()) else {
            return;
        };
        // Same-process child frames do not have their own target session. They
        // execute in the nearest ancestor target, which may itself be an
        // out-of-process iframe rather than the top-level page.
        let session_id = iframe_sessions
            .get(frame_id)
            .cloned()
            .unwrap_or_else(|| parent_session_id.to_string());
        let target = FrameTarget {
            frame_id: frame_id.to_string(),
            session_id: session_id.clone(),
            parent_id: frame
                .get("parentId")
                .and_then(|id| id.as_str())
                .map(ToString::to_string),
        };
        targets
            .entry(frame_id.to_string())
            .and_modify(|existing| {
                // A tree queried through the frame's dedicated target is the
                // authoritative source for its execution session and children.
                if iframe_sessions.get(frame_id) == Some(&session_id) {
                    let mut authoritative = target.clone();
                    if authoritative.parent_id.is_none() {
                        authoritative.parent_id.clone_from(&existing.parent_id);
                    }
                    *existing = authoritative;
                }
            })
            .or_insert(target);
        session_id
    } else {
        parent_session_id.to_string()
    };
    if let Some(children) = tree.get("childFrames").and_then(|value| value.as_array()) {
        for child in children {
            collect_frame_targets(child, &session_id, iframe_sessions, targets);
        }
    }
}

fn frame_reaches_top(
    frame_id: &str,
    top_frame_id: &str,
    targets: &HashMap<String, FrameTarget>,
) -> bool {
    let mut current = frame_id;
    let mut visited = HashSet::new();
    loop {
        if current == top_frame_id {
            return true;
        }
        if !visited.insert(current.to_string()) {
            return false;
        }
        let Some(parent_id) = targets
            .get(current)
            .and_then(|target| target.parent_id.as_deref())
        else {
            return false;
        };
        current = parent_id;
    }
}

async fn collect_frame_sessions(
    client: &CdpClient,
    top_session_id: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<(String, Vec<FrameTarget>), String> {
    let top_tree = client
        .send_command_no_params("Page.getFrameTree", Some(top_session_id))
        .await?;
    let top_frame_id = top_tree
        .get("frameTree")
        .and_then(|tree| tree.get("frame"))
        .and_then(|frame| frame.get("id"))
        .and_then(|id| id.as_str())
        .ok_or("Could not determine top-level frame ID")?
        .to_string();

    let mut targets = HashMap::new();
    if let Some(tree) = top_tree.get("frameTree") {
        collect_frame_targets(tree, top_session_id, iframe_sessions, &mut targets);
    }

    // Query every attached iframe target. The top target's frame tree can
    // omit descendants below an OOPIF, while the OOPIF's own tree exposes
    // those same-process descendants with the correct execution session.
    let mut session_entries: Vec<_> = iframe_sessions.values().collect();
    session_entries.sort_unstable();
    session_entries.dedup();
    for session_id in session_entries {
        let Some(tree) = client
            .send_command_no_params("Page.getFrameTree", Some(session_id))
            .await
            .ok()
            .and_then(|result| result.get("frameTree").cloned())
        else {
            continue;
        };
        collect_frame_targets(&tree, session_id, iframe_sessions, &mut targets);
    }

    // The daemon retains sessions for background tabs. Keep only frames whose
    // parent chain reaches the active page; audit ordering is resolved later
    // from axe's frame specs rather than HashMap or attachment order.
    let mut active_targets: Vec<_> = targets
        .values()
        .filter(|target| frame_reaches_top(&target.frame_id, &top_frame_id, &targets))
        .cloned()
        .collect();
    active_targets.sort_unstable_by(|left, right| left.frame_id.cmp(&right.frame_id));

    Ok((top_frame_id, active_targets))
}

#[cfg(test)]
fn frame_target(frame_id: &str, session_id: &str, parent_id: Option<&str>) -> FrameTarget {
    FrameTarget {
        frame_id: frame_id.to_string(),
        session_id: session_id.to_string(),
        parent_id: parent_id.map(ToString::to_string),
    }
}

/// Return the dedicated target sessions that belong to the active page's
/// frame tree. The daemon keeps iframe sessions from background tabs so an
/// audit can recover them after a tab switch, while network capture uses this
/// active subset to avoid mixing traffic from different tabs.
pub async fn active_iframe_session_ids(
    client: &CdpClient,
    top_session_id: &str,
    iframe_sessions: &HashMap<String, String>,
) -> Result<HashSet<String>, String> {
    let (_, frame_targets) =
        collect_frame_sessions(client, top_session_id, iframe_sessions).await?;
    Ok(frame_targets
        .into_iter()
        .filter_map(|target| (target.session_id != top_session_id).then_some(target.session_id))
        .collect())
}

#[derive(Debug)]
struct IsolatedFrameWorld {
    session_id: String,
    context_id: i64,
}

async fn create_isolated_frame_context(
    client: &CdpClient,
    target: &FrameTarget,
    world_name: &str,
) -> Result<IsolatedFrameWorld, String> {
    let result = client
        .send_command(
            "Page.createIsolatedWorld",
            Some(json!({
                "frameId": target.frame_id,
                "worldName": world_name,
                "grantUniveralAccess": true,
            })),
            Some(&target.session_id),
        )
        .await?;
    let context_id = result
        .get("executionContextId")
        .and_then(Value::as_i64)
        .ok_or_else(|| "Could not create isolated accessibility audit world".to_string())?;
    Ok(IsolatedFrameWorld {
        session_id: target.session_id.clone(),
        context_id,
    })
}

async fn collect_isolated_frame_contexts(
    client: &CdpClient,
    top_frame_id: &str,
    frame_targets: &[FrameTarget],
) -> Result<HashMap<String, IsolatedFrameWorld>, String> {
    let top_target = frame_targets
        .iter()
        .find(|target| target.frame_id == top_frame_id)
        .ok_or_else(|| "Could not determine top-level accessibility audit target".to_string())?;

    // The top frame must have an isolated world because every report is
    // finalized there. Child frames can detach while an audit starts; those
    // are represented by skipped partials without falling back to page-owned
    // JavaScript globals.
    let top_context = create_isolated_frame_context(client, top_target, AUDIT_WORLD_NAME).await?;
    let mut contexts = HashMap::from([(top_frame_id.to_string(), top_context)]);
    for target in frame_targets {
        if target.frame_id == top_frame_id {
            continue;
        }
        if let Ok(context) = create_isolated_frame_context(client, target, AUDIT_WORLD_NAME).await {
            contexts.insert(target.frame_id.clone(), context);
        }
    }
    Ok(contexts)
}

fn parse_audit_result(value: Value) -> Result<Value, String> {
    let serialized = value
        .as_str()
        .ok_or_else(|| "a11y returned non-string value".to_string())?;
    serde_json::from_str(serialized)
        .map_err(|error| format!("a11y returned invalid JSON: {}", error))
}

/// axe's frame merge consumes one partial per frame in tree order. A false
/// value preserves that position while telling the merge to skip the frame.
/// JSON null cannot be used because axe 4.12.1 dereferences each entry while
/// locating the report's environment data before it reaches its skip logic.
fn skipped_frame_partial() -> Value {
    Value::Bool(false)
}

enum AuditTask {
    Frame {
        frame_id: String,
        selector: Option<String>,
        frame_context: Option<Value>,
    },
    Skip,
}

/// Run `axe.runPartial` in top-to-bottom frame order, then combine those
/// serialized partials with `axe.finishRun`. Each partial runs in an isolated
/// world with axe's exact inherited frame context, avoiding both cross-frame
/// page messaging and page-owned JavaScript globals.
pub async fn run_audit(
    client: &CdpClient,
    top_session_id: &str,
    iframe_sessions: &HashMap<String, String>,
    tags: Option<&str>,
    selector: Option<&str>,
) -> Result<Value, String> {
    let (top_frame_id, frame_targets) =
        collect_frame_sessions(client, top_session_id, iframe_sessions).await?;
    let contexts = collect_isolated_frame_contexts(client, &top_frame_id, &frame_targets).await?;

    // axe.finishRun consumes partials in document preorder. Derive that order
    // from each partial's frame specs so it matches axe's exact selector scope
    // and DOM order instead of CDP target attachment or frame ID order.
    let mut partials = Vec::new();
    let mut tasks = vec![AuditTask::Frame {
        frame_id: top_frame_id.clone(),
        selector: selector.map(ToString::to_string),
        frame_context: None,
    }];
    while let Some(task) = tasks.pop() {
        let AuditTask::Frame {
            frame_id,
            selector,
            frame_context,
        } = task
        else {
            partials.push(skipped_frame_partial());
            continue;
        };
        let context = contexts
            .get(&frame_id)
            .map(|context| (context.session_id.as_str(), Some(context.context_id)));
        let Some((session_id, context_id)) = context else {
            partials.push(skipped_frame_partial());
            continue;
        };

        // runPartial still reports frame specs when iframe messaging is
        // disabled. We execute those descendants directly through CDP.
        let partial = evaluate(
            client,
            session_id,
            context_id,
            &partial_expression(tags, selector.as_deref(), frame_context.as_ref(), true),
        )
        .await
        .and_then(parse_audit_result);
        let audit_payload = match partial {
            Ok(payload) if payload.get("error").is_none() => payload,
            Ok(payload) if frame_id == top_frame_id && payload.get("error").is_some() => {
                return Ok(payload);
            }
            _ if frame_id == top_frame_id => {
                let value = evaluate(
                    client,
                    session_id,
                    context_id,
                    &run_expression(tags, selector.as_deref()),
                )
                .await?;
                return parse_audit_result(value);
            }
            _ => {
                partials.push(skipped_frame_partial());
                continue;
            }
        };
        let Some(partial) = audit_payload.get("partial").cloned() else {
            if frame_id == top_frame_id {
                return Err("a11y returned a partial result without audit data".to_string());
            }
            partials.push(skipped_frame_partial());
            continue;
        };

        let frame_specs = partial
            .get("frames")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let frame_contexts = audit_payload
            .get("frameContexts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        partials.push(partial);

        let mut child_tasks = Vec::with_capacity(frame_specs.len());
        for (index, frame_spec) in frame_specs.into_iter().enumerate() {
            let child_task = match frame_contexts.get(index).cloned() {
                Some(child_context) => {
                    match resolve_child_frame_id(client, session_id, context_id, &frame_spec).await
                    {
                        Ok(child_frame_id) if contexts.contains_key(&child_frame_id) => {
                            AuditTask::Frame {
                                frame_id: child_frame_id,
                                selector: None,
                                frame_context: Some(child_context),
                            }
                        }
                        _ => AuditTask::Skip,
                    }
                }
                None => AuditTask::Skip,
            };
            child_tasks.push(child_task);
        }
        for child_task in child_tasks.into_iter().rev() {
            tasks.push(child_task);
        }
    }

    let top_context = contexts
        .get(&top_frame_id)
        .ok_or_else(|| "Could not find isolated top-level audit world".to_string())?;
    let finished = evaluate(
        client,
        &top_context.session_id,
        Some(top_context.context_id),
        &finish_expression(&partials, tags, selector),
    )
    .await?;
    parse_audit_result(finished)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_axe_js_embedded() {
        assert!(AXE_JS.contains("axe"));
        assert!(AXE_JS.contains(&format!("axe.version=\"{}\"", AXE_VERSION)));
        assert!(AXE_JS.len() > 100_000);
    }

    #[test]
    fn test_run_expression_defaults() {
        let expr = run_expression(None, None);
        assert!(expr.contains("const module = { exports: {} }"));
        assert!(expr.contains("const define = undefined"));
        assert!(expr.contains("agentAxe = module.exports"));
        assert!(expr.contains("agentAxe.version !== \"4.12.1\""));
        assert!(expr.contains("const tags = []"));
        assert!(expr.contains("const selector = null"));
        assert!(expr.contains("target: n.target"));
    }

    #[test]
    fn test_run_expression_tags_and_selector() {
        let expr = run_expression(Some("wcag2a, wcag2aa"), Some("#main"));
        assert!(expr.contains(r#"["wcag2a","wcag2aa"]"#));
        assert!(expr.contains(r##"const selector = "#main""##));
    }

    #[test]
    fn test_run_expression_escapes_injected_values() {
        let expr = run_expression(None, Some("a\"; alert(1); //"));
        // The selector must arrive as a JSON string literal, not raw code.
        assert!(expr.contains(r#"const selector = "a\"; alert(1); //""#));
    }

    #[test]
    fn test_selector_check_guards_invalid_selectors() {
        // An invalid CSS selector makes document.querySelector throw. The audit
        // must catch it and return a clean 'Invalid selector' error instead of
        // leaking a raw evaluation exception (with isolated-world line numbers)
        // to the user. Both the top-frame and partial expressions guard it.
        for expr in [
            run_expression(None, Some("div::bogus(")),
            partial_expression(None, Some("div::bogus("), None, false),
        ] {
            assert!(expr.contains("try {"));
            assert!(expr.contains("'Invalid selector: ' + selector"));
            assert!(expr.contains("'No element matches selector: ' + selector"));
        }
    }

    #[test]
    fn test_partial_and_finish_expressions_use_private_axe() {
        let partial = partial_expression(
            Some("wcag2a"),
            None,
            Some(&json!({
                "include": [],
                "exclude": [],
                "initiator": false,
                "focusable": false,
                "size": { "width": 300, "height": 150 },
                "page": true
            })),
            false,
        );
        assert!(partial.contains("agentAxe.runPartial"));
        assert!(partial.contains("const module = { exports: {} }"));
        assert!(partial.contains(r#"["wcag2a"]"#));
        assert!(partial.contains("agentAxe.utils.getFrameContexts(context)"));
        assert!(partial.contains(r#""initiator":false"#));
        assert!(partial.contains("JSON.stringify({ partial, frameContexts })"));

        let finish = finish_expression(&[json!({ "results": [] })], Some("wcag2a"), None);
        assert!(finish.contains("agentAxe.finishRun"));
        assert!(finish.contains("const module = { exports: {} }"));
    }

    #[test]
    fn test_finish_expression_uses_false_for_skipped_frames() {
        let finish = finish_expression(&[skipped_frame_partial()], None, None);

        assert!(finish.contains("agentAxe.finishRun([false], options)"));
        assert!(!finish.contains("agentAxe.finishRun([null], options)"));
    }

    #[test]
    fn test_collect_frame_ids_recurses() {
        let tree = json!({
            "frame": { "id": "top" },
            "childFrames": [{
                "frame": { "id": "child" },
                "childFrames": [{ "frame": { "id": "grandchild" } }]
            }]
        });
        let mut frame_ids = Vec::new();

        collect_frame_ids(&tree, &mut frame_ids);

        assert_eq!(frame_ids, vec!["top", "child", "grandchild"]);
    }

    #[test]
    fn test_collect_frame_targets_inherits_nearest_ancestor_session() {
        let tree = json!({
            "frame": { "id": "top" },
            "childFrames": [{
                "frame": { "id": "oopif", "parentId": "top" },
                "childFrames": [{
                    "frame": { "id": "same-process-child", "parentId": "oopif" }
                }]
            }]
        });
        let iframe_sessions = HashMap::from([("oopif".to_string(), "oopif-session".to_string())]);
        let mut targets = HashMap::new();

        collect_frame_targets(&tree, "top-session", &iframe_sessions, &mut targets);

        assert_eq!(targets.len(), 3);
        assert_eq!(targets["top"].session_id, "top-session");
        assert_eq!(targets["oopif"].session_id, "oopif-session");
        assert_eq!(targets["same-process-child"].session_id, "oopif-session");
        assert_eq!(
            targets["same-process-child"].parent_id.as_deref(),
            Some("oopif")
        );
    }

    #[test]
    fn test_frame_reaches_top_filters_background_frames() {
        let targets = HashMap::from([
            ("top".to_string(), frame_target("top", "top-session", None)),
            (
                "active-child".to_string(),
                frame_target("active-child", "active-session", Some("top")),
            ),
            (
                "background".to_string(),
                frame_target("background", "background-session", None),
            ),
            (
                "background-child".to_string(),
                frame_target("background-child", "background-session", Some("background")),
            ),
        ]);

        assert!(frame_reaches_top("active-child", "top", &targets));
        assert!(!frame_reaches_top("background-child", "top", &targets));
    }

    #[test]
    fn test_frame_owner_expression_preserves_axe_selector_paths() {
        let expression = frame_owner_expression(&json!({
            "selector": ["#ancestor", ["#shadow-host", "iframe[data-name=\"quoted\"]"]]
        }))
        .unwrap();

        assert!(expression.contains(
            r##"const selectorPath = ["#ancestor",["#shadow-host","iframe[data-name=\"quoted\"]"]]"##
        ));
        assert!(expression.contains("root = element.shadowRoot"));
    }
}
