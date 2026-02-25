use std::sync::OnceLock;

use crate::color;
use crate::connection::Response;

static BOUNDARY_NONCE: OnceLock<String> = OnceLock::new();

/// Per-process nonce for content boundary markers. Uses a CSPRNG (getrandom) so
/// that untrusted page content cannot predict or spoof the boundary delimiter.
/// Process ID or timestamps would be insufficient since pages can read those.
fn get_boundary_nonce() -> &'static str {
    BOUNDARY_NONCE.get_or_init(|| {
        let mut buf = [0u8; 16];
        getrandom::getrandom(&mut buf).expect("failed to generate random nonce");
        buf.iter().map(|b| format!("{:02x}", b)).collect()
    })
}

#[derive(Default)]
pub struct OutputOptions {
    pub json: bool,
    pub content_boundaries: bool,
    pub max_output: Option<usize>,
}

fn truncate_if_needed(content: &str, max: Option<usize>) -> String {
    let Some(limit) = max else {
        return content.to_string();
    };
    // Fast path: byte length is a lower bound on char count, so if the
    // byte length is within the limit the char count must be too.
    if content.len() <= limit {
        return content.to_string();
    }
    // Find the byte offset of the limit-th character.
    match content.char_indices().nth(limit).map(|(i, _)| i) {
        Some(byte_offset) => {
            let total_chars = content.chars().count();
            format!(
                "{}\n[truncated: showing {} of {} chars. Use --max-output to adjust]",
                &content[..byte_offset], limit, total_chars
            )
        }
        // Content has fewer than `limit` chars despite more bytes
        None => content.to_string(),
    }
}

fn print_with_boundaries(content: &str, origin: Option<&str>, opts: &OutputOptions) {
    let content = truncate_if_needed(content, opts.max_output);
    if opts.content_boundaries {
        let origin_str = origin.unwrap_or("unknown");
        let nonce = get_boundary_nonce();
        println!("--- AGENT_BROWSER_PAGE_CONTENT nonce={} origin={} ---", nonce, origin_str);
        println!("{}", content);
        println!("--- END_AGENT_BROWSER_PAGE_CONTENT nonce={} ---", nonce);
    } else {
        println!("{}", content);
    }
}

