mod color;
mod commands;
mod connection;
mod flags;
mod install;
mod output;
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
use connection::{ensure_daemon, get_socket_dir, send_command, DaemonOptions};
use flags::{clean_args, parse_flags};
use install::run_install;
use output::{print_command_help, print_help, print_response_with_opts, print_version, OutputOptions};

use std::path::PathBuf;
use std::process::Command as ProcessCommand;

/// Run a local auth command (auth_save/list/show/delete) via node auth-cli.js.
/// These commands don't need a browser, so we handle them directly to avoid
/// sending passwords through the daemon's Unix socket channel.
fn run_auth_cli(cmd: &serde_json::Value, json_mode: bool) -> ! {
    let exe_path = env::current_exe().unwrap_or_default();
    let exe_path = exe_path.canonicalize().unwrap_or(exe_path);
    let exe_dir = exe_path.parent().unwrap_or(std::path::Path::new("."));

    let mut script_paths = vec![
        exe_dir.join("auth-cli.js"),
        exe_dir.join("../dist/auth-cli.js"),
        PathBuf::from("dist/auth-cli.js"),
    ];

    if let Ok(home) = env::var("AGENT_BROWSER_HOME") {
        let home_path = PathBuf::from(&home);
        script_paths.insert(0, home_path.join("dist/auth-cli.js"));
        script_paths.insert(1, home_path.join("auth-cli.js"));
    }

    let script_path = match script_paths.iter().find(|p| p.exists()) {
        Some(p) => p.clone(),
        None => {
            if json_mode {
                println!(r#"{{"success":false,"error":"auth-cli.js not found"}}"#);
            } else {
                eprintln!(
                    "{} auth-cli.js not found. Set AGENT_BROWSER_HOME or run from project directory.",
                    color::error_indicator()
                );
            }
            exit(1);
        }
    };

    let cmd_json = serde_json::to_string(cmd).unwrap_or_default();

    match ProcessCommand::new("node")
        .arg(&script_path)
        .arg(&cmd_json)
        .output()
    {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let stdout = stdout.trim();

            if stdout.is_empty() {
                if json_mode {
                    println!(r#"{{"success":false,"error":"No response from auth-cli"}}"#);
                } else {
                    eprintln!("{} No response from auth-cli", color::error_indicator());
                }
                exit(1);
            }

            if json_mode {
                println!("{}", stdout);
            } else {
                // Parse the JSON response and use the standard output formatter
                match serde_json::from_str::<connection::Response>(stdout) {
                    Ok(resp) => {
                        let action = cmd.get("action").and_then(|v| v.as_str());
                        let opts = OutputOptions {
                            json: false,
                            content_boundaries: false,
                            max_output: None,
                        };
                        print_response_with_opts(&resp, action, &opts);
                        if !resp.success {
                            exit(1);
                        }
                    }
                    Err(_) => {
                        println!("{}", stdout);
                    }
                }
            }
            exit(output.status.code().unwrap_or(0));
        }
        Err(e) => {
            if json_mode {
                println!(r#"{{"success":false,"error":"Failed to run auth-cli: {}"}}"#, e);
            } else {
                eprintln!("{} Failed to run auth-cli: {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }
}

fn parse_proxy(proxy_str: &str) -> serde_json::Value {
    let Some(protocol_end) = proxy_str.find("://") else {
        return json!({ "server": proxy_str });
    };
    let protocol = &proxy_str[..protocol_end + 3];
    let rest = &proxy_str[protocol_end + 3..];

    let Some(at_pos) = rest.rfind('@') else {
        return json!({ "server": proxy_str });
    };

    let creds = &rest[..at_pos];
    let server_part = &rest[at_pos + 1..];
    let server = format!("{}{}", protocol, server_part);

    let Some(colon_pos) = creds.find(':') else {
        return json!({
            "server": server,
            "username": creds,
            "password": ""
        });
    };

    json!({
        "server": server,
        "username": &creds[..colon_pos],
        "password": &creds[colon_pos + 1..]
    })
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
                                    let running = unsafe { libc::kill(pid as i32, 0) == 0 };
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
                println!(r#"{{"success":true,"data":{{"session":"{}"}}}}"#, session);
            } else {
                println!("{}", session);
            }
        }
    }
}

fn main() {
    // Ignore SIGPIPE to prevent panic when piping to head/tail
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
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

    // Handle session separately (doesn't need daemon)
    if clean.first().map(|s| s.as_str()) == Some("session") {
        run_session(&clean, &flags.session, flags.json);
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
                println!(
                    r#"{{"success":false,"error":"{}","type":"{}"}}"#,
                    e.format().replace('\n', " "),
                    error_type
                );
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
        if cmd.get("passwordStdin").and_then(|v| v.as_bool()).unwrap_or(false) {
            let mut pass = String::new();
            if std::io::stdin().read_line(&mut pass).is_err() || pass.is_empty() {
                eprintln!("{} Failed to read password from stdin", color::error_indicator());
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

    // Handle local auth commands without starting the daemon.
    // These don't need a browser, so we avoid sending passwords through the socket.
    if let Some(action) = cmd.get("action").and_then(|v| v.as_str()) {
        if matches!(action, "auth_save" | "auth_list" | "auth_show" | "auth_delete") {
            run_auth_cli(&cmd, flags.json);
        }
    }

    // Validate session name before starting daemon
    if let Some(ref name) = flags.session_name {
        if !validation::is_valid_session_name(name) {
            let msg = validation::session_name_error(name);
            if flags.json {
                println!(
                    r#"{{"success":false,"error":"{}","type":"invalid_session_name"}}"#,
                    msg.replace('"', "\\\"")
                );
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    let daemon_opts = DaemonOptions {
        headed: flags.headed,
        executable_path: flags.executable_path.as_deref(),
        extensions: &flags.extensions,
        args: flags.args.as_deref(),
        user_agent: flags.user_agent.as_deref(),
        proxy: flags.proxy.as_deref(),
        proxy_bypass: flags.proxy_bypass.as_deref(),
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
    };
    let daemon_result = match ensure_daemon(&flags.session, &daemon_opts) {
        Ok(result) => result,
        Err(e) => {
            if flags.json {
                println!(r#"{{"success":false,"error":"{}"}}"#, e);
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
            println!(r#"{{"success":false,"error":"{}"}}"#, msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.auto_connect && flags.cdp.is_some() {
        let msg = "Cannot use --auto-connect and --cdp together";
        if flags.json {
            println!(r#"{{"success":false,"error":"{}"}}"#, msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.auto_connect && flags.provider.is_some() {
        let msg = "Cannot use --auto-connect and -p/--provider together";
        if flags.json {
            println!(r#"{{"success":false,"error":"{}"}}"#, msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.provider.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with -p/--provider (extensions require local browser)";
        if flags.json {
            println!(r#"{{"success":false,"error":"{}"}}"#, msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    if flags.cdp.is_some() && !flags.extensions.is_empty() {
        let msg = "Cannot use --extension with --cdp (extensions require local browser)";
        if flags.json {
            println!(r#"{{"success":false,"error":"{}"}}"#, msg);
        } else {
            eprintln!("{} {}", color::error_indicator(), msg);
        }
        exit(1);
    }

    // Auto-connect to existing browser
    if flags.auto_connect {
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
                println!(r#"{{"success":false,"error":"{}"}}"#, msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Connect via CDP if --cdp flag is set
    // Accepts either a port number (e.g., "9222") or a full URL (e.g., "ws://..." or "wss://...")
    if let Some(ref cdp_value) = flags.cdp {
        let mut launch_cmd = if cdp_value.starts_with("ws://")
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
                        println!(r#"{{"success":false,"error":"{}"}}"#, msg);
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
                        println!(r#"{{"success":false,"error":"{}"}}"#, msg);
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
                        println!(r#"{{"success":false,"error":"{}"}}"#, msg);
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
                println!(r#"{{"success":false,"error":"{}"}}"#, msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Launch with cloud provider if -p flag is set
    if let Some(ref provider) = flags.provider {
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
                println!(r#"{{"success":false,"error":"{}"}}"#, msg);
            } else {
                eprintln!("{} {}", color::error_indicator(), msg);
            }
            exit(1);
        }
    }

    // Launch headed browser or configure browser options (without CDP or provider)
    if (flags.headed
        || flags.executable_path.is_some()
        || flags.profile.is_some()
        || flags.state.is_some()
        || flags.proxy.is_some()
        || flags.args.is_some()
        || flags.user_agent.is_some()
        || flags.allow_file_access
        || flags.color_scheme.is_some()
        || flags.download_path.is_some())
        && flags.cdp.is_none()
        && flags.provider.is_none()
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

        // Add state path if specified
        if let Some(ref state_path) = flags.state {
            cmd_obj.insert("storageState".to_string(), json!(state_path));
        }

        if let Some(ref proxy_str) = flags.proxy {
            let mut proxy_obj = parse_proxy(proxy_str);
            // Add bypass if specified
            if let Some(ref bypass) = flags.proxy_bypass {
                if let Some(obj) = proxy_obj.as_object_mut() {
                    obj.insert("bypass".to_string(), json!(bypass));
                }
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

        match send_command(launch_cmd, &flags.session) {
            Ok(resp) if !resp.success => {
                // Launch command failed (e.g., invalid state file, profile error)
                let error_msg = resp
                    .error
                    .unwrap_or_else(|| "Browser launch failed".to_string());
                if flags.json {
                    println!(r#"{{"success":false,"error":"{}"}}"#, error_msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), error_msg);
                }
                exit(1);
            }
            Err(e) => {
                if flags.json {
                    println!(r#"{{"success":false,"error":"{}"}}"#, e);
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

    let output_opts = OutputOptions {
        json: flags.json,
        content_boundaries: flags.content_boundaries,
        max_output: flags.max_output,
    };

    match send_command(cmd.clone(), &flags.session) {
        Ok(resp) => {
            let success = resp.success;
            // Handle interactive confirmation
            if flags.confirm_interactive {
                if let Some(data) = &resp.data {
                    if data.get("confirmation_required").and_then(|v| v.as_bool()).unwrap_or(false) {
                        let desc = data.get("description").and_then(|v| v.as_str()).unwrap_or("unknown action");
                        let category = data.get("category").and_then(|v| v.as_str()).unwrap_or("");
                        let cid = data.get("confirmation_id").and_then(|v| v.as_str()).unwrap_or("");

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
                println!(r#"{{"success":false,"error":"{}"}}"#, e);
            } else {
                eprintln!("{} {}", color::error_indicator(), e);
            }
            exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_proxy_simple() {
        let result = parse_proxy("http://proxy.com:8080");
        assert_eq!(result["server"], "http://proxy.com:8080");
        assert!(result.get("username").is_none());
        assert!(result.get("password").is_none());
    }

    #[test]
    fn test_parse_proxy_with_auth() {
        let result = parse_proxy("http://user:pass@proxy.com:8080");
        assert_eq!(result["server"], "http://proxy.com:8080");
        assert_eq!(result["username"], "user");
        assert_eq!(result["password"], "pass");
    }

    #[test]
    fn test_parse_proxy_username_only() {
        let result = parse_proxy("http://user@proxy.com:8080");
        assert_eq!(result["server"], "http://proxy.com:8080");
        assert_eq!(result["username"], "user");
        assert_eq!(result["password"], "");
    }

    #[test]
    fn test_parse_proxy_no_protocol() {
        let result = parse_proxy("proxy.com:8080");
        assert_eq!(result["server"], "proxy.com:8080");
        assert!(result.get("username").is_none());
    }

    #[test]
    fn test_parse_proxy_socks5() {
        let result = parse_proxy("socks5://proxy.com:1080");
        assert_eq!(result["server"], "socks5://proxy.com:1080");
        assert!(result.get("username").is_none());
    }

    #[test]
    fn test_parse_proxy_socks5_with_auth() {
        let result = parse_proxy("socks5://admin:secret@proxy.com:1080");
        assert_eq!(result["server"], "socks5://proxy.com:1080");
        assert_eq!(result["username"], "admin");
        assert_eq!(result["password"], "secret");
    }

    #[test]
    fn test_parse_proxy_complex_password() {
        let result = parse_proxy("http://user:p@ss:w0rd@proxy.com:8080");
        assert_eq!(result["server"], "http://proxy.com:8080");
        assert_eq!(result["username"], "user");
        assert_eq!(result["password"], "p@ss:w0rd");
    }
}
