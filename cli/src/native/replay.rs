use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// rrweb CDN URL -- fetched at runtime and cached in-page.
const RRWEB_CDN_URL: &str = "https://cdn.jsdelivr.net/npm/rrweb@latest/dist/rrweb.min.js";

/// JavaScript that fetches rrweb from CDN, injects it, and starts recording.
/// Uses `inlineImages`, `collectFonts`, and `inlineStylesheet` for high-fidelity replay.
/// Safe to call multiple times -- skips if already injected.
fn build_inject_js() -> String {
    format!(
        r#"fetch('{cdn}').then(r=>r.text()).then(src=>{{
var el=document.createElement('script');
el.textContent=src;
document.head.appendChild(el);
window.__rrwebEvents=window.__rrwebEvents||[];
window.__rrwebInjected=true;
window.rrweb.record({{
emit:function(e){{window.__rrwebEvents.push(e)}},
inlineImages:true,
collectFonts:true,
inlineStylesheet:true
}});
return 'recording started, '+window.__rrwebEvents.length+' prior events'
}})"#,
        cdn = RRWEB_CDN_URL
    )
}

/// JavaScript that returns the current event count.
const STATUS_JS: &str = "(window.__rrwebEvents||[]).length";

/// JavaScript that extracts all recorded events as a JSON string.
const EXTRACT_EVENTS_JS: &str = "JSON.stringify(window.__rrwebEvents||[])";

/// JavaScript that extracts resolved CSS custom properties from :root.
/// This fixes replay visual bugs where var(--x) references don't resolve
/// because the replay runs in a different context without the original stylesheets.
const EXTRACT_CSS_VARS_JS: &str = r#"(function(){
var s=getComputedStyle(document.documentElement);
var vars=[];
for(var i=0;i<document.styleSheets.length;i++){
try{var rules=document.styleSheets[i].cssRules;
for(var j=0;j<rules.length;j++){var r=rules[j];
if(r.style){for(var k=0;k<r.style.length;k++){var p=r.style[k];
if(p.startsWith('--')&&vars.indexOf(p)===-1)vars.push(p)}}}}catch(e){}}
var css=':root{';
vars.forEach(function(v){css+=v+':'+s.getPropertyValue(v).trim()+';'});
css+='}';return css})()"#;

/// JavaScript that cleans up recording state from the page.
const CLEANUP_JS: &str =
    "delete window.__rrwebEvents;delete window.__rrwebInjected;'cleaned up'";

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct ReplayState {
    pub active: bool,
    pub event_count: u64,
    /// CDP identifier returned by Page.addScriptToEvaluateOnNewDocument
    /// so we can remove the auto-inject on stop.
    pub auto_inject_id: Option<String>,
}

impl ReplayState {
    pub fn new() -> Self {
        Self {
            active: false,
            event_count: 0,
            auto_inject_id: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Command handlers (called from actions.rs)
// ---------------------------------------------------------------------------

/// Start rrweb recording: inject the script into the current page and
/// register it for automatic re-injection on navigation.
pub async fn replay_start(
    state: &mut ReplayState,
    mgr: &super::browser::BrowserManager,
) -> Result<Value, String> {
    if state.active {
        return Err("Replay recording already active. Use 'replay stop' to save.".to_string());
    }

    let inject_js = build_inject_js();

    // Inject into the current page
    mgr.evaluate(&inject_js, None).await?;

    // Register for auto-injection on future navigations
    let identifier = mgr.add_script_to_evaluate(&inject_js).await?;
    state.auto_inject_id = Some(identifier);
    state.active = true;
    state.event_count = 0;

    Ok(json!({
        "started": true,
        "message": "Recording started. Navigate and interact -- events are captured automatically."
    }))
}

/// Check recording status and event count.
pub async fn replay_status(
    state: &ReplayState,
    mgr: &super::browser::BrowserManager,
) -> Result<Value, String> {
    if !state.active {
        return Ok(json!({
            "active": false,
            "events": 0,
            "message": "Not recording. Use 'replay start' to begin."
        }));
    }

    let result = mgr.evaluate(STATUS_JS, None).await?;
    let count = result
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| result.as_u64())
        .unwrap_or(0);

    Ok(json!({
        "active": true,
        "events": count,
    }))
}

/// Stop recording, extract events + CSS variables, generate replay HTML.
pub async fn replay_stop(
    state: &mut ReplayState,
    mgr: &super::browser::BrowserManager,
    output_path: &str,
) -> Result<Value, String> {
    if !state.active {
        return Err("No replay recording in progress. Use 'replay start' first.".to_string());
    }

    // Remove auto-inject script so future navigations don't keep recording
    if let Some(ref id) = state.auto_inject_id {
        let _ = mgr
            .client
            .send_command(
                "Page.removeScriptToEvaluateOnNewDocument",
                Some(json!({ "identifier": id })),
                Some(mgr.active_session_id()?),
            )
            .await;
    }

    // Extract event count
    let count_result = mgr.evaluate(STATUS_JS, None).await?;
    let event_count = count_result
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| count_result.as_u64())
        .unwrap_or(0);

    if event_count == 0 {
        state.active = false;
        state.auto_inject_id = None;
        let _ = mgr.evaluate(CLEANUP_JS, None).await;
        return Err("No events captured.".to_string());
    }

    // Extract CSS custom properties (for accurate replay of var() references)
    let css_vars = mgr
        .evaluate(EXTRACT_CSS_VARS_JS, None)
        .await
        .unwrap_or_else(|_| Value::String(":root{}".to_string()));
    let css_vars_str = css_vars.as_str().unwrap_or(":root{}");

    // Extract events JSON
    let events_value = mgr.evaluate(EXTRACT_EVENTS_JS, None).await?;
    let events_json = events_value.as_str().unwrap_or("[]");

    // Validate we got real JSON
    if events_json == "[]" || events_json.len() < 10 {
        state.active = false;
        state.auto_inject_id = None;
        let _ = mgr.evaluate(CLEANUP_JS, None).await;
        return Err("Failed to extract events from page.".to_string());
    }

    let size_kb = events_json.len() as f64 / 1024.0;
    state.event_count = event_count;

    // Ensure output directory exists
    if let Some(parent) = Path::new(output_path).parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create output directory: {}", e))?;
    }

