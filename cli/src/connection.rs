use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::net::UnixStream;

#[derive(Serialize)]
#[allow(dead_code)]
pub struct Request {
    pub id: String,
    pub action: String,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Deserialize, Serialize, Default)]
pub struct Response {
    pub success: bool,
    pub data: Option<Value>,
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[allow(dead_code)]
pub enum Connection {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.read(buf),
            Connection::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.write(buf),
            Connection::Tcp(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.flush(),
            Connection::Tcp(s) => s.flush(),
        }
    }
}

impl Connection {
    pub fn set_read_timeout(&self, dur: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.set_read_timeout(dur),
            Connection::Tcp(s) => s.set_read_timeout(dur),
        }
    }

    pub fn set_write_timeout(&self, dur: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Connection::Unix(s) => s.set_write_timeout(dur),
            Connection::Tcp(s) => s.set_write_timeout(dur),
        }
    }
}

/// Get the base directory for socket/pid files.
/// Priority: AGENT_BROWSER_SOCKET_DIR > XDG_RUNTIME_DIR > ~/.agent-browser > tmpdir
pub fn get_socket_dir() -> PathBuf {
    // 1. Explicit override (ignore empty string)
    if let Ok(dir) = env::var("AGENT_BROWSER_SOCKET_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }

    // 2. XDG_RUNTIME_DIR (Linux standard, ignore empty string)
    if let Ok(runtime_dir) = env::var("XDG_RUNTIME_DIR") {
        if !runtime_dir.is_empty() {
            return PathBuf::from(runtime_dir).join("agent-browser");
        }
    }

    // 3. Home directory fallback (like Docker Desktop's ~/.docker/run/)
    if let Some(home) = dirs::home_dir() {
        return home.join(".agent-browser");
    }

    // 4. Last resort: temp dir
    env::temp_dir().join("agent-browser")
}

#[cfg(unix)]
fn get_socket_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.sock", session))
}

fn get_pid_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.pid", session))
}

fn get_version_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.version", session))
}

/// Clean up stale socket and PID files for a session
pub fn cleanup_stale_files(session: &str) {
    let pid_path = get_pid_path(session);
    let _ = fs::remove_file(&pid_path);
    let version_path = get_version_path(session);
    let _ = fs::remove_file(&version_path);
    let stream_path = get_socket_dir().join(format!("{}.stream", session));
    let _ = fs::remove_file(&stream_path);

    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        let _ = fs::remove_file(&socket_path);
    }

    #[cfg(windows)]
    {
        let port_path = get_port_path(session);
        let _ = fs::remove_file(&port_path);
    }
}

#[cfg(windows)]
fn get_port_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.port", session))
}

#[cfg(windows)]
pub fn get_port_for_session(session: &str) -> u16 {
    let mut hash: i32 = 0;
    for c in session.chars() {
        hash = ((hash << 5).wrapping_sub(hash)).wrapping_add(c as i32);
    }
    // Correct logic: first take absolute modulo, then cast to u16
    // Using unsigned_abs() to safely handle i32::MIN
    49152 + ((hash.unsigned_abs() as u32 % 16383) as u16)
}

/// Read the actual daemon port from the `.port` file written by the daemon.
/// Falls back to the hash-derived port if the file does not exist or is
/// unreadable (e.g. daemon has not started yet).
#[cfg(windows)]
pub fn resolve_port(session: &str) -> u16 {
    let port_path = get_port_path(session);
    fs::read_to_string(&port_path)
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
        .unwrap_or_else(|| get_port_for_session(session))
}

pub fn daemon_ready(session: &str) -> bool {
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        UnixStream::connect(&socket_path).is_ok()
    }
    #[cfg(windows)]
    {
        let port = resolve_port(session);
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_millis(50),
        )
        .is_ok()
    }
}

/// Result of ensure_daemon indicating whether a new daemon was started
pub struct DaemonResult {
    /// True if we connected to an existing daemon, false if we started a new one
    pub already_running: bool,
}

