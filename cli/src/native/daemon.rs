use serde_json::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::pin::Pin;
use std::process;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::signal;
use tokio::sync::{Notify, RwLock};

use super::actions::{
    auto_save_restore_state, close_current_browser, execute_command, maybe_autosave_restore_state,
    DaemonState,
};
use super::cdp::client::CdpClient;
use super::state;
use super::stream::StreamServer;
use crate::connection::INTERNAL_DAEMON_SHUTDOWN_ACTION;

/// Safety-net timeout for abandoned daemons. Four hours is long enough for
/// interactive and CI pauses while still reclaiming sessions that outlive
/// their caller. Set AGENT_BROWSER_IDLE_TIMEOUT_MS=0 to disable it.
const DEFAULT_IDLE_TIMEOUT_MS: u64 = 4 * 60 * 60 * 1000;

#[derive(Clone, Copy)]
struct IdleActivitySnapshot {
    active_leases: usize,
    epoch: u64,
    shutdown_claimed: bool,
}

struct IdleActivityState {
    active_leases: usize,
    epoch: u64,
    shutdown_claimed: bool,
}

/// Coordinates command leases with the idle deadline. The mutex is held only
/// for short, synchronous state transitions and is never held across an await.
/// A generation check prevents an old deadline from closing a session after a
/// command completed while the timeout branch was waiting for daemon state.
struct IdleActivity {
    state: Mutex<IdleActivityState>,
    notify: Notify,
}

impl IdleActivity {
    fn new() -> Self {
        Self {
            state: Mutex::new(IdleActivityState {
                active_leases: 0,
                epoch: 0,
                shutdown_claimed: false,
            }),
            notify: Notify::new(),
        }
    }

    fn snapshot(&self) -> IdleActivitySnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        IdleActivitySnapshot {
            active_leases: state.active_leases,
            epoch: state.epoch,
            shutdown_claimed: state.shutdown_claimed,
        }
    }

    /// Atomically claims shutdown only if no activity occurred since the
    /// deadline was armed. Once claimed, late commands are rejected by
    /// `CommandLease::acquire` and retry against the freshly spawned daemon.
    fn try_claim_shutdown(&self, deadline_epoch: u64) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.shutdown_claimed || state.active_leases > 0 || state.epoch != deadline_epoch {
            return false;
        }
        state.shutdown_claimed = true;
        true
    }
}

/// Keeps the idle timer paused while a command is queued or executing. The
/// lease ends before socket delivery so a client that stops reading cannot
/// keep an abandoned browser alive indefinitely through backpressure.
struct CommandLease {
    activity: Option<Arc<IdleActivity>>,
}

impl CommandLease {
    fn acquire(activity: Option<&Arc<IdleActivity>>) -> Option<Self> {
        if let Some(activity) = activity {
            let mut state = activity
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.shutdown_claimed {
                return None;
            }
            state.active_leases = state
                .active_leases
                .checked_add(1)
                .expect("idle command lease count overflow");
            state.epoch = state.epoch.wrapping_add(1);
            drop(state);
            activity.notify.notify_one();
        }
        Some(Self {
            activity: activity.cloned(),
        })
    }
}

impl Drop for CommandLease {
    fn drop(&mut self) {
        if let Some(ref activity) = self.activity {
            let mut state = activity
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            debug_assert!(state.active_leases > 0);
            state.active_leases -= 1;
            state.epoch = state.epoch.wrapping_add(1);
            drop(state);
            activity.notify.notify_one();
        }
    }
}

type IdleSleep = Pin<Box<tokio::time::Sleep>>;

