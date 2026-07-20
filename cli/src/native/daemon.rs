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
use tokio::sync::{mpsc, Notify, RwLock};

use super::actions::{
    auto_save_restore_state, close_current_browser, execute_command, maybe_autosave_restore_state,
    DaemonState,
};
use super::cdp::client::CdpClient;
use super::state;
use super::stream::StreamServer;
use crate::connection::INTERNAL_DAEMON_SHUTDOWN_ACTION;

pub async fn run_daemon(session: &str) {
    let socket_dir = get_daemon_socket_dir();
    if !socket_dir.exists() {
        let _ = fs::create_dir_all(&socket_dir);
    }

    // When debug mode is on, redirect stderr to a log file so daemon
    // output can be inspected (the daemon normally has stderr piped to its
    // parent which drops the read end after startup).
    #[cfg(unix)]
    if env::var("AGENT_BROWSER_DEBUG").is_ok() {
        let log_path = socket_dir.join(format!("{}.log", session));
        if let Ok(file) = fs::File::create(&log_path) {
            use std::os::unix::io::IntoRawFd;
            let fd = file.into_raw_fd();
            unsafe {
                libc::dup2(fd, 2);
                libc::close(fd);
            }
            let _ = writeln!(
                std::io::stderr(),
                "[daemon] Debug logging started for session: {}",
                session
            );
        }
    } else {
        // Redirect stderr to /dev/null to prevent daemon crash when the
        // parent CLI drops the piped stderr handle after startup.  Cloud
        // providers (AgentCore, Browserbase, etc.) may write to stderr
        // during connection setup; a broken pipe would kill the daemon.
        #[cfg(unix)]
        {
            use std::os::unix::io::IntoRawFd;
            if let Ok(devnull) = fs::File::create("/dev/null") {
                let fd = devnull.into_raw_fd();
                unsafe {
                    libc::dup2(fd, 2);
                    libc::close(fd);
                }
            }
        }
    }

    let pid_path = socket_dir.join(format!("{}.pid", session));
    let _ = fs::write(&pid_path, process::id().to_string());

    let version_path = socket_dir.join(format!("{}.version", session));
    let _ = fs::write(&version_path, env!("CARGO_PKG_VERSION"));

    // On Unix the daemon listens on a Unix domain socket; on Windows it uses
    // TCP, so there is no .sock file — only a .port file written by the server.
    let socket_path = socket_dir.join(format!("{}.sock", session));

    #[cfg(unix)]
    if socket_path.exists() {
        let _ = fs::remove_file(&socket_path);
    }

    #[cfg(windows)]
    {
        let _ = fs::remove_file(socket_dir.join(format!("{}.port", session)));
    }

    let stream_path = socket_dir.join(format!("{}.stream", session));
    let _ = fs::remove_file(&stream_path);
    let _ = fs::remove_file(socket_dir.join(format!("{}.engine", session)));
    let _ = fs::remove_file(socket_dir.join(format!("{}.provider", session)));
    let _ = fs::remove_file(socket_dir.join(format!("{}.extensions", session)));

    if let Ok(days_str) = env::var("AGENT_BROWSER_STATE_EXPIRE_DAYS") {
        if let Ok(days) = days_str.parse::<u64>() {
            if days > 0 {
                let _ = state::state_clean(days);
            }
        }
    }

    let mut stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>> = None;
    let mut stream_server_instance: Option<Arc<StreamServer>> = None;
    let preferred_port = env::var("AGENT_BROWSER_STREAM_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    match StreamServer::start_without_client(preferred_port, session.to_string(), true).await {
        Ok((stream_server, client_slot)) => {
            stream_client = Some(client_slot.clone());
            if let Err(e) = fs::write(&stream_path, stream_server.port().to_string()) {
                let _ = writeln!(std::io::stderr(), "Failed to write .stream file: {}", e);
            }
            stream_server_instance = Some(Arc::new(stream_server));
        }
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "Stream server failed to start: {}", e);
        }
    }

    // Auto-shutdown the daemon after this many ms of inactivity (no commands received).
    // Disabled when unset or 0.
    let idle_timeout_ms = env::var("AGENT_BROWSER_IDLE_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&ms| ms > 0);

    let autosave_interval_ms = autosave_interval_ms_from_env();

    let result = run_socket_server(
        &socket_path,
        session,
        stream_client,
        stream_server_instance,
        idle_timeout_ms,
        autosave_interval_ms,
    )
    .await;

    #[cfg(unix)]
    {
        let _ = fs::remove_file(&socket_path);
    }
    #[cfg(windows)]
    {
        let _ = fs::remove_file(socket_dir.join(format!("{}.port", session)));
    }
    let _ = fs::remove_file(&pid_path);
    let _ = fs::remove_file(&version_path);
    let _ = fs::remove_file(&stream_path);
    let _ = fs::remove_file(socket_dir.join(format!("{}.engine", session)));
    let _ = fs::remove_file(socket_dir.join(format!("{}.provider", session)));
    let _ = fs::remove_file(socket_dir.join(format!("{}.extensions", session)));

    if let Err(e) = result {
        let _ = writeln!(std::io::stderr(), "Daemon error: {}", e);
        process::exit(1);
    }
}

