use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::io::{self, BufRead};

use crate::color;
use crate::flags::Flags;
use crate::validation::{is_valid_session_name, session_name_error};

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
    /// Argument exists but has an invalid value
    InvalidValue {
        message: String,
        usage: &'static str,
    },
    /// Invalid session name (path traversal or invalid characters)
    InvalidSessionName { name: String },
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
            ParseError::InvalidValue { message, usage } => {
                format!("{}\nUsage: agent-browser {}", message, usage)
            }
            ParseError::InvalidSessionName { name } => session_name_error(name),
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

    if flags.annotate && cmd != "screenshot" {
        eprintln!(
            "{} --annotate only applies to the screenshot command",
            color::warning_indicator()
        );
    }

    match cmd {
        // === Navigation ===
        "open" | "goto" | "navigate" => {
            let url = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: cmd.to_string(),
                usage: "open <url>",
            })?;
            let url_lower = url.to_lowercase();
            let url = if url_lower.starts_with("http://")
                || url_lower.starts_with("https://")
                || url_lower.starts_with("about:")
                || url_lower.starts_with("data:")
                || url_lower.starts_with("file:")
            {
                url.to_string()
            } else {
                format!("https://{}", url)
            };
            let mut nav_cmd = json!({ "id": id, "action": "navigate", "url": url });
            // If --headers flag is set, include headers (scoped to this origin)
            if let Some(ref headers_json) = flags.headers {
                let headers = serde_json::from_str::<serde_json::Value>(headers_json)
                    .map_err(|_| ParseError::InvalidValue {
                        message: format!("Invalid JSON for --headers: {}", headers_json),
                        usage: "open <url> --headers '{\"Key\": \"Value\"}'",
                    })?;
                nav_cmd["headers"] = headers;
            }
            // Include iOS device info if specified (needed for auto-launch with existing daemon)
            if flags.provider.as_deref() == Some("ios") {
                if let Some(ref device) = flags.device {
                    nav_cmd["iosDevice"] = json!(device);
                }
            }
            Ok(nav_cmd)
        }
        "back" => Ok(json!({ "id": id, "action": "back" })),
        "forward" => Ok(json!({ "id": id, "action": "forward" })),
        "reload" => Ok(json!({ "id": id, "action": "reload" })),

        // === Core Actions ===
        "click" => {
            let new_tab = rest.iter().any(|arg| *arg == "--new-tab");
            let sel = rest
                .iter()
                .find(|arg| **arg != "--new-tab")
                .ok_or_else(|| ParseError::MissingArguments {
                    context: "click".to_string(),
                    usage: "click <selector> [--new-tab]",
                })?;
            if new_tab {
                Ok(json!({ "id": id, "action": "click", "selector": sel, "newTab": true }))
            } else {
                Ok(json!({ "id": id, "action": "click", "selector": sel }))
            }
        }
        "dblclick" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "dblclick".to_string(),
                usage: "dblclick <selector>",
            })?;
            Ok(json!({ "id": id, "action": "dblclick", "selector": sel }))
        }
        "fill" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "fill".to_string(),
                usage: "fill <selector> <text>",
            })?;
            Ok(json!({ "id": id, "action": "fill", "selector": sel, "value": rest[1..].join(" ") }))
        }
        "type" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "type".to_string(),
                usage: "type <selector> <text>",
            })?;
            Ok(json!({ "id": id, "action": "type", "selector": sel, "text": rest[1..].join(" ") }))
        }
        "hover" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "hover".to_string(),
                usage: "hover <selector>",
            })?;
            Ok(json!({ "id": id, "action": "hover", "selector": sel }))
        }
        "focus" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "focus".to_string(),
                usage: "focus <selector>",
            })?;
            Ok(json!({ "id": id, "action": "focus", "selector": sel }))
        }
        "check" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "check".to_string(),
                usage: "check <selector>",
            })?;
            Ok(json!({ "id": id, "action": "check", "selector": sel }))
        }
        "uncheck" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "uncheck".to_string(),
                usage: "uncheck <selector>",
            })?;
            Ok(json!({ "id": id, "action": "uncheck", "selector": sel }))
        }
        "select" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
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
            let src = rest.first().ok_or_else(|| ParseError::MissingArguments {
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
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "upload".to_string(),
                usage: "upload <selector> <files...>",
            })?;
            Ok(json!({ "id": id, "action": "upload", "selector": sel, "files": &rest[1..] }))
        }
        "download" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "download".to_string(),
                usage: "download <selector> <path>",
            })?;
            let path = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "download".to_string(),
                usage: "download <selector> <path>",
            })?;
            Ok(json!({ "id": id, "action": "download", "selector": sel, "path": path }))
        }

        // === Keyboard ===
        "press" | "key" => {
            let key = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "press".to_string(),
                usage: "press <key>",
            })?;
            Ok(json!({ "id": id, "action": "press", "key": key }))
        }
        "keydown" => {
            let key = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "keydown".to_string(),
                usage: "keydown <key>",
            })?;
            Ok(json!({ "id": id, "action": "keydown", "key": key }))
        }
        "keyup" => {
            let key = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "keyup".to_string(),
                usage: "keyup <key>",
            })?;
            Ok(json!({ "id": id, "action": "keyup", "key": key }))
        }

        // === Scroll ===
        "scroll" => {
            let dir = rest.first().unwrap_or(&"down");
            let amount = rest
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(300);
            Ok(json!({ "id": id, "action": "scroll", "direction": dir, "amount": amount }))
        }
        "scrollintoview" | "scrollinto" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "scrollintoview".to_string(),
                usage: "scrollintoview <selector>",
            })?;
            Ok(json!({ "id": id, "action": "scrollintoview", "selector": sel }))
        }

        // === Wait ===
        "wait" => {
            // Check for --url flag: wait --url "**/dashboard"
            if let Some(idx) = rest.iter().position(|&s| s == "--url" || s == "-u") {
                let url = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --url".to_string(),
                        usage: "wait --url <pattern>",
                    })?;
                return Ok(json!({ "id": id, "action": "waitforurl", "url": url }));
            }

            // Check for --load flag: wait --load networkidle
            if let Some(idx) = rest.iter().position(|&s| s == "--load" || s == "-l") {
                let state = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --load".to_string(),
                        usage: "wait --load <state>",
                    })?;
                return Ok(json!({ "id": id, "action": "waitforloadstate", "state": state }));
            }

            // Check for --fn flag: wait --fn "window.ready === true"
            if let Some(idx) = rest.iter().position(|&s| s == "--fn" || s == "-f") {
                let expr = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --fn".to_string(),
                        usage: "wait --fn <expression>",
                    })?;
                return Ok(json!({ "id": id, "action": "waitforfunction", "expression": expr }));
            }

            // Check for --text flag: wait --text "Welcome"
            if let Some(idx) = rest.iter().position(|&s| s == "--text" || s == "-t") {
                let text = rest
                    .get(idx + 1)
                    .ok_or_else(|| ParseError::MissingArguments {
                        context: "wait --text".to_string(),
                        usage: "wait --text <text>",
                    })?;
                // Use getByText locator to wait for text to appear
                return Ok(
                    json!({ "id": id, "action": "wait", "selector": format!("text={}", text) }),
                );
            }

            // Check for --download flag: wait --download [path] [--timeout ms]
            if rest.iter().any(|&s| s == "--download" || s == "-d") {
                let mut cmd = json!({ "id": id, "action": "waitfordownload" });
                // Check for optional path (first non-flag argument after --download)
                let download_idx = rest
                    .iter()
                    .position(|&s| s == "--download" || s == "-d")
                    .unwrap();
                if let Some(path) = rest.get(download_idx + 1) {
                    if !path.starts_with("--") {
                        cmd["path"] = json!(path);
                    }
                }
                // Check for optional timeout
                if let Some(idx) = rest.iter().position(|&s| s == "--timeout") {
                    if let Some(timeout_str) = rest.get(idx + 1) {
                        if let Ok(timeout) = timeout_str.parse::<u64>() {
                            cmd["timeout"] = json!(timeout);
                        }
                    }
                }
                return Ok(cmd);
            }

            // Default: selector or timeout
            if let Some(arg) = rest.first() {
                if let Ok(timeout) = arg.parse::<u64>() {
                    Ok(json!({ "id": id, "action": "wait", "timeout": timeout }))
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
            // screenshot [selector] [path]
            // selector: @ref or CSS selector
            // path: file path (contains / or . or ends with known extension)
            let (selector, path) = match (rest.first(), rest.get(1)) {
                (Some(first), Some(second)) => {
                    // Two args: first is selector, second is path
                    (Some(*first), Some(*second))
                }
                (Some(first), None) => {
                    // One arg: determine if it's a selector or a path
                    let is_relative_path = first.starts_with("./") || first.starts_with("../");
                    let is_selector = !is_relative_path
                        && (first.starts_with('.')
                            || first.starts_with('#')
                            || first.starts_with('@'));
                    let has_path_extension = first.ends_with(".png")
                        || first.ends_with(".jpg")
                        || first.ends_with(".jpeg")
                        || first.ends_with(".webp");
                    let is_path = is_relative_path || first.contains('/') || has_path_extension;
                    if is_selector || !is_path {
                        (Some(*first), None)
                    } else {
                        (None, Some(*first))
                    }
                }
                _ => (None, None),
            };
            Ok(
                json!({ "id": id, "action": "screenshot", "path": path, "selector": selector, "fullPage": flags.full, "annotate": flags.annotate }),
            )
        }
        "pdf" => {
            let path = rest.first().ok_or_else(|| ParseError::MissingArguments {
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
                    "-C" | "--cursor" => {
                        obj.insert("cursor".to_string(), json!(true));
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
        "eval" => {
            // Check for flags: -b/--base64 or --stdin
            let (is_base64, is_stdin, script_parts): (bool, bool, &[&str]) =
                if rest.first() == Some(&"-b") || rest.first() == Some(&"--base64") {
                    (true, false, &rest[1..])
                } else if rest.first() == Some(&"--stdin") {
                    (false, true, &rest[1..])
                } else {
                    (false, false, rest.as_slice())
                };

            let script = if is_stdin {
                // Read script from stdin
                let stdin = io::stdin();
                let lines: Vec<String> = stdin
                    .lock()
                    .lines()
                    .map(|l| l.unwrap_or_default())
                    .collect();
                lines.join("\n")
            } else {
                let raw_script = script_parts.join(" ");
                if is_base64 {
                    let decoded =
                        STANDARD
                            .decode(&raw_script)
                            .map_err(|_| ParseError::InvalidValue {
                                message: "Invalid base64 encoding".to_string(),
                                usage: "eval -b <base64-encoded-script>",
                            })?;
                    String::from_utf8(decoded).map_err(|_| ParseError::InvalidValue {
                        message: "Base64 decoded to invalid UTF-8".to_string(),
                        usage: "eval -b <base64-encoded-script>",
                    })?
                } else {
                    raw_script
                }
            };
            Ok(json!({ "id": id, "action": "evaluate", "script": script }))
        }

        // === Close ===
        "close" | "quit" | "exit" => Ok(json!({ "id": id, "action": "close" })),

        // === Connect (CDP) ===
        "connect" => {
            let endpoint = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "connect".to_string(),
                usage: "connect <port|url>",
            })?;
            // Check if it's a URL (ws://, wss://, http://, https://)
            if endpoint.starts_with("ws://")
                || endpoint.starts_with("wss://")
                || endpoint.starts_with("http://")
                || endpoint.starts_with("https://")
            {
                Ok(json!({ "id": id, "action": "launch", "cdpUrl": endpoint }))
            } else {
                // It's a port number - validate and use cdpPort field
                let port: u16 = match endpoint.parse::<u32>() {
                    Ok(0) => {
                        return Err(ParseError::InvalidValue {
                            message: "Invalid port: port must be greater than 0".to_string(),
                            usage: "connect <port|url>",
                        });
                    }
                    Ok(p) if p > 65535 => {
                        return Err(ParseError::InvalidValue {
                            message: format!(
                                "Invalid port: {} is out of range (valid range: 1-65535)",
                                p
                            ),
                            usage: "connect <port|url>",
                        });
                    }
                    Ok(p) => p as u16,
                    Err(_) => {
                        return Err(ParseError::InvalidValue {
                            message: format!(
                                "Invalid value: '{}' is not a valid port number or URL",
                                endpoint
                            ),
                            usage: "connect <port|url>",
                        });
                    }
                };
                Ok(json!({ "id": id, "action": "launch", "cdpPort": port }))
            }
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
            let op = rest.first().unwrap_or(&"get");
            match *op {
                "set" => {
                    let name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "cookies set".to_string(),
                        usage: "cookies set <name> <value> [--url <url>] [--domain <domain>] [--path <path>] [--httpOnly] [--secure] [--sameSite <Strict|Lax|None>] [--expires <timestamp>]",
                    })?;
                    let value = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                        context: "cookies set".to_string(),
                        usage: "cookies set <name> <value> [--url <url>] [--domain <domain>] [--path <path>] [--httpOnly] [--secure] [--sameSite <Strict|Lax|None>] [--expires <timestamp>]",
                    })?;

                    let mut cookie = json!({ "name": name, "value": value });

                    // Parse optional flags
                    let mut i = 3;
                    while i < rest.len() {
                        match rest[i] {
                            "--url" => {
                                if let Some(url) = rest.get(i + 1) {
                                    cookie["url"] = json!(url);
                                    i += 2;
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --url".to_string(),
                                        usage: "--url <url>",
                                    });
                                }
                            }
                            "--domain" => {
                                if let Some(domain) = rest.get(i + 1) {
                                    cookie["domain"] = json!(domain);
                                    i += 2;
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --domain".to_string(),
                                        usage: "--domain <domain>",
                                    });
                                }
                            }
                            "--path" => {
                                if let Some(path) = rest.get(i + 1) {
                                    cookie["path"] = json!(path);
                                    i += 2;
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --path".to_string(),
                                        usage: "--path <path>",
                                    });
                                }
                            }
                            "--httpOnly" => {
                                cookie["httpOnly"] = json!(true);
                                i += 1;
                            }
                            "--secure" => {
                                cookie["secure"] = json!(true);
                                i += 1;
                            }
                            "--sameSite" => {
                                if let Some(same_site) = rest.get(i + 1) {
                                    // Validate sameSite value
                                    if *same_site == "Strict"
                                        || *same_site == "Lax"
                                        || *same_site == "None"
                                    {
                                        cookie["sameSite"] = json!(same_site);
                                        i += 2;
                                    } else {
                                        return Err(ParseError::MissingArguments {
                                            context: "cookies set --sameSite".to_string(),
                                            usage: "--sameSite <Strict|Lax|None>",
                                        });
                                    }
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --sameSite".to_string(),
                                        usage: "--sameSite <Strict|Lax|None>",
                                    });
                                }
                            }
                            "--expires" => {
                                if let Some(expires_str) = rest.get(i + 1) {
                                    if let Ok(expires) = expires_str.parse::<i64>() {
                                        cookie["expires"] = json!(expires);
                                        i += 2;
                                    } else {
                                        return Err(ParseError::MissingArguments {
                                            context: "cookies set --expires".to_string(),
                                            usage: "--expires <timestamp>",
                                        });
                                    }
                                } else {
                                    return Err(ParseError::MissingArguments {
                                        context: "cookies set --expires".to_string(),
                                        usage: "--expires <timestamp>",
                                    });
                                }
                            }
                            _ => {
                                // Unknown flag, skip it (or could error)
                                i += 1;
                            }
                        }
                    }

                    Ok(json!({ "id": id, "action": "cookies_set", "cookies": [cookie] }))
                }
                "clear" => Ok(json!({ "id": id, "action": "cookies_clear" })),
                _ => Ok(json!({ "id": id, "action": "cookies_get" })),
            }
        }

        // === Tabs ===
        "tab" => match rest.first().copied() {
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
                let index = n.parse::<i32>().expect("already checked parse succeeds");
                Ok(json!({ "id": id, "action": "tab_switch", "index": index }))
            }
            _ => Ok(json!({ "id": id, "action": "tab_list" })),
        },

        // === Window ===
        "window" => {
            const VALID: &[&str] = &["new"];
            match rest.first().copied() {
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
            if rest.first().copied() == Some("main") {
                Ok(json!({ "id": id, "action": "mainframe" }))
            } else {
                let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                    context: "frame".to_string(),
                    usage: "frame <selector|main>",
                })?;
                Ok(json!({ "id": id, "action": "frame", "selector": sel }))
            }
        }

        // === Dialog ===
        "dialog" => {
            const VALID: &[&str] = &["accept", "dismiss"];
            match rest.first().copied() {
                Some("accept") => {
                    let mut cmd = json!({ "id": id, "action": "dialog", "response": "accept" });
                    if let Some(prompt_text) = rest.get(1) {
                        cmd["promptText"] = json!(prompt_text);
                    }
                    Ok(cmd)
                }
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
            match rest.first().copied() {
                Some("start") => Ok(json!({ "id": id, "action": "trace_start" })),
                Some("stop") => {
                    let mut cmd = json!({ "id": id, "action": "trace_stop" });
                    if let Some(path) = rest.get(1) {
                        cmd["path"] = json!(path);
                    }
                    Ok(cmd)
                }
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

        // === Profiler (CDP Tracing / Chromium profiling) ===
        "profiler" => {
            const VALID: &[&str] = &["start", "stop"];
            match rest.first().copied() {
                Some("start") => {
                    let mut cmd = json!({ "id": id, "action": "profiler_start" });
                    if let Some(idx) = rest.iter().position(|s| *s == "--categories") {
                        if let Some(cats) = rest.get(idx + 1) {
                            let categories: Vec<&str> = cats.split(',').collect();
                            cmd["categories"] = json!(categories);
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "profiler start --categories".to_string(),
                                usage: "--categories <list>",
                            });
                        }
                    }
                    Ok(cmd)
                }
                Some("stop") => {
                    let mut cmd = json!({ "id": id, "action": "profiler_stop" });
                    if let Some(path) = rest.get(1) {
                        cmd["path"] = json!(path);
                    }
                    Ok(cmd)
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "profiler".to_string(),
                    usage: "profiler <start|stop> [options]",
                }),
            }
        }

        // === Recording (Playwright native video recording) ===
        "record" => {
            const VALID: &[&str] = &["start", "stop", "restart"];
            match rest.first().copied() {
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
            let clear = rest.contains(&"--clear");
            Ok(json!({ "id": id, "action": "console", "clear": clear }))
        }
        "errors" => {
            let clear = rest.contains(&"--clear");
            Ok(json!({ "id": id, "action": "errors", "clear": clear }))
        }
        "highlight" => {
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "highlight".to_string(),
                usage: "highlight <selector>",
            })?;
            Ok(json!({ "id": id, "action": "highlight", "selector": sel }))
        }

        // === State ===
        "state" => {
            const VALID: &[&str] = &["save", "load", "list", "clear", "show", "clean", "rename"];
            match rest.first().copied() {
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
                Some("list") => {
                    Ok(json!({ "id": id, "action": "state_list" }))
                }
                Some("clear") => {
                    let mut session_name: Option<&str> = None;
                    let mut all = false;

                    let mut i = 1;
                    while i < rest.len() {
                        match rest[i] {
                            "--all" | "-a" => {
                                all = true;
                            }
                            arg if !arg.starts_with('-') => {
                                session_name = Some(arg);
                            }
                            _ => {}
                        }
                        i += 1;
                    }

                    if let Some(name) = session_name {
                        if !is_valid_session_name(name) {
                            return Err(ParseError::InvalidSessionName { name: name.to_string() });
                        }
                    }

                    let mut cmd = json!({ "id": id, "action": "state_clear" });
                    if all {
                        cmd["all"] = json!(true);
                    }
                    if let Some(name) = session_name {
                        cmd["sessionName"] = json!(name);
                    }
                    Ok(cmd)
                }
                Some("show") => {
                    let filename = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state show".to_string(),
                        usage: "state show <filename>",
                    })?;
                    Ok(json!({ "id": id, "action": "state_show", "filename": filename }))
                }
                Some("clean") => {
                    let mut days: Option<i64> = None;

                    let mut i = 1;
                    while i < rest.len() {
                        if rest[i] == "--older-than" {
                            if let Some(d) = rest.get(i + 1) {
                                days = d.parse().ok();
                                i += 1;
                            }
                        }
                        i += 1;
                    }

                    let days = days.ok_or_else(|| ParseError::MissingArguments {
                        context: "state clean".to_string(),
                        usage: "state clean --older-than <days>",
                    })?;

                    Ok(json!({ "id": id, "action": "state_clean", "days": days }))
                }
                Some("rename") => {
                    let old_name = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                        context: "state rename".to_string(),
                        usage: "state rename <old-name> <new-name>",
                    })?;
                    let new_name = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                        context: "state rename".to_string(),
                        usage: "state rename <old-name> <new-name>",
                    })?;
                    let old_name = old_name.trim_end_matches(".json");
                    let new_name = new_name.trim_end_matches(".json");

                    if !is_valid_session_name(old_name) {
                        return Err(ParseError::InvalidSessionName { name: old_name.to_string() });
                    }
                    if !is_valid_session_name(new_name) {
                        return Err(ParseError::InvalidSessionName { name: new_name.to_string() });
                    }

                    Ok(json!({ "id": id, "action": "state_rename", "oldName": old_name, "newName": new_name }))
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: VALID,
                }),
                None => Err(ParseError::MissingArguments {
                    context: "state".to_string(),
                    usage: "state <save|load|list|clear|show|clean|rename> ...",
                }),
            }
        }

        // === iOS-specific commands ===
        "tap" => {
            // Alias for click (semantic clarity for touch interfaces)
            let sel = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "tap".to_string(),
                usage: "tap <selector>",
            })?;
            Ok(json!({ "id": id, "action": "tap", "selector": sel }))
        }
        "swipe" => {
            let direction = rest.first().ok_or_else(|| ParseError::MissingArguments {
                context: "swipe".to_string(),
                usage: "swipe <up|down|left|right> [distance]",
            })?;
            let valid_directions = ["up", "down", "left", "right"];
            if !valid_directions.contains(direction) {
                return Err(ParseError::InvalidValue {
                    message: format!("Invalid swipe direction: {}", direction),
                    usage: "swipe <up|down|left|right> [distance]",
                });
            }
            let mut cmd = json!({ "id": id, "action": "swipe", "direction": direction });
            if let Some(distance) = rest.get(1) {
                if let Ok(d) = distance.parse::<u32>() {
                    cmd.as_object_mut()
                        .unwrap()
                        .insert("distance".to_string(), json!(d));
                }
            }
            Ok(cmd)
        }
        "device" => {
            match rest.first().copied() {
                Some("list") | None => {
                    // List available iOS simulators
                    Ok(json!({ "id": id, "action": "device_list" }))
                }
                Some(sub) => Err(ParseError::UnknownSubcommand {
                    subcommand: sub.to_string(),
                    valid_options: &["list"],
                }),
            }
        }

        "diff" => parse_diff(&rest, &id, flags),

        _ => Err(ParseError::UnknownCommand {
            command: cmd.to_string(),
        }),
    }
}

