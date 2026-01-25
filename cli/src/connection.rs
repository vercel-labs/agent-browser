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

/// Find node executable, checking fnm directories on Windows.
/// This is necessary because fnm creates temporary shell directories that aren't
/// inherited when spawning new processes via Command.
///
/// Search order:
/// 1. `where node` - works for direct install, nvm-windows, volta, etc.
/// 2. FNM_MULTISHELL_PATH env var - if set by fnm
/// 3. fnm_multishells directory scan - fallback for fnm users
/// 4. Common installation paths - last resort
///
/// Note: Tested on Windows 10/11 with default fnm installation.
/// Custom fnm configurations may require adjustments.
/// Ref: https://github.com/Schniz/fnm/issues/1228
#[cfg(windows)]
fn find_node_executable() -> Option<PathBuf> {
    // First, check if node is directly in PATH (works for non-fnm setups)
    if let Ok(output) = Command::new("where")
        .arg("node")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = stdout.lines().next() {
                let path = PathBuf::from(first_line.trim());
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }

    // Check FNM_MULTISHELL_PATH env var (set by `fnm env`)
    // This is the official fnm environment variable for the current shell's node path
    if let Some(fnm_path) = env::var_os("FNM_MULTISHELL_PATH") {
        let node_path = PathBuf::from(&fnm_path).join("node.exe");
        if node_path.exists() {
            return Some(node_path);
        }
    }

    // Fallback: scan fnm multishells directory
    // This handles cases where FNM_MULTISHELL_PATH isn't inherited
    // Default location: %LOCALAPPDATA%\fnm_multishells\
    // Note: Use LOCALAPPDATA directly, avoid HOME which may have Unix-style path in Git Bash
    let fnm_dir = if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        PathBuf::from(&local_app_data).join("fnm_multishells")
    } else if let Some(user_profile) = env::var_os("USERPROFILE") {
        PathBuf::from(&user_profile)
            .join("AppData")
            .join("Local")
            .join("fnm_multishells")
    } else {
        PathBuf::new()
    };

    if fnm_dir.exists() {
        // Get all fnm shell directories, sorted by modification time (newest first)
        if let Ok(entries) = fs::read_dir(&fnm_dir) {
            let mut dirs: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .collect();

            // Sort by modification time, newest first
            dirs.sort_by(|a, b| {
                let time_a = a.metadata().and_then(|m| m.modified()).ok();
                let time_b = b.metadata().and_then(|m| m.modified()).ok();
                time_b.cmp(&time_a)
            });

            for entry in dirs {
                let node_path = entry.path().join("node.exe");
                if node_path.exists() {
                    return Some(node_path);
                }
            }
        }
    }

    // Check common installation paths
    let common_paths = [
        "C:\\Program Files\\nodejs\\node.exe",
        "C:\\Program Files (x86)\\nodejs\\node.exe",
    ];

    for path_str in &common_paths {
        let p = PathBuf::from(path_str);
        if p.exists() {
            return Some(p);
        }
    }

    None
}

/// Find npx executable, checking fnm directories on Windows.
/// Same search strategy as find_node_executable().
///
/// Note: Tested on Windows 10/11 with default fnm installation.
/// Custom fnm configurations may require adjustments.
#[cfg(windows)]
pub fn find_npx_executable() -> Option<PathBuf> {
    // First, check if npx is directly in PATH
    if let Ok(output) = Command::new("where")
        .arg("npx")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = stdout.lines().next() {
                let path = PathBuf::from(first_line.trim());
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }

    // Check FNM_MULTISHELL_PATH env var (set by `fnm env`)
    if let Some(fnm_path) = env::var_os("FNM_MULTISHELL_PATH") {
        let npx_path = PathBuf::from(&fnm_path).join("npx.cmd");
        if npx_path.exists() {
            return Some(npx_path);
        }
    }

    // Fallback: scan fnm multishells directory
    // Note: Use LOCALAPPDATA directly, avoid HOME which may have Unix-style path in Git Bash
    let fnm_dir = if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
        PathBuf::from(&local_app_data).join("fnm_multishells")
    } else if let Some(user_profile) = env::var_os("USERPROFILE") {
        PathBuf::from(&user_profile)
            .join("AppData")
            .join("Local")
            .join("fnm_multishells")
    } else {
        PathBuf::new()
    };

    if fnm_dir.exists() {
        if let Ok(entries) = fs::read_dir(&fnm_dir) {
            let mut dirs: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .collect();

            dirs.sort_by(|a, b| {
                let time_a = a.metadata().and_then(|m| m.modified()).ok();
                let time_b = b.metadata().and_then(|m| m.modified()).ok();
                time_b.cmp(&time_a)
            });

            for entry in dirs {
                // Check for npx.cmd (Windows uses .cmd wrapper)
                let npx_cmd = entry.path().join("npx.cmd");
                if npx_cmd.exists() {
                    return Some(npx_cmd);
                }
            }
        }
    }

    // Check common installation paths
    let common_paths = [
        "C:\\Program Files\\nodejs\\npx.cmd",
        "C:\\Program Files (x86)\\nodejs\\npx.cmd",
    ];

    for path_str in &common_paths {
        let p = PathBuf::from(path_str);
        if p.exists() {
            return Some(p);
        }
    }

    None
}

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

#[cfg(windows)]
fn get_port_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.port", session))
}

