mod chat;
mod color;
mod commands;
mod connection;
mod doctor;
mod flags;
mod install;
mod mcp;
mod native;
mod output;
mod plugins;
mod read;
mod skills;
#[cfg(test)]
mod test_utils;
mod upgrade;
mod validation;

use serde_json::json;
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::{exit, Command};

#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::OpenProcess;

use commands::{gen_id, parse_command, ParseError};
use connection::{
    cleanup_stale_files, daemon_unreachable, ensure_daemon, get_socket_dir, is_pid_alive,
    read_provider_session_id, send_command, walk_daemons, DaemonOptions, Response,
};
use flags::{clean_args, parse_flags, Flags};
use install::run_install;
use output::{
    print_command_help, print_help, print_response_with_opts, print_version, OutputOptions,
};
use upgrade::run_upgrade;

fn serialize_json_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| {
        r#"{"success":false,"error":"Failed to serialize JSON response"}"#.to_string()
    })
}

fn print_json_value(value: serde_json::Value) {
    println!("{}", serialize_json_value(&value));
}

fn print_json_error(message: impl AsRef<str>) {
    print_json_value(json!({
        "success": false,
        "error": message.as_ref(),
    }));
}

fn print_json_error_with_type(message: impl AsRef<str>, error_type: &str) {
    print_json_value(json!({
        "success": false,
        "error": message.as_ref(),
        "type": error_type,
    }));
}

fn should_send_hide_scrollbars_launch_option(
    cli_hide_scrollbars: bool,
    hide_scrollbars: bool,
) -> bool {
    cli_hide_scrollbars || !hide_scrollbars
}

fn apply_hide_scrollbars_launch_option(
    launch_cmd: &mut serde_json::Value,
    cli_hide_scrollbars: bool,
    hide_scrollbars: bool,
) {
    if should_send_hide_scrollbars_launch_option(cli_hide_scrollbars, hide_scrollbars) {
        launch_cmd["hideScrollbars"] = json!(hide_scrollbars);
    }
}

fn attach_script_launch_options(launch_cmd: &mut serde_json::Value, flags: &Flags) {
    if !flags.init_scripts.is_empty() {
        launch_cmd["initScripts"] = json!(&flags.init_scripts);
    }
    if !flags.enable.is_empty() {
        launch_cmd["enable"] = json!(&flags.enable);
    }
}

fn attach_plugins_to_command(cmd: &mut serde_json::Value, plugins: &[plugins::PluginConfig]) {
    cmd["plugins"] = json!(plugins);
}

fn restore_key_from_flags(flags: &Flags) -> Option<&str> {
    flags.restore.as_deref().or(flags.session_name.as_deref())
}

fn is_valid_restore_save_policy(policy: &str) -> bool {
    matches!(policy, "auto" | "always" | "never")
}

fn incompatible_launch_mode_error(flags: &Flags) -> Option<&'static str> {
    if flags.cdp.is_some() && flags.provider.is_some() {
        return Some("Cannot use --cdp and -p/--provider together");
    }

    if flags.auto_connect && flags.cdp.is_some() {
        return Some("Cannot use --auto-connect and --cdp together");
    }

    if flags.auto_connect && flags.provider.is_some() {
        return Some("Cannot use --auto-connect and -p/--provider together");
    }

    if flags.provider.is_some() && !flags.extensions.is_empty() {
        return Some(
            "Cannot use --extension with -p/--provider (extensions require local browser)",
        );
    }

    if flags.cdp.is_some() && !flags.extensions.is_empty() {
        return Some("Cannot use --extension with --cdp (extensions require local browser)");
    }

    None
}

fn attach_restore_config_to_command(cmd: &mut serde_json::Value, flags: &Flags) {
    if let Some(restore_key) = restore_key_from_flags(flags) {
        cmd["restoreKey"] = json!(restore_key);
        cmd["restoreSave"] = json!(flags.restore_save.as_deref().unwrap_or("auto"));
        cmd["restoreCheckUrl"] = flags
            .restore_check_url
            .as_ref()
            .map(|check| json!(check))
            .unwrap_or(serde_json::Value::Null);
        cmd["restoreCheckText"] = flags
            .restore_check_text
            .as_ref()
            .map(|check| json!(check))
            .unwrap_or(serde_json::Value::Null);
        cmd["restoreCheckFn"] = flags
            .restore_check_fn
            .as_ref()
            .map(|check| json!(check))
            .unwrap_or(serde_json::Value::Null);
    }
}

