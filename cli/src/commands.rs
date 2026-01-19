use serde_json::{json, Value};

use crate::flags::Flags;

/// Error type for command parsing with contextual information
#[derive(Debug)]
pub enum ParseError {
    /// Command does not exist
    UnknownCommand { command: String },
    /// Command exists but subcommand is invalid
    UnknownSubcommand {
        subcommand: String,
        valid_options: &'static [&'static str],
    },
    /// Command/subcommand exists but required arguments are missing
    MissingArguments {
        context: String,
        usage: &'static str,
    },
}

impl ParseError {
    pub fn format(&self) -> String {
        match self {
            ParseError::UnknownCommand { command } => {
                format!("Unknown command: {}", command)
            }
            ParseError::UnknownSubcommand {
                subcommand,
                valid_options,
            } => {
                format!(
                    "Unknown subcommand: {}\nValid options: {}",
                    subcommand,
                    valid_options.join(", ")
                )
            }
            ParseError::MissingArguments { context, usage } => {
                format!(
                    "Missing arguments for: {}\nUsage: agent-browser {}",
                    context, usage
                )
            }
        }
    }
}

pub fn gen_id() -> String {
    format!(
        "r{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros()
            % 1000000
    )
}

pub fn parse_command(args: &[String], flags: &Flags) -> Result<Value, ParseError> {
    if args.is_empty() {
        return Err(ParseError::MissingArguments {
            context: "".to_string(),
            usage: "<command> [args...]",
        });
    }

    let cmd = args[0].as_str();
    let rest: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    let id = gen_id();

    match cmd {
        // === Navigation ===
        "open" | "goto" | "navigate" => {
            let url = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: cmd.to_string(),
                usage: "open <url>",
            })?;
            let url_lower = url.to_lowercase();
            let url = if url_lower.starts_with("http://") 
                || url_lower.starts_with("https://")
                || url_lower.starts_with("about:") 
                || url_lower.starts_with("data:") 
                || url_lower.starts_with("file:") {
                url.to_string()
            } else {
                format!("https://{}", url)
            };
            let mut nav_cmd = json!({ "id": id, "action": "navigate", "url": url });
            // If --headers flag is set, include headers (scoped to this origin)
            if let Some(ref headers_json) = flags.headers {
                if let Ok(headers) = serde_json::from_str::<serde_json::Value>(headers_json) {
                    nav_cmd["headers"] = headers;
                }
            }
            Ok(nav_cmd)
        }
        "back" => Ok(json!({ "id": id, "action": "back" })),
        "forward" => Ok(json!({ "id": id, "action": "forward" })),
        "reload" => Ok(json!({ "id": id, "action": "reload" })),

        // === Core Actions ===
        "click" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "click".to_string(),
                usage: "click <selector>",
            })?;
            Ok(json!({ "id": id, "action": "click", "selector": sel }))
        }
        "dblclick" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "dblclick".to_string(),
                usage: "dblclick <selector>",
            })?;
            Ok(json!({ "id": id, "action": "dblclick", "selector": sel }))
        }
        "fill" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "fill".to_string(),
                usage: "fill <selector> <text>",
            })?;
            Ok(json!({ "id": id, "action": "fill", "selector": sel, "value": rest[1..].join(" ") }))
        }
        "type" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "type".to_string(),
                usage: "type <selector> <text>",
            })?;
            Ok(json!({ "id": id, "action": "type", "selector": sel, "text": rest[1..].join(" ") }))
        }
        "hover" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "hover".to_string(),
                usage: "hover <selector>",
            })?;
            Ok(json!({ "id": id, "action": "hover", "selector": sel }))
        }
        "focus" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "focus".to_string(),
                usage: "focus <selector>",
            })?;
            Ok(json!({ "id": id, "action": "focus", "selector": sel }))
        }
        "check" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "check".to_string(),
                usage: "check <selector>",
            })?;
            Ok(json!({ "id": id, "action": "check", "selector": sel }))
        }
        "uncheck" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "uncheck".to_string(),
                usage: "uncheck <selector>",
            })?;
            Ok(json!({ "id": id, "action": "uncheck", "selector": sel }))
        }
        "select" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "select".to_string(),
                usage: "select <selector> <value...>",
            })?;
            let _val = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "select".to_string(),
                usage: "select <selector> <value...>",
            })?;
            let values = &rest[1..];
            if values.len() == 1 {
                Ok(json!({ "id": id, "action": "select", "selector": sel, "values": values[0] }))
            } else {
                Ok(json!({ "id": id, "action": "select", "selector": sel, "values": values }))
            }
        }
        "drag" => {
            let src = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "drag".to_string(),
                usage: "drag <source> <target>",
            })?;
            let tgt = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "drag".to_string(),
                usage: "drag <source> <target>",
            })?;
            Ok(json!({ "id": id, "action": "drag", "source": src, "target": tgt }))
        }
        "upload" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "upload".to_string(),
                usage: "upload <selector> <files...>",
            })?;
            Ok(json!({ "id": id, "action": "upload", "selector": sel, "files": &rest[1..] }))
        }

        // === Keyboard ===
        "press" | "key" => {
            let key = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "press".to_string(),
                usage: "press <key>",
            })?;
            Ok(json!({ "id": id, "action": "press", "key": key }))
        }
        "keydown" => {
            let key = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "keydown".to_string(),
                usage: "keydown <key>",
            })?;
            Ok(json!({ "id": id, "action": "keydown", "key": key }))
        }
        "keyup" => {
            let key = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "keyup".to_string(),
                usage: "keyup <key>",
            })?;
            Ok(json!({ "id": id, "action": "keyup", "key": key }))
        }

        // === Scroll ===
        "scroll" => {
            let dir = rest.get(0).unwrap_or(&"down");
            let amount = rest.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(300);
            Ok(json!({ "id": id, "action": "scroll", "direction": dir, "amount": amount }))
        }
        "scrollintoview" | "scrollinto" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "scrollintoview".to_string(),
                usage: "scrollintoview <selector>",
            })?;
            Ok(json!({ "id": id, "action": "scrollintoview", "selector": sel }))
        }

        // === Wait ===
        "wait" => {
            // Check for --url flag: wait --url "**/dashboard"
            if let Some(idx) = rest.iter().position(|&s| s == "--url" || s == "-u") {
                let url = rest.get(idx + 1).ok_or_else(|| ParseError::MissingArguments {
                    context: "wait --url".to_string(),
                    usage: "wait --url <pattern>",
                })?;
                return Ok(json!({ "id": id, "action": "waitforurl", "url": url }));
            }
            
            // Check for --load flag: wait --load networkidle
            if let Some(idx) = rest.iter().position(|&s| s == "--load" || s == "-l") {
                let state = rest.get(idx + 1).ok_or_else(|| ParseError::MissingArguments {
                    context: "wait --load".to_string(),
                    usage: "wait --load <state>",
                })?;
                return Ok(json!({ "id": id, "action": "waitforloadstate", "state": state }));
            }
            
            // Check for --fn flag: wait --fn "window.ready === true"
            if let Some(idx) = rest.iter().position(|&s| s == "--fn" || s == "-f") {
                let expr = rest.get(idx + 1).ok_or_else(|| ParseError::MissingArguments {
                    context: "wait --fn".to_string(),
                    usage: "wait --fn <expression>",
                })?;
                return Ok(json!({ "id": id, "action": "waitforfunction", "expression": expr }));
            }
            
            // Check for --text flag: wait --text "Welcome"
            if let Some(idx) = rest.iter().position(|&s| s == "--text" || s == "-t") {
                let text = rest.get(idx + 1).ok_or_else(|| ParseError::MissingArguments {
                    context: "wait --text".to_string(),
                    usage: "wait --text <text>",
                })?;
                // Use getByText locator to wait for text to appear
                return Ok(json!({ "id": id, "action": "wait", "selector": format!("text={}", text) }));
            }
            
            // Default: selector or timeout
            if let Some(arg) = rest.get(0) {
                if arg.parse::<u64>().is_ok() {
                    Ok(json!({ "id": id, "action": "wait", "timeout": arg.parse::<u64>().unwrap() }))
                } else {
                    Ok(json!({ "id": id, "action": "wait", "selector": arg }))
                }
            } else {
                Err(ParseError::MissingArguments {
                    context: "wait".to_string(),
                    usage: "wait <selector|ms|--url|--load|--fn|--text>",
                })
            }
        }

        // === Screenshot/PDF ===
        "screenshot" => {
            let mut cmd = json!({ "id": id, "action": "screenshot", "fullPage": flags.full });
            if let Some(path) = rest.get(0) {
                cmd["path"] = json!(path);
            }
            Ok(cmd)
        }
        "pdf" => {
            let path = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "pdf".to_string(),
                usage: "pdf <path>",
            })?;
            Ok(json!({ "id": id, "action": "pdf", "path": path }))
        }

        // === Snapshot ===
        "snapshot" => {
            let mut cmd = json!({ "id": id, "action": "snapshot" });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "-i" | "--interactive" => {
                        obj.insert("interactive".to_string(), json!(true));
                    }
                    "-c" | "--compact" => {
                        obj.insert("compact".to_string(), json!(true));
                    }
                    "-d" | "--depth" => {
                        if let Some(d) = rest.get(i + 1) {
                            if let Ok(n) = d.parse::<i32>() {
                                obj.insert("maxDepth".to_string(), json!(n));
                                i += 1;
                            }
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            Ok(cmd)
        }

        // === Eval ===
        "eval" => Ok(json!({ "id": id, "action": "evaluate", "script": rest.join(" ") })),

        // === Close ===
        "close" | "quit" | "exit" => Ok(json!({ "id": id, "action": "close" })),

        // === Connect (CDP) ===
        "connect" => {
            let port_str = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "connect".to_string(),
                usage: "connect <port>",
            })?;
            let port: u16 = port_str.parse().map_err(|_| ParseError::MissingArguments {
                context: format!("connect: invalid port '{}'", port_str),
                usage: "connect <port>",
            })?;
            Ok(json!({ "id": id, "action": "launch", "cdpPort": port }))
        }

        // === Get ===
        "get" => parse_get(&rest, &id),

        // === Is (state checks) ===
        "is" => parse_is(&rest, &id),

        // === Find (locators) ===
        "find" => parse_find(&rest, &id),

        // === Mouse ===
        "mouse" => parse_mouse(&rest, &id),

        // === Set (browser settings) ===
        "set" => parse_set(&rest, &id),

        // === Network ===
        "network" => parse_network(&rest, &id),

        // === Storage ===
        "storage" => parse_storage(&rest, &id),

        // === Cookies ===
        "cookies" => {
            let op = rest.get(0).unwrap_or(&"get");
            match *op {
                "set" => {
                    let name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "cookies set".to_string(),
                        usage: "cookies set <name> <value>",
                    })?;
                    let value = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                        context: "cookies set".to_string(),
                        usage: "cookies set <name> <value>",
                    })?;
                    Ok(json!({ "id": id, "action": "cookies_set", "cookies": [{ "name": name, "value": value }] }))
                }
                "clear" => Ok(json!({ "id": id, "action": "cookies_clear" })),
                _ => Ok(json!({ "id": id, "action": "cookies_get" })),
            }
        }

        // === Tabs ===
        "tab" => {
            match rest.get(0).map(|s| *s) {
                Some("new") => {
                    let mut cmd = json!({ "id": id, "action": "tab_new" });
                    if let Some(url) = rest.get(1) {
                        cmd["url"] = json!(url);
                    }
                    Ok(cmd)
                }
                Some("list") => Ok(json!({ "id": id, "action": "tab_list" })),
                Some("close") => {
                    let mut cmd = json!({ "id": id, "action": "tab_close" });
                    if let Some(index) = rest.get(1).and_then(|s| s.parse::<i32>().ok()) {
                        cmd["index"] = json!(index);
                    }
                    Ok(cmd)
                }
                Some(n) if n.parse::<i32>().is_ok() => {
                    Ok(json!({ "id": id, "action": "tab_switch", "index": n.parse::<i32>().unwrap() }))
                }
                _ => Ok(json!({ "id": id, "action": "tab_list" })),
            }
        }

        // === Window ===
        "window" => {
            const VALID: &[&str] = &["new"];
            match rest.get(0).map(|s| *s) {
                Some("new") => Ok(json!({ "id": id, "action": "window_new" })),
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "window".to_string(),
                    usage: "window <new>",
                }),
            }
        }

        // === Frame ===
        "frame" => {
            if rest.get(0).map(|s| *s) == Some("main") {
                Ok(json!({ "id": id, "action": "mainframe" }))
            } else {
                let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                    context: "frame".to_string(),
                    usage: "frame <selector|main>",
                })?;
                Ok(json!({ "id": id, "action": "frame", "selector": sel }))
            }
        }

        // === Dialog ===
        "dialog" => {
            const VALID: &[&str] = &["accept", "dismiss"];
            match rest.get(0).map(|s| *s) {
                Some("accept") => {
                    let mut cmd = json!({ "id": id, "action": "dialog", "response": "accept" });
                    if let Some(prompt_text) = rest.get(1) {
                        cmd["promptText"] = json!(prompt_text);
                    }
                    Ok(cmd)
                }
                Some("dismiss") => Ok(json!({ "id": id, "action": "dialog", "response": "dismiss" })),
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "dialog".to_string(),
                    usage: "dialog <accept|dismiss> [text]",
                }),
            }
        }

        // === Debug ===
        "trace" => {
            const VALID: &[&str] = &["start", "stop"];
            match rest.get(0).map(|s| *s) {
                Some("start") => Ok(json!({ "id": id, "action": "trace_start" })),
                Some("stop") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "trace stop".to_string(),
                        usage: "trace stop <path>",
                    })?;
                    Ok(json!({ "id": id, "action": "trace_stop", "path": path }))
                },
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "trace".to_string(),
                    usage: "trace <start|stop> [path]",
                }),
            }
        }

        // === Recording (Playwright native video recording) ===
        "record" => {
            const VALID: &[&str] = &["start", "stop", "restart"];
            match rest.get(0).map(|s| *s) {
                Some("start") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "record start".to_string(),
                        usage: "record start <output.webm> [url]",
                    })?;
                    // Optional URL parameter
                    let url = rest.get(2);
                    let mut cmd = json!({ "id": id, "action": "recording_start", "path": path });
                    if let Some(u) = url {
                        // Add https:// prefix if needed
                        let url_str = if u.starts_with("http") {
                            u.to_string()
                        } else {
                            format!("https://{}", u)
                        };
                        cmd["url"] = json!(url_str);
                    }
                    Ok(cmd)
                }
                Some("stop") => Ok(json!({ "id": id, "action": "recording_stop" })),
                Some("restart") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "record restart".to_string(),
                        usage: "record restart <output.webm> [url]",
                    })?;
                    // Optional URL parameter
                    let url = rest.get(2);
                    let mut cmd = json!({ "id": id, "action": "recording_restart", "path": path });
                    if let Some(u) = url {
                        // Add https:// prefix if needed
                        let url_str = if u.starts_with("http") {
                            u.to_string()
                        } else {
                            format!("https://{}", u)
                        };
                        cmd["url"] = json!(url_str);
                    }
                    Ok(cmd)
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "record".to_string(),
                    usage: "record <start|stop|restart> [path] [url]",
                }),
            }
        }
        "console" => {
            let clear = rest.iter().any(|&s| s == "--clear");
            Ok(json!({ "id": id, "action": "console", "clear": clear }))
        }
        "errors" => {
            let clear = rest.iter().any(|&s| s == "--clear");
            Ok(json!({ "id": id, "action": "errors", "clear": clear }))
        }
        "highlight" => {
            let sel = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
                context: "highlight".to_string(),
                usage: "highlight <selector>",
            })?;
            Ok(json!({ "id": id, "action": "highlight", "selector": sel }))
        }

        // === State ===
        "state" => {
            const VALID: &[&str] = &["save", "load"];
            match rest.get(0).map(|s| *s) {
                Some("save") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state save".to_string(),
                        usage: "state save <path>",
                    })?;
                    Ok(json!({ "id": id, "action": "state_save", "path": path }))
                }
                Some("load") => {
                    let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state load".to_string(),
                        usage: "state load <path>",
                    })?;
                    Ok(json!({ "id": id, "action": "state_load", "path": path }))
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "state".to_string(),
                    usage: "state <save|load> <path>",
                }),
            }
        }

        _ => Err(ParseError::UnknownCommand {
            command: cmd.to_string(),
        }),
    }
}

