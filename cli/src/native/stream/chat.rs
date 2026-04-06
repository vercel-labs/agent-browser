use std::sync::OnceLock;

use serde_json::{json, Value};

use tokio::io::AsyncWriteExt;

use super::http::cors_headers_for_origin;

pub(crate) const DEFAULT_AI_GATEWAY_URL: &str = "https://ai-gateway.vercel.sh";

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub(crate) fn http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(reqwest::Client::new)
}

pub(crate) fn is_chat_enabled() -> bool {
    std::env::var("AI_GATEWAY_API_KEY").is_ok()
}

pub(super) fn chat_status_json() -> String {
    let enabled = is_chat_enabled();
    let mut obj = json!({ "enabled": enabled });
    if enabled {
        if let Ok(model) = std::env::var("AI_GATEWAY_MODEL") {
            obj["model"] = Value::String(model);
        }
    }
    obj.to_string()
}

pub(super) async fn handle_models_request(
    stream: &mut tokio::net::TcpStream,
    origin: Option<&str>,
) {
    let cors = cors_headers_for_origin(origin);
    let gateway_url = std::env::var("AI_GATEWAY_URL")
        .unwrap_or_else(|_| DEFAULT_AI_GATEWAY_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let api_key = match std::env::var("AI_GATEWAY_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            let body = r#"{"data":[]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{cors}\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
            return;
        }
    };

    let url = format!("{}/v1/models", gateway_url);
    let client = http_client();
    let result = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await;

    let body = match result {
        Ok(r) if r.status().is_success() => r
            .text()
            .await
            .unwrap_or_else(|_| r#"{"data":[]}"#.to_string()),
        _ => r#"{"data":[]}"#.to_string(),
    };

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{cors}\r\n",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
}

const SKILL_NAMES: &[&str] = &["agent-browser", "slack", "electron", "dogfood", "agentcore"];

/// Locate the `skills/` directory by walking up from the executable.
/// Works for npm installs (binary in `bin/`, skills at `../skills/`) and
/// dev builds (binary deep in `cli/target/`, skills at repo root).
fn find_skills_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let real = exe.canonicalize().unwrap_or(exe);
    let mut dir = real.parent();
    while let Some(d) = dir {
        let candidate = d.join("skills");
        if candidate.join("agent-browser").join("SKILL.md").exists() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}

fn load_skills() -> Vec<(String, String)> {
    let Some(skills_dir) = find_skills_dir() else {
        return Vec::new();
    };
    SKILL_NAMES
        .iter()
        .filter_map(|name| {
            let path = skills_dir.join(name).join("SKILL.md");
            let content = std::fs::read_to_string(&path).ok()?;
            Some((name.to_string(), content))
        })
        .collect()
}

fn strip_frontmatter(s: &str) -> &str {
    if !s.starts_with("---") {
        return s;
    }
    if let Some(end) = s[3..].find("---") {
        let after = &s[3 + end + 3..];
        after.trim_start_matches(['\n', '\r'])
    } else {
        s
    }
}

pub(crate) fn get_system_prompt() -> &'static str {
    static PROMPT: OnceLock<String> = OnceLock::new();
    PROMPT.get_or_init(|| {
        let skills = load_skills();

        let mut sections = String::new();
        for (name, content) in &skills {
            let body = strip_frontmatter(content);
            sections.push_str(&format!("\n\n<skill name=\"{}\">\n{}\n</skill>", name, body.trim()));
        }

        format!(
            r#"You are an AI assistant that controls a browser through agent-browser. You have an active browser session, but you can also create new sessions.

RULES:
- You MUST use the agent_browser tool for every browser action. NEVER claim you performed an action without calling the tool.
- If the user asks you to do something, call the tool first, then describe the result.
- If a request is outside your capabilities (e.g. system operations), say so honestly. Do not improvise or pretend.
- One tool call per command. Do not chain with `&&` or `;`.
- Do not add `--json`.
- Do not run non-agent-browser programs.
- Keep responses concise.
- For screenshots, omit the path argument so they save to the default location (which will be displayed inline). Screenshots from tool calls are ALREADY shown to the user. Do NOT re-display them with markdown image syntax in your text response. Never use `![...]()` to reference screenshots.
- To create a new session: add `--session <name>` to any command (e.g. `agent-browser --session my-session open https://example.com`). If the session does not exist, it will be created automatically.
- To use a different browser engine: add `--engine <engine>` (e.g. `agent-browser --session lp-session --engine lightpanda open https://example.com`). Supported engines: chrome (default), lightpanda.

The following skill references describe agent-browser capabilities in detail. Use them when deciding which commands to run and how to approach tasks.
{sections}"#,
        )
    })
}

pub(crate) const CHAT_TOOLS: &str = r#"[{"type":"function","function":{"name":"agent_browser","description":"Execute an agent-browser command. Runs against the active session by default. Add --session <name> to target or create a different session, and --engine <engine> to choose a browser engine.","parameters":{"type":"object","properties":{"command":{"type":"string","description":"The command to execute, e.g. 'agent-browser open https://google.com' or 'agent-browser --session new-session open https://example.com' or 'agent-browser snapshot -i' or 'agent-browser click @e3'"}},"required":["command"]}}}]"#;

pub(crate) const COMPACT_THRESHOLD_CHARS: usize = 200_000;
pub(crate) const KEEP_RECENT_MESSAGES: usize = 6;

pub(crate) fn estimate_chars(messages: &[Value]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_len = m
                .get("content")
                .map(|c| {
                    if let Some(s) = c.as_str() {
                        s.len()
                    } else {
                        c.to_string().len()
                    }
                })
                .unwrap_or(0);
            let tc_len = m
                .get("tool_calls")
                .map(|t| t.to_string().len())
                .unwrap_or(0);
            content_len + tc_len
        })
        .sum()
}

