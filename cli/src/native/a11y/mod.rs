//! Accessibility auditing backed by a vendored copy of Deque's axe-core.
//!
//! `axe.min.js` is the unmodified upstream build (MPL-2.0 — see
//! LICENSE-axe-core.txt alongside it; the file-level license permits
//! bundling the unmodified source). Each audit evaluates this exact build
//! through a local CommonJS export so page-owned `window.axe` values cannot
//! replace the audit engine.

use serde_json::json;

/// Unmodified axe-core build, injected via `Runtime.evaluate` (which is
/// not subject to the page's CSP, unlike a CDN `<script>` tag).
pub const AXE_JS: &str = include_str!("axe.min.js");

/// Version of the vendored axe-core build. Keep this in sync with `axe.min.js`.
pub const AXE_VERSION: &str = "4.12.1";

/// Build the `axe.run()` expression. `tags` is a comma-separated list of
/// axe rule tags (e.g. "wcag2a,wcag2aa"); `selector` scopes the audit to
/// a CSS subtree. Results are trimmed to what an agent needs to locate
/// and fix each issue — full pass/inapplicable node lists stay in the
/// browser.
pub fn run_expression(tags: Option<&str>, selector: Option<&str>) -> String {
    let tag_values: Vec<&str> = tags
        .map(|t| {
            t.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    // JSON-encode injected values so selectors/tags can't break out of the
    // script.
    let tags_json = json!(tag_values).to_string();
    let selector_json = json!(selector).to_string();
    let axe_js = AXE_JS;
    let axe_version_json = json!(AXE_VERSION).to_string();
    format!(
        r#"(() => {{
  const previousAxe = Object.getOwnPropertyDescriptor(window, 'axe');
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
  }}
  if (!agentAxe || agentAxe.version !== {axe_version_json} || typeof agentAxe.run !== 'function') {{
    return JSON.stringify({{ error: 'Failed to initialize vendored axe-core {axe_version}' }});
  }}
  const tags = {tags_json};
  const selector = {selector_json};
  if (selector !== null && !document.querySelector(selector)) {{
    return JSON.stringify({{ error: 'No element matches selector: ' + selector }});
  }}
  const options = {{ resultTypes: ['violations', 'incomplete'] }};
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
  return agentAxe.run(selector === null ? document : selector, options).then((r) => JSON.stringify({{
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
}