/// Calculate port number for a session (must match daemon.js implementation).
/// Port range: 49152-65534 (dynamic/private ports)
#[cfg(windows)]
fn get_port_for_session(session: &str) -> u16 {
    let mut hash: i32 = 0;
    for c in session.chars() {
        hash = ((hash << 5).wrapping_sub(hash)).wrapping_add(c as i32);
    }
    // Correct logic: first take absolute modulo, then cast to u16
    // Using unsigned_abs() to safely handle i32::MIN
    49152 + ((hash.unsigned_abs() as u32 % 16383) as u16)
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
    let port = get_port_for_session(session);
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{}", port).parse().unwrap(),
        Duration::from_millis(100),
    )
    .is_ok()
}

fn daemon_ready(session: &str) -> bool {
    #[cfg(unix)]
    {
        let socket_path = get_socket_path(session);
        UnixStream::connect(&socket_path).is_ok()
    }
    #[cfg(windows)]
    {
        let port = get_port_for_session(session);
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

pub fn ensure_daemon(
    session: &str,
    headed: bool,
    executable_path: Option<&str>,
    extensions: &[String],
    args: Option<&str>,
    user_agent: Option<&str>,
    proxy: Option<&str>,
    proxy_bypass: Option<&str>,
    ignore_https_errors: bool,
) -> Result<DaemonResult, String> {
    if is_daemon_running(session) && daemon_ready(session) {
        return Ok(DaemonResult {
            already_running: true,
        });
    }

    // Ensure socket directory exists
    let socket_dir = get_socket_dir();
    if !socket_dir.exists() {
        fs::create_dir_all(&socket_dir).map_err(|e| format!("Failed to create socket directory: {}", e))?;
    }

    let exe_path = env::current_exe().map_err(|e| e.to_string())?;
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
        cmd.arg(daemon_path)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session);

        if headed {
            cmd.env("AGENT_BROWSER_HEADED", "1");
        }

        if let Some(path) = executable_path {
            cmd.env("AGENT_BROWSER_EXECUTABLE_PATH", path);
        }

        if !extensions.is_empty() {
            cmd.env("AGENT_BROWSER_EXTENSIONS", extensions.join(","));
        }

        if let Some(a) = args {
            cmd.env("AGENT_BROWSER_ARGS", a);
        }

        if let Some(ua) = user_agent {
            cmd.env("AGENT_BROWSER_USER_AGENT", ua);
        }

        if let Some(p) = proxy {
            cmd.env("AGENT_BROWSER_PROXY", p);
        }

        if let Some(pb) = proxy_bypass {
            cmd.env("AGENT_BROWSER_PROXY_BYPASS", pb);
        }

        if ignore_https_errors {
            cmd.env("AGENT_BROWSER_IGNORE_HTTPS_ERRORS", "1");
        }

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

        // Find node executable, checking fnm directories
        // This fixes the issue where fnm's temporary shell directories aren't inherited
        let node_path = find_node_executable()
            .ok_or("Node.js not found. Please ensure Node.js is installed. If using fnm, make sure a Node.js version is installed.")?;

        // Use the full path to node.exe directly instead of cmd.exe /c
        // This avoids PATH resolution issues with fnm
        let mut cmd = Command::new(&node_path);
        cmd.arg(daemon_path)
            .env("AGENT_BROWSER_DAEMON", "1")
            .env("AGENT_BROWSER_SESSION", session);

        if headed {
            cmd.env("AGENT_BROWSER_HEADED", "1");
        }

        if let Some(path) = executable_path {
            cmd.env("AGENT_BROWSER_EXECUTABLE_PATH", path);
        }

        if !extensions.is_empty() {
            cmd.env("AGENT_BROWSER_EXTENSIONS", extensions.join(","));
        }

        if let Some(a) = args {
            cmd.env("AGENT_BROWSER_ARGS", a);
        }

        if let Some(ua) = user_agent {
            cmd.env("AGENT_BROWSER_USER_AGENT", ua);
        }

        if let Some(p) = proxy {
            cmd.env("AGENT_BROWSER_PROXY", p);
        }

        if let Some(pb) = proxy_bypass {
            cmd.env("AGENT_BROWSER_PROXY_BYPASS", pb);
        }

         if ignore_https_errors {
            cmd.env("AGENT_BROWSER_IGNORE_HTTPS_ERRORS", "1");
        }

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

    Err("Daemon failed to start".to_string())
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
        let port = get_port_for_session(session);
        TcpStream::connect(format!("127.0.0.1:{}", port))
            .map(Connection::Tcp)
            .map_err(|e| format!("Failed to connect: {}", e))
    }
}

pub fn send_command(cmd: Value, session: &str) -> Result<Response, String> {
    let mut stream = connect(session)?;

    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let mut json_str = serde_json::to_string(&cmd).map_err(|e| e.to_string())?;
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

        assert!(get_socket_dir().to_string_lossy().ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::remove_var("AGENT_BROWSER_SOCKET_DIR");
        env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");

        assert_eq!(get_socket_dir(), PathBuf::from("/run/user/1000/agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_ignores_empty_xdg_runtime() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::set_var("AGENT_BROWSER_SOCKET_DIR", "");
        env::set_var("XDG_RUNTIME_DIR", "");

        assert!(get_socket_dir().to_string_lossy().ends_with(".agent-browser"));
    }

    #[test]
    fn test_get_socket_dir_home_fallback() {
        let _guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);

        env::remove_var("AGENT_BROWSER_SOCKET_DIR");
        env::remove_var("XDG_RUNTIME_DIR");

        let result = get_socket_dir();
        assert!(result.to_string_lossy().ends_with(".agent-browser"));
        assert!(result.to_string_lossy().contains("home") || result.to_string_lossy().contains("Users"));
    }
}
