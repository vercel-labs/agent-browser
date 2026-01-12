mod commands;
mod connection;
mod flags;
mod install;
mod output;

use serde_json::json;
use std::env;
use std::process::exit;

use commands::{gen_id, parse_command};
use connection::{ensure_daemon, send_command};
use flags::{clean_args, parse_flags};
use install::run_install;
use output::{print_help, print_response};

fn parse_proxy(proxy_str: &str) -> serde_json::Value {
    // Parse URL format: http://user:pass@host:port or http://host:port
    let Some(protocol_end) = proxy_str.find("://") else {
        return json!({ "server": proxy_str });
    };
    let protocol = &proxy_str[..protocol_end + 3];

    // Check for credentials (user:pass@host format)
    let rest = &proxy_str[protocol_end + 3..];
    let Some(at_pos) = rest.rfind('@') else {
        return json!({ "server": proxy_str });
    };

    let creds = &rest[..at_pos];
    let Some(colon_pos) = creds.find(':') else {
        return json!({ "server": proxy_str });
    };

    let username = &creds[..colon_pos];
    let password = &creds[colon_pos + 1..];
    let server_part = &rest[at_pos + 1..];

    json!({
        "server": format!("{}{}", protocol, server_part),
        "username": username,
        "password": password
    })
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

    let cmd = match parse_command(&clean, &flags) {
        Some(c) => c,
        None => {
            eprintln!(
                "\x1b[31mUnknown command:\x1b[0m {}",
                clean.get(0).unwrap_or(&String::new())
            );
            eprintln!("\x1b[2mRun: agent-browser --help\x1b[0m");
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

    // If --headed flag or --proxy is set, send launch command first to configure browser
    if flags.headed || flags.proxy.is_some() {
        let mut launch_cmd = json!({
            "id": gen_id(),
            "action": "launch",
            "headless": !flags.headed
        });
        if let Some(ref proxy_str) = flags.proxy {
            let proxy_obj = parse_proxy(proxy_str);
            launch_cmd.as_object_mut().unwrap().insert(
                "proxy".to_string(),
                proxy_obj
            );
        }
        if let Err(e) = send_command(launch_cmd, &flags.session) {
            if !flags.json {
                eprintln!("\x1b[33m⚠\x1b[0m Could not configure browser: {}", e);
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
