use serde_json::{json, Value};

use crate::flags::Flags;

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

pub fn parse_command(args: &[String], flags: &Flags) -> Option<Value> {
    if args.is_empty() {
        return None;
    }

    let cmd = args[0].as_str();
    let rest: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
    let id = gen_id();

    match cmd {
        // === Navigation ===
        "open" | "goto" | "navigate" => {
            let url = rest.get(0)?;
            let url = if url.starts_with("http") {
                url.to_string()
            } else {
                format!("https://{}", url)
            };
            Some(json!({ "id": id, "action": "navigate", "url": url }))
        }
        "back" => Some(json!({ "id": id, "action": "back" })),
        "forward" => Some(json!({ "id": id, "action": "forward" })),
        "reload" => Some(json!({ "id": id, "action": "reload" })),

        // === Core Actions ===
        "click" => Some(json!({ "id": id, "action": "click", "selector": rest.get(0)? })),
        "dblclick" => Some(json!({ "id": id, "action": "dblclick", "selector": rest.get(0)? })),
        "fill" => Some(json!({ "id": id, "action": "fill", "selector": rest.get(0)?, "value": rest[1..].join(" ") })),
        "type" => Some(json!({ "id": id, "action": "type", "selector": rest.get(0)?, "text": rest[1..].join(" ") })),
        "hover" => Some(json!({ "id": id, "action": "hover", "selector": rest.get(0)? })),
        "focus" => Some(json!({ "id": id, "action": "focus", "selector": rest.get(0)? })),
        "check" => Some(json!({ "id": id, "action": "check", "selector": rest.get(0)? })),
        "uncheck" => Some(json!({ "id": id, "action": "uncheck", "selector": rest.get(0)? })),
        "select" => Some(json!({ "id": id, "action": "select", "selector": rest.get(0)?, "value": rest.get(1)? })),
        "drag" => Some(json!({ "id": id, "action": "drag", "source": rest.get(0)?, "target": rest.get(1)? })),
        "upload" => Some(json!({ "id": id, "action": "upload", "selector": rest.get(0)?, "files": &rest[1..] })),

        // === Keyboard ===
        "press" | "key" => Some(json!({ "id": id, "action": "press", "key": rest.get(0)? })),
        "keydown" => Some(json!({ "id": id, "action": "keydown", "key": rest.get(0)? })),
        "keyup" => Some(json!({ "id": id, "action": "keyup", "key": rest.get(0)? })),

        // === Scroll ===
        "scroll" => {
            let dir = rest.get(0).unwrap_or(&"down");
            let amount = rest.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(300);
            Some(json!({ "id": id, "action": "scroll", "direction": dir, "amount": amount }))
        }
        "scrollintoview" | "scrollinto" => {
            Some(json!({ "id": id, "action": "scrollintoview", "selector": rest.get(0)? }))
        }

        // === Wait ===
        "wait" => {
            if let Some(arg) = rest.get(0) {
                if arg.parse::<u64>().is_ok() {
                    Some(json!({ "id": id, "action": "wait", "timeout": arg.parse::<u64>().unwrap() }))
                } else {
                    Some(json!({ "id": id, "action": "wait", "selector": arg }))
                }
            } else {
                None
            }
        }

        // === Screenshot/PDF ===
        "screenshot" => {
            Some(json!({ "id": id, "action": "screenshot", "path": rest.get(0), "fullPage": flags.full }))
        }
        "pdf" => Some(json!({ "id": id, "action": "pdf", "path": rest.get(0)? })),

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
            Some(cmd)
        }

        // === Eval ===
        "eval" => Some(json!({ "id": id, "action": "evaluate", "script": rest.join(" ") })),

        // === Close ===
        "close" | "quit" | "exit" => Some(json!({ "id": id, "action": "close" })),

        // === Get ===
        "get" => match rest.get(0).map(|s| *s) {
            Some("text") => Some(json!({ "id": id, "action": "gettext", "selector": rest.get(1)? })),
            Some("html") => Some(json!({ "id": id, "action": "innerhtml", "selector": rest.get(1)? })),
            Some("value") => Some(json!({ "id": id, "action": "inputvalue", "selector": rest.get(1)? })),
            Some("attr") => Some(json!({ "id": id, "action": "getattribute", "selector": rest.get(1)?, "attribute": rest.get(2)? })),
            Some("url") => Some(json!({ "id": id, "action": "url" })),
            Some("title") => Some(json!({ "id": id, "action": "title" })),
            Some("count") => Some(json!({ "id": id, "action": "count", "selector": rest.get(1)? })),
            Some("box") => Some(json!({ "id": id, "action": "boundingbox", "selector": rest.get(1)? })),
            _ => None,
        },

        // === Is (state checks) ===
        "is" => match rest.get(0).map(|s| *s) {
            Some("visible") => Some(json!({ "id": id, "action": "isvisible", "selector": rest.get(1)? })),
            Some("enabled") => Some(json!({ "id": id, "action": "isenabled", "selector": rest.get(1)? })),
            Some("checked") => Some(json!({ "id": id, "action": "ischecked", "selector": rest.get(1)? })),
            _ => None,
        },

        // === Find (locators) ===
        "find" => parse_find(&rest, &id),

        // === Mouse ===
        "mouse" => match rest.get(0).map(|s| *s) {
            Some("move") => {
                let x = rest.get(1)?.parse::<i32>().ok()?;
                let y = rest.get(2)?.parse::<i32>().ok()?;
                Some(json!({ "id": id, "action": "mousemove", "x": x, "y": y }))
            }
            Some("down") => {
                Some(json!({ "id": id, "action": "mousedown", "button": rest.get(1).unwrap_or(&"left") }))
            }
            Some("up") => {
                Some(json!({ "id": id, "action": "mouseup", "button": rest.get(1).unwrap_or(&"left") }))
            }
            Some("wheel") => {
                let dy = rest.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(100);
                let dx = rest.get(2).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
                Some(json!({ "id": id, "action": "mousewheel", "deltaX": dx, "deltaY": dy }))
            }
            _ => None,
        },

        // === Set (browser settings) ===
        "set" => parse_set(&rest, &id),

        // === Network ===
        "network" => match rest.get(0).map(|s| *s) {
            Some("route") => {
                let url = rest.get(1)?;
                let abort = rest.iter().any(|&s| s == "--abort");
                let body_idx = rest.iter().position(|&s| s == "--body");
                let body = body_idx.and_then(|i| rest.get(i + 1).map(|s| *s));
                Some(json!({ "id": id, "action": "route", "url": url, "abort": abort, "body": body }))
            }
            Some("unroute") => Some(json!({ "id": id, "action": "unroute", "url": rest.get(1) })),
            Some("requests") => {
                let clear = rest.iter().any(|&s| s == "--clear");
                let filter_idx = rest.iter().position(|&s| s == "--filter");
                let filter = filter_idx.and_then(|i| rest.get(i + 1).map(|s| *s));
                Some(json!({ "id": id, "action": "requests", "clear": clear, "filter": filter }))
            }
            _ => None,
        },

        // === Storage ===
        "storage" => match rest.get(0).map(|s| *s) {
            Some("local") | Some("session") => {
                let storage_type = rest.get(0)?;
                let op = rest.get(1).unwrap_or(&"get");
                let key = rest.get(2);
                let value = rest.get(3);
                match *op {
                    "set" => Some(json!({ "id": id, "action": "storage_set", "type": storage_type, "key": key?, "value": value? })),
                    "clear" => Some(json!({ "id": id, "action": "storage_clear", "type": storage_type })),
                    _ => {
                        let mut cmd = json!({ "id": id, "action": "storage_get", "type": storage_type });
                        if let Some(k) = key {
                            cmd.as_object_mut().unwrap().insert("key".to_string(), json!(k));
                        }
                        Some(cmd)
                    }
                }
            }
            _ => None,
        },

        // === Cookies ===
        "cookies" => {
            let op = rest.get(0).unwrap_or(&"get");
            match *op {
                "set" => {
                    let name = rest.get(1)?;
                    let value = rest.get(2)?;
                    Some(json!({ "id": id, "action": "cookies_set", "cookies": [{ "name": name, "value": value }] }))
                }
                "clear" => Some(json!({ "id": id, "action": "cookies_clear" })),
                _ => Some(json!({ "id": id, "action": "cookies_get" })),
            }
        }

        // === Tabs ===
        "tab" => match rest.get(0).map(|s| *s) {
            Some("new") => Some(json!({ "id": id, "action": "tab_new", "url": rest.get(1) })),
            Some("list") => Some(json!({ "id": id, "action": "tab_list" })),
            Some("close") => {
                Some(json!({ "id": id, "action": "tab_close", "index": rest.get(1).and_then(|s| s.parse::<i32>().ok()) }))
            }
            Some(n) if n.parse::<i32>().is_ok() => {
                Some(json!({ "id": id, "action": "tab_switch", "index": n.parse::<i32>().unwrap() }))
            }
            _ => Some(json!({ "id": id, "action": "tab_list" })),
        },

        // === Window ===
        "window" => match rest.get(0).map(|s| *s) {
            Some("new") => Some(json!({ "id": id, "action": "window_new" })),
            _ => None,
        },

        // === Frame ===
        "frame" => {
            if rest.get(0).map(|s| *s) == Some("main") {
                Some(json!({ "id": id, "action": "frame_main" }))
            } else {
                Some(json!({ "id": id, "action": "frame", "selector": rest.get(0)? }))
            }
        }

        // === Dialog ===
        "dialog" => match rest.get(0).map(|s| *s) {
            Some("accept") => {
                Some(json!({ "id": id, "action": "dialog", "response": "accept", "promptText": rest.get(1) }))
            }
            Some("dismiss") => Some(json!({ "id": id, "action": "dialog", "response": "dismiss" })),
            _ => None,
        },

        // === Debug ===
        "trace" => match rest.get(0).map(|s| *s) {
            Some("start") => Some(json!({ "id": id, "action": "trace_start", "path": rest.get(1) })),
            Some("stop") => Some(json!({ "id": id, "action": "trace_stop", "path": rest.get(1) })),
            _ => None,
        },
        "console" => {
            let clear = rest.iter().any(|&s| s == "--clear");
            Some(json!({ "id": id, "action": "console", "clear": clear }))
        }
        "errors" => {
            let clear = rest.iter().any(|&s| s == "--clear");
            Some(json!({ "id": id, "action": "errors", "clear": clear }))
        }
        "highlight" => Some(json!({ "id": id, "action": "highlight", "selector": rest.get(0)? })),

        // === State ===
        "state" => match rest.get(0).map(|s| *s) {
            Some("save") => Some(json!({ "id": id, "action": "state_save", "path": rest.get(1)? })),
            Some("load") => Some(json!({ "id": id, "action": "state_load", "path": rest.get(1)? })),
            _ => None,
        },

        _ => None,
    }
}

