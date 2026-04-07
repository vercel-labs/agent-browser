use std::io::Write as _;
use std::process::exit;

use serde_json::{json, Value};

use crate::color;
use crate::flags::Flags;
use crate::native::stream::chat;

const DEFAULT_MODEL: &str = "anthropic/claude-sonnet-4.6";

#[derive(Clone, Copy, PartialEq)]
enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

pub fn run_chat(flags: &Flags, message: Option<String>) {
    if !chat::is_chat_enabled() {
        if flags.json {
            println!(
                "{}",
                json!({"success": false, "error": "AI_GATEWAY_API_KEY not set. Set the AI_GATEWAY_API_KEY environment variable to enable chat."})
            );
        } else {
            eprintln!(
                "{} AI_GATEWAY_API_KEY not set. Set the AI_GATEWAY_API_KEY environment variable to enable chat.",
                color::error_indicator()
            );
        }
        exit(1);
    }

    let verbosity = if flags.quiet {
        Verbosity::Quiet
    } else if flags.verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };

    let model = flags
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());

    let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");

    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());

    match message {
        Some(msg) => {
            rt.block_on(run_single_turn(
                &flags.session,
                &model,
                &msg,
                verbosity,
                flags.json,
            ));
        }
        None if !is_tty => {
            let mut input = String::new();
            if let Err(e) = std::io::stdin().read_line(&mut input) {
                if flags.json {
                    println!(
                        "{}",
                        json!({"success": false, "error": format!("Failed to read stdin: {}", e)})
                    );
                } else {
                    eprintln!("{} Failed to read stdin: {}", color::error_indicator(), e);
                }
                exit(1);
            }
            let input = input.trim();
            if input.is_empty() {
                if flags.json {
                    println!(
                        "{}",
                        json!({"success": false, "error": "No input provided"})
                    );
                } else {
                    eprintln!("{} No input provided", color::error_indicator());
                }
                exit(1);
            }
            rt.block_on(run_single_turn(
                &flags.session,
                &model,
                input,
                verbosity,
                flags.json,
            ));
        }
        None => {
            rt.block_on(run_interactive(
                &flags.session,
                &model,
                verbosity,
                flags.json,
            ));
        }
    }
}

async fn run_single_turn(
    session: &str,
    model: &str,
    message: &str,
    verbosity: Verbosity,
    json_mode: bool,
) {
    let mut openai_messages: Vec<Value> =
        vec![json!({"role": "system", "content": chat::get_system_prompt()})];
    openai_messages.push(json!({"role": "user", "content": message}));

    let result = run_chat_turn(session, model, &mut openai_messages, verbosity, json_mode).await;
    if !result {
        exit(1);
    }
}