/// Options forwarded to the daemon process as environment variables.
/// Note: `confirm_interactive` is intentionally absent -- it is a CLI-side
/// UX concern (prompting the user on stdin) and not a daemon configuration.
/// The daemon only needs `confirm_actions` to gate action categories.
pub struct DaemonOptions<'a> {
    pub headed: bool,
    pub debug: bool,
    pub executable_path: Option<&'a str>,
    pub extensions: &'a [String],
    pub args: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub proxy: Option<&'a str>,
    pub proxy_bypass: Option<&'a str>,
    pub proxy_username: Option<&'a str>,
    pub proxy_password: Option<&'a str>,
    pub ignore_https_errors: bool,
    pub allow_file_access: bool,
    pub profile: Option<&'a str>,
    pub state: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub device: Option<&'a str>,
    pub session_name: Option<&'a str>,
    pub download_path: Option<&'a str>,
    pub allowed_domains: Option<&'a [String]>,
    pub action_policy: Option<&'a str>,
    pub confirm_actions: Option<&'a str>,
    pub engine: Option<&'a str>,
    pub auto_connect: bool,
    pub idle_timeout: Option<&'a str>,
    pub default_timeout: Option<u64>,
    pub cdp: Option<&'a str>,
    pub no_auto_dialog: bool,
}

fn apply_daemon_env(cmd: &mut Command, session: &str, opts: &DaemonOptions) {
    cmd.env("AGENT_BROWSER_DAEMON", "1")
        .env("AGENT_BROWSER_SESSION", session);

    if opts.headed {
        cmd.env("AGENT_BROWSER_HEADED", "1");
    }
    if opts.debug {
        cmd.env("AGENT_BROWSER_DEBUG", "1");
    }
    if let Some(path) = opts.executable_path {
        cmd.env("AGENT_BROWSER_EXECUTABLE_PATH", path);
    }
    if !opts.extensions.is_empty() {
        cmd.env("AGENT_BROWSER_EXTENSIONS", opts.extensions.join(","));
    }
    if let Some(a) = opts.args {
        cmd.env("AGENT_BROWSER_ARGS", a);
    }
    if let Some(ua) = opts.user_agent {
        cmd.env("AGENT_BROWSER_USER_AGENT", ua);
    }
    if let Some(p) = opts.proxy {
        cmd.env("AGENT_BROWSER_PROXY", p);
    }
    if let Some(pb) = opts.proxy_bypass {
        cmd.env("AGENT_BROWSER_PROXY_BYPASS", pb);
    }
    if let Some(pu) = opts.proxy_username {
        cmd.env("AGENT_BROWSER_PROXY_USERNAME", pu);
    }
    if let Some(pp) = opts.proxy_password {
        cmd.env("AGENT_BROWSER_PROXY_PASSWORD", pp);
    }
    if opts.ignore_https_errors {
        cmd.env("AGENT_BROWSER_IGNORE_HTTPS_ERRORS", "1");
    }
    if opts.allow_file_access {
        cmd.env("AGENT_BROWSER_ALLOW_FILE_ACCESS", "1");
    }
    if let Some(prof) = opts.profile {
        cmd.env("AGENT_BROWSER_PROFILE", prof);
    }
    if let Some(st) = opts.state {
        cmd.env("AGENT_BROWSER_STATE", st);
    }
    if let Some(p) = opts.provider {
        cmd.env("AGENT_BROWSER_PROVIDER", p);
    }
    if let Some(d) = opts.device {
        cmd.env("AGENT_BROWSER_IOS_DEVICE", d);
    }
    if let Some(sn) = opts.session_name {
        cmd.env("AGENT_BROWSER_SESSION_NAME", sn);
    }
    if let Some(dp) = opts.download_path {
        cmd.env("AGENT_BROWSER_DOWNLOAD_PATH", dp);
    }
    if let Some(ad) = opts.allowed_domains {
        cmd.env("AGENT_BROWSER_ALLOWED_DOMAINS", ad.join(","));
    }
    if let Some(ap) = opts.action_policy {
        cmd.env("AGENT_BROWSER_ACTION_POLICY", ap);
    }
    if let Some(ca) = opts.confirm_actions {
        cmd.env("AGENT_BROWSER_CONFIRM_ACTIONS", ca);
    }
    if let Some(engine) = opts.engine {
        cmd.env("AGENT_BROWSER_ENGINE", engine);
    }
    if opts.auto_connect {
        cmd.env("AGENT_BROWSER_AUTO_CONNECT", "1");
    }
    if let Some(idle) = opts.idle_timeout {
        cmd.env("AGENT_BROWSER_IDLE_TIMEOUT_MS", idle);
    }
    if let Some(timeout) = opts.default_timeout {
        cmd.env("AGENT_BROWSER_DEFAULT_TIMEOUT", timeout.to_string());
    }
    if let Some(cdp) = opts.cdp {
        cmd.env("AGENT_BROWSER_CDP", cdp);
    }
    if opts.no_auto_dialog {
        cmd.env("AGENT_BROWSER_NO_AUTO_DIALOG", "1");
    }
}

