use serde_json::Value;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::signal;

use super::actions::{execute_command, pre_command_setup, AliveCheckInfo, DaemonState};
use super::state;

pub async fn run_daemon(session: &str) {
    let socket_dir = get_daemon_socket_dir();
    if !socket_dir.exists() {
        let _ = fs::create_dir_all(&socket_dir);
    }

    let pid_path = socket_dir.join(format!("{}.pid", session));
    let _ = fs::write(&pid_path, process::id().to_string());

    let socket_path = socket_dir.join(format!("{}.sock", session));

    if socket_path.exists() {
        let _ = fs::remove_file(&socket_path);
    }

    if let Ok(days_str) = env::var("AGENT_BROWSER_STATE_EXPIRE_DAYS") {
        if let Ok(days) = days_str.parse::<u64>() {
            if days > 0 {
                let _ = state::state_clean(days);
            }
        }
    }

    let result = run_socket_server(&socket_path, session).await;

    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(&pid_path);
    let stream_path = socket_dir.join(format!("{}.stream", session));
    let _ = fs::remove_file(&stream_path);

    if let Err(e) = result {
        eprintln!("Daemon error: {}", e);
        process::exit(1);
    }
}

#[cfg(unix)]
async fn run_socket_server(socket_path: &PathBuf, _session: &str) -> Result<(), String> {
    use tokio::net::UnixListener;

    let listener =
        UnixListener::bind(socket_path).map_err(|e| format!("Failed to bind socket: {}", e))?;

    let state: std::sync::Arc<tokio::sync::Mutex<DaemonState>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(DaemonState::new()));

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, state).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                    }
                }
            }
            _ = shutdown_signal() => {
                let mut s = state.lock().await;
                if let Some(ref mut mgr) = s.browser {
                    let _ = mgr.close().await;
                }
                break;
            }
        }
    }

    Ok(())
}

#[cfg(windows)]
async fn run_socket_server(socket_path: &PathBuf, session: &str) -> Result<(), String> {
    use tokio::net::TcpListener;

    let port = get_port_for_session(session);
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| format!("Failed to bind TCP: {}", e))?;

    let socket_dir = socket_path.parent().unwrap_or(std::path::Path::new("."));
    let port_path = socket_dir.join(format!("{}.port", session));
    let _ = fs::write(&port_path, port.to_string());

    let state: std::sync::Arc<tokio::sync::Mutex<DaemonState>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(DaemonState::new()));

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, state).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                    }
                }
            }
            _ = shutdown_signal() => {
                let mut s = state.lock().await;
                if let Some(ref mut mgr) = s.browser {
                    let _ = mgr.close().await;
                }
                let _ = fs::remove_file(&port_path);
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection<S>(stream: S, state: std::sync::Arc<tokio::sync::Mutex<DaemonState>>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();

    loop {
        line.clear();
        match buf_reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if looks_like_http(trimmed) {
                    break;
                }

                let cmd: Value = match serde_json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(e) => {
                        let err = serde_json::json!({
                            "success": false,
                            "error": format!("Invalid JSON: {}", e),
                        });
                        let mut resp = serde_json::to_string(&err).unwrap_or_default();
                        resp.push('\n');
                        let _ = writer.write_all(resp.as_bytes()).await;
                        continue;
                    }
                };

                let is_close = cmd.get("action").and_then(|v| v.as_str()) == Some("close");

                // Phase 1: Determine if a slow connection-liveness check is
                // needed (quick lock, no I/O).
                let check_info = {
                    let s = state.lock().await;
                    s.alive_check_info(&cmd)
                };

                // Phase 2: Perform the slow CDP ping *outside* the lock so
                // other connections can make progress.
                let alive_hint = match check_info {
                    AliveCheckInfo::Skip => None,
                    AliveCheckInfo::Check(handle) => Some(handle.is_alive().await),
                };

                // Phase 3: Run pre-command setup and execution atomically
                // under a single lock, using the alive hint to skip the
                // redundant inline check when the connection was verified.
                let response = {
                    let mut s = state.lock().await;
                    if let Some(resp) = pre_command_setup(&cmd, &mut s, alive_hint).await {
                        resp
                    } else {
                        execute_command(&cmd, &mut s).await
                    }
                };

                let mut resp = serde_json::to_string(&response).unwrap_or_default();
                resp.push('\n');
                if writer.write_all(resp.as_bytes()).await.is_err() {
                    break;
                }

                if is_close {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    process::exit(0);
                }
            }
            Err(_) => break,
        }
    }
}

fn looks_like_http(line: &str) -> bool {
    let prefixes = [
        "GET ", "POST ", "PUT ", "DELETE ", "PATCH ", "HEAD ", "OPTIONS ", "CONNECT ", "TRACE ",
    ];
    prefixes.iter().any(|p| line.starts_with(p))
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut sigint = match signal::unix::signal(signal::unix::SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to install SIGINT handler: {}", e);
                process::exit(1);
            }
        };
        let mut sigterm = match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to install SIGTERM handler: {}", e);
                process::exit(1);
            }
        };
        let mut sighup = match signal::unix::signal(signal::unix::SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to install SIGHUP handler: {}", e);
                process::exit(1);
            }
        };

        tokio::select! {
            _ = sigint.recv() => {}
            _ = sigterm.recv() => {}
            _ = sighup.recv() => {}
        }
    }

    #[cfg(windows)]
    {
        if let Err(e) = signal::ctrl_c().await {
            eprintln!("Failed to install Ctrl+C handler: {}", e);
            process::exit(1);
        }
    }
}

fn get_daemon_socket_dir() -> PathBuf {
    if let Ok(dir) = env::var("AGENT_BROWSER_SOCKET_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }

    if let Ok(xdg) = env::var("XDG_RUNTIME_DIR") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("agent-browser");
        }
    }

    if let Some(home) = dirs::home_dir() {
        return home.join(".agent-browser");
    }

    std::env::temp_dir().join("agent-browser")
}

#[cfg(windows)]
fn get_port_for_session(session: &str) -> u16 {
    // Must match the hash algorithm in connection.rs and daemon.ts
    let mut hash: i32 = 0;
    for c in session.chars() {
        hash = ((hash << 5).wrapping_sub(hash)).wrapping_add(c as i32);
    }
    49152 + ((hash.unsigned_abs() as u32 % 16383) as u16)
}
