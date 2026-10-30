use serde_json::{json, Value};
use std::fs;
use std::path::Path;

/// rrweb CDN URL -- fetched at runtime and cached in-page.
const RRWEB_CDN_URL: &str = "https://cdn.jsdelivr.net/npm/rrweb@latest/dist/rrweb.min.js";

/// Maximum gap (ms) between events before compression.
/// Gaps larger than this are compressed down to this value, removing
/// idle periods caused by navigation redirects, network waits, etc.
const MAX_EVENT_GAP_MS: i64 = 200;

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

    // Compress gaps: remove idle periods caused by navigation, network waits, etc.
    // This prevents the rrweb-player timeline from showing grey inactive zones
    // that block the UI controls (a known rrweb-player bug).
    let compressed_json = compress_event_gaps(events_json);
    let final_events = compressed_json.as_deref().unwrap_or(events_json);

    let size_kb = final_events.len() as f64 / 1024.0;
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

    // Write recording.json (raw, uncompressed for programmatic use)
    fs::write(&json_path, events_json)
        .map_err(|e| format!("Failed to write {}: {}", json_path, e))?;

    // Escape </script> sequences to prevent broken HTML
    let escaped_events = final_events.replace("</script>", "<\\/script>");
    let escaped_css = css_vars_str.replace("</", "<\\/");

    // Generate self-contained replay HTML
    let html = generate_replay_html(&escaped_events, &escaped_css, event_count, size_kb);
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
// Gap compression
// ---------------------------------------------------------------------------