pub(crate) fn find_safe_split(messages: &[Value], keep_recent: usize) -> usize {
    if messages.len() <= keep_recent + 1 {
        return 1;
    }
    let desired = messages.len() - keep_recent;
    for i in (1..=desired).rev() {
        if messages[i].get("role").and_then(|r| r.as_str()) == Some("user") {
            return i;
        }
    }
    desired.max(1)
}

fn build_summary_text(messages: &[Value]) -> String {
    let mut text = String::new();
    for msg in messages {
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown");
        if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                let truncated = if content.len() > 2000 {
                    format!("{}...[truncated]", &content[..2000])
                } else {
                    content.to_string()
                };
                text.push_str(&format!("[{}] {}\n\n", role, truncated));
            }
        }
        if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in tcs {
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("");
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .unwrap_or("");
                text.push_str(&format!("[assistant tool:{}] {}\n", name, args));
            }
        }
    }
    text
}

pub(crate) async fn summarize_for_compaction(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    model: &str,
    messages: &[Value],
) -> Option<String> {
    let conversation = build_summary_text(messages);
    if conversation.is_empty() {
        return None;
    }

    let body = json!({
        "model": model,
        "messages": [
            {
                "role": "system",
                "content": "Summarize this browser automation conversation concisely. Preserve: URLs visited, actions performed, current page state, errors encountered, and user goals. Output only the summary."
            },
            {
                "role": "user",
                "content": conversation
            }
        ],
        "max_tokens": 1024,
        "stream": false,
    });

    let resp = client
        .post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let result: Value = resp.json().await.ok()?;
    result
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

const SCREENSHOT_MAX_WIDTH: u32 = 1024;
const SCREENSHOT_JPEG_QUALITY: u8 = 40;

fn compress_image_to_jpeg(raw_bytes: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(raw_bytes).ok()?;
    let img = if img.width() > SCREENSHOT_MAX_WIDTH {
        img.resize(
            SCREENSHOT_MAX_WIDTH,
            u32::MAX,
            image::imageops::FilterType::Triangle,
        )
    } else {
        img
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    let encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, SCREENSHOT_JPEG_QUALITY);
    img.write_with_encoder(encoder).ok()?;
    Some(buf.into_inner())
}

fn has_image_extension(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

fn extract_image_path(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        // Whole line is a path (handles paths with spaces)
        if has_image_extension(trimmed) && std::path::Path::new(trimmed).exists() {
            return Some(trimmed.to_string());
        }
        for suffix in [".png", ".jpg", ".jpeg"] {
            if let Some(pos) = trimmed.to_lowercase().rfind(suffix) {
                let end = pos + suffix.len();
                let candidate = &trimmed[..end];
                let start = candidate
                    .rfind(|c: char| c.is_whitespace())
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let path = &candidate[start..];
                if !path.is_empty() && std::path::Path::new(path).exists() {
                    return Some(path.to_string());
                }
            }
        }
    }
    None
}

fn enrich_tool_output(result: &str) -> String {
    let Some(path) = extract_image_path(result) else {
        return result.to_string();
    };

    let Ok(raw_bytes) = std::fs::read(&path) else {
        return result.to_string();
    };

    let (jpeg_bytes, mime) = match compress_image_to_jpeg(&raw_bytes) {
        Some(compressed) => (compressed, "image/jpeg"),
        None => {
            let lower = path.to_lowercase();
            (
                raw_bytes,
                if lower.ends_with(".png") {
                    "image/png"
                } else {
                    "image/jpeg"
                },
            )
        }
    };

    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg_bytes);
    let data_url = format!("data:{};base64,{}", mime, b64);

    json!({
        "text": result,
        "image": data_url
    })
    .to_string()
}

