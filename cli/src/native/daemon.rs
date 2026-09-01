use fs2::FileExt;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::signal;
use tokio::sync::{watch, Notify, RwLock};

use super::actions::{
    close_current_browser, close_for_lifecycle, execute_command, maybe_autosave_restore_state,
    DaemonState,
};
use super::cdp::client::CdpClient;
use super::policy::{ActionPolicy, ConfirmActions, PolicyResult};
use super::state;
use super::stream::{IdleActivity, StreamServer};
use crate::connection::INTERNAL_DAEMON_SHUTDOWN_ACTION;

pub async fn run_daemon(session: &str) {
    let socket_dir = get_daemon_socket_dir();
    if !socket_dir.exists() {
        let _ = fs::create_dir_all(&socket_dir);
    }

    // Claim ownership before creating or removing any session sidecar. The
    // advisory lock, rather than the diagnostic PID in its contents, remains
    // held for this daemon's entire lifetime so a reused PID cannot strand a
    // stale endpoint or let another daemon replace a saturated live one.
    let owner_lock = match acquire_daemon_owner_lock(session) {
        Ok(Some(file)) => file,
        Ok(None) => return,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "Failed to acquire daemon ownership: {}",
                error
            );
            return;
        }
    };
    // Keep the locked marker handle open until final sidecar cleanup. Its PID
    // is visible before endpoint binding for diagnostics, but lock ownership
    // is the only liveness authority.
    let _owner_lock = owner_lock;

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
    let idle_activity = Arc::new(IdleActivity::new());
    let preferred_port = env::var("AGENT_BROWSER_STREAM_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    match StreamServer::start_without_client(
        preferred_port,
        session.to_string(),
        true,
        idle_activity.clone(),
    )
    .await
    {
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

    // Auto-shutdown the daemon after this many ms of inactivity (no commands
    // or dashboard input received). Applies a default when
    // AGENT_BROWSER_IDLE_TIMEOUT_MS is unset; an explicit 0 disables idle
    // shutdown entirely.
    let idle_timeout = resolve_idle_timeout(env::var("AGENT_BROWSER_IDLE_TIMEOUT_MS").ok());

    let autosave_interval_ms = autosave_interval_ms_from_env();

    let result = run_socket_server(
        &socket_path,
        session,
        stream_client,
        stream_server_instance,
        idle_activity,
        idle_timeout,
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
    let _ = fs::remove_file(socket_dir.join(format!("{}.config", session)));
    // All endpoints and owned resources are gone while `_owner_lock` still
    // protects this inode, so removing the marker here cannot split ownership
    // with a live daemon. A subsequent daemon creates and locks a fresh one.
    let _ = fs::remove_file(crate::connection::get_owner_lock_path(session));

    if let Err(e) = result {
        let _ = writeln!(std::io::stderr(), "Daemon error: {}", e);
        process::exit(1);
    }
}

fn acquire_daemon_owner_lock(session: &str) -> Result<Option<fs::File>, String> {
    use std::fs::OpenOptions;

    let path = crate::connection::get_owner_lock_path(session);
    let (mut file, already_existed) = match OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => (file, false),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|error| error.to_string())?,
            true,
        ),
        Err(error) => return Err(error.to_string()),
    };
    match file.try_lock_exclusive() {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(()) => {}
    }
    if already_existed {
        // `create_new` and `flock` are separate system calls. An empty marker
        // that has just appeared belongs to a racing creator which has not
        // yet locked it. Do not steal that startup window.
        let marker_is_empty = fs::read_to_string(&path)
            .map(|contents| contents.trim().is_empty())
            .unwrap_or(false);
        let recently_created = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age < Duration::from_secs(5));
        if marker_is_empty && recently_created {
            let _ = FileExt::unlock(&file);
            return Ok(None);
        }
    }
    // An unlocked PID marker always belongs to a crashed former owner. PID
    // liveness is deliberately ignored because the OS can reuse it for an
    // unrelated process before a client reaches this recovery path.
    file.set_len(0).map_err(|error| error.to_string())?;
    file.write_all(process::id().to_string().as_bytes())
        .map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())?;
    Ok(Some(file))
}

