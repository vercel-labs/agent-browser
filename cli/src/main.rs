mod agent_env;
mod chat;
mod color;
mod commands;
mod connection;
mod flags;
mod install;
mod native;
mod output;
mod runtime_profile;
#[cfg(test)]
mod test_utils;
mod upgrade;
mod validation;

use serde_json::json;
use std::env;
use std::fs;
use std::process::exit;

#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use commands::{gen_id, parse_command, ParseError};
use connection::{cleanup_stale_files, ensure_daemon, get_socket_dir, send_command, DaemonOptions};
use flags::{clean_args, parse_flags, upsert_runtime_profile_in_user_config, Flags};
use install::run_install;
use native::cdp::chrome::{launch_chrome_detached, LaunchOptions};
use output::{
    print_command_help, print_help, print_response_with_opts, print_version, OutputOptions,
};
use runtime_profile::{
    list_runtime_profiles, runtime_status_with_user_data_dir, RuntimeProfileSummary, RuntimeStatus,
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

fn print_runtime_status(status: &RuntimeStatus, json_mode: bool) {
    if json_mode {
        print_json_value(serde_json::to_value(status).unwrap_or_default());
        return;
    }

    println!("Runtime profile: {}", status.runtime_profile);
    println!("User data dir: {}", status.user_data_dir);
    println!("State file: {}", status.state_path);
    println!(
        "Browser PID: {}",
        status
            .browser_pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    println!("Browser alive: {}", status.browser_alive);
    if let Some(ref launch_mode) = status.launch_mode {
        println!("Launch mode: {}", launch_mode);
    }
    if let Some(headed) = status.headed {
        println!("Headed: {}", headed);
    }
    if let Some(port) = status.devtools_port {
        println!("DevTools port: {}", port);
    }
    if let Some(ref ws_url) = status.ws_url {
        println!("CDP URL: {}", ws_url);
    }
    if status.targets.is_empty() {
        println!("Targets: none");
    } else {
        println!("Targets:");
        for target in &status.targets {
            println!(
                "  - [{}] {} {}",
                target.target_type,
                if target.title.is_empty() {
                    "<untitled>"
                } else {
                    &target.title
                },
                target.url
            );
        }
    }
}

fn print_runtime_list(items: &[RuntimeProfileSummary], json_mode: bool) {
    if json_mode {
        print_json_value(serde_json::to_value(items).unwrap_or_default());
        return;
    }

    if items.is_empty() {
        println!("Runtime profiles: none");
        return;
    }

    println!("Runtime profiles:");
    for item in items {
        let mut labels = Vec::new();
        if item.default {
            labels.push("default");
        }
        if item.configured {
            labels.push("configured");
        }
        if item.browser_alive {
            labels.push("running");
        }

        let label_suffix = if labels.is_empty() {
            String::new()
        } else {
            format!(" ({})", labels.join(", "))
        };

        println!("  {}{}", item.runtime_profile, label_suffix);
        println!("    User data dir: {}", item.user_data_dir);
        println!("    State file: {}", item.state_path);
        println!(
            "    Browser PID: {}",
            item.browser_pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
        if let Some(ref launch_mode) = item.launch_mode {
            println!("    Launch mode: {}", launch_mode);
        }
        if let Some(headed) = item.headed {
            println!("    Headed: {}", headed);
        }
        if let Some(port) = item.devtools_port {
            println!("    DevTools port: {}", port);
        }
    }
}

fn first_runtime_positional(clean: &[String], start_index: usize) -> Option<String> {
    clean
        .iter()
        .skip(start_index)
        .find(|arg| !arg.starts_with("--"))
        .cloned()
}

fn selected_runtime_name(clean: &[String], flags: &Flags, positional_index: usize) -> String {
    first_runtime_positional(clean, positional_index)
        .or_else(|| flags.runtime_profile.clone())
        .or_else(|| {
            flags.profile.as_ref().and_then(|profile| {
                if runtime_profile::looks_like_path(profile) {
                    None
                } else {
                    Some(profile.clone())
                }
            })
        })
        .or_else(|| flags.default_runtime_profile.clone())
        .unwrap_or_else(runtime_profile::default_runtime_profile_name)
}

fn run_runtime_command(clean: &[String], flags: &Flags) {
    match clean.get(1).map(|s| s.as_str()) {
        Some("create") => {
            let runtime_name = selected_runtime_name(clean, flags, 2);
            if let Err(e) = runtime_profile::validate_runtime_profile_name(&runtime_name) {
                if flags.json {
                    print_json_error(e);
                } else {
                    eprintln!("{} {}", color::error_indicator(), e);
                }
                exit(1);
            }

            let user_data_dir = flags
                .configured_runtime_profiles
                .get(&runtime_name)
                .and_then(|value| value.clone())
                .unwrap_or_else(|| {
                    runtime_profile::runtime_profile_user_data_dir(&runtime_name)
                        .unwrap_or_else(|_| std::path::PathBuf::from("."))
                        .display()
                        .to_string()
                });
            let profile_root = runtime_profile::runtime_profile_root(&runtime_name)
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let set_default = clean.iter().any(|arg| arg == "--set-default");

            if let Err(e) = fs::create_dir_all(&user_data_dir) {
                let msg = format!(
                    "Failed to create runtime profile user-data-dir {}: {}",
                    user_data_dir, e
                );
                if flags.json {
                    print_json_error(msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }

            if let Err(e) = fs::create_dir_all(&profile_root) {
                let msg = format!(
                    "Failed to create runtime profile root {}: {}",
                    profile_root.display(),
                    e
                );
                if flags.json {
                    print_json_error(msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }

            match upsert_runtime_profile_in_user_config(
                &runtime_name,
                Some(&user_data_dir),
                set_default,
            ) {
                Ok(config_path) => {
                    if flags.json {
                        print_json_value(json!({
                            "created": true,
                            "runtimeProfile": runtime_name,
                            "userDataDir": user_data_dir,
                            "configPath": config_path.display().to_string(),
                            "default": set_default,
                        }));
                    } else {
                        println!(
                            "Runtime profile created\nName: {}\nUser data dir: {}\nConfig: {}{}",
                            runtime_name,
                            user_data_dir,
                            config_path.display(),
                            if set_default {
                                "\nDefault runtime profile: true"
                            } else {
                                ""
                            }
                        );
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
        Some("list") => {
            let configured_profiles = flags
                .configured_runtime_profiles
                .iter()
                .map(|(name, path)| (name.clone(), path.as_ref().map(std::path::PathBuf::from)))
                .collect::<Vec<_>>();
            let default_name = flags.default_runtime_profile.as_deref().or(Some("default"));
            match list_runtime_profiles(&configured_profiles, default_name) {
                Ok(items) => print_runtime_list(&items, flags.json),
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
        Some("status") => {
            let runtime_name = selected_runtime_name(clean, flags, 2);
            let configured_user_data_dir = flags
                .configured_runtime_profiles
                .get(&runtime_name)
                .and_then(|path| path.as_deref())
                .map(std::path::Path::new);
            match runtime_status_with_user_data_dir(&runtime_name, configured_user_data_dir) {
                Ok(status) => print_runtime_status(&status, flags.json),
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
        Some("login") => {
            let attachable = clean.iter().any(|arg| arg == "--attachable");
            let (proxy_server, proxy_username, proxy_password) =
                if let Some(ref proxy_str) = flags.proxy {
                    let parsed = parse_proxy(proxy_str);
                    (Some(parsed.server), parsed.username, parsed.password)
                } else {
                    (None, None, None)
                };
            let mut options = LaunchOptions {
                headless: false,
                executable_path: flags.executable_path.clone(),
                proxy: proxy_server,
                proxy_bypass: flags.proxy_bypass.clone(),
                proxy_username,
                proxy_password,
                profile: flags.profile.clone(),
                runtime_profile: flags.runtime_profile.clone(),
                args: Vec::new(),
                allow_file_access: flags.allow_file_access,
                extensions: if flags.extensions.is_empty() {
                    None
                } else {
                    Some(flags.extensions.clone())
                },
                storage_state: flags.state.clone(),
                user_agent: flags.user_agent.clone(),
                ignore_https_errors: flags.ignore_https_errors,
                color_scheme: flags.color_scheme.clone(),
                download_path: flags.download_path.clone(),
                viewport_size: None,
                use_real_keychain: env::var("AGENT_BROWSER_USE_REAL_KEYCHAIN").is_ok_and(|v| {
                    !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "")
                }) || env::var("AGENT_BROWSER_KEYCHAIN_PASSWORD").is_ok(),
                keychain_password: env::var("AGENT_BROWSER_KEYCHAIN_PASSWORD").ok(),
                attachable,
            };

            if let Some(url) = first_runtime_positional(clean, 2) {
                let url = if url.contains("://") {
                    url
                } else {
                    format!("https://{}", url)
                };
                options.args.push(url);
            }

            match launch_chrome_detached(&options) {
                Ok(result) => {
                    if flags.json {
                        print_json_value(json!({
                            "launched": true,
                            "manual": true,
                            "attachable": attachable,
                            "pid": result.pid,
                            "userDataDir": result.user_data_dir.display().to_string(),
                            "runtimeProfile": result.runtime_profile,
                            "devtoolsPort": result.devtools_port,
                        }));
                    } else {
                        println!(
                            "Manual browser launched\nPID: {}\nUser data dir: {}{}{}",
                            result.pid,
                            result.user_data_dir.display(),
                            result
                                .runtime_profile
                                .as_ref()
                                .map_or(String::new(), |name| {
                                    format!("\nRuntime profile: {}", name)
                                }),
                            result.devtools_port.map_or(String::new(), |port| {
                                format!("\nDevTools port: {}", port)
                            }),
                        );
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
        Some("attach") => {
            let runtime_name = selected_runtime_name(clean, flags, 2);
            let configured_user_data_dir = flags
                .configured_runtime_profiles
                .get(&runtime_name)
                .and_then(|path| path.as_deref())
                .map(std::path::Path::new);
            let status =
                match runtime_status_with_user_data_dir(&runtime_name, configured_user_data_dir) {
                    Ok(status) => status,
                    Err(e) => {
                        if flags.json {
                            print_json_error(e);
                        } else {
                            eprintln!("{} {}", color::error_indicator(), e);
                        }
                        exit(1);
                    }
                };

            let attach_to_existing = status.browser_alive && status.devtools_port.is_some();
            if status.browser_alive && status.devtools_port.is_none() {
                let msg = format!(
                    "Runtime profile '{}' has a live browser without a DevTools port. Close that browser before attaching automation, or relaunch manual login with `agent-browser runtime login --attachable`.",
                    runtime_name
                );
                if flags.json {
                    print_json_error(msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }

            let cdp_port_str = status.devtools_port.map(|port| port.to_string());
            let keychain_password = env::var("AGENT_BROWSER_KEYCHAIN_PASSWORD").ok();
            let use_real_keychain = env::var("AGENT_BROWSER_USE_REAL_KEYCHAIN").is_ok_and(|v| {
                !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no" | "")
            }) || keychain_password.is_some();
            let daemon_opts = DaemonOptions {
                headed: flags.headed,
                debug: flags.debug,
                executable_path: flags.executable_path.as_deref(),
                extensions: &flags.extensions,
                args: flags.args.as_deref(),
                user_agent: flags.user_agent.as_deref(),
                runtime_profile: Some(&runtime_name),
                proxy: None,
                proxy_bypass: None,
                proxy_username: None,
                proxy_password: None,
                ignore_https_errors: flags.ignore_https_errors,
                allow_file_access: flags.allow_file_access,
                profile: if attach_to_existing {
                    None
                } else {
                    Some(status.user_data_dir.as_str())
                },
                state: flags.state.as_deref(),
                provider: None,
                device: None,
                session_name: flags.session_name.as_deref(),
                download_path: flags.download_path.as_deref(),
                allowed_domains: flags.allowed_domains.as_deref(),
                action_policy: flags.action_policy.as_deref(),
                confirm_actions: flags.confirm_actions.as_deref(),
                engine: flags.engine.as_deref(),
                use_real_keychain,
                keychain_password: keychain_password.as_deref(),
                auto_connect: false,
                idle_timeout: flags.idle_timeout.as_deref(),
                default_timeout: flags.default_timeout,
                cdp: cdp_port_str.as_deref(),
                no_auto_dialog: flags.no_auto_dialog,
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

            let launch_cmd = if let Some(port) = status.devtools_port {
                json!({
                    "id": gen_id(),
                    "action": "launch",
                    "cdpPort": port,
                })
            } else {
                json!({
                    "id": gen_id(),
                    "action": "launch",
                    "headless": !flags.headed,
                    "runtimeProfile": runtime_name,
                })
            };

            match send_command(launch_cmd, &flags.session) {
                Ok(resp) if resp.success => {
                    if flags.json {
                        print_json_value(json!({
                            "attached": true,
                            "runtimeProfile": runtime_name,
                            "session": flags.session,
                            "reusedDaemon": daemon_result.already_running,
                            "attachedToExistingBrowser": attach_to_existing,
                            "devtoolsPort": status.devtools_port,
                        }));
                    } else {
                        println!(
                            "Runtime profile attached\nName: {}\nSession: {}\nMode: {}",
                            runtime_name,
                            flags.session,
                            if attach_to_existing {
                                "existing browser"
                            } else {
                                "new automation browser"
                            }
                        );
                    }
                }
                Ok(resp) => {
                    let msg = resp
                        .error
                        .unwrap_or_else(|| "Failed to attach runtime profile".to_string());
                    if flags.json {
                        print_json_error(msg);
                    } else {
                        eprintln!("{} {}", color::error_indicator(), msg);
                    }
                    exit(1);
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
        _ => {
            let msg = "Usage: agent-browser runtime <create|list|status|login|attach> [name|url]";
            if flags.json {
                print_json_error(msg);
            } else {
                eprintln!("{}", color::red(msg));
            }
            exit(1);
        }
    }
}

fn run_session(args: &[String], session: &str, json_mode: bool) {
    let subcommand = args.get(1).map(|s| s.as_str());

    match subcommand {
        Some("list") => {
            let socket_dir = get_socket_dir();
            let mut sessions: Vec<String> = Vec::new();

            if let Ok(entries) = fs::read_dir(&socket_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Look for pid files in socket directory
                    if name.ends_with(".pid") {
                        let session_name = name.strip_suffix(".pid").unwrap_or("");
                        if !session_name.is_empty() {
                            // Check if session is actually running
                            let pid_path = socket_dir.join(&name);
                            if let Ok(pid_str) = fs::read_to_string(&pid_path) {
                                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                                    #[cfg(unix)]
                                    let running = unsafe {
                                        libc::kill(pid as i32, 0) == 0
                                            || std::io::Error::last_os_error().raw_os_error()
                                                != Some(libc::ESRCH)
                                    };
                                    #[cfg(windows)]
                                    let running = unsafe {
                                        let handle =
                                            OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                                        if handle != 0 {
                                            CloseHandle(handle);
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if running {
                                        sessions.push(session_name.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }

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

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle != 0 {
                CloseHandle(handle);
                true
            } else {
                false
            }
        }
    }
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

fn run_close_all(flags: &Flags) {
    let socket_dir = get_socket_dir();
    let mut sessions: Vec<(String, u32)> = Vec::new();

    if let Ok(entries) = fs::read_dir(&socket_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(session_name) = name.strip_suffix(".pid") {
                if session_name.is_empty() {
                    continue;
                }
                let pid_path = socket_dir.join(&name);
                if let Ok(pid_str) = fs::read_to_string(&pid_path) {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        #[cfg(unix)]
                        let running = unsafe {
                            libc::kill(pid as i32, 0) == 0
                                || std::io::Error::last_os_error().raw_os_error()
                                    != Some(libc::ESRCH)
                        };
                        #[cfg(windows)]
                        let running = unsafe {
                            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                            if handle != 0 {
                                CloseHandle(handle);
                                true
                            } else {
                                false
                            }
                        };
                        if running {
                            sessions.push((session_name.to_string(), pid));
                        } else {
                            // Process is gone but stale files remain; clean them up
                            cleanup_stale_files(session_name);
                        }
                    }
                } else {
                    // PID file exists but is unreadable; clean up stale files
                    cleanup_stale_files(session_name);
                }
            }
        }
    }

    // Also scan for orphaned .sock files without corresponding .pid files
    #[cfg(unix)]
    if let Ok(entries) = fs::read_dir(&socket_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(session_name) = name.strip_suffix(".sock") {
                if session_name.is_empty() {
                    continue;
                }
                let pid_path = socket_dir.join(format!("{}.pid", session_name));
                if !pid_path.exists() {
                    // Orphaned socket file with no PID file; remove it
                    cleanup_stale_files(session_name);
                }
            }
        }
    }

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

    if let Err(err) = agent_env::load_env_file() {
        if !env::args().any(|arg| arg == "--help" || arg == "-h") {
            eprintln!("{} {}", color::warning_indicator(), err);
        }
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
    let flags = parse_flags(&args);
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

    if clean.first().map(|s| s.as_str()) == Some("runtime") {
        run_runtime_command(&clean, &flags);
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

    // Validate session name before starting daemon
    if let Some(ref name) = flags.session_name {
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

    // Parse proxy URL to separate server from credentials for the daemon.
    let (proxy_server, proxy_username, proxy_password) = if let Some(ref proxy_str) = flags.proxy {
        let parsed = parse_proxy(proxy_str);
        (Some(parsed.server), parsed.username, parsed.password)
    } else {
        (None, None, None)
    };
    let keychain_password = env::var("AGENT_BROWSER_KEYCHAIN_PASSWORD").ok();
    let use_real_keychain = env::var("AGENT_BROWSER_USE_REAL_KEYCHAIN")
        .is_ok_and(|v| !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no" | ""))
        || keychain_password.is_some();
    let daemon_opts = DaemonOptions {
        headed: flags.headed,
        debug: flags.debug,
        executable_path: flags.executable_path.as_deref(),
        extensions: &flags.extensions,
        args: flags.args.as_deref(),
        user_agent: flags.user_agent.as_deref(),
        runtime_profile: flags.runtime_profile.as_deref(),
        proxy: proxy_server.as_deref(),
        proxy_bypass: flags.proxy_bypass.as_deref(),
        proxy_username: proxy_username.as_deref(),
        proxy_password: proxy_password.as_deref(),
        ignore_https_errors: flags.ignore_https_errors,
        allow_file_access: flags.allow_file_access,
        profile: flags.profile.as_deref(),
        state: flags.state.as_deref(),
        provider: flags.provider.as_deref(),
        device: flags.device.as_deref(),
        session_name: flags.session_name.as_deref(),
        download_path: flags.download_path.as_deref(),
        allowed_domains: flags.allowed_domains.as_deref(),
        action_policy: flags.action_policy.as_deref(),
        confirm_actions: flags.confirm_actions.as_deref(),
        engine: flags.engine.as_deref(),
        use_real_keychain,
        keychain_password: keychain_password.as_deref(),
        auto_connect: flags.auto_connect,
        idle_timeout: flags.idle_timeout.as_deref(),
        default_timeout: flags.default_timeout,
        cdp: flags.cdp.as_deref(),
        no_auto_dialog: flags.no_auto_dialog,
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

    // Warn if launch-time options were explicitly passed via CLI but daemon was already running
    // Only warn about flags that were passed on the command line, not those set via environment
    // variables (since the daemon already uses the env vars when it starts).
    if daemon_result.already_running {
        let ignored_flags: Vec<&str> = [
            if flags.cli_executable_path {
                Some("--executable-path")
            } else {
                None
            },
            if flags.cli_extensions {
                Some("--extension")
            } else {
                None
            },
            if flags.cli_profile {
                Some("--profile")
            } else {
                None
            },
            if flags.cli_runtime_profile {
                Some("--runtime-profile")
            } else {
                None
            },
            if flags.cli_state {
                Some("--state")
            } else {
                None
            },
            if flags.cli_args { Some("--args") } else { None },
            if flags.cli_user_agent {
                Some("--user-agent")
            } else {
                None
            },
            if flags.cli_proxy {
                Some("--proxy")
            } else {
                None
            },
            if flags.cli_proxy_bypass {
                Some("--proxy-bypass")
            } else {
                None
            },
            flags.ignore_https_errors.then_some("--ignore-https-errors"),
            flags.cli_allow_file_access.then_some("--allow-file-access"),
            flags.cli_download_path.then_some("--download-path"),
            flags.cli_headed.then_some("--headed"),
        ]
        .into_iter()
        .flatten()
        .collect();

        if !ignored_flags.is_empty() && !flags.json {
            eprintln!(
                "{} {} ignored: daemon already running. Use 'agent-browser close' first to restart with new options.",
                color::warning_indicator(),
                ignored_flags.join(", ")
            );
        }
    }

    // Validate mutually exclusive options
    if flags.cdp.is_some() && flags.provider.is_some() {
        let msg = "Cannot use --cdp and -p/--provider together";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.auto_connect && flags.cdp.is_some() {
        let msg = "Cannot use --auto-connect and --cdp together";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.auto_connect && flags.provider.is_some() {
        let msg = "Cannot use --auto-connect and -p/--provider together";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.provider.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with -p/--provider (extensions require local browser)";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.cdp.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with --cdp (extensions require local browser)";
        if flags.json {
            print_json_error(msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    // Auto-connect to existing browser.
    // Skip when the daemon was already running — it already holds the connection
    // from a previous auto-connect launch, so re-sending the launch command would
    // redundantly probe Chrome and may trigger repeated permission prompts (#962).
    if flags.auto_connect && !daemon_result.already_running {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "autoConnect": true
        });

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
    // Skip when daemon already running — it already holds the CDP connection.
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

        if !daemon_result.already_running {
            let mut launch_cmd = launch_cmd;

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
    }

    // Launch with cloud provider if -p flag is set
    // Skip when daemon already running — it already holds the provider connection.
    if let Some(ref provider) = flags.provider {
        if !daemon_result.already_running {
            let mut launch_cmd = json!({
                "id": gen_id(),
                "action": "launch",
                "provider": provider
            });

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
    }

    // Launch headed browser or configure browser options (without CDP or provider)
    if (flags.headed
        || flags.cli_headed  // User explicitly set --headed (even if false)
        || flags.executable_path.is_some()
        || flags.runtime_profile.is_some()
        || flags.profile.is_some()
        || flags.state.is_some()
        || flags.proxy.is_some()
        || flags.args.is_some()
        || flags.user_agent.is_some()
        || flags.allow_file_access
        || flags.color_scheme.is_some()
        || flags.download_path.is_some()
        || flags.engine.is_some()
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
        if let Some(ref runtime_profile) = flags.runtime_profile {
            cmd_obj.insert("runtimeProfile".to_string(), json!(runtime_profile));
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

        if flags.ignore_https_errors {
            launch_cmd["ignoreHTTPSErrors"] = json!(true);
        }

        if flags.allow_file_access {
            launch_cmd["allowFileAccess"] = json!(true);
        }

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
        run_batch(&flags, bail, arg_commands);
        return;
    }

    let output_opts = OutputOptions::from_flags(&flags);

    match send_command(cmd.clone(), &flags.session) {
        Ok(resp) => {
            let success = resp.success;
            // Handle interactive confirmation
            if flags.confirm_interactive {
                if let Some(data) = &resp.data {
                    if data
                        .get("confirmation_required")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        let desc = data
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown action");
                        let category = data.get("category").and_then(|v| v.as_str()).unwrap_or("");
                        let cid = data
                            .get("confirmation_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        eprintln!("[agent-browser] Action requires confirmation:");
                        eprintln!("  {}: {}", category, desc);
                        eprint!("  Allow? [y/N]: ");

                        let mut input = String::new();
                        let approved = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                            std::io::stdin().read_line(&mut input).is_ok()
                                && matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
                        } else {
                            false
                        };

                        let confirm_cmd = if approved {
                            json!({ "id": gen_id(), "action": "confirm", "confirmationId": cid })
                        } else {
                            json!({ "id": gen_id(), "action": "deny", "confirmationId": cid })
                        };

                        match send_command(confirm_cmd, &flags.session) {
                            Ok(r) => {
                                if !approved {
                                    eprintln!("{} Action denied", color::error_indicator());
                                    exit(1);
                                }
                                print_response_with_opts(&r, None, &output_opts);
                            }
                            Err(e) => {
                                eprintln!("{} {}", color::error_indicator(), e);
                                exit(1);
                            }
                        }
                        return;
                    }
                }
            }
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

fn run_batch(flags: &Flags, bail: bool, arg_commands: Option<Vec<Vec<String>>>) {
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

        let parsed = match parse_command(cmd_args, flags) {
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

        match send_command(parsed, &flags.session) {
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
}