const ALLOWED_COMMANDS: &[&str] = &[
    "open",
    "goto",
    "navigate",
    "back",
    "forward",
    "reload",
    "click",
    "dblclick",
    "fill",
    "type",
    "hover",
    "focus",
    "check",
    "uncheck",
    "select",
    "drag",
    "upload",
    "download",
    "press",
    "key",
    "keydown",
    "keyup",
    "keyboard",
    "scroll",
    "scrollintoview",
    "scrollinto",
    "wait",
    "screenshot",
    "pdf",
    "snapshot",
    "eval",
    "close",
    "quit",
    "exit",
    "inspect",
    "auth",
    "confirm",
    "deny",
    "connect",
    "cookies",
    "storage",
    "window",
    "frame",
    "dialog",
    "trace",
    "profiler",
    "record",
    "har",
    "network",
    "title",
    "url",
    "console",
    "errors",
    "highlight",
    "state",
    "emulate",
    "video",
    "tap",
    "swipe",
    "device",
    "batch",
    "diff",
    "find",
    "role",
    "text",
    "label",
    "placeholder",
    "alt",
    "testid",
    "first",
    "last",
    "nth",
    "mouse",
    "touchscreen",
    "attribute",
    "property",
    "set",
    "get",
    "is",
    "stream",
    "tab",
    "clipboard",
    "session",
];

const ALLOWED_GLOBAL_FLAGS: &[&str] = &["--session", "--engine"];

pub(crate) async fn execute_chat_tool(session: &str, command: &str) -> String {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => return format!("Failed to resolve executable: {}", e),
    };

    let single = command.split("&&").next().unwrap_or(command);
    let single = single.split(';').next().unwrap_or(single).trim();
    let stripped = single.strip_prefix("agent-browser ").unwrap_or(single);
    let words = crate::commands::shell_words_split(stripped);

    let mut global_flags: Vec<String> = Vec::new();
    let mut cmd_words: Vec<String> = Vec::new();
    let mut has_session_flag = false;
    let mut i = 0;
    while i < words.len() {
        if ALLOWED_GLOBAL_FLAGS.contains(&words[i].as_str()) {
            if words[i] == "--session" {
                has_session_flag = true;
            }
            global_flags.push(words[i].clone());
            if i + 1 < words.len() {
                global_flags.push(words[i + 1].clone());
                i += 2;
            } else {
                i += 1;
            }
        } else {
            cmd_words.push(words[i].clone());
            i += 1;
        }
    }

    let first_cmd = cmd_words.first().map(|s| s.as_str()).unwrap_or("");
    if !ALLOWED_COMMANDS.contains(&first_cmd) {
        return format!(
            "Blocked: '{}' is not a valid agent-browser command.",
            first_cmd
        );
    }

    let mut args: Vec<String> = Vec::new();
    if !has_session_flag {
        args.push("--session".into());
        args.push(session.into());
    }
    args.extend(global_flags);
    args.extend(cmd_words);

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .env_remove("AGENT_BROWSER_DASHBOARD")
        .env_remove("AGENT_BROWSER_DASHBOARD_PORT")
        .env_remove("AGENT_BROWSER_STREAM_PORT");

    match cmd.output().await {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stdout.is_empty() && !stderr.is_empty() {
                stderr
            } else if stdout.is_empty() {
                "Command completed with no output.".to_string()
            } else {
                stdout
            }
        }
        Err(e) => format!("Failed to execute command: {}", e),
    }
}

