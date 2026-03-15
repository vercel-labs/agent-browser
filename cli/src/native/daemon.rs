use serde_json::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::signal;
use tokio::sync::{mpsc, RwLock};

use super::actions::{execute_command, DaemonState};
use super::cdp::client::CdpClient;
use super::state;
use super::stream::StreamServer;

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

    let mut stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>> = None;
    let mut stream_server_instance: Option<Arc<StreamServer>> = None;
    if let Ok(port_str) = env::var("AGENT_BROWSER_STREAM_PORT") {
        if let Ok(port) = port_str.parse::<u16>() {
            if port > 0 {
                match StreamServer::start_without_client(port, session.to_string()).await {
                    Ok((stream_server, client_slot)) => {
                        stream_client = Some(client_slot.clone());
                        let stream_path = socket_dir.join(format!("{}.stream", session));
                        if let Err(e) = fs::write(&stream_path, stream_server.port().to_string()) {
                            let _ =
                                writeln!(std::io::stderr(), "Failed to write .stream file: {}", e);
                        }
                        stream_server_instance = Some(Arc::new(stream_server));
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "Stream server failed to start: {}", e);
                    }
                }
            }
        }
    }

    // Auto-shutdown the daemon after this many ms of inactivity (no commands received).
    // Disabled when unset or 0.
    let idle_timeout_ms = env::var("AGENT_BROWSER_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&ms| ms > 0);

    let result = run_socket_server(
        &socket_path,
        session,
        stream_client,
        stream_server_instance,
        idle_timeout_ms,
    )
    .await;

    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(&pid_path);
    let stream_path = socket_dir.join(format!("{}.stream", session));
    let _ = fs::remove_file(&stream_path);

    if let Err(e) = result {
        let _ = writeln!(std::io::stderr(), "Daemon error: {}", e);
        process::exit(1);
    }
}

#[cfg(unix)]
async fn run_socket_server(
    socket_path: &PathBuf,
    _session: &str,
    stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
    stream_server: Option<Arc<StreamServer>>,
    idle_timeout_ms: Option<u64>,
) -> Result<(), String> {
    use tokio::net::UnixListener;

    let listener =
        UnixListener::bind(socket_path).map_err(|e| format!("Failed to bind socket: {}", e))?;

    let state: std::sync::Arc<tokio::sync::Mutex<DaemonState>> = std::sync::Arc::new(
        tokio::sync::Mutex::new(DaemonState::new_with_stream(stream_client, stream_server)),
    );

    let (reset_tx, mut reset_rx) = mpsc::channel::<()>(64);
    let reset_tx = idle_timeout_ms.map(|_| Arc::new(reset_tx));

    loop {
        let sleep_future = idle_timeout_ms.map(|ms| tokio::time::sleep(Duration::from_millis(ms)));
        let mut sleep_pin = sleep_future.map(Box::pin);

        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let reset_tx = reset_tx.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, state, reset_tx).await;
                        });
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "Accept error: {}", e);
                    }
                }
            }
            _ = async {
                if let Some(ref mut s) = sleep_pin {
                    s.as_mut().await
                } else {
                    std::future::pending::<()>().await
                }
            }, if idle_timeout_ms.is_some() => {
                let mut s = state.lock().await;
                if let Some(ref mut mgr) = s.browser {
                    let _ = mgr.close().await;
                }
                break;
            }
            _ = reset_rx.recv(), if idle_timeout_ms.is_some() => {
                continue;
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
async fn run_socket_server(
    socket_path: &PathBuf,
    session: &str,
    stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
    stream_server: Option<Arc<StreamServer>>,
    idle_timeout_ms: Option<u64>,
) -> Result<(), String> {
    use tokio::net::TcpListener;

    let port = get_port_for_session(session);
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))
        .await
        .map_err(|e| format!("Failed to bind TCP: {}", e))?;

    let socket_dir = socket_path.parent().unwrap_or(std::path::Path::new("."));
    let port_path = socket_dir.join(format!("{}.port", session));
    let _ = fs::write(&port_path, port.to_string());

    let state: std::sync::Arc<tokio::sync::Mutex<DaemonState>> = std::sync::Arc::new(
        tokio::sync::Mutex::new(DaemonState::new_with_stream(stream_client, stream_server)),
    );

    let (reset_tx, mut reset_rx) = mpsc::channel::<()>(64);
    let reset_tx = idle_timeout_ms.map(|_| Arc::new(reset_tx));

    loop {
        let sleep_future = idle_timeout_ms.map(|ms| tokio::time::sleep(Duration::from_millis(ms)));
        let mut sleep_pin = sleep_future.map(Box::pin);

        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let reset_tx = reset_tx.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, state, reset_tx).await;
                        });
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "Accept error: {}", e);
                    }
                }
            }
            _ = async {
                if let Some(ref mut s) = sleep_pin {
                    s.as_mut().await
                } else {
                    std::future::pending::<()>().await
                }
            }, if idle_timeout_ms.is_some() => {
                let mut s = state.lock().await;
                if let Some(ref mut mgr) = s.browser {
                    let _ = mgr.close().await;
                }
                let _ = fs::remove_file(&port_path);
                break;
            }
            _ = reset_rx.recv(), if idle_timeout_ms.is_some() => {
                continue;
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

async fn handle_connection<S>(
    stream: S,
    state: std::sync::Arc<tokio::sync::Mutex<DaemonState>>,
    idle_reset_tx: Option<Arc<mpsc::Sender<()>>>,
) where
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

                if let Some(ref tx) = idle_reset_tx {
                    let _ = tx.try_send(());
                }

                let is_close = cmd.get("action").and_then(|v| v.as_str()) == Some("close");

                let response = {
                    let mut s = state.lock().await;
                    execute_command(&cmd, &mut s).await
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
                let _ = writeln!(std::io::stderr(), "Failed to install SIGINT handler: {}", e);
                process::exit(1);
            }
        };
        let mut sigterm = match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Failed to install SIGTERM handler: {}",
                    e
                );
                process::exit(1);
            }
        };
        let mut sighup = match signal::unix::signal(signal::unix::SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "Failed to install SIGHUP handler: {}", e);
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
            let _ = writeln!(std::io::stderr(), "Failed to install Ctrl+C handler: {}", e);
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
    let mut hash: i32 = 0;
    for c in session.chars() {
        hash = ((hash << 5).wrapping_sub(hash)).wrapping_add(c as i32);
    }
    49152 + ((hash.unsigned_abs() as u32 % 16383) as u16)
}

#[cfg(test)]
#[cfg(windows)]
mod tests {
    use super::*;

    #[test]
    fn test_port_matches_client_algorithm() {
        // These values are computed by the identical djb2 implementation in
        // connection.rs. Both sides must agree on the port for the daemon to
        // start successfully.
        assert_eq!(get_port_for_session("default"), 50838);
        assert_eq!(get_port_for_session("my-session"), 63105);
        assert_eq!(get_port_for_session("work"), 51184);
        assert_eq!(get_port_for_session(""), 49152);
    }
}