fn mark_restarted_background(resp: &mut Response) {
    if !resp.success {
        return;
    }

    let Some(data) = resp.data.as_mut().and_then(|v| v.as_object_mut()) else {
        return;
    };

    let lifecycle = data
        .entry("lifecycle".to_string())
        .or_insert_with(|| json!({}));
    if !lifecycle.is_object() {
        *lifecycle = json!({});
    }
    if let Some(obj) = lifecycle.as_object_mut() {
        obj.insert("restartedBackground".to_string(), json!(true));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfirmationPrompt {
    action: String,
    category: String,
    description: String,
    confirmation_id: String,
}

fn confirmation_prompt_from_data(data: &serde_json::Value) -> Option<ConfirmationPrompt> {
    if data
        .get("confirmation_required")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let action = data
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        return Some(ConfirmationPrompt {
            action: action.clone(),
            category: data
                .get("category")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            description: data
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(action.as_str())
                .to_string(),
            confirmation_id: data
                .get("confirmation_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        });
    }

    data.get("result")
        .and_then(|v| v.get("data"))
        .and_then(confirmation_prompt_from_data)
}

fn confirmation_prompt_from_response(resp: &Response) -> Option<ConfirmationPrompt> {
    resp.data.as_ref().and_then(confirmation_prompt_from_data)
}

fn run_interactive_confirmations(
    mut resp: Response,
    flags: &Flags,
    output_opts: &OutputOptions,
) -> Response {
    while let Some(prompt) = confirmation_prompt_from_response(&resp) {
        eprintln!("[agent-browser] Action requires confirmation:");
        if prompt.category.is_empty() {
            eprintln!("  {}", prompt.description);
        } else {
            eprintln!("  {}: {}", prompt.category, prompt.description);
        }
        eprint!("  Allow? [y/N]: ");

        let mut input = String::new();
        let approved = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            std::io::stdin().read_line(&mut input).is_ok()
                && matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
        } else {
            false
        };

        let confirm_cmd = if approved {
            json!({
                "id": gen_id(),
                "action": "confirm",
                "confirmationId": prompt.confirmation_id
            })
        } else {
            json!({
                "id": gen_id(),
                "action": "deny",
                "confirmationId": prompt.confirmation_id
            })
        };

        match send_command(confirm_cmd, &flags.session) {
            Ok(next_resp) => {
                if !approved {
                    eprintln!("{} Action denied", color::error_indicator());
                    exit(1);
                }
                resp = next_resp;
            }
            Err(e) => {
                eprintln!("{} {}", color::error_indicator(), e);
                exit(1);
            }
        }
    }

    print_response_with_opts(&resp, None, output_opts);
    resp
}

struct ParsedProxy {
    server: String,
    username: Option<String>,
    password: Option<String>,
}

fn parse_proxy(proxy_str: &str) -> ParsedProxy {
    let Some(protocol_end) = proxy_str.find("://") else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };
    let protocol = &proxy_str[..protocol_end + 3];
    let rest = &proxy_str[protocol_end + 3..];

    let Some(at_pos) = rest.rfind('@') else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };

    let creds = &rest[..at_pos];
    let server_part = &rest[at_pos + 1..];
    let server = format!("{}{}", protocol, server_part);

    let (username, password) = match creds.find(':') {
        Some(colon_pos) => {
            let u = &creds[..colon_pos];
            let p = &creds[colon_pos + 1..];
            (
                if u.is_empty() {
                    None
                } else {
                    Some(u.to_string())
                },
                if p.is_empty() {
                    None
                } else {
                    Some(p.to_string())
                },
            )
        }
        None => (
            if creds.is_empty() {
                None
            } else {
                Some(creds.to_string())
            },
            None,
        ),
    };

    ParsedProxy {
        server,
        username,
        password,
    }
}

fn run_profiles(json_mode: bool) {
    use crate::native::cdp::chrome::{find_chrome_user_data_dir, list_chrome_profiles};

    let user_data_dir = match find_chrome_user_data_dir() {
        Some(dir) => dir,
        None => {
            if json_mode {
                print_json_error("No Chrome user data directory found");
            } else {
                eprintln!("{}", color::red("No Chrome user data directory found"));
            }
            exit(1);
        }
    };

    let profiles = list_chrome_profiles(&user_data_dir);
    if profiles.is_empty() {
        if json_mode {
            print_json_value(json!({
                "success": true,
                "data": []
            }));
        } else {
            println!("No Chrome profiles found");
        }
        return;
    }

    if json_mode {
        let items: Vec<serde_json::Value> = profiles
            .iter()
            .map(|p| {
                json!({
                    "directory": p.directory,
                    "name": p.name
                })
            })
            .collect();
        print_json_value(json!({
            "success": true,
            "data": items
        }));
    } else {
        println!(
            "{} ({}):\n",
            color::bold("Chrome profiles"),
            user_data_dir.display()
        );
        for p in &profiles {
            println!(
                "  {}  {}",
                color::bold(&p.directory),
                color::dim(&format!("({})", p.name))
            );
        }
    }
}

fn canonical_path(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn git_toplevel() -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?;
    let path = raw.trim();
    if path.is_empty() {
        None
    } else {
        Some(canonical_path(PathBuf::from(path)))
    }
}

fn resolve_session_id_scope(scope: &str) -> Result<(String, PathBuf), String> {
    match scope {
        "worktree" => {
            let path = git_toplevel().unwrap_or_else(|| {
                canonical_path(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            });
            Ok(("worktree".to_string(), path))
        }
        "cwd" => {
            let path = canonical_path(env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            Ok(("cwd".to_string(), path))
        }
        "git-root" => git_toplevel()
            .map(|path| ("git-root".to_string(), path))
            .ok_or_else(|| "Not inside a Git working tree".to_string()),
        other => Err(format!(
            "Unknown session id scope '{}'. Use worktree, cwd, or git-root.",
            other
        )),
    }
}

fn run_session_id(args: &[String], json_mode: bool) {
    let mut scope = "worktree".to_string();
    let mut prefix: Option<String> = None;
    let mut json_output = json_mode;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--scope" => {
                if let Some(value) = args.get(i + 1) {
                    scope = value.clone();
                    i += 1;
                }
            }
            "--prefix" => {
                if let Some(value) = args.get(i + 1) {
                    prefix = Some(value.clone());
                    i += 1;
                }
            }
            "--json" => json_output = true,
            _ => {}
        }
        i += 1;
    }

    let (resolved_scope, path) = match resolve_session_id_scope(&scope) {
        Ok(result) => result,
        Err(e) => {
            if json_output {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    };

    let path_str = path.to_string_lossy().to_string();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let suffix = &hash[..12];

    let prefix = prefix
        .as_deref()
        .map(validation::sanitize_session_component)
        .filter(|s| !s.is_empty());
    let session = match prefix {
        Some(prefix) => format!("{}-{}", prefix, suffix),
        None => suffix.to_string(),
    };

    if json_output {
        print_json_value(json!({
            "success": true,
            "data": {
                "session": session,
                "scope": resolved_scope,
                "path": path_str,
                "hash": suffix
            }
        }));
    } else {
        println!("{}", session);
    }
}

fn run_session_info(session: &str, json_mode: bool) {
    let inventory = walk_daemons();
    let active = inventory.sessions.iter().find(|s| s.name == session);
    let runtime = active.and_then(|_| {
        send_command(
            json!({
                "id": gen_id(),
                "action": "session_info"
            }),
            session,
        )
        .ok()
    });

    let runtime_data = runtime.as_ref().and_then(|resp| resp.data.clone());
    let runtime_error = runtime.as_ref().and_then(|resp| {
        if resp.success {
            None
        } else {
            resp.error.clone()
        }
    });

    if json_mode {
        print_json_value(json!({
            "success": true,
            "data": {
                "session": session,
                "namespace": env::var("AGENT_BROWSER_NAMESPACE").ok(),
                "socketDir": get_socket_dir().to_string_lossy(),
                "active": active.is_some(),
                "pid": active.map(|s| s.pid),
                "version": active.and_then(|s| s.version.clone()),
                "runtime": runtime_data,
                "runtimeError": runtime_error,
            }
        }));
        return;
    }

    println!("Session: {}", session);
    println!("Socket dir: {}", get_socket_dir().to_string_lossy());
    if let Ok(namespace) = env::var("AGENT_BROWSER_NAMESPACE") {
        println!("Namespace: {}", namespace);
    }
    if let Some(active) = active {
        println!("Daemon: running (pid {})", active.pid);
        if let Some(ref version) = active.version {
            println!("Version: {}", version);
        }
    } else {
        println!("Daemon: not running");
    }
    if let Some(data) = runtime_data {
        if let Some(restore_status) = data.get("restoreStatus").and_then(|v| v.as_str()) {
            println!("Restore status: {}", restore_status);
        }
        if let Some(save_status) = data.get("saveStatus").and_then(|v| v.as_str()) {
            println!("Save status: {}", save_status);
        }
        if let Some(engine) = data.get("engine").and_then(|v| v.as_str()) {
            println!("Engine: {}", engine);
        }
        if let Some(launched) = data.get("browserLaunched").and_then(|v| v.as_bool()) {
            println!("Browser launched: {}", launched);
        }
    } else if let Some(err) = runtime_error {
        println!("Runtime info unavailable: {}", err);
    }
}

fn run_session(args: &[String], session: &str, json_mode: bool) {
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        Some("id") => run_session_id(args, json_mode),
        Some("info") => run_session_info(session, json_mode),
        Some("list") => {
            let sessions: Vec<String> = walk_daemons()
                .sessions
                .into_iter()
                .map(|s| s.name)
                .collect();

            if json_mode {
                println!(
                    r#"{{"success":true,"data":{{"sessions":{}}}}}"#,
                    serde_json::to_string(&sessions).unwrap_or_default()
                );
            } else if sessions.is_empty() {
                println!("No active sessions");
            } else {
                println!("Active sessions:");
                for s in &sessions {
                    let marker = if s == session {
                        color::cyan("→")
                    } else {
                        " ".to_string()
                    };
                    println!("{} {}", marker, s);
                }
            }
        }
        None | Some(_) => {
            // Just show current session
            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": {
                        "session": session,
                    },
                }));
            } else {
                println!("{}", session);
            }
        }
    }
}