fn parse_find(rest: &[&str], id: &str) -> Option<Value> {
    let locator = rest.get(0)?;
    let value = rest.get(1)?;
    let subaction = rest.get(2).unwrap_or(&"click");
    let fill_value = if rest.len() > 3 {
        Some(rest[3..].join(" "))
    } else {
        None
    };

    let name_idx = rest.iter().position(|&s| s == "--name");
    let name = name_idx.and_then(|i| rest.get(i + 1).map(|s| *s));
    let exact = rest.iter().any(|&s| s == "--exact");

    match *locator {
        "role" => Some(json!({ "id": id, "action": "getbyrole", "role": value, "subaction": subaction, "value": fill_value, "name": name, "exact": exact })),
        "text" => Some(json!({ "id": id, "action": "getbytext", "text": value, "subaction": subaction, "exact": exact })),
        "label" => Some(json!({ "id": id, "action": "getbylabel", "label": value, "subaction": subaction, "value": fill_value, "exact": exact })),
        "placeholder" => Some(json!({ "id": id, "action": "getbyplaceholder", "placeholder": value, "subaction": subaction, "value": fill_value, "exact": exact })),
        "alt" => Some(json!({ "id": id, "action": "getbyalttext", "text": value, "subaction": subaction, "exact": exact })),
        "title" => Some(json!({ "id": id, "action": "getbytitle", "text": value, "subaction": subaction, "exact": exact })),
        "testid" => Some(json!({ "id": id, "action": "getbytestid", "testId": value, "subaction": subaction, "value": fill_value })),
        "first" => Some(json!({ "id": id, "action": "nth", "selector": value, "index": 0, "subaction": subaction, "value": fill_value })),
        "last" => Some(json!({ "id": id, "action": "nth", "selector": value, "index": -1, "subaction": subaction, "value": fill_value })),
        "nth" => {
            let idx = value.parse::<i32>().ok()?;
            let sel = rest.get(2)?;
            let sub = rest.get(3).unwrap_or(&"click");
            let fv = if rest.len() > 4 {
                Some(rest[4..].join(" "))
            } else {
                None
            };
            Some(json!({ "id": id, "action": "nth", "selector": sel, "index": idx, "subaction": sub, "value": fv }))
        }
        _ => None,
    }
}

