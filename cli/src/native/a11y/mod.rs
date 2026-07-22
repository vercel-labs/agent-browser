//! Accessibility auditing backed by a vendored copy of Deque's axe-core.
//!
//! `axe.min.js` is the unmodified upstream build (MPL-2.0 — see
//! LICENSE-axe-core.txt alongside it; the file-level license permits
//! bundling the unmodified source). The top-level audit captures this exact
//! build through a private CommonJS export in every frame. Serialized partial
//! results are merged outside axe's cross-frame messaging, so page-owned
//! `window.axe` values remain intact.

use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

use super::cdp::client::CdpClient;
use super::cdp::types::EvaluateResult;

/// Unmodified axe-core build, injected via `Runtime.evaluate` (which is
/// not subject to the page's CSP, unlike a CDN `<script>` tag).
pub const AXE_JS: &str = include_str!("axe.min.js");

/// Version of the vendored axe-core build. Keep this in sync with `axe.min.js`.
pub const AXE_VERSION: &str = "4.12.1";

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
    // The vendored UMD build exports through this lexical CommonJS module.
    // Capturing that export avoids trusting a page-owned `window.axe` value.
    const module = {{ exports: {{}} }};
    {axe_js}
    agentAxe = module.exports;
  }} finally {{
    // axe-core also assigns window.axe in browsers. Restore the page exactly
    // after capturing our private export so the audit has no lasting global.
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
  if (selector !== null && !document.querySelector(selector)) {{
    return JSON.stringify({{ error: 'No element matches selector: ' + selector }});
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

fn partial_expression(tags: Option<&str>, selector: Option<&str>, disable_iframes: bool) -> String {
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
  if (!agentAxe || agentAxe.version !== {axe_version_json} || typeof agentAxe.runPartial !== 'function') {{
    return JSON.stringify({{ error: 'Failed to initialize vendored axe-core {axe_version}' }});
  }}
  const tags = {tags_json};
  const selector = {selector_json};
  if (selector !== null && !document.querySelector(selector)) {{
    return JSON.stringify({{ error: 'No element matches selector: ' + selector }});
  }}
  const options = {{ resultTypes: ['violations', 'incomplete'] }};
  {iframes_option}
  if (tags.length > 0) options.runOnly = {{ type: 'tag', values: tags }};
  return agentAxe.runPartial(selector === null ? document : selector, options)
    .then((result) => JSON.stringify(result));
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
    depth: usize,
}

struct SessionFrameTree {
    session_id: String,
    parent_id: String,
    tree: Value,
}

fn collect_frame_targets(
    tree: &Value,
    depth: usize,
    parent_session_id: &str,
    iframe_sessions: &HashMap<String, String>,
    targets: &mut Vec<FrameTarget>,
) {
    let session_id = if let Some(frame_id) = tree
        .get("frame")
        .and_then(|frame| frame.get("id"))
        .and_then(|id| id.as_str())
    {
        // Same-process child frames do not have their own target session. They
        // execute in the nearest ancestor target, which may itself be an
        // out-of-process iframe rather than the top-level page.
        let session_id = iframe_sessions
            .get(frame_id)
            .cloned()
            .unwrap_or_else(|| parent_session_id.to_string());
        targets.push(FrameTarget {
            frame_id: frame_id.to_string(),
            session_id: session_id.clone(),
            depth,
        });
        session_id
    } else {
        parent_session_id.to_string()
    };
    if let Some(children) = tree.get("childFrames").and_then(|value| value.as_array()) {
        for child in children {
            collect_frame_targets(child, depth + 1, &session_id, iframe_sessions, targets);
        }
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

    let mut targets = Vec::new();
    if let Some(tree) = top_tree.get("frameTree") {
        collect_frame_targets(tree, 0, top_session_id, iframe_sessions, &mut targets);
    }

    // Page.getFrameTree on the top session can omit OOPIF subtrees entirely.
    // Query each attached iframe session so same-process descendants within
    // that target are included and inherit the correct session.
    let mut session_entries: Vec<_> = iframe_sessions.iter().collect();
    session_entries.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut subtrees = Vec::new();
    for (frame_id, session_id) in session_entries {
        if targets.iter().any(|target| target.frame_id == *frame_id) {
            continue;
        }
        let Some(tree) = client
            .send_command_no_params("Page.getFrameTree", Some(session_id))
            .await
            .ok()
            .and_then(|result| result.get("frameTree").cloned())
        else {
            continue;
        };
        let Some(parent_id) = tree
            .get("frame")
            .and_then(|frame| frame.get("parentId"))
            .and_then(|id| id.as_str())
            .map(ToString::to_string)
        else {
            continue;
        };
        subtrees.push(SessionFrameTree {
            session_id: session_id.clone(),
            parent_id,
            tree,
        });
    }

    // Parents must precede descendants because missing frame contexts use
    // depth to skip only that frame's subtree. Chrome does not guarantee the
    // attachment event order, so resolve the hierarchy from parentId.
    while !subtrees.is_empty() {
        let mut progressed = false;
        let mut index = 0;
        while index < subtrees.len() {
            let Some(parent_depth) = targets
                .iter()
                .find(|target| target.frame_id == subtrees[index].parent_id)
                .map(|target| target.depth)
            else {
                index += 1;
                continue;
            };

            let subtree = subtrees.remove(index);
            let depth = parent_depth + 1;
            let mut subtree_targets = Vec::new();
            collect_frame_targets(
                &subtree.tree,
                depth,
                &subtree.session_id,
                iframe_sessions,
                &mut subtree_targets,
            );
            for target in subtree_targets {
                if !targets
                    .iter()
                    .any(|existing| existing.frame_id == target.frame_id)
                {
                    targets.push(target);
                }
            }
            progressed = true;
        }

        if !progressed {
            // Remaining sessions belong to background tabs or raced a parent
            // attachment. Neither can be merged safely into this page's audit.
            break;
        }
    }

    Ok((top_frame_id, targets))
}

#[derive(Debug)]
struct FrameContext {
    session_id: String,
    context_id: i64,
}

async fn collect_default_frame_contexts(
    client: &CdpClient,
    top_session_id: &str,
    frame_targets: &[FrameTarget],
) -> Result<HashMap<String, FrameContext>, String> {
    let mut events = client.subscribe();
    let unique_sessions: HashSet<String> = frame_targets
        .iter()
        .map(|target| target.session_id.clone())
        .collect();

    // Re-enabling Runtime makes Chrome report every existing execution
    // context, including same-process child frames that have no target session.
    for session_id in &unique_sessions {
        let _ = client
            .send_command_no_params("Runtime.disable", Some(session_id))
            .await;
        client
            .send_command_no_params("Runtime.enable", Some(session_id))
            .await?;
    }

    let mut contexts: HashMap<String, FrameContext> = HashMap::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    while tokio::time::Instant::now() < deadline && contexts.len() < frame_targets.len() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let event = match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Ok(event)) => event,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) | Err(_) => break,
        };
        if event.method != "Runtime.executionContextCreated" {
            continue;
        }
        let Some(context) = event.params.get("context") else {
            continue;
        };
        let Some(context_id) = context.get("id").and_then(|id| id.as_i64()) else {
            continue;
        };
        let Some(aux_data) = context.get("auxData") else {
            continue;
        };
        if aux_data.get("isDefault").and_then(|value| value.as_bool()) != Some(true) {
            continue;
        }
        let Some(frame_id) = aux_data.get("frameId").and_then(|id| id.as_str()) else {
            continue;
        };
        let Some(expected_session_id) = frame_targets
            .iter()
            .find(|target| target.frame_id == frame_id)
            .map(|target| target.session_id.as_str())
        else {
            continue;
        };
        let event_session_id = event.session_id.as_deref().unwrap_or(top_session_id);
        if event_session_id != expected_session_id {
            continue;
        }
        contexts.insert(
            frame_id.to_string(),
            FrameContext {
                session_id: expected_session_id.to_string(),
                context_id,
            },
        );
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

/// Run `axe.runPartial` in top-to-bottom frame order, then combine those
/// serialized partials with `axe.finishRun`. This avoids cross-frame page
/// messaging and keeps every frame's page-owned `window.axe` value intact.
pub async fn run_audit(
    client: &CdpClient,
    top_session_id: &str,
    iframe_sessions: &HashMap<String, String>,
    tags: Option<&str>,
    selector: Option<&str>,
) -> Result<Value, String> {
    // A selector scopes the current document only. Disable axe's frame walk so
    // finishRun does not expect partials for frames outside that subtree.
    if selector.is_some() {
        let partial = evaluate(
            client,
            top_session_id,
            None,
            &partial_expression(tags, selector, true),
        )
        .await
        .and_then(parse_audit_result)?;
        if partial.get("error").is_some() {
            return Ok(partial);
        }
        let finished = evaluate(
            client,
            top_session_id,
            None,
            &finish_expression(&[partial], tags, selector),
        )
        .await?;
        return parse_audit_result(finished);
    }

    let (top_frame_id, frame_targets) =
        match collect_frame_sessions(client, top_session_id, iframe_sessions).await {
            Ok(frame_data) => frame_data,
            Err(_) => {
                let value = evaluate(
                    client,
                    top_session_id,
                    None,
                    &run_expression(tags, selector),
                )
                .await?;
                return parse_audit_result(value);
            }
        };
    let contexts = collect_default_frame_contexts(client, top_session_id, &frame_targets)
        .await
        .unwrap_or_default();

    let mut partials = Vec::new();
    let mut skipped_descendant_depth = None;
    for target in frame_targets {
        if let Some(skipped_depth) = skipped_descendant_depth {
            if target.depth > skipped_depth {
                continue;
            }
            skipped_descendant_depth = None;
        }

        let context = if target.frame_id == top_frame_id {
            Some((top_session_id, None))
        } else {
            contexts
                .get(&target.frame_id)
                .map(|context| (context.session_id.as_str(), Some(context.context_id)))
        };
        let Some((session_id, context_id)) = context else {
            partials.push(Value::Null);
            skipped_descendant_depth = Some(target.depth);
            continue;
        };
        let partial = evaluate(
            client,
            session_id,
            context_id,
            &partial_expression(tags, None, false),
        )
        .await
        .and_then(parse_audit_result);
        match partial {
            Ok(partial) if partial.get("error").is_none() => partials.push(partial),
            _ if target.frame_id == top_frame_id => {
                let value = evaluate(
                    client,
                    top_session_id,
                    None,
                    &run_expression(tags, selector),
                )
                .await?;
                return parse_audit_result(value);
            }
            _ => {
                partials.push(Value::Null);
                skipped_descendant_depth = Some(target.depth);
            }
        }
    }

    let finished = evaluate(
        client,
        top_session_id,
        None,
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
    fn test_partial_and_finish_expressions_use_private_axe() {
        let partial = partial_expression(Some("wcag2a"), None, false);
        assert!(partial.contains("agentAxe.runPartial"));
        assert!(partial.contains("const module = { exports: {} }"));
        assert!(partial.contains(r#"["wcag2a"]"#));

        let finish = finish_expression(&[json!({ "results": [] })], Some("wcag2a"), None);
        assert!(finish.contains("agentAxe.finishRun"));
        assert!(finish.contains("const module = { exports: {} }"));
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
                "frame": { "id": "oopif" },
                "childFrames": [{ "frame": { "id": "same-process-child" } }]
            }]
        });
        let iframe_sessions = HashMap::from([("oopif".to_string(), "oopif-session".to_string())]);
        let mut targets = Vec::new();

        collect_frame_targets(&tree, 0, "top-session", &iframe_sessions, &mut targets);

        assert_eq!(targets.len(), 3);
        assert_eq!(targets[0].session_id, "top-session");
        assert_eq!(targets[1].session_id, "oopif-session");
        assert_eq!(targets[2].session_id, "oopif-session");
    }
}