/// Compress gaps between events so the replay has no idle periods.
/// This prevents the rrweb-player timeline from showing grey inactive zones.
/// Returns None if parsing fails (caller uses original JSON).
fn compress_event_gaps(events_json: &str) -> Option<String> {
    let mut events: Vec<Value> = serde_json::from_str(events_json).ok()?;
    if events.len() < 2 {
        return None;
    }

    let mut time_saved: i64 = 0;
    for i in 1..events.len() {
        let prev_ts = events[i - 1].get("timestamp")?.as_i64()?;
        let curr_ts = events[i].get("timestamp")?.as_i64()?;
        let gap = curr_ts - prev_ts;

        if gap > MAX_EVENT_GAP_MS {
            time_saved += gap - MAX_EVENT_GAP_MS;
        }

        let new_ts = curr_ts - time_saved;
        events[i]["timestamp"] = json!(new_ts);
    }

    serde_json::to_string(&events).ok()
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
    margin: 0; padding: 24px;
    display: flex; flex-direction: column; align-items: center;
    min-height: 100vh;
    background: #0a0a1a;
    font-family: system-ui, -apple-system, sans-serif;
    color: #e0e0e0;
}}
h1 {{ font-size: 20px; margin-bottom: 4px; color: #fff; }}
.meta {{ font-size: 13px; color: #888; margin-bottom: 16px; }}
.meta span {{ color: #aaa; }}
#player {{ border-radius: 12px; overflow: hidden; box-shadow: 0 8px 32px rgba(0,0,0,0.5); }}
.actions {{ margin-top: 12px; display: flex; gap: 12px; align-items: center; }}
.actions button {{
    padding: 6px 14px; border-radius: 8px; border: 1px solid #333;
    background: #1a1a2e; color: #e0e0e0; font-size: 13px; cursor: pointer;
    transition: background 0.2s;
}}
.actions button:hover {{ background: #2a2a3e; }}
.actions button:disabled {{ opacity: 0.5; cursor: not-allowed; }}
.actions .hint {{ font-size: 12px; color: #555; }}
</style>
</head>
<body>
<h1>Session Replay</h1>
<p class="meta">
    <span>{count} events</span> &middot;
    <span>{size:.1} KB</span>
</p>
<div id="player"></div>
<div class="actions">
    <button id="exportBtn" onclick="exportVideo(this)">Record this tab as video</button>
    <span class="hint" id="exportHint"></span>
</div>
<script src="https://cdn.jsdelivr.net/npm/rrweb@latest/dist/rrweb.min.js"></script>
<script src="https://cdn.jsdelivr.net/npm/rrweb-player@latest/dist/index.js"></script>
<script>
var events = {events};
var player = new rrwebPlayer({{
    target: document.getElementById("player"),
    props: {{
        events: events,
        width: 1280,
        height: 720,
        autoPlay: false,
        showController: true,
        speedOption: [1, 2, 4, 8],
        insertStyleRules: [{css_vars}],
    }},
}});

// Record this tab as video using Screen Capture API.
// Auto-stops when the replay finishes and downloads the WebM file.
async function exportVideo(btn) {{
    var hint = document.getElementById("exportHint");
    try {{
        var stream = await navigator.mediaDevices.getDisplayMedia({{
            video: {{ displaySurface: "browser" }},
            audio: false,
            preferCurrentTab: true,
        }});

        btn.disabled = true;
        btn.textContent = "Recording...";
        hint.textContent = "Auto-stops when replay finishes";

        var recorder = new MediaRecorder(stream, {{
            mimeType: "video/webm;codecs=vp9",
            videoBitsPerSecond: 8000000,
        }});
        var chunks = [];
        recorder.ondataavailable = function(e) {{ if (e.data.size > 0) chunks.push(e.data); }};
        recorder.onstop = function() {{
            stream.getTracks().forEach(function(t) {{ t.stop(); }});
            var blob = new Blob(chunks, {{ type: "video/webm" }});
            var url = URL.createObjectURL(blob);
            var a = document.createElement("a");
            a.href = url;
            a.download = "session-replay.webm";
            a.click();
            URL.revokeObjectURL(url);
            btn.disabled = false;
            btn.textContent = "Record this tab as video";
            hint.textContent = "Done! Video downloaded.";
        }};

        stream.getVideoTracks()[0].onended = function() {{
            if (recorder.state === "recording") recorder.stop();
        }};

        recorder.start();
        player.goto(0);
        player.play();

        // Auto-stop when replay reaches the end
        var replayer = player.getReplayer();
        var checkInterval = setInterval(function() {{
            if (!replayer || !replayer.service) return;
            var meta = replayer.getMetaData();
            var current = replayer.getCurrentTime();
            if (current >= meta.totalTime - 200) {{
                clearInterval(checkInterval);
                setTimeout(function() {{
                    if (recorder.state === "recording") recorder.stop();
                }}, 500);
            }}
        }}, 200);
    }} catch(e) {{
        btn.disabled = false;
        hint.textContent = "";
        if (e.name !== "NotAllowedError") alert("Export failed: " + e.message);
    }}
}}
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
        assert!(html.contains("Record this tab as video"));
    }

    #[test]
    fn test_generate_replay_html_escapes_css() {
        let html = generate_replay_html("[]", ":root{--bg: #fff;}", 0, 0.0);
        assert!(html.contains("--bg"));
    }

    #[test]
    fn test_compress_event_gaps_removes_idle() {
        let events = json!([
            {"type": 4, "timestamp": 1000},
            {"type": 2, "timestamp": 1050},
            {"type": 3, "timestamp": 5000},  // 3950ms gap -> compressed
            {"type": 3, "timestamp": 5100},
        ]);
        let json_str = serde_json::to_string(&events).unwrap();
        let compressed = compress_event_gaps(&json_str).unwrap();
        let result: Vec<Value> = serde_json::from_str(&compressed).unwrap();

        // Gap between event 1 (1050) and event 2 should be MAX_EVENT_GAP_MS, not 3950
        let ts2 = result[2]["timestamp"].as_i64().unwrap();
        let ts1 = result[1]["timestamp"].as_i64().unwrap();
        assert_eq!(ts2 - ts1, MAX_EVENT_GAP_MS);
    }

    #[test]
    fn test_compress_event_gaps_preserves_small_gaps() {
        let events = json!([
            {"type": 4, "timestamp": 1000},
            {"type": 3, "timestamp": 1050},
            {"type": 3, "timestamp": 1100},
        ]);
        let json_str = serde_json::to_string(&events).unwrap();
        let compressed = compress_event_gaps(&json_str).unwrap();
        // Small gaps should be unchanged
        assert_eq!(json_str, compressed);
    }
}