/// Minimum ms between periodic session autosaves while the browser is open.
/// Defaults to 30s; 0 disables periodic autosave (save-on-close still runs).
fn autosave_interval_ms_from_env() -> u64 {
    env::var("AGENT_BROWSER_AUTOSAVE_INTERVAL_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30_000)
}

#[cfg(unix)]
async fn run_socket_server(
    socket_path: &PathBuf,
    session: &str,
    stream_client: Option<Arc<RwLock<Option<Arc<CdpClient>>>>>,
    stream_server: Option<Arc<StreamServer>>,
    idle_timeout_ms: Option<u64>,
    autosave_interval_ms: u64,
) -> Result<(), String> {
    use tokio::net::UnixListener;

    let listener =
        UnixListener::bind(socket_path).map_err(|e| format!("Failed to bind socket: {}", e))?;

    let stream_file: Option<PathBuf> = if stream_server.is_some() {
        let dir = socket_path.parent().unwrap_or(std::path::Path::new("."));
        Some(dir.join(format!("{}.stream", session)))
    } else {
        None
    };

    let state: std::sync::Arc<tokio::sync::Mutex<DaemonState>> = std::sync::Arc::new(
        tokio::sync::Mutex::new(DaemonState::new_with_stream(stream_client, stream_server)),
    );

    let (reset_tx, mut reset_rx) = mpsc::channel::<()>(64);
    let reset_tx = idle_timeout_ms.map(|_| Arc::new(reset_tx));

    // Notifier used by handle_connection to signal the daemon loop to exit
    // after a "close" command, instead of calling process::exit() which skips
    // destructors and can leave Chrome processes orphaned (issue #1113).
    let close_notify = Arc::new(Notify::new());

    let mut drain_interval = tokio::time::interval(Duration::from_millis(100));
    drain_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let idle_sleep = idle_timeout_ms.map(|ms| tokio::time::sleep(Duration::from_millis(ms)));
    let mut idle_sleep_pin = idle_sleep.map(Box::pin);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let reset_tx = reset_tx.clone();
                        let sf = stream_file.clone();
                        let cn = close_notify.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, state, reset_tx, sf, cn).await;
                        });
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "Accept error: {}", e);
                    }
                }
            }
            _ = drain_interval.tick() => {
                let mut s = state.lock().await;
                let process_exited = s
                    .browser
                    .as_mut()
                    .map(|mgr| mgr.has_process_exited())
                    .unwrap_or(false);
                if process_exited {
                    let _ = close_current_browser(&mut s).await;
                } else if s.browser.is_some() {
                    if let Err(error) = s.drain_cdp_events_background().await {
                        let _ = writeln!(
                            std::io::stderr(),
                            "Failed to apply browser network controls: {}",
                            error
                        );
                    } else {
                        maybe_autosave_restore_state(&mut s, autosave_interval_ms).await;
                    }
                }
            }
            _ = async {
                match idle_sleep_pin {
                    Some(ref mut s) => s.as_mut().await,
                    None => std::future::pending::<()>().await,
                }
            }, if idle_timeout_ms.is_some() => {
                let mut s = state.lock().await;
                let _ = auto_save_restore_state(&mut s).await;
                let _ = close_current_browser(&mut s).await;
                break;
            }
            _ = reset_rx.recv(), if idle_timeout_ms.is_some() => {
                idle_sleep_pin = idle_timeout_ms
                    .map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                continue;
            }
            _ = close_notify.notified() => {
                // "close" command was handled; browser already closed by
                // handle_close(). Break to run cleanup and exit gracefully
                // so destructors fire.
                break;
            }
            _ = shutdown_signal() => {
                let mut s = state.lock().await;
                let _ = auto_save_restore_state(&mut s).await;
                let _ = close_current_browser(&mut s).await;
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
    autosave_interval_ms: u64,
) -> Result<(), String> {
    use tokio::net::TcpListener;

    let preferred_port = get_port_for_session(session);
    // Try the hash-derived port first; if it is blocked (e.g. Windows Hyper-V
    // excluded port range), fall back to an OS-assigned ephemeral port.
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", preferred_port)).await {
        Ok(l) => l,
        Err(_) => TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("Failed to bind TCP: {}", e))?,
    };
    let actual_port = listener
        .local_addr()
        .map_err(|e| format!("Failed to get local address: {}", e))?
        .port();

    let socket_dir = socket_path.parent().unwrap_or(std::path::Path::new("."));
    let port_path = socket_dir.join(format!("{}.port", session));
    let _ = fs::write(&port_path, actual_port.to_string());

    let stream_file: Option<PathBuf> = if stream_server.is_some() {
        Some(socket_dir.join(format!("{}.stream", session)))
    } else {
        None
    };

    let state: std::sync::Arc<tokio::sync::Mutex<DaemonState>> = std::sync::Arc::new(
        tokio::sync::Mutex::new(DaemonState::new_with_stream(stream_client, stream_server)),
    );

    let (reset_tx, mut reset_rx) = mpsc::channel::<()>(64);
    let reset_tx = idle_timeout_ms.map(|_| Arc::new(reset_tx));

    let close_notify = Arc::new(Notify::new());

    let idle_sleep = idle_timeout_ms.map(|ms| tokio::time::sleep(Duration::from_millis(ms)));
    let mut idle_sleep_pin = idle_sleep.map(Box::pin);

    // Mirror the unix loop's background tick: reap a browser the user closed
    // by hand, and drain CDP events (dialog state in particular) before
    // autosave so a save never runs against a dialog-blocked renderer.
    let mut drain_interval = tokio::time::interval(Duration::from_millis(100));
    drain_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let reset_tx = reset_tx.clone();
                        let sf = stream_file.clone();
                        let cn = close_notify.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, state, reset_tx, sf, cn).await;
                        });
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "Accept error: {}", e);
                    }
                }
            }
            _ = drain_interval.tick() => {
                let mut s = state.lock().await;
                let process_exited = s
                    .browser
                    .as_mut()
                    .map(|mgr| mgr.has_process_exited())
                    .unwrap_or(false);
                if process_exited {
                    let _ = close_current_browser(&mut s).await;
                } else if s.browser.is_some() {
                    s.drain_cdp_events_background().await;
                    maybe_autosave_restore_state(&mut s, autosave_interval_ms).await;
                }
            }
            _ = async {
                match idle_sleep_pin {
                    Some(ref mut s) => s.as_mut().await,
                    None => std::future::pending::<()>().await,
                }
            }, if idle_timeout_ms.is_some() => {
                let mut s = state.lock().await;
                let _ = auto_save_restore_state(&mut s).await;
                let _ = close_current_browser(&mut s).await;
                let _ = fs::remove_file(&port_path);
                break;
            }
            _ = reset_rx.recv(), if idle_timeout_ms.is_some() => {
                idle_sleep_pin = idle_timeout_ms
                    .map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                continue;
            }
            _ = close_notify.notified() => {
                let _ = fs::remove_file(&port_path);
                break;
            }
            _ = shutdown_signal() => {
                let mut s = state.lock().await;
                let _ = auto_save_restore_state(&mut s).await;
                let _ = close_current_browser(&mut s).await;
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
    stream_file_cleanup: Option<PathBuf>,
    close_notify: Arc<Notify>,
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

                let action = cmd
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                let response = {
                    let mut s = state.lock().await;
                    if action == "batch" {
                        execute_batch_command(&cmd, &mut s).await
                    } else {
                        execute_command(&cmd, &mut s).await
                    }
                };

                let mut resp = serde_json::to_string(&response).unwrap_or_default();
                resp.push('\n');
                if writer.write_all(resp.as_bytes()).await.is_err() {
                    break;
                }

                if close_completed_response(&action, &response) {
                    if let Some(ref path) = stream_file_cleanup {
                        let _ = fs::remove_file(path);
                    }
                    // Signal the daemon loop to exit gracefully instead of
                    // calling process::exit(), which skips destructors and
                    // can leave Chrome processes orphaned (issue #1113).
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    close_notify.notify_one();
                    return;
                }
            }
            Err(_) => break,
        }
    }
}