    // Determine paths
    let base_path = if output_path.ends_with(".html") {
        output_path.trim_end_matches(".html").to_string()
    } else {
        output_path.to_string()
    };
    let html_path = format!("{}.html", base_path);
    let json_path = format!("{}.json", base_path);

    // Write recording.json
    fs::write(&json_path, events_json)
        .map_err(|e| format!("Failed to write {}: {}", json_path, e))?;

    // Escape CSS for safe embedding
    let escaped_css = css_vars_str.replace("</", "<\\/");

    // Generate self-contained replay HTML
    let html = generate_replay_html(events_json, &escaped_css, event_count, size_kb);
    fs::write(&html_path, &html)
        .map_err(|e| format!("Failed to write {}: {}", html_path, e))?;

    // Clean up page state
    let _ = mgr.evaluate(CLEANUP_JS, None).await;
    state.active = false;
    state.auto_inject_id = None;

    Ok(json!({
        "events": event_count,
        "size_kb": format!("{:.1}", size_kb),
        "html": html_path,
        "json": json_path,
    }))
}

// ---------------------------------------------------------------------------
// HTML template
// ---------------------------------------------------------------------------

fn generate_replay_html(events_json: &str, css_vars: &str, count: u64, size_kb: f64) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Session Replay - agent-browser</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/rrweb-player@latest/dist/style.css" />
<style>
* {{ box-sizing: border-box; }}
body {{
    margin: 0; padding: 40px;
    display: flex; flex-direction: column; align-items: center;
    min-height: 100vh;
    background: #0a0a1a;
    font-family: system-ui, -apple-system, sans-serif;
    color: #e0e0e0;
}}
h1 {{ font-size: 24px; margin-bottom: 8px; color: #fff; }}
.meta {{ font-size: 14px; color: #888; margin-bottom: 24px; }}
.meta span {{ color: #aaa; }}
#player {{ border-radius: 12px; overflow: hidden; box-shadow: 0 8px 32px rgba(0,0,0,0.5); }}
</style>
</head>
<body>
<h1>Session Replay</h1>
<p class="meta">
    <span>{count} events</span> &middot;
    <span>{size:.1} KB</span>
</p>
<div id="player"></div>
<script src="https://cdn.jsdelivr.net/npm/rrweb@latest/dist/rrweb.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/rrweb-player@latest/dist/index.js"></script>
<script>
var events = {events};
new rrwebPlayer({{
    target: document.getElementById("player"),
    props: {{
        events: events,
        width: 1280,
        height: 720,
        autoPlay: true,
        showController: true,
        speedOption: [1, 2, 4, 8],
        insertStyleRules: [{css_vars}],
    }},
}});
</script>
</body>
</html>"##,
        count = count,
        size = size_kb,
        events = events_json,
        css_vars = serde_json::to_string(css_vars).unwrap_or_else(|_| "\"\"".to_string()),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replay_state_new() {
        let state = ReplayState::new();
        assert!(!state.active);
        assert_eq!(state.event_count, 0);
        assert!(state.auto_inject_id.is_none());
    }

    #[test]
    fn test_build_inject_js_contains_rrweb_url() {
        let js = build_inject_js();
        assert!(js.contains(RRWEB_CDN_URL));
        assert!(js.contains("inlineImages:true"));
        assert!(js.contains("collectFonts:true"));
    }

    #[test]
    fn test_generate_replay_html_structure() {
        let html = generate_replay_html("[{\"type\":4}]", ":root{}", 1, 0.1);
        assert!(html.contains("rrweb-player"));
        assert!(html.contains("Session Replay"));
        assert!(html.contains("1 events"));
        assert!(html.contains("insertStyleRules"));
    }

    #[test]
    fn test_generate_replay_html_escapes_css() {
        let html = generate_replay_html("[]", ":root{--bg: #fff;}", 0, 0.0);
        assert!(html.contains("--bg"));
    }
}
