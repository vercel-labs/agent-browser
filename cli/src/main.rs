mod color;
mod commands;
mod connection;
mod flags;
mod install;
mod output;

use serde_json::json;
use std::env;
use std::fs;
use std::process::exit;

#[cfg(unix)]
use libc;

#[cfg(windows)]
use windows_sys::Win32::Foundation::CloseHandle;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

use commands::{gen_id, parse_command, ParseError};
use connection::{ensure_daemon, send_command};
use flags::{clean_args, parse_flags};
use install::run_install;
use output::{print_command_help, print_help, print_response, print_version};

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
            let tmp = env::temp_dir();
            let mut sessions: Vec<String> = Vec::new();

            if let Ok(entries) = fs::read_dir(&tmp) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Look for socket files (Unix) or pid files
                    if name.starts_with("agent-browser-") && name.ends_with(".pid") {
                        let session_name = name
                            .strip_prefix("agent-browser-")
                            .and_then(|s| s.strip_suffix(".pid"))
                            .unwrap_or("");
                        if !session_name.is_empty() {
                            // Check if session is actually running
                            let pid_path = tmp.join(&name);
                            if let Ok(pid_str) = fs::read_to_string(&pid_path) {
                                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                                    #[cfg(unix)]
                                    let running = unsafe { libc::kill(pid as i32, 0) == 0 };
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
                    let marker = if s == session { color::cyan("→") } else { " ".to_string() };
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
    let args: Vec<String> = env::args().skip(1).collect();
    let flags = parse_flags(&args);
    let clean = clean_args(&args);

    let has_help = args.iter().any(|a| a == "--help" || a == "-h");
    let has_version = args.iter().any(|a| a == "--version" || a == "-V");

    if has_help {
        if let Some(cmd) = clean.get(0) {
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
    if clean.get(0).map(|s| s.as_str()) == Some("install") {
        let with_deps = args.iter().any(|a| a == "--with-deps" || a == "-d");
        run_install(with_deps);
        return;
    }

    // Handle session separately (doesn't need daemon)
    if clean.get(0).map(|s| s.as_str()) == Some("session") {
        run_session(&clean, &flags.session, flags.json);
        return;
    }

    let cmd = match parse_command(&clean, &flags) {
        Ok(c) => c,
        Err(e) => {
            if flags.json {
                let error_type = match &e {
                    ParseError::UnknownCommand { .. } => "unknown_command",
                    ParseError::UnknownSubcommand { .. } => "unknown_subcommand",
                    ParseError::MissingArguments { .. } => "missing_arguments",
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

    let daemon_result = match ensure_daemon(&flags.session, flags.headed, flags.executable_path.as_deref(), &flags.extensions) {
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

    // Warn if executable_path was specified but daemon was already running
    if daemon_result.already_running && (flags.executable_path.is_some() || !flags.extensions.is_empty()) {
        if !flags.json {
            if flags.executable_path.is_some() {
                eprintln!("{} --executable-path ignored: daemon already running. Use 'agent-browser close' first to restart with new path.", color::warning_indicator());
            }
            if !flags.extensions.is_empty() {
                eprintln!("{} --extension ignored: daemon already running. Use 'agent-browser close' first to restart with extensions.", color::warning_indicator());
            }
        }
    }

    // Connect via CDP if --cdp flag is set
    if let Some(ref port) = flags.cdp {
        let cdp_port: u16 = match port.parse::<u32>() {
            Ok(p) if p == 0 => {
                let msg = "Invalid CDP port: port must be greater than 0".to_string();
                if flags.json {
                    println!(r#"{{"success":false,"error":"{}"}}"#, msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
            Ok(p) if p > 65535 => {
                let msg = format!("Invalid CDP port: {} is out of range (valid range: 1-65535)", p);
                if flags.json {
                    println!(r#"{{"success":false,"error":"{}"}}"#, msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
            Ok(p) => p as u16,
            Err(_) => {
                let msg = format!("Invalid CDP port: '{}' is not a valid number. Port must be a number between 1 and 65535", port);
                if flags.json {
                    println!(r#"{{"success":false,"error":"{}"}}"#, msg);
                } else {
                    eprintln!("{} {}", color::error_indicator(), msg);
                }
                exit(1);
            }
        };

        let launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "cdpPort": cdp_port
        });

        let err = match send_command(launch_cmd, &flags.session) {
            Ok(resp) if resp.success => None,
            Ok(resp) => Some(resp.error.unwrap_or_else(|| "CDP connection failed".to_string())),
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

    // Launch headed browser or proxy if flags are set (without CDP)
    if (flags.headed || flags.proxy.is_some()) && flags.cdp.is_none() {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "headless": !flags.headed
        });

        if let Some(ref proxy_str) = flags.proxy {
            let proxy_obj = parse_proxy(proxy_str);
            launch_cmd.as_object_mut()
                .expect("json! macro guarantees object type")
                .insert("proxy".to_string(), proxy_obj);
        }

        if let Err(e) = send_command(launch_cmd, &flags.session) {
            if !flags.json {
                eprintln!("{} Could not configure browser: {}", color::warning_indicator(), e);
            }
        }
    }

    match send_command(cmd, &flags.session) {
        Ok(resp) => {
            let success = resp.success;
            print_response(&resp, flags.json);
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