fn get_dashboard_pid_path() -> std::path::PathBuf {
    get_socket_dir().join("dashboard.pid")
}

fn run_dashboard_start(port: u16, json_mode: bool) {
    let pid_path = get_dashboard_pid_path();

    // Check if already running
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            if is_pid_alive(pid) {
                if json_mode {
                    print_json_value(json!({
                        "success": true,
                        "data": { "port": port, "pid": pid, "already_running": true },
                    }));
                } else {
                    println!("Dashboard already running at http://localhost:{}", port);
                }
                return;
            }
        }
        let _ = fs::remove_file(&pid_path);
    }

    let socket_dir = get_socket_dir();
    if !socket_dir.exists() {
        let _ = fs::create_dir_all(&socket_dir);
    }

    let exe_path = match env::current_exe() {
        Ok(p) => p.canonicalize().unwrap_or(p),
        Err(e) => {
            if json_mode {
                print_json_error(format!("Failed to get executable path: {}", e));
            } else {
                eprintln!(
                    "{} Failed to get executable path: {}",
                    color::error_indicator(),
                    e
                );
            }
            exit(1);
        }
    };

    let mut cmd = std::process::Command::new(&exe_path);
    cmd.env("AGENT_BROWSER_DASHBOARD", "1")
        .env("AGENT_BROWSER_DASHBOARD_PORT", port.to_string());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    match cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => {
            let pid = child.id();
            let _ = fs::write(&pid_path, pid.to_string());

            if json_mode {
                print_json_value(json!({
                    "success": true,
                    "data": { "port": port, "pid": pid },
                }));
            } else {
                println!("Dashboard started at http://localhost:{}", port);
            }
        }
        Err(e) => {
            if json_mode {
                print_json_error(format!("Failed to start dashboard: {}", e));
            } else {
                eprintln!(
                    "{} Failed to start dashboard: {}",
                    color::error_indicator(),
                    e
                );
            }
            exit(1);
        }
    }
}

fn run_dashboard_stop(json_mode: bool) {
    let pid_path = get_dashboard_pid_path();

    let pid_str = match fs::read_to_string(&pid_path) {
        Ok(s) => s,
        Err(_) => {
            if json_mode {
                print_json_value(
                    json!({ "success": true, "data": { "stopped": false, "reason": "not running" } }),
                );
            } else {
                println!("Dashboard is not running");
            }
            return;
        }
    };

    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            let _ = fs::remove_file(&pid_path);
            if json_mode {
                print_json_value(
                    json!({ "success": true, "data": { "stopped": false, "reason": "invalid pid" } }),
                );
            } else {
                println!("Dashboard is not running");
            }
            return;
        }
    };

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(windows)]
    {
        unsafe {
            let handle = OpenProcess(1, 0, pid); // PROCESS_TERMINATE = 1
            if handle != 0 {
                windows_sys::Win32::System::Threading::TerminateProcess(handle, 0);
                CloseHandle(handle);
            }
        }
    }

    let _ = fs::remove_file(&pid_path);

    if json_mode {
        print_json_value(json!({ "success": true, "data": { "stopped": true } }));
    } else {
        println!("{} Dashboard stopped", color::green("✓"));
    }
}