async fn run_interactive(session: &str, model: &str, verbosity: Verbosity, json_mode: bool) {
    let mut openai_messages: Vec<Value> =
        vec![json!({"role": "system", "content": chat::get_system_prompt()})];

    let gateway_url = std::env::var("AI_GATEWAY_URL")
        .unwrap_or_else(|_| chat::DEFAULT_AI_GATEWAY_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let api_key = std::env::var("AI_GATEWAY_API_KEY").unwrap_or_default();
    let url = format!("{}/v1/chat/completions", gateway_url);
    let client = chat::http_client();

    loop {
        if !json_mode {
            eprint!("{} ", color::cyan(">"));
            let _ = std::io::stderr().flush();
        }

        let mut input = String::new();
        match std::io::stdin().read_line(&mut input) {
            Ok(0) => break,
            Err(_) => break,
            Ok(_) => {}
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        if matches!(input, "quit" | "exit" | "q") {
            break;
        }

        openai_messages.push(json!({"role": "user", "content": input}));

        // Compaction check
        let total_chars = chat::estimate_chars(&openai_messages);
        if total_chars > chat::COMPACT_THRESHOLD_CHARS
            && openai_messages.len() > chat::KEEP_RECENT_MESSAGES + 2
        {
            let split = chat::find_safe_split(&openai_messages, chat::KEEP_RECENT_MESSAGES);
            let to_summarize = &openai_messages[1..split];
            if let Some(summary) =
                chat::summarize_for_compaction(client, &url, &api_key, model, to_summarize).await
            {
                let summary_msg = json!({
                    "role": "system",
                    "content": format!("[Conversation summary]\n{}", summary)
                });
                let recent = openai_messages[split..].to_vec();
                openai_messages = vec![openai_messages[0].clone(), summary_msg];
                openai_messages.extend(recent);
            }
        }

        let success =
            run_chat_turn(session, model, &mut openai_messages, verbosity, json_mode).await;

        if !success && !json_mode {
            // Continue the loop on error; don't exit interactive mode
        }

        if !json_mode {
            eprintln!();
        }
    }
}

/// Runs one chat turn: sends messages to the gateway, streams text/tool calls,
/// executes tools in a loop until the model is done. Appends assistant and tool
/// messages to `openai_messages`. Returns true on success.
async fn run_chat_turn(
    session: &str,
    model: &str,
    openai_messages: &mut Vec<Value>,
    verbosity: Verbosity,
    json_mode: bool,
) -> bool {
    let gateway_url = std::env::var("AI_GATEWAY_URL")
        .unwrap_or_else(|_| chat::DEFAULT_AI_GATEWAY_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let api_key = match std::env::var("AI_GATEWAY_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            if json_mode {
                println!(
                    "{}",
                    json!({"success": false, "error": "AI_GATEWAY_API_KEY not set"})
                );
            } else {
                eprintln!("{} AI_GATEWAY_API_KEY not set", color::error_indicator());
            }
            return false;
        }
    };

    let tools: Value = serde_json::from_str(chat::CHAT_TOOLS).unwrap();
    let url = format!("{}/v1/chat/completions", gateway_url);
    let client = chat::http_client();

    let total_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);
    let tool_timeout = std::time::Duration::from_secs(60);

    let mut all_text = String::new();
    let mut all_tool_calls: Vec<Value> = Vec::new();
    let mut had_text = false;

    for _step in 0..50 {
        if tokio::time::Instant::now() >= total_deadline {
            if json_mode {
                println!(
                    "{}",
                    json!({"success": false, "error": "Chat session timed out (5 minute limit)."})
                );
            } else {
                eprintln!(
                    "\n{} Chat session timed out (5 minute limit).",
                    color::error_indicator()
                );
            }
            return false;
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
                if json_mode {
                    println!(
                        "{}",
                        json!({"success": false, "error": format!("Gateway request failed: {}", e)})
                    );
                } else {
                    eprintln!(
                        "\n{} Gateway request failed: {}",
                        color::error_indicator(),
                        e
                    );
                }
                return false;
            }
        };

        if !gw_response.status().is_success() {
            let body_text = gw_response.text().await.unwrap_or_default();
            if json_mode {
                println!("{}", json!({"success": false, "error": body_text}));
            } else {
                eprintln!("\n{} {}", color::error_indicator(), body_text);
            }
            return false;
        }

        let (text_chunks, tool_calls) =
            parse_gateway_stream(gw_response, verbosity, json_mode).await;

        if !text_chunks.is_empty() {
            let text = text_chunks.join("");
            all_text.push_str(&text);
            if !json_mode {
                if !had_text && verbosity != Verbosity::Quiet {
                    // Add blank line before text if we showed tool calls
                    if !all_tool_calls.is_empty() {
                        println!();
                    }
                }
                had_text = true;
            }

            let mut content = json!(text);
            if let Some(last) = openai_messages.last() {
                if last.get("role").and_then(|r| r.as_str()) == Some("assistant")
                    && last.get("tool_calls").is_some()
                {
                    content = json!(text);
                }
            }
            openai_messages.push(json!({"role": "assistant", "content": content}));
        }

        if tool_calls.is_empty() {
            break;
        }

        let tc_values: Vec<Value> = tool_calls
            .iter()
            .map(|(id, name, args)| {
                json!({"id": id, "type": "function", "function": {"name": name, "arguments": args}})
            })
            .collect();

        if text_chunks.is_empty() {
            openai_messages.push(json!({"role": "assistant", "tool_calls": tc_values}));
        } else {
            // If we had both text and tool calls in the same response, merge them
            if let Some(last) = openai_messages.last_mut() {
                if last.get("role").and_then(|r| r.as_str()) == Some("assistant")
                    && last.get("tool_calls").is_none()
                {
                    last["tool_calls"] = json!(tc_values);
                } else {
                    openai_messages.push(json!({"role": "assistant", "tool_calls": tc_values}));
                }
            }
        }

        for (tc_id, _tc_name, tc_args) in &tool_calls {
            let input: Value = serde_json::from_str(tc_args).unwrap_or(json!({}));
            let command = input.get("command").and_then(|c| c.as_str()).unwrap_or("");

            if !json_mode && verbosity != Verbosity::Quiet {
                eprintln!("{}", color::dim(&format!("> {}", command)));
            }

            let result =
                match tokio::time::timeout(tool_timeout, chat::execute_chat_tool(session, command))
                    .await
                {
                    Ok(r) => r,
                    Err(_) => "Tool execution timed out after 60 seconds.".to_string(),
                };

            if !json_mode && verbosity == Verbosity::Verbose {
                for line in result.lines() {
                    eprintln!("  {}", color::dim(line));
                }
            }

            all_tool_calls.push(json!({
                "command": command,
                "output": result
            }));

            openai_messages.push(json!({
                "role": "tool",
                "tool_call_id": tc_id,
                "content": result
            }));
        }
    }

    if json_mode {
        println!(
            "{}",
            json!({
                "success": true,
                "text": all_text,
                "tool_calls": all_tool_calls
            })
        );
    } else if !had_text && !json_mode {
        // Model returned only tool calls with no final text; print newline for clean output
        println!();
    }

    true
}

