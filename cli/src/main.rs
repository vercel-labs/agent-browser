mod commands;
mod connection;
mod flags;
mod install;
mod output;
mod skill;

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
use output::{print_help, print_response};
use skill::run_skill;

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
                    let marker = if s == session { "→" } else { " " };
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

    if clean.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
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

    // Handle skill separately (doesn't need daemon)
    if clean.get(0).map(|s| s.as_str()) == Some("skill") {
        run_skill(&clean, flags.json);
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
                eprintln!("\x1b[31m{}\x1b[0m", e.format());
            }
            exit(1);
        }
    };

    if let Err(e) = ensure_daemon(&flags.session, flags.headed) {
        if flags.json {
            println!(r#"{{"success":false,"error":"{}"}}"#, e);
        } else {
            eprintln!("\x1b[31m✗\x1b[0m {}", e);
        }
        exit(1);
    }

    // If --headed flag is set, send launch command first to switch to headed mode
    if flags.headed {
        let launch_cmd = json!({ "id": gen_id(), "action": "launch", "headless": false });
        if let Err(e) = send_command(launch_cmd, &flags.session) {
            if !flags.json {
                eprintln!("\x1b[33m⚠\x1b[0m Could not switch to headed mode: {}", e);
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
                eprintln!("\x1b[31m✗\x1b[0m {}", e);
            }
            exit(1);
        }
    }
}