fn parse_get(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["text", "html", "value", "attr", "url", "title", "count", "box", "styles"];
    
    match rest.get(0).map(|s| *s) {
        Some("text") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get text".to_string(),
                usage: "get text <selector>",
            })?;
            Ok(json!({ "id": id, "action": "gettext", "selector": sel }))
        }
        Some("html") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get html".to_string(),
                usage: "get html <selector>",
            })?;
            Ok(json!({ "id": id, "action": "innerhtml", "selector": sel }))
        }
        Some("value") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get value".to_string(),
                usage: "get value <selector>",
            })?;
            Ok(json!({ "id": id, "action": "inputvalue", "selector": sel }))
        }
        Some("attr") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get attr".to_string(),
                usage: "get attr <selector> <attribute>",
            })?;
            let attr = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "get attr".to_string(),
                usage: "get attr <selector> <attribute>",
            })?;
            Ok(json!({ "id": id, "action": "getattribute", "selector": sel, "attribute": attr }))
        }
        Some("url") => Ok(json!({ "id": id, "action": "url" })),
        Some("title") => Ok(json!({ "id": id, "action": "title" })),
        Some("count") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get count".to_string(),
                usage: "get count <selector>",
            })?;
            Ok(json!({ "id": id, "action": "count", "selector": sel }))
        }
        Some("box") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get box".to_string(),
                usage: "get box <selector>",
            })?;
            Ok(json!({ "id": id, "action": "boundingbox", "selector": sel }))
        }
        Some("styles") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "get styles".to_string(),
                usage: "get styles <selector>",
            })?;
            Ok(json!({ "id": id, "action": "styles", "selector": sel }))
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "get".to_string(),
            usage: "get <text|html|value|attr|url|title|count|box|styles> [args...]",
        }),
    }
}