fn reset_idle_deadline(
    idle_timeout_ms: Option<u64>,
    activity: Option<&Arc<IdleActivity>>,
    idle_sleep: &mut Option<IdleSleep>,
    deadline_epoch: &mut Option<u64>,
) {
    let (Some(timeout_ms), Some(activity)) = (idle_timeout_ms, activity) else {
        *idle_sleep = None;
        *deadline_epoch = None;
        return;
    };

    let snapshot = activity.snapshot();
    if snapshot.active_leases == 0 && !snapshot.shutdown_claimed {
        *idle_sleep = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
            timeout_ms,
        ))));
        *deadline_epoch = Some(snapshot.epoch);
    } else {
        *idle_sleep = None;
        *deadline_epoch = None;
    }
}

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

    // Auto-shutdown abandoned daemons after four hours by default. Explicit 0
    // preserves indefinitely-lived sessions for integrations that need them.
    let idle_timeout_ms = idle_timeout_ms_from_env();

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

fn idle_timeout_ms_from_env() -> Option<u64> {
    match env::var("AGENT_BROWSER_IDLE_TIMEOUT_MS") {
        Ok(value) => match value.parse::<u64>() {
            Ok(0) => None,
            Ok(ms) => Some(ms),
            Err(_) => Some(DEFAULT_IDLE_TIMEOUT_MS),
        },
        Err(_) => Some(DEFAULT_IDLE_TIMEOUT_MS),
    }
}

