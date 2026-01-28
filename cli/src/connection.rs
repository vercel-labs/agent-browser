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

#[cfg(windows)]
fn get_port_path(session: &str) -> PathBuf {
    get_socket_dir().join(format!("{}.port", session))
}

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
    profile: Option<&str>,
    state: Option<&str>,
) -> Result<DaemonResult, String> {
    if is_daemon_running(session) && daemon_ready(session) {
        return Ok(DaemonResult {
            already_running: true,
        });
    }

    // Ensure socket directory exists
    let socket_dir = get_socket_dir();
    if !socket_dir.exists() {
        fs::create_dir_all(&socket_dir)
            .map_err(|e| format!("Failed to create socket directory: {}", e))?;
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

        if let Some(prof) = profile {
            cmd.env("AGENT_BROWSER_PROFILE", prof);
        }

        if let Some(st) = state {
            cmd.env("AGENT_BROWSER_STATE", st);
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

        // On Windows, call node directly. Command::new handles PATH resolution (node.exe or node.cmd)
        // and automatically quotes arguments containing spaces.
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

        if let Some(prof) = profile {
            cmd.env("AGENT_BROWSER_PROFILE", prof);
        }

        if let Some(st) = state {
            cmd.env("AGENT_BROWSER_STATE", st);
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
}