fn parse_diff(rest: &[&str], id: &str, flags: &Flags) -> Result<Value, ParseError> {
    const VALID: &[&str] = &["snapshot", "screenshot", "url"];

    match rest.first().copied() {
        Some("snapshot") => {
            let mut cmd = json!({ "id": id, "action": "diff_snapshot" });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 1;
            while i < rest.len() {
                match rest[i] {
                    "-b" | "--baseline" => {
                        if let Some(path) = rest.get(i + 1) {
                            obj.insert("baseline".to_string(), json!(path));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff snapshot --baseline".to_string(),
                                usage: "diff snapshot --baseline <file>",
                            });
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff snapshot --selector".to_string(),
                                usage: "diff snapshot --selector <sel>",
                            });
                        }
                    }
                    "-c" | "--compact" => {
                        obj.insert("compact".to_string(), json!(true));
                    }
                    "-d" | "--depth" => {
                        if let Some(d) = rest.get(i + 1) {
                            match d.parse::<u32>() {
                                Ok(n) => {
                                    obj.insert("maxDepth".to_string(), json!(n));
                                    i += 1;
                                }
                                Err(_) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!("Depth must be a non-negative integer, got: {}", d),
                                        usage: "diff snapshot --depth <n>",
                                    });
                                }
                            }
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff snapshot --depth".to_string(),
                                usage: "diff snapshot --depth <n>",
                            });
                        }
                    }
                    other if other.starts_with('-') => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unknown flag: {}", other),
                            usage: "diff snapshot [--baseline <file>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                    other => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unexpected argument: {}", other),
                            usage: "diff snapshot [--baseline <file>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                }
                i += 1;
            }
            Ok(cmd)
        }
        Some("screenshot") => {
            let mut cmd = json!({ "id": id, "action": "diff_screenshot" });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 1;
            while i < rest.len() {
                match rest[i] {
                    "-b" | "--baseline" => {
                        if let Some(path) = rest.get(i + 1) {
                            obj.insert("baseline".to_string(), json!(path));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --baseline".to_string(),
                                usage: "diff screenshot --baseline <file>",
                            });
                        }
                    }
                    "-o" | "--output" => {
                        if let Some(path) = rest.get(i + 1) {
                            obj.insert("output".to_string(), json!(path));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --output".to_string(),
                                usage: "diff screenshot --output <file>",
                            });
                        }
                    }
                    "-t" | "--threshold" => {
                        if let Some(t) = rest.get(i + 1) {
                            match t.parse::<f64>() {
                                Ok(n) if (0.0..=1.0).contains(&n) => {
                                    obj.insert("threshold".to_string(), json!(n));
                                    i += 1;
                                }
                                Ok(n) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!("Threshold must be between 0 and 1, got {}", n),
                                        usage: "diff screenshot --threshold <0-1>",
                                    });
                                }
                                Err(_) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!("Invalid threshold value: {}", t),
                                        usage: "diff screenshot --threshold <0-1>",
                                    });
                                }
                            }
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --threshold".to_string(),
                                usage: "diff screenshot --threshold <0-1>",
                            });
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff screenshot --selector".to_string(),
                                usage: "diff screenshot --selector <sel>",
                            });
                        }
                    }
                    "--full" => {
                        obj.insert("fullPage".to_string(), json!(true));
                    }
                    other if other.starts_with('-') => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unknown flag: {}", other),
                            usage: "diff screenshot --baseline <file> [--output <file>] [--threshold <0-1>] [--selector <sel>] [--full]",
                        });
                    }
                    other => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unexpected argument: {}", other),
                            usage: "diff screenshot --baseline <file> [--output <file>] [--threshold <0-1>] [--selector <sel>] [--full]",
                        });
                    }
                }
                i += 1;
            }
            if flags.full {
                obj.insert("fullPage".to_string(), json!(true));
            }
            if !obj.contains_key("baseline") {
                return Err(ParseError::MissingArguments {
                    context: "diff screenshot".to_string(),
                    usage: "diff screenshot --baseline <file>",
                });
            }
            Ok(cmd)
        }
        Some("url") => {
            let url1 = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "diff url".to_string(),
                usage: "diff url <url1> <url2>",
            })?;
            let url2 = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "diff url".to_string(),
                usage: "diff url <url1> <url2>",
            })?;
            let mut cmd = json!({
                "id": id,
                "action": "diff_url",
                "url1": url1,
                "url2": url2,
            });
            let obj = cmd.as_object_mut().unwrap();
            let mut i = 3;
            while i < rest.len() {
                match rest[i] {
                    "--screenshot" => {
                        obj.insert("screenshot".to_string(), json!(true));
                    }
                    "--full" => {
                        obj.insert("fullPage".to_string(), json!(true));
                    }
                    "--wait-until" => {
                        if let Some(val) = rest.get(i + 1) {
                            obj.insert("waitUntil".to_string(), json!(val));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff url --wait-until".to_string(),
                                usage: "diff url <url1> <url2> --wait-until <load|domcontentloaded|networkidle>",
                            });
                        }
                    }
                    "-s" | "--selector" => {
                        if let Some(s) = rest.get(i + 1) {
                            obj.insert("selector".to_string(), json!(s));
                            i += 1;
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff url --selector".to_string(),
                                usage: "diff url <url1> <url2> --selector <sel>",
                            });
                        }
                    }
                    "-c" | "--compact" => {
                        obj.insert("compact".to_string(), json!(true));
                    }
                    "-d" | "--depth" => {
                        if let Some(d) = rest.get(i + 1) {
                            match d.parse::<u32>() {
                                Ok(n) => {
                                    obj.insert("maxDepth".to_string(), json!(n));
                                    i += 1;
                                }
                                Err(_) => {
                                    return Err(ParseError::InvalidValue {
                                        message: format!("Depth must be a non-negative integer, got: {}", d),
                                        usage: "diff url <url1> <url2> --depth <n>",
                                    });
                                }
                            }
                        } else {
                            return Err(ParseError::MissingArguments {
                                context: "diff url --depth".to_string(),
                                usage: "diff url <url1> <url2> --depth <n>",
                            });
                        }
                    }
                    other if other.starts_with('-') => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unknown flag: {}", other),
                            usage: "diff url <url1> <url2> [--screenshot] [--full] [--wait-until <strategy>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                    other => {
                        return Err(ParseError::InvalidValue {
                            message: format!("Unexpected argument: {}", other),
                            usage: "diff url <url1> <url2> [--screenshot] [--full] [--wait-until <strategy>] [--selector <sel>] [--compact] [--depth <n>]",
                        });
                    }
                }
                i += 1;
            }
            if flags.full {
                obj.insert("fullPage".to_string(), json!(true));
            }
            Ok(cmd)
        }
        Some(sub) => Err(ParseError::UnknownSubcommand {
            subcommand: sub.to_string(),
            valid_options: VALID,
        }),
        None => Err(ParseError::MissingArguments {
            context: "diff".to_string(),
            usage: "diff <snapshot|screenshot|url>",
        }),
    }
}