/// Check if the running daemon's version matches this CLI binary.
/// Returns false when the version file is missing — an unversioned daemon
/// is most likely a stale leftover from before version tracking was added
/// (or from the Node.js era), and silently reusing it is the exact bug
/// this check exists to prevent. The one-time cost of an unnecessary
/// restart on the first upgrade is preferable to silent failures.
fn daemon_version_matches(session: &str) -> bool {
    let version_path = get_version_path(session);
    match fs::read_to_string(&version_path) {
        Ok(v) => v.trim() == env!("CARGO_PKG_VERSION"),
        Err(_) => false,
    }
}

/// Kill a running daemon by reading its PID file and sending a kill signal.
fn kill_stale_daemon(session: &str) {
    // Remove the socket first so no new connections reach the old daemon
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        let _ = fs::remove_file(&socket_path);
    }

    let pid_path = get_pid_path(session);
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
                // Wait up to 1s for graceful shutdown, then force-kill
                for _ in 0..10 {
                    thread::sleep(Duration::from_millis(100));
                    if unsafe { libc::kill(pid as i32, 0) } != 0 {
                        break;
                    }
                }
                // Force-kill if still alive
                if unsafe { libc::kill(pid as i32, 0) } == 0 {
                    unsafe {
                        libc::kill(pid as i32, libc::SIGKILL);
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
                thread::sleep(Duration::from_millis(500));
            }
        }
    }

    // Clean up leftover files regardless
    cleanup_stale_files(session);
}