async fn stream_gateway_response(
    stream: &mut tokio::net::TcpStream,
    gw_response: reqwest::Response,
) -> Vec<(String, String, String)> {
    use futures_util::StreamExt as _;

    let mut text_part_id = uuid::Uuid::new_v4().to_string();
    let mut text_started = false;
    let mut tool_calls: Vec<(String, String, String)> = Vec::new();
    let mut tool_call_args: std::collections::HashMap<usize, (String, String, String)> =
        std::collections::HashMap::new();
    let mut byte_stream = gw_response.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk_result) = byte_stream.next().await {
        let chunk = match chunk_result {
            Ok(c) => c,
            Err(_) => break,
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
            buffer = buffer[newline_pos + 1..].to_string();

            if line.is_empty() {
                continue;
            }
            let Some(data) = line.strip_prefix("data: ") else {
                continue;
            };
            if data == "[DONE]" {
                if text_started {
                    let ev = format!("data: {}\n\n", json!({"type":"text-end","id":text_part_id}));
                    let _ = stream.write_all(ev.as_bytes()).await;
                }
                let mut indices: Vec<usize> = tool_call_args.keys().copied().collect();
                indices.sort();
                for idx in indices {
                    if let Some(tc) = tool_call_args.remove(&idx) {
                        tool_calls.push(tc);
                    }
                }
                return tool_calls;
            }
            let Ok(sse_json) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            let delta = sse_json
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"));
            let Some(delta) = delta else { continue };

            if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                if !text.is_empty() {
                    if !text_started {
                        let ev = format!(
                            "data: {}\n\n",
                            json!({"type":"text-start","id":text_part_id})
                        );
                        if stream.write_all(ev.as_bytes()).await.is_err() {
                            return tool_calls;
                        }
                        text_started = true;
                    }
                    let ev = format!(
                        "data: {}\n\n",
                        json!({"type":"text-delta","id":text_part_id,"delta":text})
                    );
                    if stream.write_all(ev.as_bytes()).await.is_err() {
                        return tool_calls;
                    }
                }
            }

            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                if text_started {
                    let ev = format!("data: {}\n\n", json!({"type":"text-end","id":text_part_id}));
                    let _ = stream.write_all(ev.as_bytes()).await;
                    text_started = false;
                    text_part_id = uuid::Uuid::new_v4().to_string();
                }

                for tc in tcs {
                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                    if let std::collections::hash_map::Entry::Vacant(e) = tool_call_args.entry(idx)
                    {
                        let id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let ev = format!(
                            "data: {}\n\n",
                            json!({"type":"tool-input-start","toolCallId":id,"toolName":name})
                        );
                        let _ = stream.write_all(ev.as_bytes()).await;
                        e.insert((id, name, String::new()));
                    }
                    if let Some(arg_delta) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                    {
                        let entry = tool_call_args.get_mut(&idx).unwrap();
                        entry.2.push_str(arg_delta);
                        let ev = format!(
                            "data: {}\n\n",
                            json!({"type":"tool-input-delta","toolCallId":entry.0,"inputTextDelta":arg_delta})
                        );
                        let _ = stream.write_all(ev.as_bytes()).await;
                    }
                }
            }
        }
    }

    if text_started {
        let ev = format!("data: {}\n\n", json!({"type":"text-end","id":text_part_id}));
        let _ = stream.write_all(ev.as_bytes()).await;
    }
    let mut indices: Vec<usize> = tool_call_args.keys().copied().collect();
    indices.sort();
    for idx in indices {
        if let Some(tc) = tool_call_args.remove(&idx) {
            tool_calls.push(tc);
        }
    }
    tool_calls
}