fn parse_set(rest: &[&str], id: &str) -> Option<Value> {
    match rest.get(0).map(|s| *s) {
        Some("viewport") => {
            let w = rest.get(1)?.parse::<i32>().ok()?;
            let h = rest.get(2)?.parse::<i32>().ok()?;
            Some(json!({ "id": id, "action": "viewport", "width": w, "height": h }))
        }
        Some("device") => Some(json!({ "id": id, "action": "device", "device": rest.get(1)? })),
        Some("geo") | Some("geolocation") => {
            let lat = rest.get(1)?.parse::<f64>().ok()?;
            let lng = rest.get(2)?.parse::<f64>().ok()?;
            Some(json!({ "id": id, "action": "geolocation", "latitude": lat, "longitude": lng }))
        }
        Some("offline") => {
            let off = rest.get(1).map(|s| *s != "off" && *s != "false").unwrap_or(true);
            Some(json!({ "id": id, "action": "offline", "offline": off }))
        }
        Some("headers") => {
            let headers_json = rest.get(1)?;
            Some(json!({ "id": id, "action": "headers", "headers": headers_json }))
        }
        Some("credentials") | Some("auth") => {
            Some(json!({ "id": id, "action": "credentials", "username": rest.get(1)?, "password": rest.get(2)? }))
        }
        Some("media") => {
            let color = if rest.iter().any(|&s| s == "dark") {
                "dark"
            } else if rest.iter().any(|&s| s == "light") {
                "light"
            } else {
                "no-preference"
            };
            let reduced = rest.iter().any(|&s| s == "reduced-motion");
            Some(json!({ "id": id, "action": "media", "colorScheme": color, "reducedMotion": reduced }))
        }
        _ => None,
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
        assert!(result.is_none());
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
        assert!(result.is_none());
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
        assert!(result.is_none());
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

    // === Tabs ===

    #[test]
    fn test_tab_new() {
        let cmd = parse_command(&args("tab new"), &default_flags()).unwrap();
        assert_eq!(cmd["action"], "tab_new");
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

    // === Unknown command ===

    #[test]
    fn test_unknown_command() {
        let result = parse_command(&args("unknowncommand"), &default_flags());
        assert!(result.is_none());
    }

    #[test]
    fn test_empty_args() {
        let result = parse_command(&[], &default_flags());
        assert!(result.is_none());
    }
}