/// Idle timeout applied when AGENT_BROWSER_IDLE_TIMEOUT_MS is unset, so an
/// integration that dies without calling `close` cannot leak the daemon and
/// its Chrome tree indefinitely (issue: leaked daemons observed running for
/// days). Socket commands and dashboard input reset the timer. Unlike an
/// explicit timeout, the default never closes a headed browser (including
/// Safari and iOS WebDriver sessions) or a user-attached browser because those
/// may be in direct human use that the daemon cannot observe. Provider-owned
/// CDP browsers remain eligible for cleanup.
pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 60 * 60 * 1000;

#[derive(Clone, Copy)]
struct IdleTimeout {
    ms: u64,
    /// True when the value came from DEFAULT_IDLE_TIMEOUT_MS rather than an
    /// explicit AGENT_BROWSER_IDLE_TIMEOUT_MS. Only the default exempts
    /// headed and user-attached browsers from shutdown.
    is_default: bool,
}

/// One daemon owns one mutable browser state. Normal commands are admitted
/// without waiting, which preserves the established tab/frame serialization
/// invariant while preventing a stuck command from filling the socket and
/// mutex queues. Lifecycle work owns a separate priority path and cancels the
/// normal future before taking the state.
struct DaemonCoordinator {
    state: Arc<tokio::sync::Mutex<DaemonState>>,
    closing: AtomicBool,
    active: StdMutex<Option<ActiveCommand>>,
    cancel_tx: watch::Sender<bool>,
    lifecycle_gate: StdMutex<LifecycleGate>,
    terminal_response: StdMutex<Option<Value>>,
    lifecycle_finished: Notify,
}

#[derive(Clone)]
struct ActiveCommand {
    action: String,
    started: Instant,
}

struct LifecycleGate {
    policy: Option<ActionPolicy>,
    confirm_actions: Option<ConfirmActions>,
    pending_close_confirmation: Option<String>,
}

enum LifecycleAdmission {
    Normal,
    Response(Value),
    Close { confirmed: bool },
}

impl LifecycleGate {
    fn new() -> Self {
        Self {
            policy: ActionPolicy::load_if_exists(),
            confirm_actions: ConfirmActions::from_env(),
            pending_close_confirmation: None,
        }
    }

    fn confirmation_response(id: &str) -> Value {
        serde_json::json!({
            "id": id,
            "success": true,
            "data": {
                "confirmation_required": true,
                "confirmation_id": id,
                "action": "close"
            }
        })
    }

    fn admit(&mut self, cmd: &Value) -> LifecycleAdmission {
        let action = cmd
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let id = cmd.get("id").and_then(Value::as_str).unwrap_or_default();

        if action == INTERNAL_DAEMON_SHUTDOWN_ACTION {
            return LifecycleAdmission::Close { confirmed: false };
        }

        if action == "confirm" || action == "deny" {
            let confirmation_id = cmd
                .get("confirmationId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if self.pending_close_confirmation.as_deref() != Some(confirmation_id) {
                return LifecycleAdmission::Normal;
            }
            self.pending_close_confirmation = None;
            return if action == "confirm" {
                // The policy is deliberately checked again at confirmation
                // time. A hot reload that denies close must not be bypassed
                // merely because an older policy previously requested a
                // confirmation.
                if let Some(policy) = self.policy.as_mut() {
                    let _ = policy.reload();
                    if let PolicyResult::Deny(reason) = policy.check("close") {
                        return LifecycleAdmission::Response(serde_json::json!({
                            "id": id,
                            "success": false,
                            "error": format!("Action 'close' denied by policy: {}", reason)
                        }));
                    }
                }
                LifecycleAdmission::Close { confirmed: true }
            } else {
                LifecycleAdmission::Response(serde_json::json!({
                    "id": id,
                    "success": true,
                    "data": { "denied": true, "action": "close" }
                }))
            };
        }

        if action != "close" {
            return LifecycleAdmission::Normal;
        }

        if let Some(policy) = self.policy.as_mut() {
            let _ = policy.reload();
            match policy.check("close") {
                PolicyResult::Allow => {}
                PolicyResult::Deny(reason) => {
                    return LifecycleAdmission::Response(serde_json::json!({
                        "id": id,
                        "success": false,
                        "error": format!("Action 'close' denied by policy: {}", reason)
                    }));
                }
                PolicyResult::RequiresConfirmation => {
                    self.pending_close_confirmation = Some(id.to_string());
                    return LifecycleAdmission::Response(Self::confirmation_response(id));
                }
            }
        }
        if self
            .confirm_actions
            .as_ref()
            .is_some_and(|actions| actions.requires_confirmation("close"))
        {
            self.pending_close_confirmation = Some(id.to_string());
            return LifecycleAdmission::Response(Self::confirmation_response(id));
        }
        LifecycleAdmission::Close { confirmed: false }
    }
}