fn parse_is(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["visible", "enabled", "checked"];
    
    match rest.get(0).map(|s| *s) {
        Some("visible") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "is visible".to_string(),
                usage: "is visible <selector>",
            })?;
            Ok(json!({ "id": id, "action": "isvisible", "selector": sel }))
        }
        Some("enabled") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "is enabled".to_string(),
                usage: "is enabled <selector>",
            })?;
            Ok(json!({ "id": id, "action": "isenabled", "selector": sel }))
        }
        Some("checked") => {
            let sel = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "is checked".to_string(),
                usage: "is checked <selector>",
            })?;
            Ok(json!({ "id": id, "action": "ischecked", "selector": sel }))
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "is".to_string(),
            usage: "is <visible|enabled|checked> <selector>",
        }),
    }
}

fn parse_find(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["role", "text", "label", "placeholder", "alt", "title", "testid", "first", "last", "nth"];
    
    let locator = rest.get(0).ok_or_else(|| ParseError::MissingArguments {
        context: "find".to_string(),
        usage: "find <locator> <value> [action] [text]",
    })?;
    
    let name_idx = rest.iter().position(|&s| s == "--name");
    let name = name_idx.and_then(|i| rest.get(i + 1).map(|s| *s));
    let exact = rest.iter().any(|&s| s == "--exact");

    match *locator {
        "role" | "text" | "label" | "placeholder" | "alt" | "title" | "testid" | "first" | "last" => {
            let value = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: format!("find {}", locator),
                usage: match *locator {
                    "role" => "find role <role> [action] [--name <name>] [--exact]",
                    "text" => "find text <text> [action] [--exact]",
                    "label" => "find label <label> [action] [text] [--exact]",
                    "placeholder" => "find placeholder <text> [action] [text] [--exact]",
                    "alt" => "find alt <text> [action] [--exact]",
                    "title" => "find title <text> [action] [--exact]",
                    "testid" => "find testid <id> [action] [text]",
                    "first" => "find first <selector> [action] [text]",
                    "last" => "find last <selector> [action] [text]",
                    _ => "find <locator> <value> [action] [text]",
                },
            })?;
            let subaction = rest.get(2).unwrap_or(&"click");
            let fill_value = if rest.len() > 3 {
                Some(rest[3..].join(" "))
            } else {
                None
            };

            match *locator {
                "role" => {
                    let mut cmd = json!({ "id": id, "action": "getbyrole", "role": value, "subaction": subaction, "name": name, "exact": exact });
                    if let Some(v) = fill_value { cmd["value"] = json!(v); }
                    Ok(cmd)
                }
                "text" => Ok(json!({ "id": id, "action": "getbytext", "text": value, "subaction": subaction, "exact": exact })),
                "label" => {
                    let mut cmd = json!({ "id": id, "action": "getbylabel", "label": value, "subaction": subaction, "exact": exact });
                    if let Some(v) = fill_value { cmd["value"] = json!(v); }
                    Ok(cmd)
                }
                "placeholder" => {
                    let mut cmd = json!({ "id": id, "action": "getbyplaceholder", "placeholder": value, "subaction": subaction, "exact": exact });
                    if let Some(v) = fill_value { cmd["value"] = json!(v); }
                    Ok(cmd)
                }
                "alt" => Ok(json!({ "id": id, "action": "getbyalttext", "text": value, "subaction": subaction, "exact": exact })),
                "title" => Ok(json!({ "id": id, "action": "getbytitle", "text": value, "subaction": subaction, "exact": exact })),
                "testid" => {
                    let mut cmd = json!({ "id": id, "action": "getbytestid", "testId": value, "subaction": subaction });
                    if let Some(v) = fill_value { cmd["value"] = json!(v); }
                    Ok(cmd)
                }
                "first" => {
                    let mut cmd = json!({ "id": id, "action": "nth", "selector": value, "index": 0, "subaction": subaction });
                    if let Some(v) = fill_value { cmd["value"] = json!(v); }
                    Ok(cmd)
                }
                "last" => {
                    let mut cmd = json!({ "id": id, "action": "nth", "selector": value, "index": -1, "subaction": subaction });
                    if let Some(v) = fill_value { cmd["value"] = json!(v); }
                    Ok(cmd)
                }
                _ => unreachable!(),
            }
        }
        "nth" => {
            let idx_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "find nth".to_string(),
                usage: "find nth <index> <selector> [action] [text]",
            })?;
            let idx = idx_str.parse::<i32>().map_err(|_| ParseError::MissingArguments {
                context: "find nth".to_string(),
                usage: "find nth <index> <selector> [action] [text]",
            })?;
            let sel = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "find nth".to_string(),
                usage: "find nth <index> <selector> [action] [text]",
            })?;
            let sub = rest.get(3).unwrap_or(&"click");
            let fv = if rest.len() > 4 {
                Some(rest[4..].join(" "))
            } else {
                None
            };
            let mut cmd = json!({ "id": id, "action": "nth", "selector": sel, "index": idx, "subaction": sub });
            if let Some(v) = fv { cmd["value"] = json!(v); }
            Ok(cmd)
        }
        _ => Err(ParseError::UnknownSubcommand {
            subcommand: locator.to_string(),
            valid_options: VALID,
        }),
    }
}