pub fn ensure_daemon(session: &str, opts: &DaemonOptions) -> Result<DaemonResult, String> {
    // Socket connectivity is the sole liveness check — no PID check — so
    // callers in a different PID namespace (e.g. unshare) can still reuse
    // an existing daemon they can reach over the socket.
    if daemon_ready(session) {
        // Double-check it's actually responsive by waiting and checking again
        // This handles the race condition where daemon is shutting down
        // (daemon has a 100ms shutdown delay, so we wait longer)
        thread::sleep(Duration::from_millis(150));
        if daemon_ready(session) {
            // Check version: if the running daemon is from a different CLI
            // version (e.g. after an upgrade), kill it and start a fresh one.
            if !daemon_version_matches(session) {
                eprintln!(
                    "{} Daemon version mismatch detected, restarting...",
                    crate::color::warning_indicator()
                );
                kill_stale_daemon(session);
                // Fall through to spawn a new daemon below
            } else {
                return Ok(DaemonResult {
                    already_running: true,
                });
            }
        }
    }

    // Clean up any stale socket/pid files before starting fresh
    cleanup_stale_files(session);

    // Ensure socket directory exists
    let socket_dir = get_socket_dir();
    if !socket_dir.exists() {
        fs::create_dir_all(&socket_dir)
            .map_err(|e| format!("Failed to create socket directory: {}", e))?;
    }

    // Pre-flight check: Validate socket path length (Unix limit is 104 bytes including null terminator)
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        let path_len = socket_path.as_os_str().len();
        if path_len > 103 {
            return Err(format!(
                "Session name '{}' is too long. Socket path would be {} bytes (max 103).\n\
                 Use a shorter session name or set AGENT_BROWSER_SOCKET_DIR to a shorter path.",
                session, path_len
            ));
        }
    }

    // Pre-flight check: Verify socket directory is writable
    {
        let test_file = socket_dir.join(".write_test");
        match fs::write(&test_file, b"") {
            Ok(_) => {
                let _ = fs::remove_file(&test_file);
            }
            Err(e) => {
                return Err(format!(
                    "Socket directory '{}' is not writable: {}",
                    socket_dir.display(),
                    e
                ));
            }
        }
    }

    let exe_path = env::current_exe().map_err(|e| e.to_string())?;
    let exe_path = exe_path.canonicalize().unwrap_or(exe_path);

    #[allow(unused_assignments)]
    let mut daemon_child: Option<std::process::Child> = None;

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let mut cmd = Command::new(&exe_path);
        cmd.env("AGENT_BROWSER_DAEMON", "1");
        apply_daemon_env(&mut cmd, session, opts);

        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }

        daemon_child = Some(
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to start daemon: {}", e))?,
        );
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        let mut cmd = Command::new(&exe_path);
        cmd.env("AGENT_BROWSER_DAEMON", "1");
        apply_daemon_env(&mut cmd, session, opts);

        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;

        daemon_child = Some(
            cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("Failed to start daemon: {}", e))?,
        );
    }

    for _ in 0..50 {
        if daemon_ready(session) {
            return Ok(DaemonResult {
                already_running: false,
            });
        }

        // Detect early daemon exit and surface the real error from stderr
        if let Some(ref mut child) = daemon_child {
            if let Ok(Some(_)) = child.try_wait() {
                let mut stderr_output = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut stderr_output);
                }
                let stderr_trimmed = stderr_output.trim();

                // If the daemon failed because another instance won the bind
                // race ("Address already in use"), check whether that winner is
                // now accepting connections and piggyback on it.
                if stderr_trimmed.contains("Address already in use")
                    || stderr_trimmed.contains("Failed to bind")
                {
                    thread::sleep(Duration::from_millis(200));
                    if daemon_ready(session) {
                        return Ok(DaemonResult {
                            already_running: true,
                        });
                    }
                }

                if !stderr_trimmed.is_empty() {
                    let msg = if stderr_trimmed.len() > 500 {
                        let mut end = 500;
                        while !stderr_trimmed.is_char_boundary(end) {
                            end -= 1;
                        }
                        &stderr_trimmed[..end]
                    } else {
                        stderr_trimmed
                    };
                    return Err(format!("Daemon process exited during startup:\n{}", msg));
                }
                return Err(
                    "Daemon process exited during startup with no error output. \
                     Re-run with --debug for more details."
                        .to_string(),
                );
            }
        }

        thread::sleep(Duration::from_millis(100));
    }

    #[cfg(unix)]
    let endpoint_info = format!(
        "socket: {}",
        get_socket_dir().join(format!("{}.sock", session)).display()
    );
    #[cfg(windows)]
    let endpoint_info = format!("port: 127.0.0.1:{}", resolve_port(session));

    Err(format!("Daemon failed to start ({})", endpoint_info))
}

fn connect(session: &str) -> Result<Connection, String> {
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        UnixStream::connect(&socket_path)
            .map(Connection::Unix)
            .map_err(|e| format!("Failed to connect: {}", e))
    }
    #[cfg(windows)]
    {
        let port = resolve_port(session);
        TcpStream::connect(format!("127.0.0.1:{}", port))
            .map(Connection::Tcp)
            .map_err(|e| format!("Failed to connect: {}", e))
    }
}

pub fn send_command(cmd: Value, session: &str) -> Result<Response, String> {
    // Retry logic for transient errors (EAGAIN/EWOULDBLOCK/connection issues)
    const MAX_RETRIES: u32 = 5;
    const RETRY_DELAY_MS: u64 = 200;

    let mut last_error = String::new();

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(RETRY_DELAY_MS * (attempt as u64)));
        }

        match send_command_once(&cmd, session) {
            Ok(response) => return Ok(response),
            Err(e) => {
                if is_transient_error(&e) {
                    last_error = e;
                    continue;
                }
                // Non-transient error, fail immediately
                return Err(e);
            }
        }
    }

    Err(format!(
        "{} (after {} retries - daemon may be busy or unresponsive)",
        last_error, MAX_RETRIES
    ))
}