pub fn print_response_with_opts(resp: &Response, action: Option<&str>, opts: &OutputOptions) {
    if opts.json {
        if opts.content_boundaries {
            let mut json_val = serde_json::to_value(resp).unwrap_or_default();
            if let Some(obj) = json_val.as_object_mut() {
                let nonce = get_boundary_nonce();
                let origin = obj.get("data")
                    .and_then(|d| d.get("origin"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                obj.insert("_boundary".to_string(), serde_json::json!({
                    "nonce": nonce,
                    "origin": origin,
                }));
            }
            println!("{}", serde_json::to_string(&json_val).unwrap_or_default());
        } else {
            println!("{}", serde_json::to_string(resp).unwrap_or_default());
        }
        return;
    }

    if !resp.success {
        eprintln!(
            "{} {}",
            color::error_indicator(),
            resp.error.as_deref().unwrap_or("Unknown error")
        );
        return;
    }

    if let Some(data) = &resp.data {
        // Navigation response
        if let Some(url) = data.get("url").and_then(|v| v.as_str()) {
            if let Some(title) = data.get("title").and_then(|v| v.as_str()) {
                println!("{} {}", color::success_indicator(), color::bold(title));
                println!("  {}", color::dim(url));
                return;
            }
            println!("{}", url);
            return;
        }
        // Diff responses -- route by action to avoid fragile shape probing
        if let Some(obj) = data.as_object() {
            match action {
                Some("diff_snapshot") => {
                    print_snapshot_diff(obj);
                    return;
                }
                Some("diff_screenshot") => {
                    print_screenshot_diff(obj);
                    return;
                }
                Some("diff_url") => {
                    if let Some(snap_data) =
                        obj.get("snapshot").and_then(|v| v.as_object())
                    {
                        println!("{}", color::bold("Snapshot diff:"));
                        print_snapshot_diff(snap_data);
                    }
                    if let Some(ss_data) =
                        obj.get("screenshot").and_then(|v| v.as_object())
                    {
                        println!("\n{}", color::bold("Screenshot diff:"));
                        print_screenshot_diff(ss_data);
                    }
                    return;
                }
                _ => {}
            }
        }
        let origin = data.get("origin").and_then(|v| v.as_str());
        // Snapshot
        if let Some(snapshot) = data.get("snapshot").and_then(|v| v.as_str()) {
            print_with_boundaries(snapshot, origin, opts);
            return;
        }
        // Title
        if let Some(title) = data.get("title").and_then(|v| v.as_str()) {
            println!("{}", title);
            return;
        }
        // Text
        if let Some(text) = data.get("text").and_then(|v| v.as_str()) {
            print_with_boundaries(text, origin, opts);
            return;
        }
        // HTML
        if let Some(html) = data.get("html").and_then(|v| v.as_str()) {
            print_with_boundaries(html, origin, opts);
            return;
        }
        // Value
        if let Some(value) = data.get("value").and_then(|v| v.as_str()) {
            println!("{}", value);
            return;
        }
        // Count
        if let Some(count) = data.get("count").and_then(|v| v.as_i64()) {
            println!("{}", count);
            return;
        }
        // Boolean results
        if let Some(visible) = data.get("visible").and_then(|v| v.as_bool()) {
            println!("{}", visible);
            return;
        }
        if let Some(enabled) = data.get("enabled").and_then(|v| v.as_bool()) {
            println!("{}", enabled);
            return;
        }
        if let Some(checked) = data.get("checked").and_then(|v| v.as_bool()) {
            println!("{}", checked);
            return;
        }
        // Eval result
        if let Some(result) = data.get("result") {
            let formatted = serde_json::to_string_pretty(result).unwrap_or_default();
            print_with_boundaries(&formatted, origin, opts);
            return;
        }
        // iOS Devices
        if let Some(devices) = data.get("devices").and_then(|v| v.as_array()) {
            if devices.is_empty() {
                println!("No iOS devices available. Open Xcode to download simulator runtimes.");
                return;
            }

            // Separate real devices from simulators
            let real_devices: Vec<_> = devices
                .iter()
                .filter(|d| {
                    d.get("isRealDevice")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .collect();
            let simulators: Vec<_> = devices
                .iter()
                .filter(|d| {
                    !d.get("isRealDevice")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
                .collect();

            if !real_devices.is_empty() {
                println!("Connected Devices:\n");
                for device in real_devices.iter() {
                    let name = device
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let runtime = device.get("runtime").and_then(|v| v.as_str()).unwrap_or("");
                    let udid = device.get("udid").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  {} {} ({})", color::green("●"), name, runtime);
                    println!("    {}", color::dim(udid));
                }
                println!();
            }

            if !simulators.is_empty() {
                println!("Simulators:\n");
                for device in simulators.iter() {
                    let name = device
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let runtime = device.get("runtime").and_then(|v| v.as_str()).unwrap_or("");
                    let state = device
                        .get("state")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let udid = device.get("udid").and_then(|v| v.as_str()).unwrap_or("");
                    let state_indicator = if state == "Booted" {
                        color::green("●")
                    } else {
                        color::dim("○")
                    };
                    println!("  {} {} ({})", state_indicator, name, runtime);
                    println!("    {}", color::dim(udid));
                }
            }
            return;
        }
        // Tabs
        if let Some(tabs) = data.get("tabs").and_then(|v| v.as_array()) {
            for (i, tab) in tabs.iter().enumerate() {
                let title = tab
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Untitled");
                let url = tab.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let active = tab.get("active").and_then(|v| v.as_bool()).unwrap_or(false);
                let marker = if active {
                    color::cyan("→")
                } else {
                    " ".to_string()
                };
                println!("{} [{}] {} - {}", marker, i, title, url);
            }
            return;
        }
        // Console logs
        if let Some(logs) = data.get("messages").and_then(|v| v.as_array()) {
            if opts.content_boundaries {
                let mut console_output = String::new();
                for log in logs {
                    let level = log.get("type").and_then(|v| v.as_str()).unwrap_or("log");
                    let text = log.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    console_output.push_str(&format!("{} {}\n", color::console_level_prefix(level), text));
                }
                if console_output.ends_with('\n') {
                    console_output.pop();
                }
                print_with_boundaries(&console_output, origin, opts);
            } else {
                for log in logs {
                    let level = log.get("type").and_then(|v| v.as_str()).unwrap_or("log");
                    let text = log.get("text").and_then(|v| v.as_str()).unwrap_or("");
                    println!("{} {}", color::console_level_prefix(level), text);
                }
            }
            return;
        }
        // Errors
        if let Some(errors) = data.get("errors").and_then(|v| v.as_array()) {
            for err in errors {
                let msg = err.get("message").and_then(|v| v.as_str()).unwrap_or("");
                println!("{} {}", color::error_indicator(), msg);
            }
            return;
        }
        // Cookies
        if let Some(cookies) = data.get("cookies").and_then(|v| v.as_array()) {
            for cookie in cookies {
                let name = cookie.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let value = cookie.get("value").and_then(|v| v.as_str()).unwrap_or("");
                println!("{}={}", name, value);
            }
            return;
        }
        // Network requests
        if let Some(requests) = data.get("requests").and_then(|v| v.as_array()) {
            if requests.is_empty() {
                println!("No requests captured");
            } else {
                for req in requests {
                    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("GET");
                    let url = req.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let resource_type = req
                        .get("resourceType")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    println!("{} {} ({})", method, url, resource_type);
                }
            }
            return;
        }
        // Cleared requests
        if let Some(cleared) = data.get("cleared").and_then(|v| v.as_bool()) {
            if cleared {
                println!("{} Request log cleared", color::success_indicator());
                return;
            }
        }
        // Bounding box
        if let Some(box_data) = data.get("box") {
            println!(
                "{}",
                serde_json::to_string_pretty(box_data).unwrap_or_default()
            );
            return;
        }
        // Element styles
        if let Some(elements) = data.get("elements").and_then(|v| v.as_array()) {
            for (i, el) in elements.iter().enumerate() {
                let tag = el.get("tag").and_then(|v| v.as_str()).unwrap_or("?");
                let text = el.get("text").and_then(|v| v.as_str()).unwrap_or("");
                println!("[{}] {} \"{}\"", i, tag, text);

                if let Some(box_data) = el.get("box") {
                    let w = box_data.get("width").and_then(|v| v.as_i64()).unwrap_or(0);
                    let h = box_data.get("height").and_then(|v| v.as_i64()).unwrap_or(0);
                    let x = box_data.get("x").and_then(|v| v.as_i64()).unwrap_or(0);
                    let y = box_data.get("y").and_then(|v| v.as_i64()).unwrap_or(0);
                    println!("    box: {}x{} at ({}, {})", w, h, x, y);
                }

                if let Some(styles) = el.get("styles") {
                    let font_size = styles
                        .get("fontSize")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let font_weight = styles
                        .get("fontWeight")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let font_family = styles
                        .get("fontFamily")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let color = styles.get("color").and_then(|v| v.as_str()).unwrap_or("");
                    let bg = styles
                        .get("backgroundColor")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let radius = styles
                        .get("borderRadius")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    println!("    font: {} {} {}", font_size, font_weight, font_family);
                    println!("    color: {}", color);
                    println!("    background: {}", bg);
                    if radius != "0px" {
                        println!("    border-radius: {}", radius);
                    }
                }
                println!();
            }
            return;
        }
        // Closed
        if data.get("closed").is_some() {
            println!("{} Browser closed", color::success_indicator());
            return;
        }
        // Recording start (has "started" field)
        if let Some(started) = data.get("started").and_then(|v| v.as_bool()) {
            if started {
                match action {
                    Some("profiler_start") => {
                        println!("{} Profiling started", color::success_indicator());
                    }
                    _ => {
                        if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                            println!(
                                "{} Recording started: {}",
                                color::success_indicator(),
                                path
                            );
                        } else {
                            println!("{} Recording started", color::success_indicator());
                        }
                    }
                }
                return;
            }
        }
        // Recording restart (has "stopped" field - from recording_restart action)
        if data.get("stopped").is_some() {
            let path = data
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if let Some(prev_path) = data.get("previousPath").and_then(|v| v.as_str()) {
                println!(
                    "{} Recording restarted: {} (previous saved to {})",
                    color::success_indicator(),
                    path,
                    prev_path
                );
            } else {
                println!("{} Recording started: {}", color::success_indicator(), path);
            }
            return;
        }
        // Recording stop (has "frames" field - from recording_stop action)
        if data.get("frames").is_some() {
            if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                if let Some(error) = data.get("error").and_then(|v| v.as_str()) {
                    println!(
                        "{} Recording saved to {} - {}",
                        color::warning_indicator(),
                        path,
                        error
                    );
                } else {
                    println!("{} Recording saved to {}", color::success_indicator(), path);
                }
            } else {
                println!("{} Recording stopped", color::success_indicator());
            }
            return;
        }
        // Download response (has "suggestedFilename" or "filename" field)
        if data.get("suggestedFilename").is_some() || data.get("filename").is_some() {
            if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
                let filename = data
                    .get("suggestedFilename")
                    .or_else(|| data.get("filename"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if filename.is_empty() {
                    println!(
                        "{} Downloaded to {}",
                        color::success_indicator(),
                        color::green(path)
                    );
                } else {
                    println!(
                        "{} Downloaded to {} ({})",
                        color::success_indicator(),
                        color::green(path),
                        filename
                    );
                }
                return;
            }
        }
        // Trace stop without path
        if data.get("traceStopped").is_some() {
            println!("{} Trace stopped", color::success_indicator());
            return;
        }
        // Path-based operations (screenshot/pdf/trace/har/download/state/video)
        if let Some(path) = data.get("path").and_then(|v| v.as_str()) {
            match action.unwrap_or("") {
                "screenshot" => {
                    println!(
                        "{} Screenshot saved to {}",
                        color::success_indicator(),
                        color::green(path)
                    );
                    if let Some(annotations) = data.get("annotations").and_then(|v| v.as_array()) {
                        for ann in annotations {
                            let num = ann.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                            let ref_id = ann.get("ref").and_then(|r| r.as_str()).unwrap_or("");
                            let role = ann.get("role").and_then(|r| r.as_str()).unwrap_or("");
                            let name = ann.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            if name.is_empty() {
                                println!(
                                    "   {} @{} {}",
                                    color::dim(&format!("[{}]", num)),
                                    ref_id,
                                    role,
                                );
                            } else {
                                println!(
                                    "   {} @{} {} {:?}",
                                    color::dim(&format!("[{}]", num)),
                                    ref_id,
                                    role,
                                    name,
                                );
                            }
                        }
                    }
                }
                "pdf" => println!(
                    "{} PDF saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "trace_stop" => println!(
                    "{} Trace saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "profiler_stop" => println!(
                    "{} Profile saved to {} ({} events)",
                    color::success_indicator(),
                    color::green(path),
                    data.get("eventCount").and_then(|c| c.as_u64()).unwrap_or(0)
                ),
                "har_stop" => println!(
                    "{} HAR saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "download" | "waitfordownload" => println!(
                    "{} Download saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "video_stop" => println!(
                    "{} Video saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "state_save" => println!(
                    "{} State saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
                "state_load" => {
                    if let Some(note) = data.get("note").and_then(|v| v.as_str()) {
                        println!("{}", note);
                    }
                    println!(
                        "{} State path set to {}",
                        color::success_indicator(),
                        color::green(path)
                    );
                }
                // video_start and other commands that provide a path with a note
                "video_start" => {
                    if let Some(note) = data.get("note").and_then(|v| v.as_str()) {
                        println!("{}", note);
                    }
                    println!("Path: {}", path);
                }
                _ => println!(
                    "{} Saved to {}",
                    color::success_indicator(),
                    color::green(path)
                ),
            }
            return;
        }

        // State list
        if let Some(files) = data.get("files").and_then(|v| v.as_array()) {
            if let Some(dir) = data.get("directory").and_then(|v| v.as_str()) {
                println!("{}", color::bold(&format!("Saved states in {}", dir)));
            }
            if files.is_empty() {
                println!("{}", color::dim("  No state files found"));
            } else {
                for file in files {
                    let filename = file.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                    let size = file.get("size").and_then(|v| v.as_i64()).unwrap_or(0);
                    let modified = file.get("modified").and_then(|v| v.as_str()).unwrap_or("");
                    let encrypted = file.get("encrypted").and_then(|v| v.as_bool()).unwrap_or(false);
                    let size_str = if size > 1024 {
                        format!("{:.1}KB", size as f64 / 1024.0)
                    } else {
                        format!("{}B", size)
                    };
                    let date_str = modified.split('T').next().unwrap_or(modified);
                    let enc_str = if encrypted { " [encrypted]" } else { "" };
                    println!("  {} {}", filename, color::dim(&format!("({}, {}){}", size_str, date_str, enc_str)));
                }
            }
            return;
        }

        // State rename
        if let Some(true) = data.get("renamed").and_then(|v| v.as_bool()) {
            let old_name = data.get("oldName").and_then(|v| v.as_str()).unwrap_or("");
            let new_name = data.get("newName").and_then(|v| v.as_str()).unwrap_or("");
            println!("{} Renamed {} -> {}", color::success_indicator(), old_name, new_name);
            return;
        }

        // State clear
        if let Some(cleared) = data.get("cleared").and_then(|v| v.as_i64()) {
            println!("{} Cleared {} state file(s)", color::success_indicator(), cleared);
            return;
        }

        // State show summary
        if let Some(summary) = data.get("summary") {
            let cookies = summary.get("cookies").and_then(|v| v.as_i64()).unwrap_or(0);
            let origins = summary.get("origins").and_then(|v| v.as_i64()).unwrap_or(0);
            let encrypted = data.get("encrypted").and_then(|v| v.as_bool()).unwrap_or(false);
            let enc_str = if encrypted { " (encrypted)" } else { "" };
            println!("State file summary{}:", enc_str);
            println!("  Cookies: {}", cookies);
            println!("  Origins with localStorage: {}", origins);
            return;
        }

        // State clean
        if let Some(cleaned) = data.get("cleaned").and_then(|v| v.as_i64()) {
            println!("{} Cleaned {} old state file(s)", color::success_indicator(), cleaned);
            return;
        }

        // Informational note
        if let Some(note) = data.get("note").and_then(|v| v.as_str()) {
            println!("{}", note);
            return;
        }
        // Auth list
        if let Some(profiles) = data.get("profiles").and_then(|v| v.as_array()) {
            if profiles.is_empty() {
                println!("{}", color::dim("No auth profiles saved"));
            } else {
                println!("{}", color::bold("Auth profiles:"));
                for p in profiles {
                    let name = p.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let url = p.get("url").and_then(|v| v.as_str()).unwrap_or("");
                    let user = p.get("username").and_then(|v| v.as_str()).unwrap_or("");
                    println!("  {} {} {}", color::green(name), color::dim(user), color::dim(url));
                }
            }
            return;
        }

        // Auth show
        if let Some(profile) = data.get("profile").and_then(|v| v.as_object()) {
            let name = profile.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let url = profile.get("url").and_then(|v| v.as_str()).unwrap_or("");
            let user = profile.get("username").and_then(|v| v.as_str()).unwrap_or("");
            let created = profile.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");
            let last_login = profile.get("lastLoginAt").and_then(|v| v.as_str());
            println!("Name: {}", name);
            println!("URL: {}", url);
            println!("Username: {}", user);
            println!("Created: {}", created);
            if let Some(ll) = last_login {
                println!("Last login: {}", ll);
            }
            return;
        }

        // Auth save/update/login/delete
        if data.get("saved").and_then(|v| v.as_bool()).unwrap_or(false) {
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
            println!("{} Auth profile '{}' saved", color::success_indicator(), name);
            return;
        }
        if data.get("updated").and_then(|v| v.as_bool()).unwrap_or(false)
            && !data.get("saved").and_then(|v| v.as_bool()).unwrap_or(false) {
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
            println!("{} Auth profile '{}' updated", color::success_indicator(), name);
            return;
        }
        if data.get("loggedIn").and_then(|v| v.as_bool()).unwrap_or(false) {
            let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(title) = data.get("title").and_then(|v| v.as_str()) {
                println!("{} Logged in as '{}' - {}", color::success_indicator(), name, title);
            } else {
                println!("{} Logged in as '{}'", color::success_indicator(), name);
            }
            return;
        }
        if data.get("deleted").and_then(|v| v.as_bool()).unwrap_or(false) {
            if let Some(name) = data.get("name").and_then(|v| v.as_str()) {
                println!("{} Auth profile '{}' deleted", color::success_indicator(), name);
                return;
            }
        }

        // Confirmation required (for orchestrator use)
        if data.get("confirmation_required").and_then(|v| v.as_bool()).unwrap_or(false) {
            let category = data.get("category").and_then(|v| v.as_str()).unwrap_or("");
            let description = data.get("description").and_then(|v| v.as_str()).unwrap_or("");
            let cid = data.get("confirmation_id").and_then(|v| v.as_str()).unwrap_or("");
            println!("Confirmation required:");
            println!("  {}: {}", category, description);
            println!("  Run: agent-browser confirm {}", cid);
            println!("  Or:  agent-browser deny {}", cid);
            return;
        }
        if data.get("confirmed").and_then(|v| v.as_bool()).unwrap_or(false) {
            println!("{} Action confirmed", color::success_indicator());
            return;
        }
        if data.get("denied").and_then(|v| v.as_bool()).unwrap_or(false) {
            println!("{} Action denied", color::success_indicator());
            return;
        }

        // Default success
        println!("{} Done", color::success_indicator());
    }
}

/// Print command-specific help. Returns true if help was printed, false if command unknown.
pub fn print_command_help(command: &str) -> bool {
    let help = match command {
        // === Navigation ===
        "open" | "goto" | "navigate" => {
            r##"
agent-browser open - Navigate to a URL

Usage: agent-browser open <url>

Navigates the browser to the specified URL. If no protocol is provided,
https:// is automatically prepended.

Aliases: goto, navigate

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session
  --headers <json>     Set HTTP headers (scoped to this origin)
  --headed             Show browser window

Examples:
  agent-browser open example.com
  agent-browser open https://github.com
  agent-browser open localhost:3000
  agent-browser open api.example.com --headers '{"Authorization": "Bearer token"}'
    # ^ Headers only sent to api.example.com, not other domains
"##
        }
        "back" => {
            r##"
agent-browser back - Navigate back in history

Usage: agent-browser back

Goes back one page in the browser history, equivalent to clicking
the browser's back button.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser back
"##
        }
        "forward" => {
            r##"
agent-browser forward - Navigate forward in history

Usage: agent-browser forward

Goes forward one page in the browser history, equivalent to clicking
the browser's forward button.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser forward
"##
        }
        "reload" => {
            r##"
agent-browser reload - Reload the current page

Usage: agent-browser reload

Reloads the current page, equivalent to pressing F5 or clicking
the browser's reload button.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser reload
"##
        }

        // === Core Actions ===
        "click" => {
            r##"
agent-browser click - Click an element

Usage: agent-browser click <selector> [--new-tab]

Clicks on the specified element. The selector can be a CSS selector,
XPath, or an element reference from snapshot (e.g., @e1).

Options:
  --new-tab            Open link in a new tab instead of navigating current tab
                       (only works on elements with href attribute)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser click "#submit-button"
  agent-browser click @e1
  agent-browser click "button.primary"
  agent-browser click "//button[@type='submit']"
  agent-browser click @e3 --new-tab
"##
        }
        "dblclick" => {
            r##"
agent-browser dblclick - Double-click an element

Usage: agent-browser dblclick <selector>

Double-clicks on the specified element. Useful for text selection
or triggering double-click handlers.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser dblclick "#editable-text"
  agent-browser dblclick @e5
"##
        }
        "fill" => {
            r##"
agent-browser fill - Clear and fill an input field

Usage: agent-browser fill <selector> <text>

Clears the input field and fills it with the specified text.
This replaces any existing content in the field.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser fill "#email" "user@example.com"
  agent-browser fill @e3 "Hello World"
  agent-browser fill "input[name='search']" "query"
"##
        }
        "type" => {
            r##"
agent-browser type - Type text into an element

Usage: agent-browser type <selector> <text>

Types text into the specified element character by character.
Unlike fill, this does not clear existing content first.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser type "#search" "hello"
  agent-browser type @e2 "additional text"

See Also:
  For typing into contenteditable editors (Lexical, ProseMirror, etc.)
  without a selector, use 'keyboard type' instead:
    agent-browser keyboard type "# My Heading"
"##
        }
        "hover" => {
            r##"
agent-browser hover - Hover over an element

Usage: agent-browser hover <selector>

Moves the mouse to hover over the specified element. Useful for
triggering hover states or dropdown menus.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser hover "#dropdown-trigger"
  agent-browser hover @e4
"##
        }
        "focus" => {
            r##"
agent-browser focus - Focus an element

Usage: agent-browser focus <selector>

Sets keyboard focus to the specified element.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser focus "#input-field"
  agent-browser focus @e2
"##
        }
        "check" => {
            r##"
agent-browser check - Check a checkbox

Usage: agent-browser check <selector>

Checks a checkbox element. If already checked, no action is taken.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser check "#terms-checkbox"
  agent-browser check @e7
"##
        }
        "uncheck" => {
            r##"
agent-browser uncheck - Uncheck a checkbox

Usage: agent-browser uncheck <selector>

Unchecks a checkbox element. If already unchecked, no action is taken.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser uncheck "#newsletter-opt-in"
  agent-browser uncheck @e8
"##
        }
        "select" => {
            r##"
agent-browser select - Select a dropdown option

Usage: agent-browser select <selector> <value...>

Selects one or more options in a <select> dropdown by value.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser select "#country" "US"
  agent-browser select @e5 "option2"
  agent-browser select "#menu" "opt1" "opt2" "opt3"
"##
        }
        "drag" => {
            r##"
agent-browser drag - Drag and drop

Usage: agent-browser drag <source> <target>

Drags an element from source to target location.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser drag "#draggable" "#drop-zone"
  agent-browser drag @e1 @e2
"##
        }
        "upload" => {
            r##"
agent-browser upload - Upload files

Usage: agent-browser upload <selector> <files...>

Uploads one or more files to a file input element.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser upload "#file-input" ./document.pdf
  agent-browser upload @e3 ./image1.png ./image2.png
"##
        }
        "download" => {
            r##"
agent-browser download - Download a file by clicking an element

Usage: agent-browser download <selector> <path>

Clicks an element that triggers a download and saves the file to the specified path.

Arguments:
  selector             Element to click (CSS selector or @ref)
  path                 Path where the downloaded file will be saved

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser download "#download-btn" ./file.pdf
  agent-browser download @e5 ./report.xlsx
  agent-browser download "a[href$='.zip']" ./archive.zip
"##
        }

        // === Keyboard ===
        "press" | "key" => {
            r##"
agent-browser press - Press a key or key combination

Usage: agent-browser press <key>

Presses a key or key combination. Supports special keys and modifiers.

Aliases: key

Special Keys:
  Enter, Tab, Escape, Backspace, Delete, Space
  ArrowUp, ArrowDown, ArrowLeft, ArrowRight
  Home, End, PageUp, PageDown
  F1-F12

Modifiers (combine with +):
  Control, Alt, Shift, Meta

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser press Enter
  agent-browser press Tab
  agent-browser press Control+a
  agent-browser press Control+Shift+s
  agent-browser press Escape
"##
        }
        "keydown" => {
            r##"
agent-browser keydown - Press a key down (without release)

Usage: agent-browser keydown <key>

Presses a key down without releasing it. Use keyup to release.
Useful for holding modifier keys.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser keydown Shift
  agent-browser keydown Control
"##
        }
        "keyup" => {
            r##"
agent-browser keyup - Release a key

Usage: agent-browser keyup <key>

Releases a key that was pressed with keydown.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser keyup Shift
  agent-browser keyup Control
"##
        }
        "keyboard" => {
            r##"
agent-browser keyboard - Raw keyboard input (no selector needed)

Usage: agent-browser keyboard <subcommand> <text>

Sends keyboard input to whatever element currently has focus.
Unlike 'type' which requires a selector, 'keyboard' operates on
the current focus — essential for contenteditable editors like
Lexical, ProseMirror, CodeMirror, and Monaco.

Subcommands:
  type <text>          Type text character-by-character with real
                       key events (keydown, keypress, keyup per char)
  inserttext <text>    Insert text without key events (like paste)

Note: For key combos (Enter, Control+a), use the 'press' command
directly — it already operates on the current focus.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser keyboard type "Hello, World!"
  agent-browser keyboard type "# My Heading"
  agent-browser keyboard inserttext "pasted content"

Use Cases:
  # Type into a Lexical/ProseMirror contenteditable editor:
  agent-browser click "[contenteditable]"
  agent-browser keyboard type "# My Heading"
  agent-browser press Enter
  agent-browser keyboard type "Some paragraph text"
"##
        }

        // === Scroll ===
        "scroll" => {
            r##"
agent-browser scroll - Scroll the page

Usage: agent-browser scroll [direction] [amount] [options]

Scrolls the page or a specific element in the specified direction.

Arguments:
  direction            up, down, left, right (default: down)
  amount               Pixels to scroll (default: 300)

Options:
  -s, --selector <sel> CSS selector for a scrollable container

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser scroll
  agent-browser scroll down 500
  agent-browser scroll up 200
  agent-browser scroll left 100
  agent-browser scroll down 500 --selector "div.scroll-container"
"##
        }
        "scrollintoview" | "scrollinto" => {
            r##"
agent-browser scrollintoview - Scroll element into view

Usage: agent-browser scrollintoview <selector>

Scrolls the page until the specified element is visible in the viewport.

Aliases: scrollinto

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser scrollintoview "#footer"
  agent-browser scrollintoview @e15
"##
        }

        // === Wait ===
        "wait" => {
            r##"
agent-browser wait - Wait for condition

Usage: agent-browser wait <selector|ms|option>

Waits for an element to appear, a timeout, or other conditions.

Modes:
  <selector>           Wait for element to appear
  <ms>                 Wait for specified milliseconds
  --url <pattern>      Wait for URL to match pattern
  --load <state>       Wait for load state (load, domcontentloaded, networkidle)
  --fn <expression>    Wait for JavaScript expression to be truthy
  --text <text>        Wait for text to appear on page
  --download [path]    Wait for a download to complete (optionally save to path)

Download Options (with --download):
  --timeout <ms>       Timeout in milliseconds for download to start

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser wait "#loading-spinner"
  agent-browser wait 2000
  agent-browser wait --url "**/dashboard"
  agent-browser wait --load networkidle
  agent-browser wait --fn "window.appReady === true"
  agent-browser wait --text "Welcome back"
  agent-browser wait --download ./file.pdf
  agent-browser wait --download ./report.xlsx --timeout 30000
"##
        }

        // === Screenshot/PDF ===
        "screenshot" => {
            r##"
agent-browser screenshot - Take a screenshot

Usage: agent-browser screenshot [path]

Captures a screenshot of the current page. If no path is provided,
saves to a temporary directory with a generated filename.

Options:
  --full, -f           Capture full page (not just viewport)
  --annotate           Overlay numbered labels on interactive elements.
                       Each label [N] corresponds to ref @eN from snapshot.
                       Prints a legend mapping labels to element roles/names.
                       With --json, annotations are included in the response.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser screenshot
  agent-browser screenshot ./screenshot.png
  agent-browser screenshot --full ./full-page.png
  agent-browser screenshot --annotate              # Labeled screenshot + legend
  agent-browser screenshot --annotate ./page.png   # Save annotated screenshot
  agent-browser screenshot --annotate --json       # JSON output with annotations
"##
        }
        "pdf" => {
            r##"
agent-browser pdf - Save page as PDF

Usage: agent-browser pdf <path>

Saves the current page as a PDF file.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser pdf ./page.pdf
  agent-browser pdf ~/Documents/report.pdf
"##
        }

        // === Snapshot ===
        "snapshot" => {
            r##"
agent-browser snapshot - Get accessibility tree snapshot

Usage: agent-browser snapshot [options]

Returns an accessibility tree representation of the page with element
references (like @e1, @e2) that can be used in subsequent commands.
Designed for AI agents to understand page structure.

Options:
  -i, --interactive    Only include interactive elements
  -C, --cursor         Include cursor-interactive elements (cursor:pointer, onclick, tabindex)
  -c, --compact        Remove empty structural elements
  -d, --depth <n>      Limit tree depth
  -s, --selector <sel> Scope snapshot to CSS selector

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser snapshot
  agent-browser snapshot -i
  agent-browser snapshot -i -C         # Interactive + cursor-interactive elements
  agent-browser snapshot --compact --depth 5
  agent-browser snapshot -s "#main-content"
"##
        }

        // === Eval ===
        "eval" => {
            r##"
agent-browser eval - Execute JavaScript

Usage: agent-browser eval [options] <script>

Executes JavaScript code in the browser context and returns the result.

Options:
  -b, --base64         Decode script from base64 (avoids shell escaping issues)
  --stdin              Read script from stdin (useful for heredocs/multiline)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser eval "document.title"
  agent-browser eval "window.location.href"
  agent-browser eval "document.querySelectorAll('a').length"
  agent-browser eval -b "ZG9jdW1lbnQudGl0bGU="

  # Read from stdin with heredoc
  cat <<'EOF' | agent-browser eval --stdin
  const links = document.querySelectorAll('a');
  links.length;
  EOF
"##
        }

        // === Close ===
        "close" | "quit" | "exit" => {
            r##"
agent-browser close - Close the browser

Usage: agent-browser close

Closes the browser instance for the current session.

Aliases: quit, exit

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser close
  agent-browser close --session mysession
"##
        }

        // === Get ===
        "get" => {
            r##"
agent-browser get - Retrieve information from elements or page

Usage: agent-browser get <subcommand> [args]

Retrieves various types of information from elements or the page.

Subcommands:
  text <selector>            Get text content of element
  html <selector>            Get inner HTML of element
  value <selector>           Get value of input element
  attr <selector> <name>     Get attribute value
  title                      Get page title
  url                        Get current URL
  count <selector>           Count matching elements
  box <selector>             Get bounding box (x, y, width, height)
  styles <selector>          Get computed styles of elements

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser get text @e1
  agent-browser get html "#content"
  agent-browser get value "#email-input"
  agent-browser get attr "#link" href
  agent-browser get title
  agent-browser get url
  agent-browser get count "li.item"
  agent-browser get box "#header"
  agent-browser get styles "button"
  agent-browser get styles @e1
"##
        }

        // === Is ===
        "is" => {
            r##"
agent-browser is - Check element state

Usage: agent-browser is <subcommand> <selector>

Checks the state of an element and returns true/false.

Subcommands:
  visible <selector>   Check if element is visible
  enabled <selector>   Check if element is enabled (not disabled)
  checked <selector>   Check if checkbox/radio is checked

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser is visible "#modal"
  agent-browser is enabled "#submit-btn"
  agent-browser is checked "#agree-checkbox"
"##
        }

        // === Find ===
        "find" => {
            r##"
agent-browser find - Find and interact with elements by locator

Usage: agent-browser find <locator> <value> [action] [text]

Finds elements using semantic locators and optionally performs an action.

Locators:
  role <role>              Find by ARIA role (--name <n>, --exact)
  text <text>              Find by text content (--exact)
  label <label>            Find by associated label (--exact)
  placeholder <text>       Find by placeholder text (--exact)
  alt <text>               Find by alt text (--exact)
  title <text>             Find by title attribute (--exact)
  testid <id>              Find by data-testid attribute
  first <selector>         First matching element
  last <selector>          Last matching element
  nth <index> <selector>   Nth matching element (0-based)

Actions (default: click):
  click, fill, type, hover, focus, check, uncheck

Options:
  --name <name>        Filter role by accessible name
  --exact              Require exact text match

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser find role button click --name Submit
  agent-browser find text "Sign In" click
  agent-browser find label "Email" fill "user@example.com"
  agent-browser find placeholder "Search..." type "query"
  agent-browser find testid "login-form" click
  agent-browser find first "li.item" click
  agent-browser find nth 2 ".card" hover
"##
        }

        // === Mouse ===
        "mouse" => {
            r##"
agent-browser mouse - Low-level mouse operations

Usage: agent-browser mouse <subcommand> [args]

Performs low-level mouse operations for precise control.

Subcommands:
  move <x> <y>         Move mouse to coordinates
  down [button]        Press mouse button (left, right, middle)
  up [button]          Release mouse button
  wheel <dy> [dx]      Scroll mouse wheel

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser mouse move 100 200
  agent-browser mouse down
  agent-browser mouse up
  agent-browser mouse down right
  agent-browser mouse wheel 100
  agent-browser mouse wheel -50 0
"##
        }

        // === Set ===
        "set" => {
            r##"
agent-browser set - Configure browser settings

Usage: agent-browser set <setting> [args]

Configures various browser settings and emulation options.

Settings:
  viewport <w> <h>           Set viewport size
  device <name>              Emulate device (e.g., "iPhone 12")
  geo <lat> <lng>            Set geolocation
  offline [on|off]           Toggle offline mode
  headers <json>             Set extra HTTP headers
  credentials <user> <pass>  Set HTTP authentication
  media [dark|light]         Set color scheme preference
        [reduced-motion]     Enable reduced motion

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser set viewport 1920 1080
  agent-browser set device "iPhone 12"
  agent-browser set geo 37.7749 -122.4194
  agent-browser set offline on
  agent-browser set headers '{"X-Custom": "value"}'
  agent-browser set credentials admin secret123
  agent-browser set media dark
  agent-browser set media light reduced-motion
"##
        }

        // === Network ===
        "network" => {
            r##"
agent-browser network - Network interception and monitoring

Usage: agent-browser network <subcommand> [args]

Intercept, mock, or monitor network requests.

Subcommands:
  route <url> [options]      Intercept requests matching URL pattern
    --abort                  Abort matching requests
    --body <json>            Respond with custom body
  unroute [url]              Remove route (all if no URL)
  requests [options]         List captured requests
    --clear                  Clear request log
    --filter <pattern>       Filter by URL pattern

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser network route "**/api/*" --abort
  agent-browser network route "**/data.json" --body '{"mock": true}'
  agent-browser network unroute
  agent-browser network requests
  agent-browser network requests --filter "api"
  agent-browser network requests --clear
"##
        }

        // === Storage ===
        "storage" => {
            r##"
agent-browser storage - Manage web storage

Usage: agent-browser storage <type> [operation] [key] [value]

Manage localStorage and sessionStorage.

Types:
  local                localStorage
  session              sessionStorage

Operations:
  get [key]            Get all storage or specific key
  set <key> <value>    Set a key-value pair
  clear                Clear all storage

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser storage local
  agent-browser storage local get authToken
  agent-browser storage local set theme "dark"
  agent-browser storage local clear
  agent-browser storage session get userId
"##
        }

        // === Cookies ===
        "cookies" => {
            r##"
agent-browser cookies - Manage browser cookies

Usage: agent-browser cookies [operation] [args]

Manage browser cookies for the current context.

Operations:
  get                                Get all cookies (default)
  set <name> <value> [options]       Set a cookie with optional properties
  clear                              Clear all cookies

Cookie Set Options:
  --url <url>                        URL for the cookie (allows setting before page load)
  --domain <domain>                  Cookie domain (e.g., ".example.com")
  --path <path>                      Cookie path (e.g., "/api")
  --httpOnly                         Set HttpOnly flag (prevents JavaScript access)
  --secure                           Set Secure flag (HTTPS only)
  --sameSite <Strict|Lax|None>       SameSite policy
  --expires <timestamp>              Expiration time (Unix timestamp in seconds)

Note: If --url, --domain, and --path are all omitted, the cookie will be set
for the current page URL.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Simple cookie for current page
  agent-browser cookies set session_id "abc123"

  # Set cookie for a URL before loading it (useful for authentication)
  agent-browser cookies set session_id "abc123" --url https://app.example.com

  # Set secure, httpOnly cookie with domain and path
  agent-browser cookies set auth_token "xyz789" --domain example.com --path /api --httpOnly --secure

  # Set cookie with SameSite policy
  agent-browser cookies set tracking_consent "yes" --sameSite Strict

  # Set cookie with expiration (Unix timestamp)
  agent-browser cookies set temp_token "temp123" --expires 1735689600

  # Get all cookies
  agent-browser cookies

  # Clear all cookies
  agent-browser cookies clear
"##
        }

        // === Tabs ===
        "tab" => {
            r##"
agent-browser tab - Manage browser tabs

Usage: agent-browser tab [operation] [args]

Manage browser tabs in the current window.

Operations:
  list                 List all tabs (default)
  new [url]            Open new tab
  close [index]        Close tab (current if no index)
  <index>              Switch to tab by index

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser tab
  agent-browser tab list
  agent-browser tab new
  agent-browser tab new https://example.com
  agent-browser tab 2
  agent-browser tab close
  agent-browser tab close 1
"##
        }

        // === Window ===
        "window" => {
            r##"
agent-browser window - Manage browser windows

Usage: agent-browser window <operation>

Manage browser windows.

Operations:
  new                  Open new browser window

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser window new
"##
        }

        // === Frame ===
        "frame" => {
            r##"
agent-browser frame - Switch frame context

Usage: agent-browser frame <selector|main>

Switch to an iframe or back to the main frame.

Arguments:
  <selector>           CSS selector for iframe
  main                 Switch back to main frame

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser frame "#embed-iframe"
  agent-browser frame "iframe[name='content']"
  agent-browser frame main
"##
        }

        // === Auth ===
        "auth" => {
            r##"
agent-browser auth - Manage authentication profiles

Usage: agent-browser auth <subcommand> [args]

Subcommands:
  save <name>              Save credentials for a login profile
  login <name>             Login using saved credentials
  list                     List saved profiles (names and URLs only)
  show <name>              Show profile metadata (no passwords)
  delete <name>            Delete a saved profile

Save Options:
  --url <url>              Login page URL (required)
  --username <user>        Username (required)
  --password <pass>        Password (required unless --password-stdin)
  --password-stdin          Read password from stdin (recommended)
  --username-selector <s>  Custom CSS selector for username field
  --password-selector <s>  Custom CSS selector for password field
  --submit-selector <s>    Custom CSS selector for submit button

Global Options:
  --json                   Output as JSON
  --session <name>         Use specific session

Examples:
  echo "pass" | agent-browser auth save github --url https://github.com/login --username user --password-stdin
  agent-browser auth save github --url https://github.com/login --username user --password pass
  agent-browser auth login github
  agent-browser auth list
  agent-browser auth show github
  agent-browser auth delete github
"##
        }

        // === Confirm/Deny ===
        "confirm" | "deny" => {
            r##"
agent-browser confirm/deny - Approve or deny pending actions

Usage:
  agent-browser confirm <confirmation-id>
  agent-browser deny <confirmation-id>

When --confirm-actions is set, certain action categories return a
confirmation_required response with a confirmation ID. Use confirm/deny
to approve or reject the action.

Pending confirmations auto-deny after 60 seconds.

Examples:
  agent-browser confirm c_8f3a1234
  agent-browser deny c_8f3a1234
"##
        }

        // === Dialog ===
        "dialog" => {
            r##"
agent-browser dialog - Handle browser dialogs

Usage: agent-browser dialog <response> [text]

Respond to browser dialogs (alert, confirm, prompt).

Operations:
  accept [text]        Accept dialog, optionally with prompt text
  dismiss              Dismiss/cancel dialog

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser dialog accept
  agent-browser dialog accept "my input"
  agent-browser dialog dismiss
"##
        }

        // === Trace ===
        "trace" => {
            r##"
agent-browser trace - Record execution trace

Usage: agent-browser trace <operation> [path]

Record a trace for debugging with Playwright Trace Viewer.

Operations:
  start [path]         Start recording trace
  stop [path]          Stop recording and save trace

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser trace start
  agent-browser trace start ./my-trace
  agent-browser trace stop
  agent-browser trace stop ./debug-trace.zip
"##
        }

        // === Profile (CDP Tracing) ===
        "profiler" => {
            r##"
agent-browser profiler - Record Chrome DevTools performance profile

Usage: agent-browser profiler <operation> [options]

Record a performance profile using Chrome DevTools Protocol (CDP) Tracing.
The output JSON file can be loaded into Chrome DevTools Performance panel,
Perfetto UI (https://ui.perfetto.dev/), or other trace analysis tools.

Operations:
  start                Start profiling
  stop [path]          Stop profiling and save to file

Start Options:
  --categories <list>  Comma-separated trace categories (default includes
                       devtools.timeline, v8.execute, blink, and others)

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Basic profiling
  agent-browser profiler start
  agent-browser navigate https://example.com
  agent-browser click "#button"
  agent-browser profiler stop ./trace.json

  # With custom categories
  agent-browser profiler start --categories "devtools.timeline,v8.execute,blink.user_timing"
  agent-browser profiler stop ./custom-trace.json

The output file can be viewed in:
  - Chrome DevTools: Performance panel > Load profile
  - Perfetto: https://ui.perfetto.dev/
"##
        }

        // === Record (video) ===
        "record" => {
            r##"
agent-browser record - Record browser session to video

Usage: agent-browser record start <path.webm> [url]
       agent-browser record stop
       agent-browser record restart <path.webm> [url]

Record the browser to a WebM video file using Playwright's native recording.
Creates a fresh browser context but preserves cookies and localStorage.
If no URL is provided, automatically navigates to your current page.

Operations:
  start <path> [url]     Start recording (defaults to current URL if omitted)
  stop                   Stop recording and save video
  restart <path> [url]   Stop current recording (if any) and start a new one

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Record from current page (preserves login state)
  agent-browser open https://app.example.com/dashboard
  agent-browser snapshot -i            # Explore and plan
  agent-browser record start ./demo.webm
  agent-browser click @e3              # Execute planned actions
  agent-browser record stop

  # Or specify a different URL
  agent-browser record start ./demo.webm https://example.com

  # Restart recording with a new file (stops previous, starts new)
  agent-browser record restart ./take2.webm
"##
        }

        // === Console/Errors ===
        "console" => {
            r##"
agent-browser console - View console logs

Usage: agent-browser console [--clear]

View browser console output (log, warn, error, info).

Options:
  --clear              Clear console log buffer

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser console
  agent-browser console --clear
"##
        }
        "errors" => {
            r##"
agent-browser errors - View page errors

Usage: agent-browser errors [--clear]

View JavaScript errors and uncaught exceptions.

Options:
  --clear              Clear error buffer

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser errors
  agent-browser errors --clear
"##
        }

        // === Highlight ===
        "highlight" => {
            r##"
agent-browser highlight - Highlight an element

Usage: agent-browser highlight <selector>

Visually highlights an element on the page for debugging.

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser highlight "#target-element"
  agent-browser highlight @e5
"##
        }

        // === State ===
        "state" => {
            r##"
agent-browser state - Manage browser state

Usage: agent-browser state <operation> [args]

Save, restore, list, and manage browser state (cookies, localStorage, sessionStorage).

Operations:
  save <path>                        Save current state to file
  load <path>                        Load state from file
  list                               List saved state files
  show <filename>                    Show state summary
  rename <old-name> <new-name>       Rename state file
  clear [session-name] [--all]       Clear saved states
  clean --older-than <days>          Delete expired state files

Automatic State Persistence:
  Use --session-name to auto-save/restore state across restarts:
  agent-browser --session-name myapp open https://example.com
  Or set AGENT_BROWSER_SESSION_NAME environment variable.

State Encryption:
  Set AGENT_BROWSER_ENCRYPTION_KEY (64-char hex) for AES-256-GCM encryption.
  Generate a key: openssl rand -hex 32

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser state save ./auth-state.json
  agent-browser state load ./auth-state.json
  agent-browser state list
  agent-browser state show myapp-default.json
  agent-browser state rename old-name new-name
  agent-browser state clear --all
  agent-browser state clean --older-than 7
"##
        }

        // === Session ===
        "session" => {
            r##"
agent-browser session - Manage sessions

Usage: agent-browser session [operation]

Manage isolated browser sessions. Each session has its own browser
instance with separate cookies, storage, and state.

Operations:
  (none)               Show current session name
  list                 List all active sessions

Environment:
  AGENT_BROWSER_SESSION    Default session name

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser session
  agent-browser session list
  agent-browser --session test open example.com
"##
        }

        // === Install ===
        "install" => {
            r##"
agent-browser install - Install browser binaries

Usage: agent-browser install [--with-deps]

Downloads and installs browser binaries required for automation.

Options:
  -d, --with-deps      Also install system dependencies (Linux only)

Examples:
  agent-browser install
  agent-browser install --with-deps
"##
        }

        // === Connect ===
        "connect" => {
            r##"
agent-browser connect - Connect to browser via CDP

Usage: agent-browser connect <port|url>

Connects to a running browser instance via Chrome DevTools Protocol (CDP).
This allows controlling browsers, Electron apps, or remote browser services.

Arguments:
  <port>               Local port number (e.g., 9222)
  <url>                Full WebSocket URL (ws://, wss://, http://, https://)

Supported URL formats:
  - Port number: 9222 (connects to http://localhost:9222)
  - WebSocket URL: ws://localhost:9222/devtools/browser/...
  - Remote service: wss://remote-browser.example.com/cdp?token=...

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  # Connect to local Chrome with remote debugging
  # Start Chrome: google-chrome --remote-debugging-port=9222
  agent-browser connect 9222

  # Connect using WebSocket URL from /json/version endpoint
  agent-browser connect "ws://localhost:9222/devtools/browser/abc123"

  # Connect to remote browser service
  agent-browser connect "wss://browser-service.example.com/cdp?token=xyz"

  # After connecting, run commands normally
  agent-browser snapshot
  agent-browser click @e1
"##
        }

        // === iOS Commands ===
        "tap" => {
            r##"
agent-browser tap - Tap an element (touch gesture)

Usage: agent-browser tap <selector>

Taps an element. This is an alias for 'click' that provides semantic clarity
for touch-based interfaces like iOS Safari.

Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser tap "#submit-button"
  agent-browser tap @e1
  agent-browser -p ios tap "button:has-text('Sign In')"
"##
        }
        "swipe" => {
            r##"
agent-browser swipe - Swipe gesture (iOS)

Usage: agent-browser swipe <direction> [distance]

Performs a swipe gesture on iOS Safari. The direction determines
which way the content moves (swipe up scrolls down, etc.).

Arguments:
  direction    up, down, left, or right
  distance     Optional distance in pixels (default: 300)

Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser -p ios swipe up
  agent-browser -p ios swipe down 500
  agent-browser -p ios swipe left
"##
        }
        "device" => {
            r##"
agent-browser device - Manage iOS simulators

Usage: agent-browser device <subcommand>

Subcommands:
  list    List available iOS simulators

Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser device list
  agent-browser -p ios device list
"##
        }

        "diff" => {
            r##"
agent-browser diff - Compare page states

Subcommands:

  diff snapshot                   Compare current snapshot to last snapshot in session
  diff screenshot --baseline <f>  Visual pixel diff against a baseline image
  diff url <url1> <url2>          Compare two pages

Snapshot Diff:

  Usage: agent-browser diff snapshot [options]

  Options:
    -b, --baseline <file>    Compare against a saved snapshot file
    -s, --selector <sel>     Scope snapshot to a CSS selector or @ref
    -c, --compact            Use compact snapshot format
    -d, --depth <n>          Limit snapshot tree depth

  Without --baseline, compares against the last snapshot taken in this session.

Screenshot Diff:

  Usage: agent-browser diff screenshot --baseline <file> [options]

  Options:
    -b, --baseline <file>    Baseline image to compare against (required)
    -o, --output <file>      Path for the diff image (default: temp dir)
    -t, --threshold <0-1>    Color distance threshold (default: 0.1)
    -s, --selector <sel>     Scope screenshot to element
        --full               Full page screenshot

URL Diff:

  Usage: agent-browser diff url <url1> <url2> [options]

  Options:
    --screenshot             Also compare screenshots (default: snapshot only)
    --full                   Full page screenshots
    --wait-until <strategy>  Navigation wait strategy: load, domcontentloaded, networkidle (default: load)
    -s, --selector <sel>     Scope snapshots to a CSS selector or @ref
    -c, --compact            Use compact snapshot format
    -d, --depth <n>          Limit snapshot tree depth

Global Options:
  --json               Output as JSON
  --session <name>     Use specific session

Examples:
  agent-browser diff snapshot
  agent-browser diff snapshot --baseline before.txt
  agent-browser diff screenshot --baseline before.png
  agent-browser diff screenshot --baseline before.png --output diff.png --threshold 0.2
  agent-browser diff url https://staging.example.com https://prod.example.com
  agent-browser diff url https://v1.example.com https://v2.example.com --screenshot
"##
        }

        _ => return false,
    };
    println!("{}", help.trim());
    true
}

pub fn print_help() {
    println!(
        r#"
agent-browser - fast browser automation CLI for AI agents

Usage: agent-browser <command> [args] [options]

Core Commands:
  open <url>                 Navigate to URL
  click <sel>                Click element (or @ref)
  dblclick <sel>             Double-click element
  type <sel> <text>          Type into element
  fill <sel> <text>          Clear and fill
  press <key>                Press key (Enter, Tab, Control+a)
  keyboard type <text>       Type text with real keystrokes (no selector)
  keyboard inserttext <text> Insert text without key events
  hover <sel>                Hover element
  focus <sel>                Focus element
  check <sel>                Check checkbox
  uncheck <sel>              Uncheck checkbox
  select <sel> <val...>      Select dropdown option
  drag <src> <dst>           Drag and drop
  upload <sel> <files...>    Upload files
  download <sel> <path>      Download file by clicking element
  scroll <dir> [px]          Scroll (up/down/left/right)
  scrollintoview <sel>       Scroll element into view
  wait <sel|ms>              Wait for element or time
  screenshot [path]          Take screenshot
  pdf <path>                 Save as PDF
  snapshot                   Accessibility tree with refs (for AI)
  eval <js>                  Run JavaScript
  connect <port|url>         Connect to browser via CDP
  close                      Close browser

Navigation:
  back                       Go back
  forward                    Go forward
  reload                     Reload page

Get Info:  agent-browser get <what> [selector]
  text, html, value, attr <name>, title, url, count, box, styles

Check State:  agent-browser is <what> <selector>
  visible, enabled, checked

Find Elements:  agent-browser find <locator> <value> <action> [text]
  role, text, label, placeholder, alt, title, testid, first, last, nth

Mouse:  agent-browser mouse <action> [args]
  move <x> <y>, down [btn], up [btn], wheel <dy> [dx]

Browser Settings:  agent-browser set <setting> [value]
  viewport <w> <h>, device <name>, geo <lat> <lng>
  offline [on|off], headers <json>, credentials <user> <pass>
  media [dark|light] [reduced-motion]

Network:  agent-browser network <action>
  route <url> [--abort|--body <json>]
  unroute [url]
  requests [--clear] [--filter <pattern>]

Storage:
  cookies [get|set|clear]    Manage cookies (set supports --url, --domain, --path, --httpOnly, --secure, --sameSite, --expires)
  storage <local|session>    Manage web storage

Tabs:
  tab [new|list|close|<n>]   Manage tabs

Diff:
  diff snapshot              Compare current vs last snapshot
  diff screenshot --baseline Compare current vs baseline image
  diff url <u1> <u2>         Compare two pages

Debug:
  trace start|stop [path]    Record Playwright trace
  profiler start|stop [path] Record Chrome DevTools profile
  record start <path> [url]  Start video recording (WebM)
  record stop                Stop and save video
  console [--clear]          View console logs
  errors [--clear]           View page errors
  highlight <sel>            Highlight element

Auth Vault:
  auth save <name> [opts]    Save auth profile (--url, --username, --password/--password-stdin)
  auth login <name>          Login using saved credentials
  auth list                  List saved auth profiles
  auth show <name>           Show auth profile metadata
  auth delete <name>         Delete auth profile

Confirmation:
  confirm <id>               Approve a pending action
  deny <id>                  Deny a pending action

Sessions:
  session                    Show current session name
  session list               List active sessions

Setup:
  install                    Install browser binaries
  install --with-deps        Also install system dependencies (Linux)

Snapshot Options:
  -i, --interactive          Only interactive elements
  -c, --compact              Remove empty structural elements
  -d, --depth <n>            Limit tree depth
  -s, --selector <sel>       Scope to CSS selector

Options:
  --session <name>           Isolated session (or AGENT_BROWSER_SESSION env)
  --profile <path>           Persistent browser profile (or AGENT_BROWSER_PROFILE env)
  --state <path>             Load storage state from JSON file (or AGENT_BROWSER_STATE env)
  --headers <json>           HTTP headers scoped to URL's origin (for auth)
  --executable-path <path>   Custom browser executable (or AGENT_BROWSER_EXECUTABLE_PATH)
  --extension <path>         Load browser extensions (repeatable)
  --args <args>              Browser launch args, comma or newline separated (or AGENT_BROWSER_ARGS)
                             e.g., --args "--no-sandbox,--disable-blink-features=AutomationControlled"
  --user-agent <ua>          Custom User-Agent (or AGENT_BROWSER_USER_AGENT)
  --proxy <server>           Proxy server URL (or AGENT_BROWSER_PROXY)
                             e.g., --proxy "http://user:pass@127.0.0.1:7890"
  --proxy-bypass <hosts>     Bypass proxy for these hosts (or AGENT_BROWSER_PROXY_BYPASS)
                             e.g., --proxy-bypass "localhost,*.internal.com"
  --ignore-https-errors      Ignore HTTPS certificate errors
  --allow-file-access        Allow file:// URLs to access local files (Chromium only)
  -p, --provider <name>      Browser provider: ios, browserbase, kernel, browseruse
  --device <name>            iOS device name (e.g., "iPhone 15 Pro")
  --json                     JSON output
  --full, -f                 Full page screenshot
  --annotate                 Annotated screenshot with numbered labels and legend
  --headed                   Show browser window (not headless)
  --cdp <port>               Connect via CDP (Chrome DevTools Protocol)
  --auto-connect             Auto-discover and connect to running Chrome
  --color-scheme <scheme>    Color scheme: dark, light, no-preference (or AGENT_BROWSER_COLOR_SCHEME)
  --download-path <path>     Default download directory (or AGENT_BROWSER_DOWNLOAD_PATH)
  --session-name <name>      Auto-save/restore session state (cookies, localStorage)
  --content-boundaries       Wrap page output in boundary markers (or AGENT_BROWSER_CONTENT_BOUNDARIES)
  --max-output <chars>       Truncate page output to N chars (or AGENT_BROWSER_MAX_OUTPUT)
  --allowed-domains <list>   Restrict navigation domains (or AGENT_BROWSER_ALLOWED_DOMAINS)
  --action-policy <path>     Action policy JSON file (or AGENT_BROWSER_ACTION_POLICY)
  --confirm-actions <list>   Categories requiring confirmation (or AGENT_BROWSER_CONFIRM_ACTIONS)
  --confirm-interactive      Interactive confirmation prompts; auto-denies if stdin is not a TTY (or AGENT_BROWSER_CONFIRM_INTERACTIVE)
  --config <path>            Use a custom config file (or AGENT_BROWSER_CONFIG env)
  --debug                    Debug output
  --version, -V              Show version

Configuration:
  agent-browser looks for agent-browser.json in these locations (lowest to highest priority):
    1. ~/.agent-browser/config.json      User-level defaults
    2. ./agent-browser.json              Project-level overrides
    3. Environment variables             Override config file values
    4. CLI flags                         Override everything

  Use --config <path> to load a specific config file instead of the defaults.
  If --config points to a missing or invalid file, agent-browser exits with an error.

  Boolean flags accept an optional true/false value to override config:
    --headed           (same as --headed true)
    --headed false     (disables "headed": true from config)

  Extensions from user and project configs are merged (not replaced).

  Example agent-browser.json:
    {{"headed": true, "proxy": "http://localhost:8080", "profile": "./browser-data"}}

Environment:
  AGENT_BROWSER_CONFIG           Path to config file (or use --config)
  AGENT_BROWSER_SESSION          Session name (default: "default")
  AGENT_BROWSER_SESSION_NAME     Auto-save/restore state persistence name
  AGENT_BROWSER_ENCRYPTION_KEY   64-char hex key for AES-256-GCM state encryption
  AGENT_BROWSER_STATE_EXPIRE_DAYS Auto-delete states older than N days (default: 30)
  AGENT_BROWSER_EXECUTABLE_PATH  Custom browser executable path
  AGENT_BROWSER_EXTENSIONS       Comma-separated browser extension paths
  AGENT_BROWSER_HEADED           Show browser window (not headless)
  AGENT_BROWSER_JSON             JSON output
  AGENT_BROWSER_FULL             Full page screenshot
  AGENT_BROWSER_ANNOTATE         Annotated screenshot with numbered labels and legend
  AGENT_BROWSER_DEBUG            Debug output
  AGENT_BROWSER_IGNORE_HTTPS_ERRORS Ignore HTTPS certificate errors
  AGENT_BROWSER_PROVIDER         Browser provider (ios, browserbase, kernel, browseruse)
  AGENT_BROWSER_AUTO_CONNECT     Auto-discover and connect to running Chrome
  AGENT_BROWSER_ALLOW_FILE_ACCESS Allow file:// URLs to access local files
  AGENT_BROWSER_COLOR_SCHEME     Color scheme preference (dark, light, no-preference)
  AGENT_BROWSER_DOWNLOAD_PATH    Default download directory for browser downloads
  AGENT_BROWSER_DEFAULT_TIMEOUT  Default Playwright timeout in ms (default: 25000)
  AGENT_BROWSER_SESSION_NAME     Auto-save/load state persistence name
  AGENT_BROWSER_STATE_EXPIRE_DAYS Auto-delete saved states older than N days (default: 30)
  AGENT_BROWSER_ENCRYPTION_KEY   64-char hex key for AES-256-GCM session encryption
  AGENT_BROWSER_STREAM_PORT      Enable WebSocket streaming on port (e.g., 9223)
  AGENT_BROWSER_IOS_DEVICE       Default iOS device name
  AGENT_BROWSER_IOS_UDID         Default iOS device UDID
  AGENT_BROWSER_CONTENT_BOUNDARIES Wrap page output in boundary markers
  AGENT_BROWSER_MAX_OUTPUT       Max characters for page output
  AGENT_BROWSER_ALLOWED_DOMAINS  Comma-separated allowed domain patterns
  AGENT_BROWSER_ACTION_POLICY    Path to action policy JSON file
  AGENT_BROWSER_CONFIRM_ACTIONS  Action categories requiring confirmation
  AGENT_BROWSER_CONFIRM_INTERACTIVE Enable interactive confirmation prompts

Install (recommended, fastest - native Rust CLI):
  npm install -g agent-browser
  agent-browser install                  # Download Chromium (first time)

Try without installing (slower, routes through Node.js):
  npx agent-browser open example.com

Examples:
  agent-browser open example.com
  agent-browser snapshot -i              # Interactive elements only
  agent-browser click @e2                # Click by ref from snapshot
  agent-browser fill @e3 "test@example.com"
  agent-browser find role button click --name Submit
  agent-browser get text @e1
  agent-browser screenshot --full
  agent-browser screenshot --annotate    # Labeled screenshot for vision models
  agent-browser wait --load networkidle  # Wait for slow pages to load
  agent-browser --cdp 9222 snapshot      # Connect via CDP port
  agent-browser --auto-connect snapshot  # Auto-discover running Chrome
  agent-browser --color-scheme dark open example.com  # Dark mode
  agent-browser --profile ~/.myapp open example.com    # Persistent profile
  agent-browser --session-name myapp open example.com  # Auto-save/restore state

Command Chaining:
  Chain commands with && in a single shell call (browser persists via daemon):

  agent-browser open example.com && agent-browser wait --load networkidle && agent-browser snapshot -i
  agent-browser fill @e1 "user@example.com" && agent-browser fill @e2 "pass" && agent-browser click @e3
  agent-browser open example.com && agent-browser wait --load networkidle && agent-browser screenshot page.png

iOS Simulator (requires Xcode and Appium):
  agent-browser -p ios open example.com                    # Use default iPhone
  agent-browser -p ios --device "iPhone 15 Pro" open url   # Specific device
  agent-browser -p ios device list                         # List simulators
  agent-browser -p ios swipe up                            # Swipe gesture
  agent-browser -p ios tap @e1                             # Touch element
"#
    );
}

fn print_snapshot_diff(data: &serde_json::Map<String, serde_json::Value>) {
    let changed = data
        .get("changed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !changed {
        println!("{} No changes detected", color::success_indicator());
        return;
    }
    if let Some(diff) = data.get("diff").and_then(|v| v.as_str()) {
        for line in diff.lines() {
            if line.starts_with("+ ") {
                println!("{}", color::green(line));
            } else if line.starts_with("- ") {
                println!("{}", color::red(line));
            } else {
                println!("{}", color::dim(line));
            }
        }
        let additions = data.get("additions").and_then(|v| v.as_i64()).unwrap_or(0);
        let removals = data.get("removals").and_then(|v| v.as_i64()).unwrap_or(0);
        let unchanged = data.get("unchanged").and_then(|v| v.as_i64()).unwrap_or(0);
        println!(
            "\n{} additions, {} removals, {} unchanged",
            color::green(&additions.to_string()),
            color::red(&removals.to_string()),
            unchanged
        );
    }
}

fn print_screenshot_diff(data: &serde_json::Map<String, serde_json::Value>) {
    let mismatch = data
        .get("mismatchPercentage")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let is_match = data
        .get("match")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let dim_mismatch = data
        .get("dimensionMismatch")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if dim_mismatch {
        println!(
            "{} Images have different dimensions",
            color::error_indicator()
        );
    } else if is_match {
        println!("{} Images match (0% difference)", color::success_indicator());
    } else {
        println!(
            "{} {:.2}% pixels differ",
            color::error_indicator(),
            mismatch
        );
    }
    if let Some(diff_path) = data.get("diffPath").and_then(|v| v.as_str()) {
        println!("  Diff image: {}", color::green(diff_path));
    }
    let total = data.get("totalPixels").and_then(|v| v.as_i64()).unwrap_or(0);
    let different = data
        .get("differentPixels")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    println!(
        "  {} different / {} total pixels",
        color::red(&different.to_string()),
        total
    );
}

pub fn print_version() {
    println!("agent-browser {}", env!("CARGO_PKG_VERSION"));
}