/// Execute a client-prepared batch while the caller holds the session state.
///
/// The CLI parses every child through the canonical command parser before it
/// builds the envelope. Parse failures remain ordered entries so `--bail`
/// behaves exactly as it does for daemon command failures. The daemon still
/// validates the envelope and rejects nested batches so direct protocol
/// clients cannot recurse indefinitely or bypass the single-envelope model.
pub(crate) async fn execute_batch_command(cmd: &Value, state: &mut DaemonState) -> Value {
    let id = cmd.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let bail = cmd.get("bail").and_then(|v| v.as_bool()).unwrap_or(false);
    let Some(entries) = cmd.get("entries").and_then(|v| v.as_array()) else {
        return serde_json::json!({
            "id": id,
            "success": false,
            "error": "Invalid batch request: entries must be an array",
        });
    };

    let mut results = Vec::with_capacity(entries.len());
    let mut had_error = false;
    let mut stop_reason: Option<&str> = None;
    let mut closed = false;
    let mut executed_child = false;
    let mut last_executable_result_index = None;

    for entry in entries {
        let command = entry
            .get("command")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));

        if let Some(parse_error) = entry.get("parseError").and_then(|v| v.as_str()) {
            results.push(serde_json::json!({
                "command": command,
                "success": false,
                "error": parse_error,
            }));
            had_error = true;
            if bail {
                stop_reason = Some("error");
                break;
            }
            continue;
        }

        let Some(request) = entry.get("request").filter(|v| v.is_object()) else {
            results.push(serde_json::json!({
                "command": command,
                "success": false,
                "error": "Invalid batch entry: request must be an object",
            }));
            had_error = true;
            if bail {
                stop_reason = Some("error");
                break;
            }
            continue;
        };

        let child_action = request
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if child_action == "batch" {
            results.push(serde_json::json!({
                "command": command,
                "success": false,
                "error": "Nested batch commands are not supported",
            }));
            had_error = true;
            if bail {
                stop_reason = Some("error");
                break;
            }
            continue;
        }

        // Box the future because execute_command can itself resume a pending
        // confirmed command through an async recursive path.
        executed_child = true;
        let response = Box::pin(execute_command(request, state)).await;
        let success = response
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut result = serde_json::json!({
            "command": command,
            "action": child_action,
            "success": success,
            "result": response.get("data").cloned().unwrap_or(Value::Null),
            "error": response.get("error").cloned().unwrap_or(Value::Null),
        });
        if let Some(warning) = response.get("warning") {
            result["warning"] = warning.clone();
        }
        results.push(result);
        last_executable_result_index = Some(results.len() - 1);

        if response_requires_confirmation(&response) {
            stop_reason = Some("confirmation");
            break;
        }
        if close_completed_response(child_action, &response) {
            closed = true;
            stop_reason = Some("close");
            break;
        }
        if !success {
            had_error = true;
            if bail {
                stop_reason = Some("error");
                break;
            }
        }
    }

    // Each child retains its own pre/post event drains because target changes,
    // dialogs, and navigation state are inputs to the next child. Yield once
    // and drain again at the outer boundary so events queued immediately after
    // the final child are applied before the batch response is serialized.
    // This is intentionally not an implicit network-idle wait: there is no
    // generic settle condition that is correct for every command, and callers
    // should include an explicit `wait` child when their workflow needs one.
    let mut final_drain_attempted = false;
    if executed_child && !closed {
        tokio::task::yield_now().await;
        final_drain_attempted = true;
        if let Err(error) = state.drain_cdp_events_background().await {
            had_error = true;
            results.push(serde_json::json!({
                "command": [],
                "success": false,
                "error": format!(
                    "Final batch event drain failed: {}",
                    super::browser::to_ai_friendly_error(&error)
                ),
                "batchFinalization": true,
            }));
        } else if let (Some(index), Some(dialog)) =
            (last_executable_result_index, state.pending_dialog.as_ref())
        {
            if results[index].get("warning").is_none() {
                results[index]["warning"] = serde_json::json!(format!(
                    "A JavaScript {} dialog is blocking the page: \"{}\" — use `dialog accept` or `dialog dismiss` to resolve it",
                    dialog.dialog_type, dialog.message
                ));
            }
        }
    }

    serde_json::json!({
        "id": id,
        "success": !had_error,
        "data": {
            "results": results,
            "stopped": stop_reason.is_some(),
            "stopReason": stop_reason,
            "closed": closed,
            "finalDrainAttempted": final_drain_attempted,
        },
    })
}