/// Check if an error is transient and worth retrying.
/// Transient errors include:
/// - EAGAIN/EWOULDBLOCK (os error 35 on macOS, 11 on Linux)
/// - EOF errors (daemon closed connection before responding)
/// - Connection reset/broken pipe (daemon crashed or restarting)
/// - Connection refused/socket not found (daemon still starting)
fn is_transient_error(error: &str) -> bool {
    error.contains("os error 35") // EAGAIN on macOS
        || error.contains("os error 11") // EAGAIN on Linux
        || error.contains("WouldBlock")
        || error.contains("Resource temporarily unavailable")
        || error.contains("EOF")
        || error.contains("line 1 column 0") // Empty JSON response
        || error.contains("Connection reset")
        || error.contains("Broken pipe")
        || error.contains("os error 54") // Connection reset by peer (macOS)
        || error.contains("os error 104") // Connection reset by peer (Linux)
        || error.contains("os error 2") // No such file or directory (socket gone)
        || error.contains("os error 61") // Connection refused (macOS)
        || error.contains("os error 111") // Connection refused (Linux)
        || error.contains("os error 10061") // Connection refused (Windows)
        || error.contains("os error 10054") // Connection reset by peer (Windows)
}

fn send_command_once(cmd: &Value, session: &str) -> Result<Response, String> {
    let mut stream = connect(session)?;

    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let mut json_str = serde_json::to_string(cmd).map_err(|e| e.to_string())?;
    json_str.push('\n');

    stream
        .write_all(json_str.as_bytes())
        .map_err(|e| format!("Failed to send: {}", e))?;

    let mut reader = BufReader::new(stream);
    let mut response_line = String::new();
    reader
        .read_line(&mut response_line)
        .map_err(|e| format!("Failed to read: {}", e))?;

    serde_json::from_str(&response_line).map_err(|e| format!("Invalid response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;

    #[test]
    fn test_get_socket_dir_explicit_override() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.set("AGENT_BROWSER_SOCKET_DIR", "/custom/socket/path");
        _guard.remove("XDG_RUNTIME_DIR");

        assert_eq!(get_socket_dir(), PathBuf::from("/custom/socket/path"));
    }

    #[test]
    fn test_get_socket_dir_ignores_empty_socket_dir() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.set("AGENT_BROWSER_SOCKET_DIR", "");
        _guard.remove("XDG_RUNTIME_DIR");

        assert!(get_socket_dir()
            .to_string_lossy()
            .ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.remove("AGENT_BROWSER_SOCKET_DIR");
        _guard.set("XDG_RUNTIME_DIR", "/run/user/1000");

        assert_eq!(
            get_socket_dir(),
            PathBuf::from("/run/user/1000/agent-browser")
        );
    }

    #[test]
    fn test_get_socket_dir_ignores_empty_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.set("AGENT_BROWSER_SOCKET_DIR", "");
        _guard.set("XDG_RUNTIME_DIR", "");

        assert!(get_socket_dir()
            .to_string_lossy()
            .ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_home_fallback() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        _guard.remove("AGENT_BROWSER_SOCKET_DIR");
        _guard.remove("XDG_RUNTIME_DIR");

        let result = get_socket_dir();
        assert!(result.to_string_lossy().ends_with(".agent-browser"));
        assert!(
            result.to_string_lossy().contains("home") || result.to_string_lossy().contains("Users")
        );
    }

    // === Transient Error Detection Tests ===

    #[test]
    fn test_is_transient_error_eagain_macos() {
        assert!(is_transient_error(
            "Failed to read: Resource temporarily unavailable (os error 35)"
        ));
    }

    #[test]
    fn test_is_transient_error_eagain_linux() {
        assert!(is_transient_error(
            "Failed to read: Resource temporarily unavailable (os error 11)"
        ));
    }

    #[test]
    fn test_is_transient_error_would_block() {
        assert!(is_transient_error("operation WouldBlock"));
    }

    #[test]
    fn test_is_transient_error_resource_unavailable() {
        assert!(is_transient_error("Resource temporarily unavailable"));
    }

    #[test]
    fn test_is_transient_error_eof() {
        assert!(is_transient_error(
            "Invalid response: EOF while parsing a value at line 1 column 0"
        ));
    }

    #[test]
    fn test_is_transient_error_empty_json() {
        assert!(is_transient_error(
            "Invalid response: expected value at line 1 column 0"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_reset() {
        assert!(is_transient_error("Connection reset by peer"));
    }

    #[test]
    fn test_is_transient_error_broken_pipe() {
        assert!(is_transient_error("Broken pipe"));
    }

    #[test]
    fn test_is_transient_error_connection_reset_macos() {
        assert!(is_transient_error(
            "Failed to send: Connection reset by peer (os error 54)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_reset_linux() {
        assert!(is_transient_error(
            "Failed to send: Connection reset by peer (os error 104)"
        ));
    }

    #[test]
    fn test_is_transient_error_socket_not_found() {
        assert!(is_transient_error(
            "Failed to connect: No such file or directory (os error 2)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_refused_macos() {
        assert!(is_transient_error(
            "Failed to connect: Connection refused (os error 61)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_refused_linux() {
        assert!(is_transient_error(
            "Failed to connect: Connection refused (os error 111)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_refused_windows() {
        assert!(is_transient_error(
            "Failed to connect: No connection could be made because the target machine actively refused it. (os error 10061)"
        ));
    }

    #[test]
    fn test_is_transient_error_connection_reset_windows() {
        assert!(is_transient_error(
            "Failed to send: An existing connection was forcibly closed by the remote host. (os error 10054)"
        ));
    }

    #[test]
    fn test_is_transient_error_non_transient() {
        // These should NOT be considered transient
        assert!(!is_transient_error("Unknown command: foo"));
        assert!(!is_transient_error("Invalid JSON syntax"));
        assert!(!is_transient_error("Permission denied"));
        assert!(!is_transient_error("Daemon not found"));
    }

    #[test]
    #[cfg(windows)]
    fn test_get_port_for_session() {
        assert_eq!(get_port_for_session("default"), 50838);
        assert_eq!(get_port_for_session("my-session"), 63105);
        assert_eq!(get_port_for_session("work"), 51184);
        assert_eq!(get_port_for_session(""), 49152);
    }

    // === Daemon Version Mismatch Detection Tests ===

    #[test]
    fn test_daemon_version_matches_same_version() {
        let dir = std::env::temp_dir().join("ab-test-version-match");
        let _ = fs::create_dir_all(&dir);
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_str().unwrap());

        let version_path = dir.join("test-session.version");
        let _ = fs::write(&version_path, env!("CARGO_PKG_VERSION"));

        assert!(daemon_version_matches("test-session"));

        let _ = fs::remove_file(&version_path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn test_daemon_version_matches_different_version() {
        let dir = std::env::temp_dir().join("ab-test-version-mismatch");
        let _ = fs::create_dir_all(&dir);
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_str().unwrap());

        let version_path = dir.join("test-session.version");
        let _ = fs::write(&version_path, "0.0.0-old");

        assert!(!daemon_version_matches("test-session"));

        let _ = fs::remove_file(&version_path);
        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn test_daemon_version_matches_no_file() {
        let dir = std::env::temp_dir().join("ab-test-version-nofile");
        let _ = fs::create_dir_all(&dir);
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_str().unwrap());

        // No version file: treated as mismatch so stale pre-version-tracking
        // daemons (including Node.js era) are always restarted.
        assert!(!daemon_version_matches("test-session"));

        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn test_cleanup_stale_files_removes_version() {
        let dir = std::env::temp_dir().join("ab-test-cleanup-version");
        let _ = fs::create_dir_all(&dir);
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);
        _guard.set("AGENT_BROWSER_SOCKET_DIR", dir.to_str().unwrap());

        let version_path = dir.join("test-session.version");
        let _ = fs::write(&version_path, "0.1.0");
        assert!(version_path.exists());

        cleanup_stale_files("test-session");
        assert!(!version_path.exists());

        let _ = fs::remove_dir(&dir);
    }
}