pub(super) async fn handle_chat_request(
    stream: &mut tokio::net::TcpStream,
    body: &str,
    origin: Option<&str>,
) {
    let cors = cors_headers_for_origin(origin);
    let gateway_url = std::env::var("AI_GATEWAY_URL")
        .unwrap_or_else(|_| DEFAULT_AI_GATEWAY_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let api_key = match std::env::var("AI_GATEWAY_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            let err = r#"{"error":"AI_GATEWAY_API_KEY not set. Set the AI_GATEWAY_API_KEY environment variable to enable AI chat."}"#;
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{cors}\r\n",
                err.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.write_all(err.as_bytes()).await;
            return;
        }
    };

    let default_model = std::env::var("AI_GATEWAY_MODEL")
        .unwrap_or_else(|_| "anthropic/claude-sonnet-4.6".to_string());

    let parsed: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let err = format!(r#"{{"error":"Invalid JSON: {}"}}"#, e);
            let resp = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{cors}\r\n",
                err.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.write_all(err.as_bytes()).await;
            return;
        }
    };

    let messages = parsed.get("messages").cloned().unwrap_or(json!([]));
    let model = parsed
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or(&default_model)
        .to_string();
    let session = parsed
        .get("session")
        .and_then(|v| v.as_str())
        .unwrap_or("default")
        .to_string();

    let mut openai_messages: Vec<Value> =
        vec![json!({"role": "system", "content": get_system_prompt()})];
    let mut frontend_boundaries: Vec<usize> = Vec::new();
    let frontend_arr = messages.as_array();
    let frontend_count = frontend_arr.map(|a| a.len()).unwrap_or(0);
    if let Some(arr) = frontend_arr {
        for msg in arr {
            frontend_boundaries.push(openai_messages.len());
            let Some(role) = msg.get("role").and_then(|r| r.as_str()) else {
                continue;
            };
            if let Some(parts) = msg.get("parts").and_then(|p| p.as_array()) {
                let mut content_parts: Vec<Value> = Vec::new();
                for part in parts {
                    match part.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    content_parts.push(json!({"type": "text", "text": text}));
                                }
                            }
                        }
                        Some("file") => {
                            if let (Some(url), Some(media_type)) = (
                                part.get("url").and_then(|u| u.as_str()),
                                part.get("mediaType").and_then(|m| m.as_str()),
                            ) {
                                if media_type.starts_with("image/") {
                                    content_parts.push(json!({
                                        "type": "image_url",
                                        "image_url": { "url": url }
                                    }));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !content_parts.is_empty() {
                    let content = if content_parts.len() == 1
                        && content_parts[0].get("type").and_then(|t| t.as_str()) == Some("text")
                    {
                        content_parts[0]["text"].clone()
                    } else {
                        json!(content_parts)
                    };
                    openai_messages.push(json!({"role": role, "content": content}));
                }
            } else if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                openai_messages.push(json!({"role": role, "content": content}));
            }
        }
    }

    let tools: Value = serde_json::from_str(CHAT_TOOLS).unwrap();
    let url = format!("{}/v1/chat/completions", gateway_url);
    let client = http_client();

    let total_chars = estimate_chars(&openai_messages);
    let mut compaction_summary: Option<String> = None;
    let mut compaction_failed = false;
    let mut keep_last_n: usize = frontend_count;

    if total_chars > COMPACT_THRESHOLD_CHARS && openai_messages.len() > KEEP_RECENT_MESSAGES + 2 {
        let split = find_safe_split(&openai_messages, KEEP_RECENT_MESSAGES);
        let to_summarize = &openai_messages[1..split];

        if let Some(summary) =
            summarize_for_compaction(client, &url, &api_key, &model, to_summarize).await
        {
            let summary_msg = json!({
                "role": "system",
                "content": format!("[Conversation summary]\n{}", summary)
            });
            let recent = openai_messages[split..].to_vec();
            openai_messages = vec![openai_messages[0].clone(), summary_msg];
            openai_messages.extend(recent);

            let kept_frontend = frontend_boundaries
                .iter()
                .filter(|&&boundary| boundary >= split)
                .count();
            keep_last_n = kept_frontend;
            compaction_summary = Some(summary);
        } else {
            compaction_failed = true;
        }
    }

    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\nx-vercel-ai-ui-message-stream: v1\r\n{cors}\r\n"
    );
    if stream.write_all(headers.as_bytes()).await.is_err() {
        return;
    }

    let message_id = uuid::Uuid::new_v4().to_string();
    let start_ev = format!(
        "data: {}\n\n",
        json!({"type":"start","messageId":message_id})
    );
    if stream.write_all(start_ev.as_bytes()).await.is_err() {
        return;
    }

    if let Some(ref summary) = compaction_summary {
        let ev = format!(
            "data: {}\n\n",
            json!({
                "type": "message-metadata",
                "messageMetadata": {
                    "compacted": true,
                    "summary": summary,
                    "keepLastN": keep_last_n
                }
            })
        );
        let _ = stream.write_all(ev.as_bytes()).await;
    } else if compaction_failed {
        let ev = format!(
            "data: {}\n\n",
            json!({
                "type": "message-metadata",
                "messageMetadata": {
                    "compacted": false,
                    "warning": "Conversation is large but compaction failed. Responses may be degraded."
                }
            })
        );
        let _ = stream.write_all(ev.as_bytes()).await;
    }

    let total_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    const TOOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

    for _step in 0..50 {
        if tokio::time::Instant::now() >= total_deadline {
            let ev = format!(
                "data: {}\n\n",
                json!({"type":"error","errorText":"Chat session timed out (5 minute limit)."})
            );
            let _ = stream.write_all(ev.as_bytes()).await;
            break;
        }

        let step_ev = "data: {\"type\":\"start-step\"}\n\n";
        if stream.write_all(step_ev.as_bytes()).await.is_err() {
            return;
        }

        let gateway_body = json!({
            "model": model,
            "messages": openai_messages,
            "tools": tools,
            "stream": true,
        });

        let gw_response = match client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .body(gateway_body.to_string())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let ev = format!(
                    "data: {}\n\n",
                    json!({"type":"error","errorText":format!("Gateway request failed: {}", e)})
                );
                let _ = stream.write_all(ev.as_bytes()).await;
                break;
            }
        };

        if !gw_response.status().is_success() {
            let body_text = gw_response.text().await.unwrap_or_default();
            let ev = format!(
                "data: {}\n\n",
                json!({"type":"error","errorText":body_text})
            );
            let _ = stream.write_all(ev.as_bytes()).await;
            break;
        }

        let tool_calls = stream_gateway_response(stream, gw_response).await;

        if tool_calls.is_empty() {
            let finish_step_ev = "data: {\"type\":\"finish-step\"}\n\n";
            let _ = stream.write_all(finish_step_ev.as_bytes()).await;
            break;
        }

        let tc_values: Vec<Value> = tool_calls.iter().map(|(id, name, args)| {
            json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}})
        }).collect();
        openai_messages.push(json!({"role": "assistant", "tool_calls": tc_values}));

        for (tc_id, tc_name, tc_args) in &tool_calls {
            let input: Value = serde_json::from_str(tc_args).unwrap_or(json!({}));
            let command = input.get("command").and_then(|c| c.as_str()).unwrap_or("");

            let ev = format!(
                "data: {}\n\n",
                json!({
                    "type": "tool-input-available",
                    "toolCallId": tc_id,
                    "toolName": tc_name,
                    "input": input
                })
            );
            let _ = stream.write_all(ev.as_bytes()).await;

            let result = match tokio::time::timeout(
                TOOL_TIMEOUT,
                execute_chat_tool(&session, command),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => "Tool execution timed out after 60 seconds.".to_string(),
            };

            let frontend_output = enrich_tool_output(&result);
            let ev = format!(
                "data: {}\n\n",
                json!({
                    "type": "tool-output-available",
                    "toolCallId": tc_id,
                    "output": frontend_output
                })
            );
            let _ = stream.write_all(ev.as_bytes()).await;

            openai_messages.push(json!({
                "role": "tool",
                "tool_call_id": tc_id,
                "content": result
            }));
        }

        let finish_step_ev = "data: {\"type\":\"finish-step\"}\n\n";
        let _ = stream.write_all(finish_step_ev.as_bytes()).await;
    }

    let finish_ev = "data: {\"type\":\"finish\"}\n\n";
    let _ = stream.write_all(finish_ev.as_bytes()).await;
    let done_ev = "data: [DONE]\n\n";
    let _ = stream.write_all(done_ev.as_bytes()).await;
}