fn parse_mouse(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["move", "down", "up", "wheel"];
    
    match rest.get(0).map(|s| *s) {
        Some("move") => {
            let x_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            let y_str = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            let x = x_str.parse::<i32>().map_err(|_| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            let y = y_str.parse::<i32>().map_err(|_| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            Ok(json!({ "id": id, "action": "mousemove", "x": x, "y": y }))
        }
        Some("down") => {
            Ok(json!({ "id": id, "action": "mousedown", "button": rest.get(1).unwrap_or(&"left") }))
        }
        Some("up") => {
            Ok(json!({ "id": id, "action": "mouseup", "button": rest.get(1).unwrap_or(&"left") }))
        }
        Some("wheel") => {
            let dy = rest.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(100);
            let dx = rest.get(2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            Ok(json!({ "id": id, "action": "wheel", "deltaX": dx, "deltaY": dy }))
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "mouse".to_string(),
            usage: "mouse <move|down|up|wheel> [args...]",
        }),
    }
}

fn parse_set(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["viewport", "device", "geo", "geolocation", "offline", "headers", "credentials", "auth", "media"];
    
    match rest.get(0).map(|s| *s) {
        Some("viewport") => {
            let w_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set viewport".to_string(),
                usage: "set viewport <width> <height>",
            })?;
            let h_str = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "set viewport".to_string(),
                usage: "set viewport <width> <height>",
            })?;
            let w = w_str.parse::<i32>().map_err(|_| ParseError::MissingArguments {
                context: "set viewport".to_string(),
                usage: "set viewport <width> <height>",
            })?;
            let h = h_str.parse::<i32>().map_err(|_| ParseError::MissingArguments {
                context: "set viewport".to_string(),
                usage: "set viewport <width> <height>",
            })?;
            Ok(json!({ "id": id, "action": "viewport", "width": w, "height": h }))
        }
        Some("device") => {
            let dev = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set device".to_string(),
                usage: "set device <name>",
            })?;
            Ok(json!({ "id": id, "action": "device", "device": dev }))
        }
        Some("geo") | Some("geolocation") => {
            let lat_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set geo".to_string(),
                usage: "set geo <latitude> <longitude>",
            })?;
            let lng_str = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "set geo".to_string(),
                usage: "set geo <latitude> <longitude>",
            })?;
            let lat = lat_str.parse::<f64>().map_err(|_| ParseError::MissingArguments {
                context: "set geo".to_string(),
                usage: "set geo <latitude> <longitude>",
            })?;
            let lng = lng_str.parse::<f64>().map_err(|_| ParseError::MissingArguments {
                context: "set geo".to_string(),
                usage: "set geo <latitude> <longitude>",
            })?;
            Ok(json!({ "id": id, "action": "geolocation", "latitude": lat, "longitude": lng }))
        }
        Some("offline") => {
            let off = rest.get(1).map(|s| *s != "off" && *s != "false").unwrap_or(true);
            Ok(json!({ "id": id, "action": "offline", "offline": off }))
        }
        Some("headers") => {
            let headers_json = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set headers".to_string(),
                usage: "set headers <json>",
            })?;
            // Parse the JSON string into an object
            let headers: serde_json::Value = serde_json::from_str(headers_json)
                .map_err(|_| ParseError::MissingArguments {
                    context: "set headers".to_string(),
                    usage: "set headers <json> (must be valid JSON object)",
                })?;
            Ok(json!({ "id": id, "action": "headers", "headers": headers }))
        }
        Some("credentials") | Some("auth") => {
            let user = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set credentials".to_string(),
                usage: "set credentials <username> <password>",
            })?;
            let pass = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "set credentials".to_string(),
                usage: "set credentials <username> <password>",
            })?;
            Ok(json!({ "id": id, "action": "credentials", "username": user, "password": pass }))
        }
        Some("media") => {
            let color = if rest.iter().any(|&s| s == "dark") {
                "dark"
            } else if rest.iter().any(|&s| s == "light") {
                "light"
            } else {
                "no-preference"
            };
            let reduced = if rest.iter().any(|&s| s == "reduced-motion") {
                "reduce"
            } else {
                "no-preference"
            };
            Ok(json!({ "id": id, "action": "emulatemedia", "colorScheme": color, "reducedMotion": reduced }))
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "set".to_string(),
            usage: "set <viewport|device|geo|offline|headers|credentials|media> [args...]",
        }),
    }
}

