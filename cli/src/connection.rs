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

/// Clean up stale socket and PID files for a session
fn cleanup_stale_files(session: &str) {
    let pid_path = get_pid_path(session);
    let _ = fs::remove_file(&pid_path);

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
fn get_port_for_session(session: &str) -> Option<u16> {
    let port_path = get_port_path(session);
    if let Ok(content) = fs::read_to_string(&port_path) {
        content.trim().parse::<u16>().ok()
    } else {
        None
    }
}

#[cfg(unix)]
fn is_daemon_running(session: &str) -> bool {
    let pid_path = get_pid_path(session);
    if !pid_path.exists() {
        return false;
    }
    if let Ok(pid_str) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            unsafe {
                return libc::kill(pid, 0) == 0;
            }
        }
    }
    false
}

#[cfg(windows)]
fn is_daemon_running(session: &str) -> bool {
    let pid_path = get_pid_path(session);
    if !pid_path.exists() {
        return false;
    }
    if let Some(port) = get_port_for_session(session) {
        TcpStream::connect_timeout(
            &format!("127.0.0.1:{}", port).parse().unwrap(),
            Duration::from_millis(250),
        )
        .is_ok()
    } else {
        false
    }
}

fn daemon_ready(session: &str) -> bool {
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        UnixStream::connect(&socket_path).is_ok()
    }
    #[cfg(windows)]
    {
        if let Some(port) = get_port_for_session(session) {
            TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", port).parse().unwrap(),
                Duration::from_millis(250),
            )
            .is_ok()
        } else {
            false
        }
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
    pub executable_path: Option<&'a str>,
    pub extensions: &'a [String],
    pub args: Option<&'a str>,
    pub user_agent: Option<&'a str>,
    pub proxy: Option<&'a str>,
    pub proxy_bypass: Option<&'a str>,
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
}

fn apply_daemon_env(cmd: &mut Command, session: &str, opts: &DaemonOptions) {
    cmd.env("AGENT_BROWSER_DAEMON", "1")
        .env("AGENT_BROWSER_SESSION", session);

    if opts.headed {
        cmd.env("AGENT_BROWSER_HEADED", "1");
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
}

pub fn ensure_daemon(
    session: &str,
    opts: &DaemonOptions,
) -> Result<DaemonResult, String> {
    // Check if daemon is running AND responsive
    if is_daemon_running(session) && daemon_ready(session) {
        // Double-check it's actually responsive by waiting and checking again
        // This handles the race condition where daemon is shutting down
        // (daemon has a 100ms shutdown delay, so we wait longer)
        thread::sleep(Duration::from_millis(150));
        if daemon_ready(session) {
            return Ok(DaemonResult {
                already_running: true,
            });
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
    // Canonicalize to resolve symlinks (e.g., npm global bin symlink -> actual binary)
    let exe_path = exe_path.canonicalize().unwrap_or(exe_path);
    let exe_dir = exe_path.parent().unwrap();

    let mut daemon_paths = vec![
        exe_dir.join("daemon.js"),
        exe_dir.join("../dist/daemon.js"),
        PathBuf::from("dist/daemon.js"),
    ];

    // Check AGENT_BROWSER_HOME environment variable
    if let Ok(home) = env::var("AGENT_BROWSER_HOME") {
        let home_path = PathBuf::from(&home);
        daemon_paths.insert(0, home_path.join("dist/daemon.js"));
        daemon_paths.insert(1, home_path.join("daemon.js"));
    }

    let daemon_path = daemon_paths
        .iter()
        .find(|p| p.exists())
        .ok_or("Daemon not found. Set AGENT_BROWSER_HOME environment variable or run from project directory.")?;

    // Spawn daemon as a fully detached background process
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let mut cmd = Command::new("node");
        cmd.arg(daemon_path);
        apply_daemon_env(&mut cmd, session, opts);

        // Create new process group and session to fully detach
        unsafe {
            cmd.pre_exec(|| {
                // Create new session (detach from terminal)
                libc::setsid();
                Ok(())
            });
        }

        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start daemon: {}", e))?;
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // On Windows, call node directly. Command::new handles PATH resolution (node.exe or node.cmd)
        // and automatically quotes arguments containing spaces.
        let mut cmd = Command::new("node");
        cmd.arg(daemon_path);
        apply_daemon_env(&mut cmd, session, opts);

        // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const DETACHED_PROCESS: u32 = 0x00000008;

        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start daemon: {}", e))?;
    }

    for _ in 0..50 {
        if daemon_ready(session) {
            return Ok(DaemonResult {
                already_running: false,
            });
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(format!(
        "Daemon failed to start (socket: {})",
        get_socket_dir().join(format!("{}.sock", session)).display()
    ))
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
        if let Some(port) = get_port_for_session(session) {
            TcpStream::connect(format!("127.0.0.1:{}", port))
                .map(Connection::Tcp)
                .map_err(|e| format!("Failed to connect: {}", e))
        } else {
            Err("Port file not found (daemon not running?)".to_string())
        }
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
    use std::sync::{Mutex, MutexGuard};

    // Mutex to prevent parallel tests from interfering with env vars
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// RAII guard that locks env mutex and restores env vars on drop
    struct EnvGuard<'a> {
        _lock: MutexGuard<'a, ()>,
        vars: Vec<(String, Option<String>)>,
    }

    impl<'a> EnvGuard<'a> {
        fn new(var_names: &[&str]) -> Self {
            let lock = ENV_MUTEX.lock().unwrap();
            let vars = var_names
                .iter()
                .map(|&name| (name.to_string(), env::var(name).ok()))
                .collect();
            Self { _lock: lock, vars }
        }
    }

    impl Drop for EnvGuard<'_> {
        fn drop(&mut self) {
            for (name, value) in &self.vars {
                match value {
                    Some(v) => env::set_var(name, v),
                    None => env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn test_get_socket_dir_explicit_override() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::set_var("AGENT_BROWSER_SOCKET_DIR", "/custom/socket/path");
        env::remove_var("XDG_RUNTIME_DIR");

        assert_eq!(get_socket_dir(), PathBuf::from("/custom/socket/path"));
    }

    #[test]
    fn test_get_socket_dir_ignores_empty_socket_dir() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::set_var("AGENT_BROWSER_SOCKET_DIR", "");
        env::remove_var("XDG_RUNTIME_DIR");

        assert!(get_socket_dir()
            .to_string_lossy()
            .ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::remove_var("AGENT_BROWSER_SOCKET_DIR");
        env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");

        assert_eq!(
            get_socket_dir(),
            PathBuf::from("/run/user/1000/agent-browser")
        );
    }

    #[test]
    fn test_get_socket_dir_ignores_empty_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::set_var("AGENT_BROWSER_SOCKET_DIR", "");
        env::set_var("XDG_RUNTIME_DIR", "");

        assert!(get_socket_dir()
            .to_string_lossy()
            .ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_home_fallback() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::remove_var("AGENT_BROWSER_SOCKET_DIR");
        env::remove_var("XDG_RUNTIME_DIR");

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
    fn test_is_transient_error_non_transient() {
        // These should NOT be considered transient
        assert!(!is_transient_error("Unknown command: foo"));
        assert!(!is_transient_error("Invalid JSON syntax"));
        assert!(!is_transient_error("Permission denied"));
        assert!(!is_transient_error("Daemon not found"));
    }
}