fn read_provider_file(session: &str) -> Option<String> {
    let path = get_socket_dir().join(format!("{}.provider", session));
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Best-effort cleanup of a remote provider session when the local daemon is
/// unreachable. This prevents orphaned cloud sessions (e.g. Browser Use) from
/// counting against the plan's concurrent session limit.
async fn cleanup_orphaned_provider_session(session: &str) {
    let provider = match read_provider_file(session) {
        Some(p) => p,
        None => return,
    };
    let session_id = match read_provider_session_id(session) {
        Some(id) => id,
        None => return,
    };

    let client = reqwest::Client::new();
    match provider.as_str() {
        "browser-use" | "browseruse" => {
            if let Ok(api_key) = env::var("BROWSER_USE_API_KEY") {
                let _ = client
                    .patch(format!(
                        "https://api.browser-use.com/api/v3/browsers/{}",
                        session_id
                    ))
                    .header("X-Browser-Use-API-Key", &api_key)
                    .header("Content-Type", "application/json")
                    .json(&json!({ "action": "stop" }))
                    .send()
                    .await;
            }
        }
        "browserbase" => {
            if let Ok(api_key) = env::var("BROWSERBASE_API_KEY") {
                let _ = client
                    .post(format!(
                        "https://api.browserbase.com/v1/sessions/{}",
                        session_id
                    ))
                    .header("Content-Type", "application/json")
                    .header("X-BB-API-Key", &api_key)
                    .json(&serde_json::json!({ "status": "REQUEST_RELEASE" }))
                    .send()
                    .await;
            }
        }
        "browserless" => {
            let _ = client.delete(&session_id).send().await;
        }
        "kernel" => {
            if let Ok(api_key) = env::var("KERNEL_API_KEY") {
                let endpoint = env::var("KERNEL_ENDPOINT")
                    .unwrap_or_else(|_| "https://api.onkernel.com".to_string());
                let _ = client
                    .delete(format!(
                        "{}/browsers/{}",
                        endpoint.trim_end_matches('/'),
                        session_id
                    ))
                    .header("Authorization", format!("Bearer {}", api_key))
                    .send()
                    .await;
            }
        }
        _ => {}
    }
}

fn run_close_all(flags: &Flags) {
    // walk_daemons auto-cleans stale .pid / .sock / .stream sidecar files and
    // separates out the standalone dashboard. We only want to send `close` to
    // real session daemons; the dashboard has its own `dashboard stop`.
    let inventory = walk_daemons();
    let sessions: Vec<(String, u32)> = inventory
        .sessions
        .iter()
        .map(|s| (s.name.clone(), s.pid))
        .collect();

    if sessions.is_empty() {
        if flags.json {
            print_json_value(json!({
                "success": true,
                "data": { "closed": 0, "sessions": [] },
            }));
        } else {
            println!("No active sessions");
        }
        return;
    }

    let mut closed: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();

    for (session, pid) in &sessions {
        let cmd = json!({ "id": gen_id(), "action": "close" });
        match send_command(cmd, session) {
            Ok(resp) if resp.success => closed.push(session.clone()),
            Ok(resp) => {
                let err = resp.error.unwrap_or_else(|| "Unknown error".to_string());
                failed.push((session.clone(), err));
            }
            Err(_) => {
                // Daemon is unreachable despite its process existing.
                // Force-kill the process and clean up stale files so future
                // sessions are not poisoned.
                #[cfg(unix)]
                unsafe {
                    libc::kill(*pid as i32, libc::SIGKILL);
                }
                #[cfg(windows)]
                unsafe {
                    let handle = OpenProcess(1, 0, *pid); // PROCESS_TERMINATE = 1
                    if handle != 0 {
                        windows_sys::Win32::System::Threading::TerminateProcess(handle, 1);
                        CloseHandle(handle);
                    }
                }
                // Best-effort cleanup of remote provider session (e.g. Browser Use)
                // so orphaned cloud sessions don't count against plan limits.
                let rt = tokio::runtime::Runtime::new().ok();
                if let Some(rt) = rt {
                    rt.block_on(cleanup_orphaned_provider_session(session));
                }
                cleanup_stale_files(session);
                closed.push(session.clone());
            }
        }
    }

    if flags.json {
        print_json_value(json!({
            "success": failed.is_empty(),
            "data": {
                "closed": closed.len(),
                "sessions": closed,
                "failed": failed.iter().map(|(s, e)| json!({"session": s, "error": e})).collect::<Vec<_>>(),
            },
        }));
    } else {
        for s in &closed {
            println!("{} Closed session: {}", color::green("✓"), s);
        }
        for (s, e) in &failed {
            eprintln!("{} Failed to close {}: {}", color::error_indicator(), s, e);
        }
        if closed.is_empty() && !failed.is_empty() {
            exit(1);
        }
    }

    if !failed.is_empty() {
        exit(1);
    }
}

fn main() {
    // Rust ignores SIGPIPE by default, causing println! to panic on broken pipes.
    // Reset to SIG_DFL so the OS terminates the process cleanly instead.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    // Prevent MSYS/Git Bash path translation from mangling arguments
    #[cfg(windows)]
    {
        env::set_var("MSYS_NO_PATHCONV", "1");
        env::set_var("MSYS2_ARG_CONV_EXCL", "*");
    }

    // Native daemon mode: when AGENT_BROWSER_DAEMON is set, run as the daemon process
    if env::var("AGENT_BROWSER_DAEMON").is_ok() {
        // Ignore SIGPIPE so the daemon isn't killed when the parent drops
        // the piped stderr handle after confirming the daemon is ready.
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        let session = env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string());
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(native::daemon::run_daemon(&session));
        return;
    }

    // Standalone dashboard server mode
    if env::var("AGENT_BROWSER_DASHBOARD").is_ok() {
        let port: u16 = env::var("AGENT_BROWSER_DASHBOARD_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(4848);
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(native::stream::run_dashboard_server(port));
        return;
    }

    let args: Vec<String> = env::args().skip(1).collect();
    let mut flags = parse_flags(&args);
    if flags.restore_uses_session {
        flags.restore = Some(flags.session.clone());
    }
    if let Some(ref namespace) = flags.namespace {
        env::set_var("AGENT_BROWSER_NAMESPACE", namespace);
    }
    let clean = clean_args(&args);

    let has_help = args.iter().any(|a| a == "--help" || a == "-h");
    let has_version = args.iter().any(|a| a == "--version" || a == "-V");

    if has_help {
        if let Some(cmd) = clean.first() {
            if print_command_help(cmd) {
                return;
            }
        }
        print_help();
        return;
    }

    if has_version {
        print_version();
        return;
    }

    if clean.is_empty() {
        print_help();
        return;
    }

    // Handle install separately
    if clean.first().map(|s| s.as_str()) == Some("install") {
        let with_deps = args.iter().any(|a| a == "--with-deps" || a == "-d");
        run_install(with_deps);
        return;
    }

    // Handle upgrade separately
    if clean.first().map(|s| s.as_str()) == Some("upgrade") {
        run_upgrade();
        return;
    }

    // Handle doctor separately (doesn't need daemon; spawns its own scratch
    // session for the live launch test).
    if clean.first().map(|s| s.as_str()) == Some("doctor") {
        let opts = doctor::DoctorOptions {
            offline: args.iter().any(|a| a == "--offline"),
            quick: args.iter().any(|a| a == "--quick"),
            fix: args.iter().any(|a| a == "--fix"),
            json: flags.json,
        };
        exit(doctor::run_doctor(opts));
    }

    // Handle dashboard subcommand
    if clean.first().map(|s| s.as_str()) == Some("dashboard") {
        match clean.get(1).map(|s| s.as_str()) {
            Some("start") | None => {
                let port = clean
                    .iter()
                    .position(|a| a == "--port")
                    .and_then(|i| clean.get(i + 1))
                    .and_then(|s| s.parse::<u16>().ok())
                    .unwrap_or(4848);
                run_dashboard_start(port, flags.json);
                return;
            }
            Some("stop") => {
                run_dashboard_stop(flags.json);
                return;
            }
            Some(unknown) => {
                eprintln!(
                    "{} Unknown dashboard subcommand: {}",
                    color::error_indicator(),
                    unknown
                );
                exit(1);
            }
        }
    }

    // Handle profiles command (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("profiles") {
        run_profiles(flags.json);
        return;
    }

    // Handle skills command (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("skills") {
        skills::run_skills(&clean, flags.json);
        return;
    }

    // Handle plugin registry commands (doesn't need daemon)
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("plugin") | Some("plugins")
    ) {
        plugins::run_plugin_command(&clean, &flags.plugins, flags.json);
        return;
    }

    // Handle MCP stdio server mode. This must never share stdout with normal
    // CLI output because stdout is reserved for JSON-RPC protocol messages.
    if clean.first().map(|s| s.as_str()) == Some("mcp") {
        if let Err(err) = mcp::run_mcp(&clean[1..]) {
            eprintln!("{} {}", color::error_indicator(), err);
            exit(1);
        }
        return;
    }

    // Handle session separately (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("session") {
        run_session(&clean, &flags.session, flags.json);
        return;
    }

    // Handle close --all: close all active sessions
    if matches!(
        clean.first().map(|s| s.as_str()),
        Some("close") | Some("quit") | Some("exit")
    ) && clean.iter().any(|a| a == "--all")
    {
        run_close_all(&flags);
        return;
    }

    // Handle chat command
    if clean.first().map(|s| s.as_str()) == Some("chat") {
        let message = if clean.len() > 1 {
            Some(clean[1..].join(" "))
        } else {
            None
        };
        chat::run_chat(&flags, message);
        return;
    }

    let mut cmd = match parse_command(&clean, &flags) {
        Ok(c) => c,
        Err(e) => {
            if flags.json {
                let error_type = match &e {
                    ParseError::UnknownCommand { .. } => "unknown_command",
                    ParseError::UnknownSubcommand { .. } => "unknown_subcommand",
                    ParseError::MissingArguments { .. } => "missing_arguments",
                    ParseError::InvalidValue { .. } => "invalid_value",
                    ParseError::InvalidSessionName { .. } => "invalid_session_name",
                };
                print_json_error_with_type(e.format(), error_type);
            } else {
                eprintln!("{}", color::red(&e.format()));
            }
            exit(1);
        }
    };

    // Handle --password-stdin for auth save
    if cmd.get("action").and_then(|v| v.as_str()) == Some("auth_save") {
        if cmd.get("password").is_some() {
            eprintln!(
                "{} Passwords on the command line may be visible in process listings and shell history. Use --password-stdin instead.",
                color::warning_indicator()
            );
        }
        if cmd
            .get("passwordStdin")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            let mut pass = String::new();
            if std::io::stdin().read_line(&mut pass).is_err() || pass.is_empty() {
                eprintln!(
                    "{} Failed to read password from stdin",
                    color::error_indicator()
                );
                exit(1);
            }
            let pass = pass.trim_end_matches('\n').trim_end_matches('\r');
            if pass.is_empty() {
                eprintln!("{} Password from stdin is empty", color::error_indicator());
                exit(1);
            }
            cmd["password"] = json!(pass);
            cmd.as_object_mut().unwrap().remove("passwordStdin");
        }
    }

    // Send plugin config with commands so an already-running daemon can use
    // current config without a restart. The daemon strips this from stream
    // broadcasts before observers see the command payload.
    attach_plugins_to_command(&mut cmd, &flags.plugins);
    attach_restore_config_to_command(&mut cmd, &flags);

    let restore_key = restore_key_from_flags(&flags);

    // Validate restore/session persistence name before starting daemon
    if let Some(name) = restore_key {
        if !validation::is_valid_session_name(name) {
            let msg = validation::session_name_error(name);
            if flags.json {
                print_json_error_with_type(msg, "invalid_session_name");
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    if let Some(ref policy) = flags.restore_save {
        if !is_valid_restore_save_policy(policy) {
            let msg = format!(
                "Invalid --restore-save value '{}'. Use auto, always, or never.",
                policy
            );
            if flags.json {
                print_json_error_with_type(msg, "invalid_value");
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Handle state management commands locally — these are pure file operations
    // that don't need a daemon, avoiding an unnecessary daemon startup that
    // would lack runtime config like session_name.
    if let Some(result) = native::state::dispatch_state_command(&cmd) {
        let action = cmd.get("action").and_then(|v| v.as_str());
        let resp = match result {
            Ok(data) => connection::Response {
                success: true,
                data: Some(data),
                error: None,
                warning: None,
            },
            Err(e) => connection::Response {
                success: false,
                data: None,
                error: Some(e),
                warning: None,
            },
        };
        let output_opts = OutputOptions::from_flags(&flags);
        output::print_response_with_opts(&resp, action, &output_opts);
        if !resp.success {
            exit(1);
        }
        return;
    }

    if let Some(msg) = incompatible_launch_mode_error(&flags) {
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    // Parse proxy URL to separate server from credentials for the daemon.
    let (proxy_server, proxy_username, proxy_password) = if let Some(ref proxy_str) = flags.proxy {
        let parsed = parse_proxy(proxy_str);
        (Some(parsed.server), parsed.username, parsed.password)
    } else {
        (None, None, None)
    };
    let plugin_registry_json =
        serde_json::to_string(&flags.plugins).unwrap_or_else(|_| "[]".to_string());
    let daemon_opts = DaemonOptions {
        headed: flags.headed,
        debug: flags.debug,
        executable_path: flags.executable_path.as_deref(),
        extensions: &flags.extensions,
        init_scripts: &flags.init_scripts,
        enable: &flags.enable,
        args: flags.args.as_deref(),
        user_agent: flags.user_agent.as_deref(),
        proxy: proxy_server.as_deref(),
        proxy_bypass: flags.proxy_bypass.as_deref(),
        proxy_username: proxy_username.as_deref(),
        proxy_password: proxy_password.as_deref(),
        ignore_https_errors: flags.ignore_https_errors,
        allow_file_access: flags.allow_file_access,
        hide_scrollbars: flags.hide_scrollbars,
        profile: flags.profile.as_deref(),
        state: flags.state.as_deref(),
        provider: flags.provider.as_deref(),
        device: flags.device.as_deref(),
        session_name: restore_key,
        restore_save: flags.restore_save.as_deref(),
        restore_check_url: flags.restore_check_url.as_deref(),
        restore_check_text: flags.restore_check_text.as_deref(),
        restore_check_fn: flags.restore_check_fn.as_deref(),
        download_path: flags.download_path.as_deref(),
        allowed_domains: flags.allowed_domains.as_deref(),
        action_policy: flags.action_policy.as_deref(),
        confirm_actions: flags.confirm_actions.as_deref(),
        engine: flags.engine.as_deref(),
        auto_connect: flags.auto_connect,
        idle_timeout: flags.idle_timeout.as_deref(),
        default_timeout: flags.default_timeout,
        cdp: flags.cdp.as_deref(),
        no_auto_dialog: flags.no_auto_dialog,
        plugins: Some(plugin_registry_json.as_str()),
    };

    let daemon_result = match ensure_daemon(&flags.session, &daemon_opts) {
        Ok(result) => result,
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    };
    let _daemon_was_already_running = daemon_result.already_running;
    let daemon_restarted = daemon_result.restarted;

    // Auto-connect to existing browser. This is sent even when the daemon is
    // already running so launch compatibility stays idempotent.
    if flags.auto_connect {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "autoConnect": true
        });
        attach_script_launch_options(&mut launch_cmd, &flags);
        attach_restore_config_to_command(&mut launch_cmd, &flags);

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        let err = match send_command(launch_cmd, &flags.session) {
            Ok(resp) if resp.success => None,
            Ok(resp) => Some(
                resp.error
                    .unwrap_or_else(|| "Auto-connect failed".to_string()),
            ),
            Err(e) => Some(e.to_string()),
        };

        if let Some(msg) = err {
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Connect via CDP if --cdp flag is set
    // Accepts either a port number (e.g., "9222") or a full URL (e.g., "ws://..." or "wss://...")
    if let Some(ref cdp_value) = flags.cdp {
        // Validate CDP value eagerly (even when daemon is already running) so
        // the user gets an immediate error for bad input instead of a silent no-op.
        let launch_cmd = if cdp_value.starts_with("ws://")
            || cdp_value.starts_with("wss://")
            || cdp_value.starts_with("http://")
            || cdp_value.starts_with("https://")
        {
            // It's a URL - use cdpUrl field
            json!({
                "id": gen_id(),
                "action": "launch",
                "cdpUrl": cdp_value
            })
        } else {
            // It's a port number - validate and use cdpPort field
            let cdp_port: u16 = match cdp_value.parse::<u32>() {
                Ok(0) => {
                    let msg = "Invalid CDP port: port must be greater than 0".to_string();
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
                Ok(p) if p > 65535 => {
                    let msg = format!(
                        "Invalid CDP port: {} is out of range (valid range: 1-65535)",
                        p
                    );
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
                Ok(p) => p as u16,
                Err(_) => {
                    let msg = format!(
                        "Invalid CDP value: '{}' is not a valid port number or URL",
                        cdp_value
                    );
                    if flags.json {
                        print_json_error(&msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
                }
            };
            json!({
                "id": gen_id(),
                "action": "launch",
                "cdpPort": cdp_port
            })
        };

        let mut launch_cmd = launch_cmd;
        attach_script_launch_options(&mut launch_cmd, &flags);
        attach_restore_config_to_command(&mut launch_cmd, &flags);

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        let err = match send_command(launch_cmd, &flags.session) {
            Ok(resp) if resp.success => None,
            Ok(resp) => Some(
                resp.error
                    .unwrap_or_else(|| "CDP connection failed".to_string()),
            ),
            Err(e) => Some(e.to_string()),
        };

        if let Some(msg) = err {
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Launch with cloud provider if -p flag is set.
    if let Some(ref provider) = flags.provider {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "provider": provider
        });
        launch_cmd["plugins"] = json!(flags.plugins.clone());
        attach_script_launch_options(&mut launch_cmd, &flags);
        attach_restore_config_to_command(&mut launch_cmd, &flags);

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        let err = match send_command(launch_cmd, &flags.session) {
            Ok(resp) if resp.success => None,
            Ok(resp) => Some(
                resp.error
                    .unwrap_or_else(|| "Provider connection failed".to_string()),
            ),
            Err(e) => Some(e.to_string()),
        };

        if let Some(msg) = err {
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Launch headed browser or configure browser options (without CDP or provider)
    if (flags.headed
        || flags.cli_headed  // User explicitly set --headed (even if false)
        || flags.executable_path.is_some()
        || flags.profile.is_some()
        || flags.state.is_some()
        || flags.proxy.is_some()
        || flags.args.is_some()
        || flags.user_agent.is_some()
        || flags.allow_file_access
        || should_send_hide_scrollbars_launch_option(
            flags.cli_hide_scrollbars,
            flags.hide_scrollbars,
        )
        || flags.color_scheme.is_some()
        || flags.download_path.is_some()
        || flags.engine.is_some()
        || !flags.init_scripts.is_empty()
        || !flags.enable.is_empty()
        || !flags.extensions.is_empty())
        && flags.cdp.is_none()
        && flags.provider.is_none()
        && !flags.auto_connect
    {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "headless": !flags.headed
        });
        launch_cmd["plugins"] = json!(flags.plugins.clone());
        attach_restore_config_to_command(&mut launch_cmd, &flags);

        let cmd_obj = launch_cmd
            .as_object_mut()
            .expect("json! macro guarantees object type");

        // Add executable path if specified
        if let Some(ref exec_path) = flags.executable_path {
            cmd_obj.insert("executablePath".to_string(), json!(exec_path));
        }

        // Add profile path if specified
        if let Some(ref profile_path) = flags.profile {
            cmd_obj.insert("profile".to_string(), json!(profile_path));
        }

        // Add state path if specified
        if let Some(ref state_path) = flags.state {
            cmd_obj.insert("storageState".to_string(), json!(state_path));
        }

        if let Some(ref proxy_str) = flags.proxy {
            let parsed = parse_proxy(proxy_str);
            let mut proxy_obj = json!({ "server": parsed.server });
            if let Some(ref username) = parsed.username {
                proxy_obj["username"] = json!(username);
            }
            if let Some(ref password) = parsed.password {
                proxy_obj["password"] = json!(password);
            }
            if let Some(ref bypass) = flags.proxy_bypass {
                proxy_obj["bypass"] = json!(bypass);
            }
            cmd_obj.insert("proxy".to_string(), proxy_obj);
        }

        if let Some(ref ua) = flags.user_agent {
            cmd_obj.insert("userAgent".to_string(), json!(ua));
        }

        if let Some(ref a) = flags.args {
            // Parse args (comma or newline separated)
            let args_vec: Vec<String> = a
                .split(&[',', '\n'][..])
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            cmd_obj.insert("args".to_string(), json!(args_vec));
        }

        if !flags.extensions.is_empty() {
            cmd_obj.insert("extensions".to_string(), json!(&flags.extensions));
        }

        if !flags.init_scripts.is_empty() {
            cmd_obj.insert("initScripts".to_string(), json!(&flags.init_scripts));
        }

        if !flags.enable.is_empty() {
            cmd_obj.insert("enable".to_string(), json!(&flags.enable));
        }

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if flags.allow_file_access {
            launch_cmd["allowFileAccess"] = json!(true);
        }

        apply_hide_scrollbars_launch_option(
            &mut launch_cmd,
            flags.cli_hide_scrollbars,
            flags.hide_scrollbars,
        );

        if let Some(ref cs) = flags.color_scheme {
            launch_cmd["colorScheme"] = json!(cs);
        }

        if let Some(ref dp) = flags.download_path {
            launch_cmd["downloadPath"] = json!(dp);
        }

        if let Some(ref domains) = flags.allowed_domains {
            launch_cmd["allowedDomains"] = json!(domains);
        }

        if let Some(ref engine) = flags.engine {
            launch_cmd["engine"] = json!(engine);
        }

        match send_command(launch_cmd, &flags.session) {
            Ok(resp) if !resp.success => {
                // Launch command failed (e.g., invalid state file, profile error)
                let error_msg = resp
                    .error
                    .unwrap_or_else(|| "Browser launch failed".to_string());
                if flags.json {
                    print_json_error(error_msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), error_msg);
                }
                exit(1);
            }
            Err(e) => {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!(
                        "{} Could not configure browser: {}",
                        color::error_indicator(),
                        e
                    );
                }
                exit(1);
            }
            Ok(_) => {
                // Launch succeeded
            }
        }
    }

    // Handle batch command: from args or stdin
    if cmd.get("action").and_then(|v| v.as_str()) == Some("batch") {
        let bail = cmd.get("bail").and_then(|v| v.as_bool()).unwrap_or(false);
        let arg_commands = cmd.get("commands").and_then(|v| v.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(commands::shell_words_split)
                .collect::<Vec<Vec<String>>>()
        });
        run_batch(&flags, &daemon_opts, bail, arg_commands);
        return;
    }

    let output_opts = OutputOptions::from_flags(&flags);

    match send_command_with_respawn(cmd.clone(), &flags.session, &daemon_opts) {
        Ok(mut resp) => {
            if daemon_restarted {
                mark_restarted_background(&mut resp);
            }
            if flags.confirm_interactive && confirmation_prompt_from_response(&resp).is_some() {
                resp = run_interactive_confirmations(resp, &flags, &output_opts);
                if daemon_restarted {
                    mark_restarted_background(&mut resp);
                }
                if !resp.success {
                    exit(1);
                }
                return;
            }
            let success = resp.success;
            // Extract action for context-specific output handling
            let action = cmd.get("action").and_then(|v| v.as_str());
            print_response_with_opts(&resp, action, &output_opts);
            if !success {
                exit(1);
            }
        }
        Err(e) => {
            if flags.json {
                print_json_error(e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }
}

/// send_command plus the daemon-shutdown-race recovery: ensure_daemon no
/// longer pays a settle-sleep on every invocation, so a daemon that exited
/// right after its liveness check surfaces as an unreachable socket on the
/// request itself. Respawn once and retry before reporting failure.
fn send_command_with_respawn(
    cmd: serde_json::Value,
    session: &str,
    daemon_opts: &DaemonOptions,
) -> Result<connection::Response, String> {
    let first_attempt = send_command(cmd.clone(), session);
    match first_attempt {
        Err(ref e) if daemon_unreachable(e) => match ensure_daemon(session, daemon_opts) {
            Ok(_) => send_command(cmd, session),
            Err(_) => first_attempt,
        },
        other => other,
    }
}

fn run_batch(
    flags: &Flags,
    daemon_opts: &DaemonOptions,
    bail: bool,
    arg_commands: Option<Vec<Vec<String>>>,
) {
    let commands: Vec<Vec<String>> = if let Some(cmds) = arg_commands {
        cmds
    } else {
        use std::io::Read as _;

        let mut input = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut input) {
            if flags.json {
                print_json_error(format!("Failed to read stdin: {}", e));
            } else {
                eprintln!("{} Failed to read stdin: {}", color::error_indicator(), e);
            }
            exit(1);
        }

        match serde_json::from_str(&input) {
            Ok(c) => c,
            Err(e) => {
                if flags.json {
                    print_json_error(format!(
                        "Invalid JSON input: {}. Expected an array of string arrays, e.g. [[\"open\", \"https://example.com\"], [\"snapshot\"]]",
                        e
                    ));
                } else {
                    eprintln!(
                        "{} Invalid JSON input: {}. Expected an array of string arrays.",
                        color::error_indicator(),
                        e
                    );
                }
                exit(1);
            }
        }
    };

    if commands.is_empty() {
        if flags.json {
            println!("[]");
        }
        return;
    }

    let output_opts = OutputOptions::from_flags(flags);

    let mut results: Vec<serde_json::Value> = Vec::new();
    let mut had_error = false;

    for (i, cmd_args) in commands.iter().enumerate() {
        if cmd_args.is_empty() {
            continue;
        }

        let mut parsed = match parse_command(cmd_args, flags) {
            Ok(c) => c,
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.format(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!(
                        "{} Command {}: {}",
                        color::error_indicator(),
                        i + 1,
                        e.format()
                    );
                    if bail {
                        exit(1);
                    }
                }
                continue;
            }
        };

        let action = parsed
            .get("action")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        attach_plugins_to_command(&mut parsed, &flags.plugins);
        attach_restore_config_to_command(&mut parsed, flags);

        match send_command_with_respawn(parsed, &flags.session, daemon_opts) {
            Ok(resp) => {
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": resp.success,
                        "result": resp.data,
                        "error": resp.error,
                    }));
                } else {
                    if i > 0 {
                        println!();
                    }
                    print_response_with_opts(&resp, action.as_deref(), &output_opts);
                }
                if !resp.success {
                    had_error = true;
                    if bail {
                        if !flags.json {
                            exit(1);
                        }
                        break;
                    }
                }
            }
            Err(e) => {
                had_error = true;
                if flags.json {
                    results.push(json!({
                        "command": cmd_args,
                        "success": false,
                        "error": e.to_string(),
                    }));
                    if bail {
                        break;
                    }
                } else {
                    eprintln!("{} Command {}: {}", color::error_indicator(), i + 1, e);
                    if bail {
                        exit(1);
                    }
                }
            }
        }
    }

    if flags.json {
        println!(
            "{}",
            serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string())
        );
    }

    if had_error {
        exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proxy_simple() {
        let result = parse_proxy("http://proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert!(result.username.is_none());
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_with_auth() {
        let result = parse_proxy("http://user:pass@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("pass"));
    }

    #[test]
    fn test_parse_proxy_username_only() {
        let result = parse_proxy("http://user@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert!(result.password.is_none());
    }

    #[test]
    fn test_parse_proxy_no_protocol() {
        let result = parse_proxy("proxy.com:8080");
        assert_eq!(result.server, "proxy.com:8080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5() {
        let result = parse_proxy("socks5://proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert!(result.username.is_none());
    }

    #[test]
    fn test_parse_proxy_socks5_with_auth() {
        let result = parse_proxy("socks5://admin:secret@proxy.com:1080");
        assert_eq!(result.server, "socks5://proxy.com:1080");
        assert_eq!(result.username.as_deref(), Some("admin"));
        assert_eq!(result.password.as_deref(), Some("secret"));
    }

    #[test]
    fn test_parse_proxy_complex_password() {
        let result = parse_proxy("http://user:p@ss:w0rd@proxy.com:8080");
        assert_eq!(result.server, "http://proxy.com:8080");
        assert_eq!(result.username.as_deref(), Some("user"));
        assert_eq!(result.password.as_deref(), Some("p@ss:w0rd"));
    }

    #[test]
    fn test_serialize_json_value_escapes_control_characters() {
        let payload = serialize_json_value(&json!({
            "success": false,
            "error": "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m",
        }));

        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(
            parsed["error"],
            "Daemon process exited during startup:\nline \"quoted\"\u{001b}[2mansi\u{001b}[22m"
        );
    }

    #[test]
    fn test_hide_scrollbars_launch_option_serialization() {
        assert!(!should_send_hide_scrollbars_launch_option(false, true));
        assert!(should_send_hide_scrollbars_launch_option(false, false));
        assert!(should_send_hide_scrollbars_launch_option(true, true));

        let mut default_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut default_cmd, false, true);
        assert!(default_cmd.get("hideScrollbars").is_none());

        let mut config_false_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut config_false_cmd, false, false);
        assert_eq!(config_false_cmd["hideScrollbars"], false);

        let mut cli_true_cmd = json!({ "action": "launch" });
        apply_hide_scrollbars_launch_option(&mut cli_true_cmd, true, true);
        assert_eq!(cli_true_cmd["hideScrollbars"], true);
    }

    #[test]
    fn test_attach_plugins_to_command_adds_registry_payload() {
        let plugins = vec![crate::plugins::PluginConfig {
            name: "stealth".to_string(),
            command: "agent-browser-plugin-stealth".to_string(),
            capabilities: vec!["launch.mutate".to_string()],
            ..crate::plugins::PluginConfig::default()
        }];
        let mut cmd = json!({ "action": "navigate", "url": "https://example.com" });

        attach_plugins_to_command(&mut cmd, &plugins);

        assert_eq!(cmd["plugins"][0]["name"], "stealth");
        assert_eq!(cmd["plugins"][0]["capabilities"][0], "launch.mutate");
    }

    #[test]
    fn test_attach_plugins_to_command_adds_empty_registry_payload() {
        let mut cmd = json!({ "action": "navigate", "url": "https://example.com" });

        attach_plugins_to_command(&mut cmd, &[]);

        assert_eq!(cmd["plugins"], json!([]));
    }

    #[test]
    fn test_attach_restore_config_to_command_uses_session_for_bare_restore() {
        let args = vec![
            "--session".to_string(),
            "next-loop".to_string(),
            "--restore".to_string(),
            "--restore-save".to_string(),
            "always".to_string(),
            "--restore-check-url".to_string(),
            "**/dashboard".to_string(),
            "--restore-check-text".to_string(),
            "Dashboard".to_string(),
            "--restore-check-fn".to_string(),
            "!!localStorage.getItem('session')".to_string(),
            "open".to_string(),
            "http://localhost:3000".to_string(),
        ];
        let mut flags = parse_flags(&args);
        if flags.restore_uses_session {
            flags.restore = Some(flags.session.clone());
        }
        let mut cmd = json!({ "action": "launch" });

        attach_restore_config_to_command(&mut cmd, &flags);

        assert_eq!(cmd["restoreKey"], "next-loop");
        assert_eq!(cmd["restoreSave"], "always");
        assert_eq!(cmd["restoreCheckUrl"], "**/dashboard");
        assert_eq!(cmd["restoreCheckText"], "Dashboard");
        assert_eq!(cmd["restoreCheckFn"], "!!localStorage.getItem('session')");
    }

    #[test]
    fn test_attach_restore_config_to_command_sends_defaults_and_clears_checks() {
        let args = vec![
            "--session".to_string(),
            "next-loop".to_string(),
            "--restore".to_string(),
            "open".to_string(),
            "http://localhost:3000".to_string(),
        ];
        let mut flags = parse_flags(&args);
        if flags.restore_uses_session {
            flags.restore = Some(flags.session.clone());
        }
        let mut cmd = json!({ "action": "navigate" });

        attach_restore_config_to_command(&mut cmd, &flags);

        assert_eq!(cmd["restoreKey"], "next-loop");
        assert_eq!(cmd["restoreSave"], "auto");
        assert!(cmd["restoreCheckUrl"].is_null());
        assert!(cmd["restoreCheckText"].is_null());
        assert!(cmd["restoreCheckFn"].is_null());
    }

    fn launch_mode_flags(auto_connect: bool, cdp: bool, provider: bool, extensions: bool) -> Flags {
        let mut flags = parse_flags(&[]);
        flags.auto_connect = auto_connect;
        flags.cdp = cdp.then(|| "9222".to_string());
        flags.provider = provider.then(|| "ios".to_string());
        flags.extensions = if extensions {
            vec!["/tmp/ext".to_string()]
        } else {
            Vec::new()
        };
        flags
    }

    #[test]
    fn test_incompatible_launch_mode_error_matches_existing_messages() {
        let cases = [
            (
                launch_mode_flags(false, true, true, false),
                "Cannot use --cdp and -p/--provider together",
            ),
            (
                launch_mode_flags(true, true, false, false),
                "Cannot use --auto-connect and --cdp together",
            ),
            (
                launch_mode_flags(true, false, true, false),
                "Cannot use --auto-connect and -p/--provider together",
            ),
            (
                launch_mode_flags(false, false, true, true),
                "Cannot use --extension with -p/--provider (extensions require local browser)",
            ),
            (
                launch_mode_flags(false, true, false, true),
                "Cannot use --extension with --cdp (extensions require local browser)",
            ),
        ];

        for (flags, expected) in cases {
            assert_eq!(incompatible_launch_mode_error(&flags), Some(expected));
        }
    }

    #[test]
    fn test_incompatible_launch_mode_error_allows_compatible_flags() {
        assert_eq!(
            incompatible_launch_mode_error(&launch_mode_flags(true, false, false, false)),
            None
        );
        assert_eq!(
            incompatible_launch_mode_error(&launch_mode_flags(false, true, false, false)),
            None
        );
        assert_eq!(
            incompatible_launch_mode_error(&launch_mode_flags(false, false, true, false)),
            None
        );
    }

    #[test]
    fn test_resolve_session_id_scope_accepts_cwd_and_rejects_unknown() {
        let (scope, path) = resolve_session_id_scope("cwd").unwrap();

        assert_eq!(scope, "cwd");
        assert!(path.is_absolute());
        assert!(resolve_session_id_scope("branch").is_err());
    }

    #[test]
    fn test_confirmation_prompt_from_response_finds_nested_confirm_result() {
        let resp = Response {
            success: true,
            data: Some(json!({
                "confirmed": true,
                "action": "navigate",
                "result": {
                    "id": "original-command",
                    "success": true,
                    "data": {
                        "confirmation_required": true,
                        "confirmation_id": "original-command",
                        "action": "plugin:stealth:launch.mutate"
                    }
                }
            })),
            error: None,
            warning: None,
        };

        let prompt = confirmation_prompt_from_response(&resp).unwrap();

        assert_eq!(prompt.action, "plugin:stealth:launch.mutate");
        assert_eq!(prompt.description, "plugin:stealth:launch.mutate");
        assert_eq!(prompt.confirmation_id, "original-command");
    }
}