fn parse_network(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["route", "unroute", "requests"];
    
    match rest.get(0).map(|s| *s) {
        Some("route") => {
            let url = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "network route".to_string(),
                usage: "network route <url> [--abort|--body <json>]",
            })?;
            let abort = rest.iter().any(|&s| s == "--abort");
            let body_idx = rest.iter().position(|&s| s == "--body");
            let body = body_idx.and_then(|i| rest.get(i + 1).map(|s| *s));
            Ok(json!({ "id": id, "action": "route", "url": url, "abort": abort, "body": body }))
        }
        Some("unroute") => {
            let mut cmd = json!({ "id": id, "action": "unroute" });
            if let Some(url) = rest.get(1) {
                cmd["url"] = json!(url);
            }
            Ok(cmd)
        },
        Some("requests") => {
            let clear = rest.iter().any(|&s| s == "--clear");
            let filter_idx = rest.iter().position(|&s| s == "--filter");
            let filter = filter_idx.and_then(|i| rest.get(i + 1).map(|s| *s));
            let mut cmd = json!({ "id": id, "action": "requests", "clear": clear });
            if let Some(f) = filter {
                cmd["filter"] = json!(f);
            }
            Ok(cmd)
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "network".to_string(),
            usage: "network <route|unroute|requests> [args...]",
        }),
    }
}