/// Preserve restorable session state before releasing browser resources.
/// Idle expiry and process signals share this path so the default timeout does
/// not silently discard cookies or authentication state. A failed save does
/// not prevent resource reclamation.
pub(crate) async fn save_and_close_for_shutdown(state: &mut DaemonState) {
    let _ = auto_save_restore_state(state).await;
    let _ = close_current_browser(state).await;
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

    let idle_activity = idle_timeout_ms.map(|_| Arc::new(IdleActivity::new()));

    // Notifier used by handle_connection to signal the daemon loop to exit
    // after a "close" command, instead of calling process::exit() which skips
    // destructors and can leave Chrome processes orphaned (issue #1113).
    let close_notify = Arc::new(Notify::new());

    let mut drain_interval = tokio::time::interval(Duration::from_millis(100));
    drain_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut idle_sleep_pin = None;
    let mut idle_deadline_epoch = None;
    reset_idle_deadline(
        idle_timeout_ms,
        idle_activity.as_ref(),
        &mut idle_sleep_pin,
        &mut idle_deadline_epoch,
    );

    loop {
        tokio::select! {
            biased;
            _ = async {
                match idle_activity {
                    Some(ref activity) => activity.notify.notified().await,
                    None => std::future::pending::<()>().await,
                }
            }, if idle_timeout_ms.is_some() => {
                reset_idle_deadline(
                    idle_timeout_ms,
                    idle_activity.as_ref(),
                    &mut idle_sleep_pin,
                    &mut idle_deadline_epoch,
                );
                continue;
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let idle_activity = idle_activity.clone();
                        let sf = stream_file.clone();
                        let cn = close_notify.clone();
                        tokio::spawn(async move {
                            handle_connection(
                                stream,
                                state,
                                idle_activity,
                                sf,
                                cn,
                            ).await;
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
                let Some(deadline_epoch) = idle_deadline_epoch else {
                    continue;
                };
                let Some(ref activity) = idle_activity else {
                    continue;
                };
                if !activity.try_claim_shutdown(deadline_epoch) {
                    reset_idle_deadline(
                        idle_timeout_ms,
                        idle_activity.as_ref(),
                        &mut idle_sleep_pin,
                        &mut idle_deadline_epoch,
                    );
                    continue;
                }
                let mut s = state.lock().await;
                save_and_close_for_shutdown(&mut s).await;
                break;
            }
            _ = close_notify.notified() => {
                // "close" command was handled; browser already closed by
                // handle_close(). Break to run cleanup and exit gracefully
                // so destructors fire.
                break;
            }
            _ = shutdown_signal() => {
                let mut s = state.lock().await;
                save_and_close_for_shutdown(&mut s).await;
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

    let idle_activity = idle_timeout_ms.map(|_| Arc::new(IdleActivity::new()));

    let close_notify = Arc::new(Notify::new());

    let mut idle_sleep_pin = None;
    let mut idle_deadline_epoch = None;
    reset_idle_deadline(
        idle_timeout_ms,
        idle_activity.as_ref(),
        &mut idle_sleep_pin,
        &mut idle_deadline_epoch,
    );

    // Mirror the unix loop's background tick: reap a browser the user closed
    // by hand, and drain CDP events (dialog state in particular) before
    // autosave so a save never runs against a dialog-blocked renderer.
    let mut drain_interval = tokio::time::interval(Duration::from_millis(100));
    drain_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = async {
                match idle_activity {
                    Some(ref activity) => activity.notify.notified().await,
                    None => std::future::pending::<()>().await,
                }
            }, if idle_timeout_ms.is_some() => {
                reset_idle_deadline(
                    idle_timeout_ms,
                    idle_activity.as_ref(),
                    &mut idle_sleep_pin,
                    &mut idle_deadline_epoch,
                );
                continue;
            }
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let idle_activity = idle_activity.clone();
                        let sf = stream_file.clone();
                        let cn = close_notify.clone();
                        tokio::spawn(async move {
                            handle_connection(
                                stream,
                                state,
                                idle_activity,
                                sf,
                                cn,
                            ).await;
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
                let Some(deadline_epoch) = idle_deadline_epoch else {
                    continue;
                };
                let Some(ref activity) = idle_activity else {
                    continue;
                };
                if !activity.try_claim_shutdown(deadline_epoch) {
                    reset_idle_deadline(
                        idle_timeout_ms,
                        idle_activity.as_ref(),
                        &mut idle_sleep_pin,
                        &mut idle_deadline_epoch,
                    );
                    continue;
                }
                let mut s = state.lock().await;
                save_and_close_for_shutdown(&mut s).await;
                let _ = fs::remove_file(&port_path);
                break;
            }
            _ = close_notify.notified() => {
                let _ = fs::remove_file(&port_path);
                break;
            }
            _ = shutdown_signal() => {
                let mut s = state.lock().await;
                save_and_close_for_shutdown(&mut s).await;
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
    idle_activity: Option<Arc<IdleActivity>>,
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

                let Some(command_lease) = CommandLease::acquire(idle_activity.as_ref()) else {
                    // The idle deadline already claimed shutdown. Closing the
                    // stream makes the client retry after cleanup completes.
                    break;
                };

                let action = cmd
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                let response = {
                    let mut s = state.lock().await;
                    execute_command(&cmd, &mut s).await
                };

                let mut resp = serde_json::to_string(&response).unwrap_or_default();
                resp.push('\n');
                drop(command_lease);
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

fn looks_like_http(line: &str) -> bool {
    let prefixes = [
        "GET ", "POST ", "PUT ", "DELETE ", "PATCH ", "HEAD ", "OPTIONS ", "CONNECT ", "TRACE ",
    ];
    prefixes.iter().any(|p| line.starts_with(p))
}

fn close_completed_response(action: &str, response: &Value) -> bool {
    if !matches!(
        action,
        "close" | "confirm" | INTERNAL_DAEMON_SHUTDOWN_ACTION
    ) {
        return false;
    }

    fn data_closed(data: &Value) -> bool {
        data.get("closed").and_then(|v| v.as_bool()) == Some(true)
    }

    if response.get("success").and_then(|v| v.as_bool()) != Some(true) {
        return false;
    }

    let Some(data) = response.get("data") else {
        return false;
    };
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
    fn test_idle_timeout_defaults_to_four_hours_and_zero_disables() {
        let guard = crate::test_utils::EnvGuard::new(&["AGENT_BROWSER_IDLE_TIMEOUT_MS"]);

        guard.remove("AGENT_BROWSER_IDLE_TIMEOUT_MS");
        assert_eq!(idle_timeout_ms_from_env(), Some(DEFAULT_IDLE_TIMEOUT_MS));

        guard.set("AGENT_BROWSER_IDLE_TIMEOUT_MS", "0");
        assert_eq!(idle_timeout_ms_from_env(), None);

        guard.set("AGENT_BROWSER_IDLE_TIMEOUT_MS", "2500");
        assert_eq!(idle_timeout_ms_from_env(), Some(2500));

        // A malformed direct daemon environment must not silently disable the
        // safety net. The CLI parser still reports invalid user input.
        guard.set("AGENT_BROWSER_IDLE_TIMEOUT_MS", "invalid");
        assert_eq!(idle_timeout_ms_from_env(), Some(DEFAULT_IDLE_TIMEOUT_MS));
    }

    #[tokio::test]
    async fn test_command_lease_tracks_complete_command_lifetime() {
        let activity = Arc::new(IdleActivity::new());

        let lease = CommandLease::acquire(Some(&activity)).expect("lease should be acquired");
        activity.notify.notified().await;
        let active = activity.snapshot();
        assert_eq!(active.active_leases, 1);
        assert_eq!(active.epoch, 1);

        drop(lease);
        activity.notify.notified().await;
        let completed = activity.snapshot();
        assert_eq!(completed.active_leases, 0);
        assert_eq!(completed.epoch, 2);
    }

    #[tokio::test]
    async fn test_handle_connection_releases_lease_before_response_backpressure() {
        use tokio::io::AsyncReadExt;

        // A one-byte transport buffer keeps write_all backpressured after the
        // serialized response releases its activity lease.
        let (mut client, server) = tokio::io::duplex(1);
        let state = Arc::new(tokio::sync::Mutex::new(DaemonState::new()));
        let queued_state_guard = state.lock().await;
        let activity = Arc::new(IdleActivity::new());
        let close_notify = Arc::new(Notify::new());

        let handler = tokio::spawn(handle_connection(
            server,
            state.clone(),
            Some(activity.clone()),
            None,
            close_notify,
        ));

        client
            .write_all(b"{\"id\":\"lease-test\",\"action\":\"session_info\"}\n")
            .await
            .expect("command should reach the daemon");
        tokio::time::timeout(Duration::from_secs(1), activity.notify.notified())
            .await
            .expect("command should acquire a lease");
        assert_eq!(
            activity.snapshot().active_leases,
            1,
            "a command queued on daemon state must remain active"
        );

        drop(queued_state_guard);

        tokio::time::timeout(Duration::from_secs(1), activity.notify.notified())
            .await
            .expect("serialized response should release the command lease");
        assert_eq!(
            activity.snapshot().active_leases,
            0,
            "a client that stops reading must not keep the daemon active"
        );

        let mut first_response_byte = [0_u8; 1];
        tokio::time::timeout(
            Duration::from_secs(1),
            client.read_exact(&mut first_response_byte),
        )
        .await
        .expect("handler should begin writing its response")
        .expect("response byte should be readable");
        assert_eq!(first_response_byte[0], b'{');

        let mut response = first_response_byte.to_vec();
        loop {
            let byte = tokio::time::timeout(Duration::from_secs(1), client.read_u8())
                .await
                .expect("response write should make progress")
                .expect("response should end with a newline");
            response.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        let response: Value = serde_json::from_slice(&response).expect("valid JSON response");
        assert_eq!(response["success"], true);

        drop(client);
        tokio::time::timeout(Duration::from_secs(1), handler)
            .await
            .expect("handler should exit after EOF")
            .expect("handler task should not panic");
    }

    #[test]
    fn test_command_completion_invalidates_an_expired_idle_deadline() {
        let activity = Arc::new(IdleActivity::new());
        let expired_deadline_epoch = activity.snapshot().epoch;

        // Model a deadline branch that expired, then waited for daemon state
        // while a queued command acquired the state lock and completed.
        let lease = CommandLease::acquire(Some(&activity)).expect("lease should be acquired");
        drop(lease);
        assert!(
            !activity.try_claim_shutdown(expired_deadline_epoch),
            "activity that completes while shutdown waits must invalidate the old deadline"
        );

        let refreshed_deadline_epoch = activity.snapshot().epoch;
        assert_ne!(refreshed_deadline_epoch, expired_deadline_epoch);
        assert!(activity.try_claim_shutdown(refreshed_deadline_epoch));
        assert!(
            CommandLease::acquire(Some(&activity)).is_none(),
            "commands arriving after shutdown is claimed must retry on a new daemon"
        );
    }

    #[tokio::test]
    async fn test_idle_deadline_starts_after_all_queued_commands_complete() {
        let activity = Arc::new(IdleActivity::new());
        let first = CommandLease::acquire(Some(&activity)).expect("first lease");
        let second = CommandLease::acquire(Some(&activity)).expect("second lease");
        let mut idle_sleep = None;
        let mut deadline_epoch = None;

        reset_idle_deadline(
            Some(1_000),
            Some(&activity),
            &mut idle_sleep,
            &mut deadline_epoch,
        );
        assert!(idle_sleep.is_none());
        assert!(deadline_epoch.is_none());

        drop(first);
        reset_idle_deadline(
            Some(1_000),
            Some(&activity),
            &mut idle_sleep,
            &mut deadline_epoch,
        );
        assert!(idle_sleep.is_none());
        assert!(deadline_epoch.is_none());

        drop(second);
        reset_idle_deadline(
            Some(1_000),
            Some(&activity),
            &mut idle_sleep,
            &mut deadline_epoch,
        );
        assert!(idle_sleep.is_some());
        assert_eq!(deadline_epoch, Some(activity.snapshot().epoch));
    }

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

    /// Regression test for #1101 using the production idle activity and
    /// deadline helpers. Maintenance ticks must not reset the deadline, while
    /// completing a command must start a fresh full idle window.
    #[tokio::test]
    async fn test_idle_timeout_fires_despite_drain_interval() {
        let idle_timeout_ms: u64 = 200;
        let command_delay = Duration::from_millis(75);
        let activity = Arc::new(IdleActivity::new());
        let mut drain_interval = tokio::time::interval(Duration::from_millis(10));
        drain_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let start = tokio::time::Instant::now();
        let mut command_sleep = Box::pin(tokio::time::sleep(command_delay));
        let mut command_completed = false;

        let mut idle_sleep_pin = None;
        let mut idle_deadline_epoch = None;
        reset_idle_deadline(
            Some(idle_timeout_ms),
            Some(&activity),
            &mut idle_sleep_pin,
            &mut idle_deadline_epoch,
        );
        let mut drain_ticks = 0;

        let exited = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                tokio::select! {
                    biased;
                    _ = activity.notify.notified() => {
                        reset_idle_deadline(
                            Some(idle_timeout_ms),
                            Some(&activity),
                            &mut idle_sleep_pin,
                            &mut idle_deadline_epoch,
                        );
                    }
                    _ = command_sleep.as_mut(), if !command_completed => {
                        let lease = CommandLease::acquire(Some(&activity))
                            .expect("command should acquire a lease");
                        drop(lease);
                        command_completed = true;
                    }
                    _ = drain_interval.tick() => {
                        drain_ticks += 1;
                    }
                    _ = async {
                        match idle_sleep_pin {
                            Some(ref mut s) => s.as_mut().await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        let deadline_epoch = idle_deadline_epoch
                            .expect("an armed deadline must have an epoch");
                        if activity.try_claim_shutdown(deadline_epoch) {
                            break;
                        }
                        reset_idle_deadline(
                            Some(idle_timeout_ms),
                            Some(&activity),
                            &mut idle_sleep_pin,
                            &mut idle_deadline_epoch,
                        );
                    }
                }
            }
        })
        .await;

        let elapsed = start.elapsed();

        assert!(
            exited.is_ok(),
            "idle timeout never fired with periodic maintenance ticks"
        );
        assert!(
            drain_ticks >= 10,
            "maintenance ticks should run repeatedly without resetting the deadline"
        );
        assert!(command_completed, "test command should have completed");
        assert!(
            elapsed >= command_delay + Duration::from_millis(idle_timeout_ms - 25),
            "command completion did not restart the full idle window: {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(idle_timeout_ms + 500),
            "maintenance ticks appear to have delayed the idle timeout: {:?} (timeout {} ms)",
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