fn response_requires_confirmation(response: &Value) -> bool {
    response
        .get("data")
        .and_then(|v| v.get("confirmation_required"))
        .and_then(|v| v.as_bool())
        == Some(true)
}

fn looks_like_http(line: &str) -> bool {
    let prefixes = [
        "GET ", "POST ", "PUT ", "DELETE ", "PATCH ", "HEAD ", "OPTIONS ", "CONNECT ", "TRACE ",
    ];
    prefixes.iter().any(|p| line.starts_with(p))
}

fn close_completed_response(action: &str, response: &Value) -> bool {
    if !matches!(
        action,
        "close" | "confirm" | "batch" | INTERNAL_DAEMON_SHUTDOWN_ACTION
    ) {
        return false;
    }

    fn data_closed(data: &Value) -> bool {
        data.get("closed").and_then(|v| v.as_bool()) == Some(true)
    }

    let Some(data) = response.get("data") else {
        return false;
    };
    // A batch can report earlier child errors and still complete a later
    // `close`. The outer response is then unsuccessful, but the daemon must
    // honor the successful close instead of lingering without a browser.
    if action == "batch" && data_closed(data) {
        return true;
    }
    if response.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }
    if data_closed(data) {
        return true;
    }

    data.get("result").is_some_and(|result| {
        result.get("success").and_then(|v| v.as_bool()) == Some(true)
            && result.get("data").is_some_and(data_closed)
    })
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
    crate::connection::get_socket_dir()
}