impl DaemonCoordinator {
    fn new(state: DaemonState) -> Arc<Self> {
        let (cancel_tx, _) = watch::channel(false);
        Arc::new(Self {
            state: Arc::new(tokio::sync::Mutex::new(state)),
            closing: AtomicBool::new(false),
            active: StdMutex::new(None),
            cancel_tx,
            lifecycle_gate: StdMutex::new(LifecycleGate::new()),
            terminal_response: StdMutex::new(None),
            lifecycle_finished: Notify::new(),
        })
    }

    fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    fn begin_closing(&self) -> bool {
        let won = self
            .closing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok();
        if won {
            let _ = self.cancel_tx.send(true);
        }
        won
    }

    fn active_data(&self) -> Value {
        let active = self.active.lock().expect("active command mutex poisoned");
        match active.as_ref() {
            Some(command) => serde_json::json!({
                "activeAction": command.action,
                "elapsedMs": command.started.elapsed().as_millis() as u64,
            }),
            None => serde_json::json!({}),
        }
    }

    fn busy_response(&self, id: &str) -> Value {
        serde_json::json!({
            "id": id,
            "success": false,
            "code": "session_busy",
            "error": "Session is busy running another command; retry shortly or use close to cancel it.",
            "data": self.active_data(),
        })
    }

    fn closing_response(id: &str) -> Value {
        serde_json::json!({
            "id": id,
            "success": false,
            "code": "session_closing",
            "error": "Session is closing; no new commands can be started.",
        })
    }

    fn cancelled_response(id: &str) -> Value {
        serde_json::json!({
            "id": id,
            "success": false,
            "code": "operation_cancelled",
            "error": "Operation was cancelled because the session is closing.",
        })
    }

    fn with_response_id(mut response: Value, id: &str) -> Value {
        response["id"] = Value::String(id.to_string());
        response
    }

    async fn cancelled(cancel_rx: &mut watch::Receiver<bool>) {
        if !*cancel_rx.borrow() {
            let _ = cancel_rx.changed().await;
        }
    }

    fn clear_active(&self) {
        let mut active = self.active.lock().expect("active command mutex poisoned");
        *active = None;
    }

    async fn execute(self: &Arc<Self>, cmd: &Value, idle_activity: &IdleActivity) -> Value {
        let id = cmd.get("id").and_then(Value::as_str).unwrap_or_default();
        let action = cmd
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if self.is_closing() {
            if matches!(
                action,
                "close" | "confirm" | INTERNAL_DAEMON_SHUTDOWN_ACTION
            ) {
                return self.await_terminal_response(id).await;
            }
            return Self::closing_response(id);
        }

        let lifecycle = self
            .lifecycle_gate
            .lock()
            .expect("lifecycle gate mutex poisoned")
            .admit(cmd);
        match lifecycle {
            LifecycleAdmission::Response(response) => return response,
            LifecycleAdmission::Close { confirmed } => return self.close(id, confirmed).await,
            LifecycleAdmission::Normal => {}
        }

        let Ok(mut state) = self.state.clone().try_lock_owned() else {
            return self.busy_response(id);
        };
        if self.is_closing() {
            return Self::closing_response(id);
        }

        {
            let mut active = self.active.lock().expect("active command mutex poisoned");
            *active = Some(ActiveCommand {
                action: action.to_string(),
                started: Instant::now(),
            });
        }
        let mut cancel_rx = self.cancel_tx.subscribe();
        let response = tokio::select! {
            response = execute_command(cmd, &mut state) => response,
            _ = Self::cancelled(&mut cancel_rx) => Self::cancelled_response(id),
        };
        self.clear_active();
        idle_activity.mark();
        response
    }

    async fn await_terminal_response(&self, id: &str) -> Value {
        if let Some(response) = self
            .terminal_response
            .lock()
            .expect("terminal response mutex poisoned")
            .clone()
        {
            return Self::with_response_id(response, id);
        }
        let _ =
            tokio::time::timeout(Duration::from_secs(7), self.lifecycle_finished.notified()).await;
        self.terminal_response
            .lock()
            .expect("terminal response mutex poisoned")
            .clone()
            .map(|response| Self::with_response_id(response, id))
            .unwrap_or_else(|| Self::closing_response(id))
    }