/// Parses the SSE stream from the AI gateway, printing text deltas to stdout in
/// real-time. Returns (collected_text_chunks, tool_calls).
async fn parse_gateway_stream(
    gw_response: reqwest::Response,
    verbosity: Verbosity,
    json_mode: bool,
) -> (Vec<String>, Vec<(String, String, String)>) {
    use futures_util::StreamExt as _;

    let mut text_chunks: Vec<String> = Vec::new();
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
                let tool_calls = collect_tool_calls(&mut tool_call_args);
                if !json_mode && !text_chunks.is_empty() {
                    // End the streamed text line
                    let _ = std::io::stdout().flush();
                }
                return (text_chunks, tool_calls);
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
                    text_chunks.push(text.to_string());
                    if !json_mode && verbosity != Verbosity::Quiet {
                        print!("{}", text);
                        let _ = std::io::stdout().flush();
                    }
                }
            }

            if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
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
                        e.insert((id, name, String::new()));
                    }
                    if let Some(arg_delta) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                    {
                        let entry = tool_call_args.get_mut(&idx).unwrap();
                        entry.2.push_str(arg_delta);
                    }
                }
            }
        }
    }

    if !json_mode && !text_chunks.is_empty() {
        let _ = std::io::stdout().flush();
    }
    let tool_calls = collect_tool_calls(&mut tool_call_args);
    (text_chunks, tool_calls)
}

fn collect_tool_calls(
    map: &mut std::collections::HashMap<usize, (String, String, String)>,
) -> Vec<(String, String, String)> {
    let mut indices: Vec<usize> = map.keys().copied().collect();
    indices.sort();
    indices
        .into_iter()
        .filter_map(|idx| map.remove(&idx))
        .collect()
}