#[cfg(windows)]
fn get_port_for_session(session: &str) -> u16 {
    crate::connection::get_port_for_session(session)
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_daemon_socket_dir_matches_client_namespace() {
        let guard = crate::test_utils::EnvGuard::new(&[
            "AGENT_BROWSER_SOCKET_DIR",
            "XDG_RUNTIME_DIR",
            "AGENT_BROWSER_NAMESPACE",
        ]);
        let dir = tempfile::tempdir().unwrap();
        guard.set("AGENT_BROWSER_SOCKET_DIR", dir.path().to_str().unwrap());
        guard.remove("XDG_RUNTIME_DIR");
        guard.set("AGENT_BROWSER_NAMESPACE", "Worktree: One");

        let socket_dir = get_daemon_socket_dir();

        assert_eq!(socket_dir, crate::connection::get_socket_dir());
        assert!(socket_dir.ends_with(
            std::path::PathBuf::from("namespaces")
                .join("worktree-one")
                .join("run")
        ));
    }

    #[cfg(windows)]
    #[test]
    fn test_port_matches_client_algorithm() {
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_NAMESPACE"]);
        guard.remove("AGENT_BROWSER_NAMESPACE");

        assert_eq!(get_port_for_session("default"), 50838);
        assert_eq!(get_port_for_session("my-session"), 63105);
        assert_eq!(get_port_for_session("work"), 51184);
        assert_eq!(get_port_for_session(""), 49152);
    }

    #[test]
    fn test_close_completed_response_requires_actual_close_result() {
        let confirmation_response = serde_json::json!({
            "success": true,
            "data": {
                "confirmation_required": true,
                "confirmation_id": "close-1",
                "action": "close"
            }
        });

        assert!(!close_completed_response("close", &confirmation_response));
    }

    #[test]
    fn test_close_completed_response_accepts_direct_and_confirmed_close() {
        let direct = serde_json::json!({
            "success": true,
            "data": { "closed": true }
        });
        let confirmed = serde_json::json!({
            "success": true,
            "data": {
                "confirmed": true,
                "action": "close",
                "result": {
                    "success": true,
                    "data": { "closed": true }
                }
            }
        });

        assert!(close_completed_response("close", &direct));
        assert!(close_completed_response(
            crate::connection::INTERNAL_DAEMON_SHUTDOWN_ACTION,
            &direct
        ));
        assert!(close_completed_response("confirm", &confirmed));
    }

    #[tokio::test]
    async fn test_batch_preserves_order_and_bail_behavior() {
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let entries = serde_json::json!([
            { "command": ["bad-one"], "parseError": "first error" },
            {
                "command": ["stream", "status"],
                "request": { "id": "child-2", "action": "stream_status" }
            },
            { "command": ["bad-three"], "parseError": "third error" }
        ]);

        let continued = execute_batch_command(
            &serde_json::json!({
                "id": "batch-continue",
                "action": "batch",
                "entries": entries.clone(),
                "bail": false,
            }),
            &mut state,
        )
        .await;
        let continued_results = continued["data"]["results"].as_array().unwrap();
        assert_eq!(continued_results.len(), 3);
        assert_eq!(
            continued_results[0]["command"],
            serde_json::json!(["bad-one"])
        );
        assert_eq!(continued_results[1]["success"], true);
        assert_eq!(
            continued_results[2]["command"],
            serde_json::json!(["bad-three"])
        );
        assert_eq!(continued["success"], false);
        assert_eq!(continued["data"]["finalDrainAttempted"], true);

        let bailed = execute_batch_command(
            &serde_json::json!({
                "id": "batch-bail",
                "action": "batch",
                "entries": entries,
                "bail": true,
            }),
            &mut state,
        )
        .await;
        assert_eq!(bailed["data"]["results"].as_array().unwrap().len(), 1);
        assert_eq!(bailed["data"]["stopReason"], "error");
        assert_eq!(bailed["data"]["finalDrainAttempted"], false);
    }

    #[tokio::test]
    async fn test_batch_rejects_nested_requests_and_stops_for_confirmation() {
        use crate::native::policy::ConfirmActions;
        use std::collections::HashSet;

        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = Some(ConfirmActions {
            categories: HashSet::from(["stream_status".to_string()]),
        });
        let response = execute_batch_command(
            &serde_json::json!({
                "id": "batch-confirm",
                "action": "batch",
                "entries": [
                    {
                        "command": ["batch", "url"],
                        "request": { "id": "nested", "action": "batch", "entries": [] }
                    },
                    {
                        "command": ["stream", "status"],
                        "request": { "id": "confirm", "action": "stream_status" }
                    },
                    { "command": ["not-run"], "parseError": "must not execute" }
                ]
            }),
            &mut state,
        )
        .await;

        let results = response["data"]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0]["error"],
            "Nested batch commands are not supported"
        );
        assert_eq!(
            results[1]["result"]["confirmation_required"],
            serde_json::json!(true)
        );
        assert_eq!(response["data"]["stopReason"], "confirmation");
        assert_eq!(response["data"]["finalDrainAttempted"], true);
    }

    #[tokio::test]
    async fn test_confirmation_stop_is_a_successful_batch_exit() {
        use crate::native::policy::ConfirmActions;
        use std::collections::HashSet;

        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = Some(ConfirmActions {
            categories: HashSet::from(["stream_status".to_string()]),
        });
        let response = execute_batch_command(
            &serde_json::json!({
                "id": "batch-confirm-success",
                "action": "batch",
                "bail": true,
                "entries": [
                    {
                        "command": ["stream", "status"],
                        "request": { "id": "confirm", "action": "stream_status" }
                    },
                    { "command": ["not-run"], "parseError": "must not execute" }
                ]
            }),
            &mut state,
        )
        .await;

        assert_eq!(response["success"], true);
        assert_eq!(response["data"]["results"].as_array().unwrap().len(), 1);
        assert_eq!(response["data"]["stopReason"], "confirmation");
        assert_eq!(response["data"]["finalDrainAttempted"], true);
        assert_eq!(
            response["data"]["results"][0]["result"]["confirmation_required"],
            true
        );
    }

    #[tokio::test]
    async fn test_batch_close_stops_and_marks_outer_response_closed() {
        let mut state = DaemonState::new();
        state.policy = None;
        state.confirm_actions = None;
        let response = execute_batch_command(
            &serde_json::json!({
                "id": "batch-close",
                "action": "batch",
                "entries": [
                    { "command": ["bad"], "parseError": "earlier error" },
                    {
                        "command": ["close"],
                        "request": { "id": "close", "action": "close" }
                    },
                    { "command": ["not-run"], "parseError": "must not execute" }
                ]
            }),
            &mut state,
        )
        .await;

        assert_eq!(response["data"]["results"].as_array().unwrap().len(), 2);
        assert_eq!(response["success"], false);
        assert_eq!(response["data"]["stopReason"], "close");
        assert_eq!(response["data"]["closed"], true);
        assert_eq!(response["data"]["finalDrainAttempted"], false);
        assert!(close_completed_response("batch", &response));
    }

    #[tokio::test]
    async fn test_one_batch_line_returns_one_ordered_wrapper() {
        let (mut client, server) = tokio::io::duplex(16 * 1024);
        let mut daemon_state = DaemonState::new();
        daemon_state.policy = None;
        daemon_state.confirm_actions = None;
        let state = Arc::new(tokio::sync::Mutex::new(daemon_state));
        let close_notify = Arc::new(Notify::new());
        let server_task = tokio::spawn(handle_connection(server, state, None, None, close_notify));
        let request = serde_json::json!({
            "id": "one-wrapper",
            "action": "batch",
            "entries": [
                { "command": ["bad"], "parseError": "bad command" },
                {
                    "command": ["stream", "status"],
                    "request": { "id": "status", "action": "stream_status" }
                }
            ]
        });
        let mut wire = serde_json::to_vec(&request).unwrap();
        wire.push(b'\n');
        client.write_all(&wire).await.unwrap();

        let mut reader = BufReader::new(client);
        let mut response_line = String::new();
        reader.read_line(&mut response_line).await.unwrap();
        let response: Value = serde_json::from_str(&response_line).unwrap();
        assert_eq!(response["id"], "one-wrapper");
        assert_eq!(response["data"]["results"].as_array().unwrap().len(), 2);
        assert_eq!(response_line.lines().count(), 1);

        drop(reader);
        tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("connection task should finish after client closes")
            .unwrap();
    }

    /// Guard against re-introducing `waitpid(-1)` in daemon code.
    ///
    /// Issue #1035: a SIGCHLD handler that called `waitpid(-1, WNOHANG)` was
    /// added in v0.22.3 to reap zombie Chrome processes. This races with
    /// Rust's `Child::try_wait()` / `Child::wait()` because `waitpid(-1)`
    /// reaps *any* child, stealing the exit status before Rust can collect
    /// it. The result is ECHILD errors in `BrowserManager::has_process_exited()`
    /// and `ChromeProcess::kill()`, which can leave the daemon in a broken
    /// state or cause hangs on certain Linux configurations.
    ///
    /// The fix uses the existing 500ms drain interval to call
    /// `has_process_exited()` (which delegates to `Child::try_wait()`)
    /// for targeted, race-free zombie detection.
    #[test]
    fn test_no_waitpid_minus_one_in_daemon() {
        let source = include_str!("daemon.rs");
        // Only check production code (everything before `#[cfg(test)]`)
        let production_code = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(
            !production_code.contains("waitpid(-1"),
            "daemon.rs production code must not call waitpid(-1, ...). \
             Use Child::try_wait() via has_process_exited() instead. \
             See issue #1035."
        );
    }

    /// Verify that `Child::try_wait()` correctly detects a crashed child
    /// without needing a global SIGCHLD handler or `waitpid(-1)`.
    /// This is what `has_process_exited()` uses in the fixed code.
    #[cfg(unix)]
    #[test]
    fn test_child_try_wait_detects_exit_without_sigchld_handler() {
        use std::process::{Command, Stdio};

        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 42"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn child");

        std::thread::sleep(std::time::Duration::from_millis(200));

        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(
                    !status.success(),
                    "child exited with code 42, should not be success"
                );
            }
            Ok(None) => panic!("try_wait() returned None but child should have exited"),
            Err(e) => panic!("try_wait() should succeed without waitpid(-1): {}", e),
        }
    }

    /// Regression test for #1101: idle timeout must fire even while the
    /// drain interval ticks every 500 ms.  The bug was that `sleep_future`
    /// was created **inside** the loop, so each drain tick dropped the
    /// in-progress sleep and replaced it with a fresh one – the timer
    /// could never reach its deadline.
    #[tokio::test]
    async fn test_idle_timeout_fires_despite_drain_interval() {
        use tokio::sync::mpsc;

        let idle_timeout_ms: u64 = 1000;
        let mut drain_interval = tokio::time::interval(Duration::from_millis(500));
        drain_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let (_reset_tx, mut reset_rx) = mpsc::channel::<()>(64);

        let start = tokio::time::Instant::now();

        let exited = tokio::time::timeout(Duration::from_secs(5), async {
            let mut idle_sleep_pin = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
                idle_timeout_ms,
            ))));

            loop {
                tokio::select! {
                    _ = drain_interval.tick() => {}
                    _ = async {
                        match idle_sleep_pin {
                            Some(ref mut s) => s.as_mut().await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        break;
                    }
                    _ = reset_rx.recv() => {
                        idle_sleep_pin = Some(Box::pin(
                            tokio::time::sleep(Duration::from_millis(idle_timeout_ms)),
                        ));
                        continue;
                    }
                }
            }
        })
        .await;

        let elapsed = start.elapsed();

        assert!(
            exited.is_ok(),
            "idle timeout never fired – loop ran for >5 s (bug #1101)"
        );
        assert!(
            elapsed < Duration::from_millis(idle_timeout_ms + 500),
            "idle timeout took too long: {:?} (expected ~{} ms)",
            elapsed,
            idle_timeout_ms,
        );
    }

    /// Verify that `ChromeProcess::has_exited()` (which uses `Child::try_wait()`)
    /// correctly detects a killed child, the same way the drain interval does
    /// in the fixed daemon code. This ensures crash detection works without
    /// a SIGCHLD handler.
    #[cfg(unix)]
    #[test]
    fn test_has_exited_detects_killed_process() {
        use std::process::{Command, Stdio};

        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 60"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn child");

        // Process should be running
        match child.try_wait() {
            Ok(None) => {} // expected
            other => panic!("expected Ok(None) for running process, got {:?}", other),
        }

        // Kill it (simulates Chrome crash)
        child.kill().expect("failed to kill child");
        std::thread::sleep(std::time::Duration::from_millis(100));

        // try_wait should detect the exit
        match child.try_wait() {
            Ok(Some(_)) => {} // expected: detected the crash
            other => panic!(
                "expected Ok(Some(_)) after kill, got {:?}. \
                 Crash detection via try_wait() must work for the drain \
                 interval fix (issue #1035) to function correctly.",
                other
            ),
        }
    }
}