fn parse_storage(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["local", "session"];
    
    match rest.get(0).map(|s| *s) {
        Some("local") | Some("session") => {
            let storage_type = rest.get(0).unwrap();
            let op = rest.get(1).unwrap_or(&"get");
            let key = rest.get(2);
            let value = rest.get(3);
            match *op {
                "set" => {
                    let k = key.ok_or_else(|| ParseError::MissingArguments {
                        context: format!("storage {} set", storage_type),
                        usage: "storage <local|session> set <key> <value>",
                    })?;
                    let v = value.ok_or_else(|| ParseError::MissingArguments {
                        context: format!("storage {} set", storage_type),
                        usage: "storage <local|session> set <key> <value>",
                    })?;
                    Ok(json!({ "id": id, "action": "storage_set", "type": storage_type, "key": k, "value": v }))
                }
                "clear" => Ok(json!({ "id": id, "action": "storage_clear", "type": storage_type })),
                _ => {
                    let mut cmd = json!({ "id": id, "action": "storage_get", "type": storage_type });
                    if let Some(k) = key {
                        cmd.as_object_mut().unwrap().insert("key".to_string(), json!(k));
                    }
                    Ok(cmd)
                }
            }
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "storage".to_string(),
            usage: "storage <local|session> [get|set|clear] [key] [value]",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_flags() -> Flags {
        Flags {
            session: "test".to_string(),
            json: false,
            full: false,
            headed: false,
            debug: false,
            headers: None,
            executable_path: None,
            extensions: Vec::new(),
            cdp: None,
            proxy: None,
        }
    }

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    // === Cookies Tests ===

    #[test]
    fn test_cookies_get() {
        let cmd = parse_command(&args("cookies"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_get");
    }

    #[test]
    fn test_cookies_get_explicit() {
        let cmd = parse_command(&args("cookies get"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_get");
    }

    #[test]
    fn test_cookies_set() {
        let cmd = parse_command(&args("cookies set mycookie myvalue"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
    }

    #[test]
    fn test_cookies_set_missing_value() {
        let result = parse_command(&args("cookies set mycookie"), &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_cookies_clear() {
        let cmd = parse_command(&args("cookies clear"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_clear");
    }

    // === Storage Tests ===

    #[test]
    fn test_storage_local_get() {
        let cmd = parse_command(&args("storage local"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "local");
        assert!(cmd.get("key").is_none());
    }

    #[test]
    fn test_storage_local_get_key() {
        let cmd = parse_command(&args("storage local get mykey"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "local");
        assert_eq!(cmd["key"], "mykey");
    }

    #[test]
    fn test_storage_session_get() {
        let cmd = parse_command(&args("storage session"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_get");
        assert_eq!(cmd["type"], "session");
    }

    #[test]
    fn test_storage_local_set() {
        let cmd = parse_command(&args("storage local set mykey myvalue"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_set");
        assert_eq!(cmd["type"], "local");
        assert_eq!(cmd["key"], "mykey");
        assert_eq!(cmd["value"], "myvalue");
    }

    #[test]
    fn test_storage_session_set() {
        let cmd = parse_command(&args("storage session set skey svalue"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_set");
        assert_eq!(cmd["type"], "session");
        assert_eq!(cmd["key"], "skey");
        assert_eq!(cmd["value"], "svalue");
    }

    #[test]
    fn test_storage_set_missing_value() {
        let result = parse_command(&args("storage local set mykey"), &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_storage_local_clear() {
        let cmd = parse_command(&args("storage local clear"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_clear");
        assert_eq!(cmd["type"], "local");
    }

    #[test]
    fn test_storage_session_clear() {
        let cmd = parse_command(&args("storage session clear"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_clear");
        assert_eq!(cmd["type"], "session");
    }

    #[test]
    fn test_storage_invalid_type() {
        let result = parse_command(&args("storage invalid"), &default_flags());
        assert!(result.is_err());
    }

    // === Navigation Tests ===

    #[test]
    fn test_navigate_with_https() {
        let cmd = parse_command(&args("open https://example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_navigate_without_protocol() {
        let cmd = parse_command(&args("open example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_navigate_with_headers() {
        let mut flags = default_flags();
        flags.headers = Some(r#"{"Authorization": "Bearer token"}"#.to_string());
        let cmd = parse_command(&args("open api.example.com"), &flags).unwrap();
        assert_eq!(cmd["action"], "navigate");
        assert_eq!(cmd["url"], "https://api.example.com");
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
    }

    #[test]
    fn test_navigate_with_multiple_headers() {
        let mut flags = default_flags();
        flags.headers = Some(r#"{"Authorization": "Bearer token", "X-Custom": "value"}"#.to_string());
        let cmd = parse_command(&args("open api.example.com"), &flags).unwrap();
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
        assert_eq!(cmd["headers"]["X-Custom"], "value");
    }

    #[test]
    fn test_navigate_without_headers_flag() {
        let cmd = parse_command(&args("open example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "navigate");
        // headers should not be present when flag is not set
        assert!(cmd.get("headers").is_none());
    }

    #[test]
    fn test_navigate_with_invalid_headers_json() {
        let mut flags = default_flags();
        flags.headers = Some("not valid json".to_string());
        let cmd = parse_command(&args("open api.example.com"), &flags).unwrap();
        // Invalid JSON should result in no headers field (graceful handling)
        assert!(cmd.get("headers").is_none());
    }

    // === Set Headers Tests ===

    #[test]
    fn test_set_headers_parses_json() {
        let input: Vec<String> = vec![
            "set".to_string(),
            "headers".to_string(),
            r#"{"Authorization":"Bearer token"}"#.to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "headers");
        // Headers should be an object, not a string
        assert!(cmd["headers"].is_object());
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
    }

    #[test]
    fn test_set_headers_with_multiple_values() {
        let input: Vec<String> = vec![
            "set".to_string(),
            "headers".to_string(),
            r#"{"Authorization": "Bearer token", "X-Custom": "value"}"#.to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["headers"]["Authorization"], "Bearer token");
        assert_eq!(cmd["headers"]["X-Custom"], "value");
    }

    #[test]
    fn test_set_headers_invalid_json_error() {
        let input: Vec<String> = vec![
            "set".to_string(),
            "headers".to_string(),
            "not-valid-json".to_string(),
        ];
        let result = parse_command(&input, &default_flags());
        assert!(result.is_err());
    }

    #[test]
    fn test_back() {
        let cmd = parse_command(&args("back"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "back");
    }

    #[test]
    fn test_forward() {
        let cmd = parse_command(&args("forward"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "forward");
    }

    #[test]
    fn test_reload() {
        let cmd = parse_command(&args("reload"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "reload");
    }

    // === Core Actions ===

    #[test]
    fn test_click() {
        let cmd = parse_command(&args("click #button"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "click");
        assert_eq!(cmd["selector"], "#button");
    }

    #[test]
    fn test_fill() {
        let cmd = parse_command(&args("fill #input hello world"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "fill");
        assert_eq!(cmd["selector"], "#input");
        assert_eq!(cmd["value"], "hello world");
    }

    #[test]
    fn test_type_command() {
        let cmd = parse_command(&args("type #input some text"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "type");
        assert_eq!(cmd["selector"], "#input");
        assert_eq!(cmd["text"], "some text");
    }

    #[test]
    fn test_select() {
        let cmd = parse_command(&args("select #menu option1"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "select");
        assert_eq!(cmd["selector"], "#menu");
        assert_eq!(cmd["values"], "option1");
    }

    #[test]
    fn test_select_multiple_values() {
        let cmd = parse_command(
            &args("select #menu opt1 opt2 opt3"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "select");
        assert_eq!(cmd["selector"], "#menu");
        assert_eq!(cmd["values"], json!(["opt1", "opt2", "opt3"]));
    }

    #[test]
    fn test_frame_main() {
        let cmd = parse_command(&args("frame main"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "mainframe");
    }

    // === Tabs ===

    #[test]
    fn test_tab_new() {
        let cmd = parse_command(&args("tab new"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_new");
        assert!(cmd.get("url").is_none(), "url should not be present when not provided");
    }

    #[test]
    fn test_tab_new_with_url() {
        let cmd = parse_command(&args("tab new https://example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_new");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_tab_list() {
        let cmd = parse_command(&args("tab list"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_list");
    }

    #[test]
    fn test_tab_switch() {
        let cmd = parse_command(&args("tab 2"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_switch");
        assert_eq!(cmd["index"], 2);
    }

    #[test]
    fn test_tab_close() {
        let cmd = parse_command(&args("tab close"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_close");
    }

    // === Screenshot ===

    #[test]
    fn test_screenshot() {
        let cmd = parse_command(&args("screenshot"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert!(cmd.get("path").is_none());
    }

    #[test]
    fn test_screenshot_path() {
        let cmd = parse_command(&args("screenshot out.png"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["path"], "out.png");
    }

    #[test]
    fn test_screenshot_full_page() {
        let mut flags = default_flags();
        flags.full = true;
        let cmd = parse_command(&args("screenshot"), &flags).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["fullPage"], true);
    }

    // === Snapshot ===

    #[test]
    fn test_snapshot() {
        let cmd = parse_command(&args("snapshot"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
    }

    #[test]
    fn test_snapshot_interactive() {
        let cmd = parse_command(&args("snapshot -i"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["interactive"], true);
    }

    #[test]
    fn test_snapshot_compact() {
        let cmd = parse_command(&args("snapshot --compact"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["compact"], true);
    }

    #[test]
    fn test_snapshot_depth() {
        let cmd = parse_command(&args("snapshot -d 3"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["maxDepth"], 3);
    }

    // === Wait ===

    #[test]
    fn test_wait_selector() {
        let cmd = parse_command(&args("wait #element"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["selector"], "#element");
    }

    #[test]
    fn test_wait_timeout() {
        let cmd = parse_command(&args("wait 5000"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["timeout"], 5000);
    }

    #[test]
    fn test_wait_url() {
        let cmd = parse_command(&args("wait --url **/dashboard"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitforurl");
        assert_eq!(cmd["url"], "**/dashboard");
    }

    #[test]
    fn test_wait_load() {
        let cmd = parse_command(&args("wait --load networkidle"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitforloadstate");
        assert_eq!(cmd["state"], "networkidle");
    }

    #[test]
    fn test_wait_load_missing_state() {
        let result = parse_command(&args("wait --load"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::MissingArguments { .. }));
    }

    #[test]
    fn test_wait_fn() {
        let cmd = parse_command(&args("wait --fn window.ready"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitforfunction");
        assert_eq!(cmd["expression"], "window.ready");
    }

    #[test]
    fn test_wait_text() {
        let cmd = parse_command(&args("wait --text Welcome"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wait");
        assert_eq!(cmd["selector"], "text=Welcome");
    }

    // === Unknown command ===

    // === Record Tests ===

    #[test]
    fn test_record_start() {
        let cmd = parse_command(&args("record start output.webm"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "output.webm");
        assert!(cmd.get("url").is_none());
    }

    #[test]
    fn test_record_start_with_url() {
        let cmd = parse_command(&args("record start demo.webm https://example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_start_with_url_no_protocol() {
        let cmd = parse_command(&args("record start demo.webm example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_start_missing_path() {
        let result = parse_command(&args("record start"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::MissingArguments { .. }));
    }

    #[test]
    fn test_record_stop() {
        let cmd = parse_command(&args("record stop"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_stop");
    }

    #[test]
    fn test_record_restart() {
        let cmd = parse_command(&args("record restart output.webm"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_restart");
        assert_eq!(cmd["path"], "output.webm");
        assert!(cmd.get("url").is_none());
    }

    #[test]
    fn test_record_restart_with_url() {
        let cmd = parse_command(&args("record restart demo.webm https://example.com"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "recording_restart");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_restart_missing_path() {
        let result = parse_command(&args("record restart"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::MissingArguments { .. }));
    }

    #[test]
    fn test_record_invalid_subcommand() {
        let result = parse_command(&args("record foo"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::UnknownSubcommand { .. }));
    }

    #[test]
    fn test_record_missing_subcommand() {
        let result = parse_command(&args("record"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::MissingArguments { .. }));
    }

    #[test]
    fn test_unknown_command() {
        let result = parse_command(&args("unknowncommand"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::UnknownCommand { .. }));
    }

    #[test]
    fn test_empty_args() {
        let result = parse_command(&[], &default_flags());
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::MissingArguments { .. }));
    }

    // === Error message tests ===

    #[test]
    fn test_get_missing_subcommand() {
        let result = parse_command(&args("get"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::MissingArguments { .. }));
        assert!(err.format().contains("get"));
    }

    #[test]
    fn test_get_unknown_subcommand() {
        let result = parse_command(&args("get foo"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::UnknownSubcommand { .. }));
        assert!(err.format().contains("foo"));
        assert!(err.format().contains("text"));
    }

    #[test]
    fn test_get_text_missing_selector() {
        let result = parse_command(&args("get text"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::MissingArguments { .. }));
        assert!(err.format().contains("get text"));
    }

    // === Protocol alignment tests ===

    #[test]
    fn test_mouse_wheel() {
        let cmd = parse_command(&args("mouse wheel 100 50"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "wheel");
        assert_eq!(cmd["deltaY"], 100);
        assert_eq!(cmd["deltaX"], 50);
    }

    #[test]
    fn test_set_media() {
        let cmd = parse_command(&args("set media dark"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "emulatemedia");
        assert_eq!(cmd["colorScheme"], "dark");
        assert_eq!(cmd["reducedMotion"], "no-preference");
    }

    #[test]
    fn test_set_media_reduced_motion() {
        let cmd = parse_command(&args("set media light reduced-motion"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "emulatemedia");
        assert_eq!(cmd["colorScheme"], "light");
        assert_eq!(cmd["reducedMotion"], "reduce");
    }

    #[test]
    fn test_find_first_no_value() {
        let cmd = parse_command(&args("find first a click"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "nth");
        assert_eq!(cmd["index"], 0);
        assert!(cmd.get("value").is_none());
    }

    #[test]
    fn test_find_first_with_value() {
        let cmd = parse_command(&args("find first input fill hello"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "nth");
        assert_eq!(cmd["index"], 0);
        assert_eq!(cmd["value"], "hello");
    }

    #[test]
    fn test_find_nth_no_value() {
        let cmd = parse_command(&args("find nth 2 a click"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "nth");
        assert_eq!(cmd["index"], 2);
        assert!(cmd.get("value").is_none());
    }
}