    async fn close(self: &Arc<Self>, id: &str, confirmed: bool) -> Value {
        if !self.begin_closing() {
            return self.await_terminal_response(id).await;
        }
        let mut state = self.state.lock().await;
        // Cancellation causes the command task to drop its state guard before
        // this priority lane can acquire it. Clear metadata here as well so a
        // close response never reports a command that can no longer run.
        self.clear_active();
        let response = match close_for_lifecycle(&mut state).await {
            Ok(data) => serde_json::json!({ "id": id, "success": true, "data": data }),
            Err(error) => serde_json::json!({ "id": id, "success": false, "error": error }),
        };
        *self
            .terminal_response
            .lock()
            .expect("terminal response mutex poisoned") = Some(response.clone());
        self.lifecycle_finished.notify_waiters();
        if confirmed {
            serde_json::json!({
                "id": id,
                "success": true,
                "data": { "confirmed": true, "action": "close", "result": response }
            })
        } else {
            response
        }
    }

    /// Maintenance never waits for an active command. If it wins the state it
    /// still listens for lifecycle cancellation so a stuck background CDP
    /// drain cannot delay shutdown.
    async fn maintenance(self: &Arc<Self>, autosave_interval_ms: u64) {
        if self.is_closing() {
            return;
        }
        let Ok(mut state) = self.state.clone().try_lock_owned() else {
            return;
        };
        if self.is_closing() {
            return;
        }
        let mut cancel_rx = self.cancel_tx.subscribe();
        tokio::select! {
            _ = Self::cancelled(&mut cancel_rx) => {}
            _ = async {
                let process_exited = state.browser.as_mut().map(|mgr| mgr.has_process_exited()).unwrap_or(false);
                if process_exited {
                    let _ = close_current_browser(&mut state).await;
                } else if state.browser.is_some() {
                    if let Err(error) = state.drain_cdp_events_background().await {
                        let _ = writeln!(std::io::stderr(), "Failed to apply browser network controls: {}", error);
                    } else {
                        maybe_autosave_restore_state(&mut state, autosave_interval_ms).await;
                    }
                }
            } => {}
        }
    }

    async fn idle_shutdown_with_state(
        self: &Arc<Self>,
        mut state: tokio::sync::OwnedMutexGuard<DaemonState>,
        default_timeout: bool,
    ) -> bool {
        if default_timeout && state.blocks_default_idle_shutdown() {
            return false;
        }
        if !self.begin_closing() {
            return false;
        }
        let _ = close_for_lifecycle(&mut state).await;
        true
    }
}

/// Resolve AGENT_BROWSER_IDLE_TIMEOUT_MS into an effective idle timeout:
/// unset or unparseable → the default; explicit 0 → disabled (None);
/// any other value → that many milliseconds.
fn resolve_idle_timeout(raw: Option<String>) -> Option<IdleTimeout> {
    match raw.as_deref().map(str::trim).map(str::parse::<u64>) {
        Some(Ok(0)) => None,
        Some(Ok(ms)) => Some(IdleTimeout {
            ms,
            is_default: false,
        }),
        // Unparseable values are validated (with a warning) at the flags
        // layer; falling back to the default here keeps the leak backstop
        // in place rather than silently disabling it.
        Some(Err(_)) | None => Some(IdleTimeout {
            ms: DEFAULT_IDLE_TIMEOUT_MS,
            is_default: true,
        }),
    }
}