fn parse_get(rest: &[&str], id: &str) -> Result<Value, ParseError> {
    const VALID: &[&str] = &[
        "text", "html", "value", "attr", "url", "title", "count", "box", "styles",
    ];

    match rest.first().copied() {
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

    match rest.first().copied() {
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
    const VALID: &[&str] = &[
        "role",
        "text",
        "label",
        "placeholder",
        "alt",
        "title",
        "testid",
        "first",
        "last",
        "nth",
    ];

    let locator = rest.first().ok_or_else(|| ParseError::MissingArguments {
        context: "find".to_string(),
        usage: "find <locator> <value> [action] [text]",
    })?;

    let name_idx = rest.iter().position(|&s| s == "--name");
    let name = name_idx.and_then(|i| rest.get(i + 1).copied());
    let exact = rest.contains(&"--exact");

    match *locator {
        "role" | "text" | "label" | "placeholder" | "alt" | "title" | "testid" | "first"
        | "last" => {
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
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "text" => Ok(
                    json!({ "id": id, "action": "getbytext", "text": value, "subaction": subaction, "exact": exact }),
                ),
                "label" => {
                    let mut cmd = json!({ "id": id, "action": "getbylabel", "label": value, "subaction": subaction, "exact": exact });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "placeholder" => {
                    let mut cmd = json!({ "id": id, "action": "getbyplaceholder", "placeholder": value, "subaction": subaction, "exact": exact });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "alt" => Ok(
                    json!({ "id": id, "action": "getbyalttext", "text": value, "subaction": subaction, "exact": exact }),
                ),
                "title" => Ok(
                    json!({ "id": id, "action": "getbytitle", "text": value, "subaction": subaction, "exact": exact }),
                ),
                "testid" => {
                    let mut cmd = json!({ "id": id, "action": "getbytestid", "testId": value, "subaction": subaction });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "first" => {
                    let mut cmd = json!({ "id": id, "action": "nth", "selector": value, "index": 0, "subaction": subaction });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
                    Ok(cmd)
                }
                "last" => {
                    let mut cmd = json!({ "id": id, "action": "nth", "selector": value, "index": -1, "subaction": subaction });
                    if let Some(v) = fill_value {
                        cmd["value"] = json!(v);
                    }
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
            let idx = idx_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
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
            if let Some(v) = fv {
                cmd["value"] = json!(v);
            }
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

    match rest.first().copied() {
        Some("move") => {
            let x_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            let y_str = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "mouse move".to_string(),
                usage: "mouse move <x> <y>",
            })?;
            let x = x_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "mouse move".to_string(),
                    usage: "mouse move <x> <y>",
                })?;
            let y = y_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
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
            let dy = rest
                .get(1)
                .and_then(|s| s.parse::<i32>().ok())
                .unwrap_or(100);
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
    const VALID: &[&str] = &[
        "viewport",
        "device",
        "geo",
        "geolocation",
        "offline",
        "headers",
        "credentials",
        "auth",
        "media",
    ];

    match rest.first().copied() {
        Some("viewport") => {
            let w_str = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set viewport".to_string(),
                usage: "set viewport <width> <height>",
            })?;
            let h_str = rest.get(2).ok_or_else(|| ParseError::MissingArguments {
                context: "set viewport".to_string(),
                usage: "set viewport <width> <height>",
            })?;
            let w = w_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "set viewport".to_string(),
                    usage: "set viewport <width> <height>",
                })?;
            let h = h_str
                .parse::<i32>()
                .map_err(|_| ParseError::MissingArguments {
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
            let lat = lat_str
                .parse::<f64>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "set geo".to_string(),
                    usage: "set geo <latitude> <longitude>",
                })?;
            let lng = lng_str
                .parse::<f64>()
                .map_err(|_| ParseError::MissingArguments {
                    context: "set geo".to_string(),
                    usage: "set geo <latitude> <longitude>",
                })?;
            Ok(json!({ "id": id, "action": "geolocation", "latitude": lat, "longitude": lng }))
        }
        Some("offline") => {
            let off = rest
                .get(1)
                .map(|s| *s != "off" && *s != "false")
                .unwrap_or(true);
            Ok(json!({ "id": id, "action": "offline", "offline": off }))
        }
        Some("headers") => {
            let headers_json = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "set headers".to_string(),
                usage: "set headers <json>",
            })?;
            // Parse the JSON string into an object
            let headers: serde_json::Value =
                serde_json::from_str(headers_json).map_err(|_| ParseError::MissingArguments {
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
            let color = if rest.contains(&"dark") {
                "dark"
            } else if rest.contains(&"light") {
                "light"
            } else {
                "no-preference"
            };
            let reduced = if rest.contains(&"reduced-motion") {
                "reduce"
            } else {
                "no-preference"
            };
            Ok(
                json!({ "id": id, "action": "emulatemedia", "colorScheme": color, "reducedMotion": reduced }),
            )
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

    match rest.first().copied() {
        Some("route") => {
            let url = rest.get(1).ok_or_else(|| ParseError::MissingArguments {
                context: "network route".to_string(),
                usage: "network route <url> [--abort|--body <json>]",
            })?;
            let abort = rest.contains(&"--abort");
            let body_idx = rest.iter().position(|&s| s == "--body");
            let body = body_idx.and_then(|i| rest.get(i + 1).copied());
            Ok(json!({ "id": id, "action": "route", "url": url, "abort": abort, "body": body }))
        }
        Some("unroute") => {
            let mut cmd = json!({ "id": id, "action": "unroute" });
            if let Some(url) = rest.get(1) {
                cmd["url"] = json!(url);
            }
            Ok(cmd)
        }
        Some("requests") => {
            let clear = rest.contains(&"--clear");
            let filter_idx = rest.iter().position(|&s| s == "--filter");
            let filter = filter_idx.and_then(|i| rest.get(i + 1).copied());
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

    match rest.first().copied() {
        Some("local") | Some("session") => {
            let storage_type = rest.first().unwrap();
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
                    Ok(
                        json!({ "id": id, "action": "storage_set", "type": storage_type, "key": k, "value": v }),
                    )
                }
                "clear" => Ok(json!({ "id": id, "action": "storage_clear", "type": storage_type })),
                _ => {
                    let mut cmd =
                        json!({ "id": id, "action": "storage_get", "type": storage_type });
                    if let Some(k) = key {
                        cmd.as_object_mut()
                            .unwrap()
                            .insert("key".to_string(), json!(k));
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
            profile: None,
            state: None,
            proxy: None,
            proxy_bypass: None,
            args: None,
            user_agent: None,
            provider: None,
            ignore_https_errors: false,
            allow_file_access: false,
            device: None,
            auto_connect: false,
            session_name: None,
            cli_executable_path: false,
            cli_extensions: false,
            cli_profile: false,
            cli_state: false,
            cli_args: false,
            cli_user_agent: false,
            cli_proxy: false,
            cli_proxy_bypass: false,
            cli_allow_file_access: false,
            annotate: false,
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

    #[test]
    fn test_cookies_set_with_url() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --url https://example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["url"], "https://example.com");
    }

    #[test]
    fn test_cookies_set_with_domain() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --domain example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["domain"], "example.com");
    }

    #[test]
    fn test_cookies_set_with_path() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --path /api"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["path"], "/api");
    }

    #[test]
    fn test_cookies_set_with_httponly() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --httpOnly"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["httpOnly"], true);
    }

    #[test]
    fn test_cookies_set_with_secure() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --secure"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["secure"], true);
    }

    #[test]
    fn test_cookies_set_with_samesite() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --sameSite Strict"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["sameSite"], "Strict");
    }

    #[test]
    fn test_cookies_set_with_expires() {
        let cmd = parse_command(
            &args("cookies set mycookie myvalue --expires 1234567890"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["expires"], 1234567890);
    }

    #[test]
    fn test_cookies_set_with_multiple_flags() {
        let cmd = parse_command(&args("cookies set mycookie myvalue --url https://example.com --httpOnly --secure --sameSite Lax"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["url"], "https://example.com");
        assert_eq!(cmd["cookies"][0]["httpOnly"], true);
        assert_eq!(cmd["cookies"][0]["secure"], true);
        assert_eq!(cmd["cookies"][0]["sameSite"], "Lax");
    }

    #[test]
    fn test_cookies_set_with_all_flags() {
        let cmd = parse_command(&args("cookies set mycookie myvalue --url https://example.com --domain example.com --path /api --httpOnly --secure --sameSite None --expires 9999999999"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "cookies_set");
        assert_eq!(cmd["cookies"][0]["name"], "mycookie");
        assert_eq!(cmd["cookies"][0]["value"], "myvalue");
        assert_eq!(cmd["cookies"][0]["url"], "https://example.com");
        assert_eq!(cmd["cookies"][0]["domain"], "example.com");
        assert_eq!(cmd["cookies"][0]["path"], "/api");
        assert_eq!(cmd["cookies"][0]["httpOnly"], true);
        assert_eq!(cmd["cookies"][0]["secure"], true);
        assert_eq!(cmd["cookies"][0]["sameSite"], "None");
        assert_eq!(cmd["cookies"][0]["expires"], 9999999999i64);
    }

    #[test]
    fn test_cookies_set_invalid_samesite() {
        let result = parse_command(
            &args("cookies set mycookie myvalue --sameSite Invalid"),
            &default_flags(),
        );
        assert!(result.is_err());
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
        let cmd =
            parse_command(&args("storage local set mykey myvalue"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "storage_set");
        assert_eq!(cmd["type"], "local");
        assert_eq!(cmd["key"], "mykey");
        assert_eq!(cmd["value"], "myvalue");
    }

    #[test]
    fn test_storage_session_set() {
        let cmd =
            parse_command(&args("storage session set skey svalue"), &default_flags()).unwrap();
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
        flags.headers =
            Some(r#"{"Authorization": "Bearer token", "X-Custom": "value"}"#.to_string());
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
        let result = parse_command(&args("open api.example.com"), &flags);
        // Invalid JSON should return a ParseError, not silently drop headers
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.format();
        assert!(msg.contains("Invalid JSON for --headers"));
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
        let cmd = parse_command(&args("select #menu opt1 opt2 opt3"), &default_flags()).unwrap();
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
        assert!(
            cmd.get("url").is_none(),
            "url should not be present when not provided"
        );
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
        assert_eq!(cmd["path"], serde_json::Value::Null);
        assert_eq!(cmd["selector"], serde_json::Value::Null);
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

    #[test]
    fn test_screenshot_with_ref() {
        let cmd = parse_command(&args("screenshot @e1"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], "@e1");
        assert_eq!(cmd["path"], serde_json::Value::Null);
    }

    #[test]
    fn test_screenshot_with_css_class() {
        let cmd = parse_command(&args("screenshot .my-button"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], ".my-button");
        assert_eq!(cmd["path"], serde_json::Value::Null);
    }

    #[test]
    fn test_screenshot_with_css_id() {
        let cmd = parse_command(&args("screenshot #header"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], "#header");
        assert_eq!(cmd["path"], serde_json::Value::Null);
    }

    #[test]
    fn test_screenshot_with_path() {
        let cmd = parse_command(&args("screenshot ./output.png"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], serde_json::Value::Null);
        assert_eq!(cmd["path"], "./output.png");
    }

    #[test]
    fn test_screenshot_with_selector_and_path() {
        let cmd = parse_command(&args("screenshot .btn ./button.png"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "screenshot");
        assert_eq!(cmd["selector"], ".btn");
        assert_eq!(cmd["path"], "./button.png");
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
    fn test_snapshot_cursor() {
        let cmd = parse_command(&args("snapshot -C"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["cursor"], true);
    }

    #[test]
    fn test_snapshot_interactive_cursor() {
        let cmd = parse_command(&args("snapshot -i -C"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "snapshot");
        assert_eq!(cmd["interactive"], true);
        assert_eq!(cmd["cursor"], true);
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
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
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
        let cmd = parse_command(
            &args("record start demo.webm https://example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_start_with_url_no_protocol() {
        let cmd = parse_command(
            &args("record start demo.webm example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "recording_start");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_start_missing_path() {
        let result = parse_command(&args("record start"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
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
        let cmd = parse_command(
            &args("record restart demo.webm https://example.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "recording_restart");
        assert_eq!(cmd["path"], "demo.webm");
        assert_eq!(cmd["url"], "https://example.com");
    }

    #[test]
    fn test_record_restart_missing_path() {
        let result = parse_command(&args("record restart"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_record_invalid_subcommand() {
        let result = parse_command(&args("record foo"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownSubcommand { .. }
        ));
    }

    #[test]
    fn test_record_missing_subcommand() {
        let result = parse_command(&args("record"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Profile (CDP Tracing) Tests ===

    #[test]
    fn test_profiler_start() {
        let cmd = parse_command(&args("profiler start"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "profiler_start");
        assert!(cmd.get("categories").is_none());
    }

    #[test]
    fn test_profiler_start_with_categories() {
        let cmd = parse_command(
            &args("profiler start --categories devtools.timeline,v8.execute"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "profiler_start");
        let categories = cmd["categories"].as_array().unwrap();
        assert_eq!(categories.len(), 2);
        assert_eq!(categories[0], "devtools.timeline");
        assert_eq!(categories[1], "v8.execute");
    }

    #[test]
    fn test_profiler_start_categories_missing_value() {
        let result = parse_command(&args("profiler start --categories"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_profiler_stop_with_path() {
        let cmd = parse_command(&args("profiler stop trace.json"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "profiler_stop");
        assert_eq!(cmd["path"], "trace.json");
    }

    #[test]
    fn test_profiler_stop_no_path() {
        let cmd = parse_command(&args("profiler stop"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "profiler_stop");
        assert!(cmd.get("path").is_none());
    }

    #[test]
    fn test_profiler_invalid_subcommand() {
        let result = parse_command(&args("profiler foo"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownSubcommand { .. }
        ));
    }

    #[test]
    fn test_profiler_missing_subcommand() {
        let result = parse_command(&args("profiler"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Eval Tests ===

    #[test]
    fn test_eval_basic() {
        let cmd = parse_command(&args("eval document.title"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.title");
    }

    #[test]
    fn test_eval_base64_short_flag() {
        // "document.title" in base64
        let cmd = parse_command(&args("eval -b ZG9jdW1lbnQudGl0bGU="), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.title");
    }

    #[test]
    fn test_eval_base64_long_flag() {
        // "document.title" in base64
        let cmd = parse_command(
            &args("eval --base64 ZG9jdW1lbnQudGl0bGU="),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.title");
    }

    #[test]
    fn test_eval_base64_with_special_chars() {
        // "document.querySelector('[src*=\"_next\"]')" in base64
        let cmd = parse_command(
            &args("eval -b ZG9jdW1lbnQucXVlcnlTZWxlY3RvcignW3NyYyo9Il9uZXh0Il0nKQ=="),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "evaluate");
        assert_eq!(cmd["script"], "document.querySelector('[src*=\"_next\"]')");
    }

    #[test]
    fn test_eval_base64_invalid() {
        let result = parse_command(&args("eval -b !!!invalid!!!"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("Invalid base64"));
    }

    #[test]
    fn test_unknown_command() {
        let result = parse_command(&args("unknowncommand"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownCommand { .. }
        ));
    }

    #[test]
    fn test_empty_args() {
        let result = parse_command(&[], &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
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

    // === Download Tests ===

    #[test]
    fn test_download() {
        let cmd = parse_command(&args("download #btn ./file.pdf"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "download");
        assert_eq!(cmd["selector"], "#btn");
        assert_eq!(cmd["path"], "./file.pdf");
    }

    #[test]
    fn test_download_with_ref() {
        let cmd = parse_command(&args("download @e5 ./report.xlsx"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "download");
        assert_eq!(cmd["selector"], "@e5");
        assert_eq!(cmd["path"], "./report.xlsx");
    }

    #[test]
    fn test_download_missing_path() {
        let result = parse_command(&args("download #btn"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_download_missing_selector() {
        let result = parse_command(&args("download"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    // === Wait for Download Tests ===

    #[test]
    fn test_wait_download() {
        let cmd = parse_command(&args("wait --download"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert!(cmd.get("path").is_none());
    }

    #[test]
    fn test_wait_download_with_path() {
        let cmd = parse_command(&args("wait --download ./file.pdf"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["path"], "./file.pdf");
    }

    #[test]
    fn test_wait_download_with_timeout() {
        let cmd =
            parse_command(&args("wait --download --timeout 30000"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["timeout"], 30000);
    }

    #[test]
    fn test_wait_download_with_path_and_timeout() {
        let cmd = parse_command(
            &args("wait --download ./file.pdf --timeout 30000"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["path"], "./file.pdf");
        assert_eq!(cmd["timeout"], 30000);
    }

    #[test]
    fn test_wait_download_short_flag() {
        let cmd = parse_command(&args("wait -d ./file.pdf"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "waitfordownload");
        assert_eq!(cmd["path"], "./file.pdf");
    }

    // === Connect (CDP) tests ===

    #[test]
    fn test_connect_with_port() {
        let cmd = parse_command(&args("connect 9222"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpPort"], 9222);
        assert!(cmd.get("cdpUrl").is_none());
    }

    #[test]
    fn test_connect_with_ws_url() {
        let input: Vec<String> = vec![
            "connect".to_string(),
            "ws://localhost:9222/devtools/browser/abc123".to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpUrl"], "ws://localhost:9222/devtools/browser/abc123");
        assert!(cmd.get("cdpPort").is_none());
    }

    #[test]
    fn test_connect_with_wss_url() {
        let input: Vec<String> = vec![
            "connect".to_string(),
            "wss://remote-browser.example.com/cdp?token=xyz".to_string(),
        ];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(
            cmd["cdpUrl"],
            "wss://remote-browser.example.com/cdp?token=xyz"
        );
        assert!(cmd.get("cdpPort").is_none());
    }

    #[test]
    fn test_connect_with_http_url() {
        let input: Vec<String> = vec!["connect".to_string(), "http://localhost:9222".to_string()];
        let cmd = parse_command(&input, &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpUrl"], "http://localhost:9222");
        assert!(cmd.get("cdpPort").is_none());
    }

    #[test]
    fn test_connect_missing_argument() {
        let result = parse_command(&args("connect"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_connect_invalid_port() {
        let result = parse_command(&args("connect notanumber"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("not a valid port number or URL"));
    }

    #[test]
    fn test_connect_port_zero() {
        let result = parse_command(&args("connect 0"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("port must be greater than 0"));
    }

    #[test]
    fn test_connect_port_out_of_range() {
        let result = parse_command(&args("connect 65536"), &default_flags());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue { .. }));
        assert!(err.format().contains("out of range"));
        assert!(err.format().contains("1-65535"));
    }

    #[test]
    fn test_connect_port_max_valid() {
        let cmd = parse_command(&args("connect 65535"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpPort"], 65535);
    }

    #[test]
    fn test_connect_port_min_valid() {
        let cmd = parse_command(&args("connect 1"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "launch");
        assert_eq!(cmd["cdpPort"], 1);
    }

    // === Trace Tests ===

    #[test]
    fn test_trace_start() {
        let cmd = parse_command(&args("trace start"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "trace_start");
    }

    #[test]
    fn test_trace_stop_with_path() {
        let cmd = parse_command(&args("trace stop ./trace.zip"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "trace_stop");
        assert_eq!(cmd["path"], "./trace.zip");
    }

    #[test]
    fn test_trace_stop_without_path() {
        let cmd = parse_command(&args("trace stop"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "trace_stop");
        assert!(cmd.get("path").is_none() || cmd["path"].is_null());
    }

    // === Diff Tests ===

    #[test]
    fn test_diff_snapshot_basic() {
        let cmd = parse_command(&args("diff snapshot"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
    }

    #[test]
    fn test_diff_snapshot_baseline() {
        let cmd =
            parse_command(&args("diff snapshot --baseline before.txt"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
        assert_eq!(cmd["baseline"], "before.txt");
    }

    #[test]
    fn test_diff_snapshot_selector_compact_depth() {
        let cmd = parse_command(
            &args("diff snapshot --selector #main --compact --depth 3"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
        assert_eq!(cmd["selector"], "#main");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 3);
    }

    #[test]
    fn test_diff_snapshot_short_flags() {
        let cmd =
            parse_command(&args("diff snapshot -b snap.txt -s .content -c -d 2"), &default_flags())
                .unwrap();
        assert_eq!(cmd["action"], "diff_snapshot");
        assert_eq!(cmd["baseline"], "snap.txt");
        assert_eq!(cmd["selector"], ".content");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 2);
    }

    #[test]
    fn test_diff_screenshot_baseline() {
        let cmd = parse_command(
            &args("diff screenshot --baseline before.png"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_screenshot");
        assert_eq!(cmd["baseline"], "before.png");
    }

    #[test]
    fn test_diff_screenshot_all_options() {
        let cmd = parse_command(
            &args("diff screenshot --baseline b.png --output d.png --threshold 0.2 --selector #hero --full"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_screenshot");
        assert_eq!(cmd["baseline"], "b.png");
        assert_eq!(cmd["output"], "d.png");
        assert_eq!(cmd["threshold"], 0.2);
        assert_eq!(cmd["selector"], "#hero");
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_screenshot_missing_baseline() {
        let result = parse_command(&args("diff screenshot"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_global_full_flag() {
        let mut flags = default_flags();
        flags.full = true;
        let cmd =
            parse_command(&args("diff screenshot --baseline b.png"), &flags).unwrap();
        assert_eq!(cmd["action"], "diff_screenshot");
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_url_basic() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["url1"], "https://a.com");
        assert_eq!(cmd["url2"], "https://b.com");
    }

    #[test]
    fn test_diff_url_with_screenshot_full() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --screenshot --full"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["screenshot"], true);
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_url_with_wait_until() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --wait-until networkidle"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["waitUntil"], "networkidle");
    }

    #[test]
    fn test_diff_url_global_full_flag() {
        let mut flags = default_flags();
        flags.full = true;
        let cmd =
            parse_command(&args("diff url https://a.com https://b.com"), &flags).unwrap();
        assert_eq!(cmd["fullPage"], true);
    }

    #[test]
    fn test_diff_missing_subcommand() {
        let result = parse_command(&args("diff"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_unknown_subcommand() {
        let result = parse_command(&args("diff invalid"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::UnknownSubcommand { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_baseline_missing_value() {
        let result = parse_command(&args("diff snapshot --baseline"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_selector_missing_value() {
        let result = parse_command(&args("diff snapshot --selector"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_depth_missing_value() {
        let result = parse_command(&args("diff snapshot --depth"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_missing_value() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_output_missing_value() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --output"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_url_wait_until_missing_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --wait-until"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_unexpected_arg() {
        let result = parse_command(&args("diff snapshot foo"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_unexpected_arg() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png unexpected"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_unexpected_arg() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com extra"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_unknown_flag() {
        let result = parse_command(&args("diff snapshot --invalid"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_missing_urls() {
        let result = parse_command(&args("diff url"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_url_missing_second_url() {
        let result = parse_command(&args("diff url https://a.com"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_depth_invalid_value() {
        let result = parse_command(&args("diff snapshot --depth abc"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_invalid_value() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold abc"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_out_of_range() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold 1.5"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_screenshot_threshold_negative() {
        let result = parse_command(
            &args("diff screenshot --baseline b.png --threshold -0.5"),
            &default_flags(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_diff_url_with_selector() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --selector #main"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["selector"], "#main");
    }

    #[test]
    fn test_diff_url_with_compact_depth() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com --compact --depth 3"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 3);
    }

    #[test]
    fn test_diff_url_with_short_snapshot_flags() {
        let cmd = parse_command(
            &args("diff url https://a.com https://b.com -s .content -c -d 2"),
            &default_flags(),
        )
        .unwrap();
        assert_eq!(cmd["action"], "diff_url");
        assert_eq!(cmd["selector"], ".content");
        assert_eq!(cmd["compact"], true);
        assert_eq!(cmd["maxDepth"], 2);
    }

    #[test]
    fn test_diff_url_depth_invalid_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --depth abc"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_snapshot_depth_negative_value() {
        let result = parse_command(&args("diff snapshot --depth -1"), &default_flags());
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_depth_negative_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --depth -1"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::InvalidValue { .. }
        ));
    }

    #[test]
    fn test_diff_url_selector_missing_value() {
        let result = parse_command(
            &args("diff url https://a.com https://b.com --selector"),
            &default_flags(),
        );
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingArguments { .. }
        ));
    }
}