fn remaining_idle_timeout(activity: &IdleActivity, timeout_ms: u64) -> Option<Duration> {
    Duration::from_millis(timeout_ms).checked_sub(activity.elapsed())
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
    idle_activity: Arc<IdleActivity>,
    idle_timeout: Option<IdleTimeout>,
    autosave_interval_ms: u64,
) -> Result<(), String> {
    use tokio::net::UnixListener;

    let idle_timeout_ms = idle_timeout.map(|t| t.ms);

    let listener =
        UnixListener::bind(socket_path).map_err(|e| format!("Failed to bind socket: {}", e))?;

    let stream_file: Option<PathBuf> = if stream_server.is_some() {
        let dir = socket_path.parent().unwrap_or(std::path::Path::new("."));
        Some(dir.join(format!("{}.stream", session)))
    } else {
        None
    };
    let coordinator = DaemonCoordinator::new(DaemonState::new_with_stream(
        stream_client,
        stream_server,
        idle_activity.clone(),
    ));

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
                        let coordinator = coordinator.clone();
                        let idle_activity = idle_activity.clone();
                        let sf = stream_file.clone();
                        let cn = close_notify.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, coordinator, idle_activity, sf, cn).await;
                        });
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "Accept error: {}", e);
                    }
                }
            }
            _ = drain_interval.tick() => {
                // A maintenance CDP call may be blocked by the renderer.
                // Run it outside the listener select so accepting a priority
                // close or a bounded busy response is never delayed by it.
                let coordinator = coordinator.clone();
                tokio::spawn(async move {
                    coordinator.maintenance(autosave_interval_ms).await;
                });
            }
            _ = async {
                match idle_sleep_pin {
                    Some(ref mut s) => s.as_mut().await,
                    None => std::future::pending::<()>().await,
                }
            }, if idle_timeout_ms.is_some() => {
                // Idle shutdown only claims the lane when it is already idle;
                // it never queues behind or cancels a live browser command.
                let Ok(s) = coordinator.state.clone().try_lock_owned() else {
                    idle_sleep_pin = idle_timeout_ms.map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                    continue;
                };
                // The timer may have expired while a command held the state
                // lane. Re-check activity after nonblocking admission.
                if let Some(remaining) =
                    remaining_idle_timeout(&idle_activity, idle_timeout_ms.unwrap_or_default())
                {
                    idle_sleep_pin = Some(Box::pin(tokio::time::sleep(remaining)));
                    continue;
                }
                // The default timeout is a leak backstop, not a lifecycle
                // policy: never pull a headed, WebDriver, or attached browser
                // out from under a human. Re-arm and keep waiting instead.
                if idle_timeout.is_some_and(|t| t.is_default) && s.blocks_default_idle_shutdown() {
                    idle_sleep_pin = idle_timeout_ms
                        .map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                    continue;
                }
                if idle_timeout.is_some_and(|t| t.is_default) {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Idle for {}m with no commands or dashboard input; saving configured restore state and shutting down (AGENT_BROWSER_IDLE_TIMEOUT_MS=0 disables)",
                        DEFAULT_IDLE_TIMEOUT_MS / 60_000
                    );
                }
                if coordinator.idle_shutdown_with_state(s, idle_timeout.is_some_and(|t| t.is_default)).await {
                    break;
                }
                idle_sleep_pin = idle_timeout_ms.map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                continue;
            }
            _ = idle_activity.notified(), if idle_timeout_ms.is_some() => {
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
                let _ = coordinator.close("signal-shutdown", false).await;
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
    idle_activity: Arc<IdleActivity>,
    idle_timeout: Option<IdleTimeout>,
    autosave_interval_ms: u64,
) -> Result<(), String> {
    use tokio::net::TcpListener;

    let idle_timeout_ms = idle_timeout.map(|t| t.ms);

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
    let coordinator = DaemonCoordinator::new(DaemonState::new_with_stream(
        stream_client,
        stream_server,
        idle_activity.clone(),
    ));

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
                        let coordinator = coordinator.clone();
                        let idle_activity = idle_activity.clone();
                        let sf = stream_file.clone();
                        let cn = close_notify.clone();
                        tokio::spawn(async move {
                            handle_connection(stream, coordinator, idle_activity, sf, cn).await;
                        });
                    }
                    Err(e) => {
                        let _ = writeln!(std::io::stderr(), "Accept error: {}", e);
                    }
                }
            }
            _ = drain_interval.tick() => {
                let coordinator = coordinator.clone();
                tokio::spawn(async move {
                    coordinator.maintenance(autosave_interval_ms).await;
                });
            }
            _ = async {
                match idle_sleep_pin {
                    Some(ref mut s) => s.as_mut().await,
                    None => std::future::pending::<()>().await,
                }
            }, if idle_timeout_ms.is_some() => {
                let Ok(s) = coordinator.state.clone().try_lock_owned() else {
                    idle_sleep_pin = idle_timeout_ms.map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                    continue;
                };
                if let Some(remaining) =
                    remaining_idle_timeout(&idle_activity, idle_timeout_ms.unwrap_or_default())
                {
                    idle_sleep_pin = Some(Box::pin(tokio::time::sleep(remaining)));
                    continue;
                }
                // The default timeout is a leak backstop, not a lifecycle
                // policy: never pull a headed, WebDriver, or attached browser
                // out from under a human. Re-arm and keep waiting instead.
                if idle_timeout.is_some_and(|t| t.is_default) && s.blocks_default_idle_shutdown() {
                    idle_sleep_pin = idle_timeout_ms
                        .map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                    continue;
                }
                if idle_timeout.is_some_and(|t| t.is_default) {
                    let _ = writeln!(
                        std::io::stderr(),
                        "Idle for {}m with no commands or dashboard input; saving configured restore state and shutting down (AGENT_BROWSER_IDLE_TIMEOUT_MS=0 disables)",
                        DEFAULT_IDLE_TIMEOUT_MS / 60_000
                    );
                }
                if coordinator.idle_shutdown_with_state(s, idle_timeout.is_some_and(|t| t.is_default)).await {
                    let _ = fs::remove_file(&port_path);
                    break;
                }
                idle_sleep_pin = idle_timeout_ms.map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                continue;
            }
            _ = idle_activity.notified(), if idle_timeout_ms.is_some() => {
                idle_sleep_pin = idle_timeout_ms
                    .map(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms))));
                continue;
            }
            _ = close_notify.notified() => {
                let _ = fs::remove_file(&port_path);
                break;
            }
            _ = shutdown_signal() => {
                let _ = coordinator.close("signal-shutdown", false).await;
                let _ = fs::remove_file(&port_path);
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection<S>(
    stream: S,
    coordinator: Arc<DaemonCoordinator>,
    idle_activity: Arc<IdleActivity>,
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

                idle_activity.mark();

                let action = cmd
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                let response = coordinator.execute(&cmd, &idle_activity).await;

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

    #[tokio::test]
    async fn coordinator_returns_busy_and_close_cancels_active_command() {
        let activity = Arc::new(IdleActivity::new());
        let coordinator = DaemonCoordinator::new(DaemonState::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();

        // Hold the state lane exactly like a renderer command, with the
        // coordinator's active metadata visible to a concurrent request.
        let holder = coordinator.clone();
        tokio::spawn(async move {
            let _state = holder.state.clone().lock_owned().await;
            *holder.active.lock().unwrap() = Some(ActiveCommand {
                action: "evaluate".to_string(),
                started: Instant::now(),
            });
            let mut cancellation = holder.cancel_tx.subscribe();
            let _ = started_tx.send(());
            tokio::select! {
                _ = release_rx => {}
                _ = DaemonCoordinator::cancelled(&mut cancellation) => {}
            }
            *holder.active.lock().unwrap() = None;
        });
        started_rx.await.unwrap();

        let busy = coordinator
            .execute(
                &serde_json::json!({"id":"busy", "action":"tab_list"}),
                &activity,
            )
            .await;
        assert_eq!(busy["code"], "session_busy");
        assert_eq!(busy["data"]["activeAction"], "evaluate");

        let close = tokio::time::timeout(
            Duration::from_secs(1),
            coordinator.execute(
                &serde_json::json!({"id":"close", "action":"close"}),
                &activity,
            ),
        )
        .await
        .expect("close must not wait for the active command");
        assert_eq!(close["success"], true);

        let after = coordinator
            .execute(
                &serde_json::json!({"id":"after", "action":"tab_list"}),
                &activity,
            )
            .await;
        assert_eq!(after["code"], "session_closing");
        let _ = release_tx.send(());
    }

    #[test]
    fn lifecycle_gate_keeps_close_policy_and_confirmation_before_cancellation() {
        let mut gate = LifecycleGate::new();
        gate.confirm_actions = Some(ConfirmActions {
            categories: ["close".to_string()].into_iter().collect(),
        });
        let pending = gate.admit(&serde_json::json!({"id":"close-1", "action":"close"}));
        assert!(matches!(pending, LifecycleAdmission::Response(_)));
        assert!(matches!(
            gate.admit(
                &serde_json::json!({"id":"wrong", "action":"confirm", "confirmationId":"wrong"})
            ),
            LifecycleAdmission::Normal
        ));
        assert!(matches!(
            gate.admit(
                &serde_json::json!({"id":"confirm", "action":"confirm", "confirmationId":"close-1"})
            ),
            LifecycleAdmission::Close { confirmed: true }
        ));
    }

    #[test]
    fn lifecycle_gate_rechecks_a_reloaded_close_policy() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join("policy.json");
        fs::write(&policy_path, r#"{"confirm":["close"]}"#).unwrap();
        let mut gate = LifecycleGate {
            policy: Some(ActionPolicy::load(policy_path.to_str().unwrap()).unwrap()),
            confirm_actions: None,
            pending_close_confirmation: None,
        };
        assert!(matches!(
            gate.admit(&serde_json::json!({"id":"close-1", "action":"close"})),
            LifecycleAdmission::Response(_)
        ));
        fs::write(&policy_path, r#"{"deny":["close"]}"#).unwrap();
        let response = match gate.admit(&serde_json::json!({
            "id":"confirm",
            "action":"confirm",
            "confirmationId":"close-1"
        })) {
            LifecycleAdmission::Response(response) => response,
            _ => panic!("reloaded denial must not enter the lifecycle lane"),
        };
        assert_eq!(response["success"], false);
    }

    #[test]
    fn test_resolve_idle_timeout_unset_applies_default() {
        let t = resolve_idle_timeout(None).expect("default should apply when unset");
        assert_eq!(t.ms, DEFAULT_IDLE_TIMEOUT_MS);
        assert!(t.is_default);
    }

    #[test]
    fn test_resolve_idle_timeout_explicit_zero_disables() {
        assert!(resolve_idle_timeout(Some("0".to_string())).is_none());
        assert!(resolve_idle_timeout(Some(" 0 ".to_string())).is_none());
    }

    #[test]
    fn test_resolve_idle_timeout_explicit_value_is_not_default() {
        let t = resolve_idle_timeout(Some("5000".to_string())).expect("explicit value");
        assert_eq!(t.ms, 5000);
        assert!(!t.is_default);
    }

    #[test]
    fn test_resolve_idle_timeout_unparseable_falls_back_to_default() {
        for raw in ["banana", "", "-1", "30s"] {
            let t = resolve_idle_timeout(Some(raw.to_string()))
                .unwrap_or_else(|| panic!("{:?} should fall back to default", raw));
            assert_eq!(t.ms, DEFAULT_IDLE_TIMEOUT_MS);
            assert!(t.is_default);
        }
    }

    #[test]
    fn test_default_idle_timeout_does_not_close_webdriver_sessions() {
        let mut state = DaemonState::new();
        assert!(!state.blocks_default_idle_shutdown());

        state.backend_type = crate::native::actions::BackendType::WebDriver;
        assert!(state.blocks_default_idle_shutdown());
    }

    #[tokio::test]
    async fn test_idle_activity_receives_dashboard_activity() {
        let activity = Arc::new(IdleActivity::new());
        activity.mark();

        tokio::time::timeout(Duration::from_millis(100), activity.notified())
            .await
            .expect("dashboard input notification should wake the idle loop");
    }

    #[tokio::test]
    async fn test_command_completion_rearms_expired_idle_timeout() {
        let activity = IdleActivity::new();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            remaining_idle_timeout(&activity, 1).is_none(),
            "the original idle deadline should have expired"
        );

        // A command that held the daemon state lock past the deadline marks
        // completion before releasing the lock. The timeout path must then
        // wait for a new full idle period instead of closing immediately.
        activity.mark();
        assert!(remaining_idle_timeout(&activity, 100).is_some());
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

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("child did not exit before the deadline");
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(e) => panic!("try_wait() should succeed without waitpid(-1): {}", e),
            }
        };

        assert_eq!(status.code(), Some(42));
    }

    /// Regression test for #1101: idle timeout must fire even while the
    /// drain interval ticks every 500 ms.  The bug was that `sleep_future`
    /// was created **inside** the loop, so each drain tick dropped the
    /// in-progress sleep and replaced it with a fresh one – the timer
    /// could never reach its deadline.
    #[tokio::test]
    async fn test_idle_timeout_fires_despite_drain_interval() {
        let idle_timeout_ms: u64 = 1000;
        let mut drain_interval = tokio::time::interval(Duration::from_millis(500));
        drain_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let activity = IdleActivity::new();

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
                    _ = activity.notified() => {
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
