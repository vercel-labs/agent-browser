use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MOLI_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const MOLI_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MOLI_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_LOG_LINES: usize = 40;

/// Owns the Moli server and reaps it when startup fails or the session closes.
pub struct MoliProcess {
    child: Child,
    pub ws_url: String,
    _log_drainers: Vec<std::thread::JoinHandle<()>>,
}

impl MoliProcess {
    #[cfg(test)]
    pub(crate) fn for_test(child: Child, ws_url: String) -> Self {
        Self {
            child,
            ws_url,
            _log_drainers: Vec::new(),
        }
    }

    pub fn has_exited(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_some()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for MoliProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Options for `moli serve`.
#[derive(Default)]
pub struct MoliLaunchOptions {
    pub executable_path: Option<String>,
    /// Extra arguments passed directly to `moli serve`.
    pub args: Vec<String>,
    /// Maps `--profile` to Moli's `--profile-dir`.
    pub profile_dir: Option<String>,
    pub proxy: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    /// Maps CLI/config/environment bypass rules to Moli's `--http-no-proxy`.
    pub proxy_bypass: Option<String>,
    pub user_agent: Option<String>,
    pub ignore_https_errors: bool,
    /// Maps `--hide-scrollbars false` to Moli's opt-in `--scrollbars` flag.
    pub show_scrollbars: bool,
    pub port: Option<u16>,
}

/// Layout, images and fonts enable geometry, coordinate input and screenshots.
fn build_moli_serve_args(port: u16, options: &MoliLaunchOptions) -> Vec<String> {
    let proxy = moli_proxy_url(options);
    let mut args = vec![
        "serve".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--layout".to_string(),
        "--image".to_string(),
        "--font".to_string(),
    ];
    for (flag, value) in [
        ("--profile-dir", options.profile_dir.as_deref()),
        ("--http-proxy", proxy.as_deref()),
        ("--http-no-proxy", options.proxy_bypass.as_deref()),
        ("--user-agent", options.user_agent.as_deref()),
    ] {
        if let Some(value) = value {
            args.push(flag.to_string());
            args.push(value.to_string());
        }
    }
    if options.ignore_https_errors {
        args.push("--insecure-disable-tls-host-verification".to_string());
    }
    if options.show_scrollbars {
        args.push("--scrollbars".to_string());
    }
    args.extend(options.args.iter().cloned());
    args
}

fn moli_proxy_url(options: &MoliLaunchOptions) -> Option<String> {
    let proxy = options.proxy.as_deref()?;
    if options.proxy_username.is_none() && options.proxy_password.is_none() {
        return Some(proxy.to_string());
    }
    // CLI credentials retain URL escapes; avoid encoding them twice.
    let encode = |value: Option<&str>| {
        let decoded = urlencoding::decode_binary(value.unwrap_or_default().as_bytes());
        urlencoding::encode_binary(&decoded).into_owned()
    };
    let (scheme, server) = proxy.split_once("://").unwrap_or(("http", proxy));
    let server = server.rsplit_once('@').map_or(server, |(_, server)| server);
    Some(format!(
        "{scheme}://{}:{}@{server}",
        encode(options.proxy_username.as_deref()),
        encode(options.proxy_password.as_deref())
    ))
}

#[derive(Clone, Default)]
struct LaunchLogBuffer {
    stdout: Arc<Mutex<VecDeque<String>>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl LaunchLogBuffer {
    fn push_stdout(&self, line: String) {
        push_bounded(&self.stdout, line);
    }

    fn push_stderr(&self, line: String) {
        push_bounded(&self.stderr, line);
    }

    fn snapshot_stdout(&self) -> Vec<String> {
        self.stdout
            .lock()
            .expect("stdout log buffer poisoned")
            .iter()
            .cloned()
            .collect()
    }

    fn snapshot_stderr(&self) -> Vec<String> {
        self.stderr
            .lock()
            .expect("stderr log buffer poisoned")
            .iter()
            .cloned()
            .collect()
    }
}

fn push_bounded(buffer: &Mutex<VecDeque<String>>, line: String) {
    let mut guard = buffer.lock().expect("log buffer poisoned");
    if guard.len() >= MAX_LOG_LINES {
        guard.pop_front();
    }
    guard.push_back(line);
}

/// Finds Moli on PATH or in its standard installer locations.
pub fn find_moli() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(output) = Command::new("which").arg("moli").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }

    #[cfg(windows)]
    {
        if let Ok(output) = Command::new("where").arg("moli").output() {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }

    #[cfg(windows)]
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let path = PathBuf::from(local_app_data).join("Moli/bin/moli.exe");
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(home) = dirs::home_dir() {
        let binary = if cfg!(windows) { "moli.exe" } else { "moli" };
        for directory in [".local/bin", ".cargo/bin"] {
            let path = home.join(directory).join(binary);
            if path.is_file() {
                return Some(path);
            }
        }
    }

    None
}

/// Launches an owned loopback CDP server with bounded discovery and cleanup.
pub async fn launch_moli(options: &MoliLaunchOptions) -> Result<MoliProcess, String> {
    let binary_path = match &options.executable_path {
        Some(p) => PathBuf::from(p),
        None => find_moli().ok_or(
            "Moli not found. Install it from https://github.com/lexmount/moli or use --executable-path.",
        )?,
    };

    let port = match options.port {
        Some(p) => p,
        None => TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("Failed to find an available port for Moli: {}", e))?,
    };
    let args = build_moli_serve_args(port, options);

    let mut child = Command::new(&binary_path)
        .args(&args)
        // Bypass rules come from the current resolved launch options, even
        // when a daemon inherited different NO_PROXY values at startup.
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env_remove("MOLI_SCROLLBARS")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch Moli at {:?}: {}", binary_path, e))?;

    let (log_buffer, log_drainers) = start_log_drainers(&mut child)?;

    let mut process = MoliProcess {
        child,
        ws_url: String::new(),
        _log_drainers: log_drainers,
    };
    process.ws_url =
        wait_for_moli_ready(&mut process.child, port, &log_buffer, MOLI_STARTUP_TIMEOUT).await?;

    Ok(process)
}

fn start_log_drainers(
    child: &mut Child,
) -> Result<(LaunchLogBuffer, Vec<std::thread::JoinHandle<()>>), String> {
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "Failed to capture Moli stdout".to_string()
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "Failed to capture Moli stderr".to_string()
    })?;

    let logs = LaunchLogBuffer::default();
    let stdout_logs = logs.clone();
    let stderr_logs = logs.clone();

    let stdout_handle =
        std::thread::spawn(move || drain_reader(stdout, move |line| stdout_logs.push_stdout(line)));
    let stderr_handle =
        std::thread::spawn(move || drain_reader(stderr, move |line| stderr_logs.push_stderr(line)));

    Ok((logs, vec![stdout_handle, stderr_handle]))
}

fn drain_reader<R, F>(reader: R, mut push: F)
where
    R: std::io::Read,
    F: FnMut(String),
{
    for line in BufReader::new(reader).lines() {
        match line {
            Ok(line) => push(line),
            Err(_) => break,
        }
    }
}

async fn wait_for_moli_ready(
    child: &mut Child,
    port: u16,
    logs: &LaunchLogBuffer,
    startup_timeout: Duration,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + startup_timeout;
    let mut last_probe_error = None;
    // The owned server is always local. System HTTP proxies must not route
    // discovery traffic away from its loopback endpoint.
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(MOLI_DISCOVERY_TIMEOUT)
        .build()
        .map_err(|error| format!("Failed to create Moli discovery client: {error}"))?;

    loop {
        if let Ok(Some(status)) = child.try_wait() {
            // Allow the log drainers to collect startup errors before taking the snapshot.
            tokio::time::sleep(Duration::from_millis(25)).await;
            return Err(moli_launch_error(
                &format!("Moli exited before CDP became ready (status: {})", status),
                logs,
                last_probe_error.as_deref(),
            ));
        }

        if std::time::Instant::now() >= deadline {
            return Err(moli_launch_error(
                &format!(
                    "Timed out after {}ms waiting for Moli CDP endpoint on port {}",
                    startup_timeout.as_millis(),
                    port
                ),
                logs,
                last_probe_error.as_deref(),
            ));
        }

        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(remaining, discover_moli_url(&client, port)).await {
            Ok(Ok(ws_url)) => return Ok(ws_url),
            Ok(Err(err)) => last_probe_error = Some(err),
            Err(_) => last_probe_error = Some("CDP discovery exceeded the startup deadline".into()),
        }

        tokio::time::sleep(
            MOLI_POLL_INTERVAL.min(deadline.saturating_duration_since(std::time::Instant::now())),
        )
        .await;
    }
}

/// Moli exposes browser discovery at `/json/version`; normalize its advertised
/// WebSocket host and port to the owned loopback endpoint.
async fn discover_moli_url(client: &reqwest::Client, port: u16) -> Result<String, String> {
    let info: serde_json::Value = client
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| format!("Failed to connect to CDP at 127.0.0.1:{port}: {error}"))?
        .json()
        .await
        .map_err(|error| format!("Invalid Moli /json/version response: {error}"))?;
    let ws_url = info["webSocketDebuggerUrl"]
        .as_str()
        .ok_or("No webSocketDebuggerUrl in Moli /json/version")?;
    let mut endpoint = url::Url::parse(ws_url)
        .map_err(|error| format!("Invalid Moli CDP WebSocket URL: {error}"))?;
    if !matches!(endpoint.scheme(), "ws" | "wss") {
        return Err("Moli CDP discovery must return a WebSocket URL".into());
    }
    endpoint
        .set_host(Some("127.0.0.1"))
        .map_err(|_| "Invalid Moli CDP WebSocket host")?;
    endpoint
        .set_port(Some(port))
        .map_err(|_| "Invalid Moli CDP WebSocket port")?;
    Ok(endpoint.to_string())
}

fn moli_launch_error(
    message: &str,
    logs: &LaunchLogBuffer,
    last_probe_error: Option<&str>,
) -> String {
    let stdout_lines = logs.snapshot_stdout();
    let stderr_lines = logs.snapshot_stderr();
    let mut details = Vec::new();

    if let Some(err) = last_probe_error {
        details.push(format!("Last probe error: {}", err));
    }

    if !stderr_lines.is_empty() {
        details.push(format!(
            "Moli stderr (last {} lines):\n  {}",
            stderr_lines.len(),
            stderr_lines.join("\n  ")
        ));
    }

    if !stdout_lines.is_empty() {
        details.push(format!(
            "Moli stdout (last {} lines):\n  {}",
            stdout_lines.len(),
            stdout_lines.join("\n  ")
        ));
    }

    if details.is_empty() {
        format!("{} (no stdout/stderr output from Moli)", message)
    } else {
        format!("{}\n{}", message, details.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener as TokioTcpListener;

    fn unused_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    async fn serve_json_version_once_after_delay(port: u16, delay_ms: u64, body: &'static str) {
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        let listener = TokioTcpListener::bind(("127.0.0.1", port)).await.unwrap();
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = socket.read(&mut buf).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn waits_for_ready_without_logs() {
        let env =
            crate::test_utils::EnvGuard::new(&["HTTP_PROXY", "http_proxy", "NO_PROXY", "no_proxy"]);
        env.set("HTTP_PROXY", "http://127.0.0.1:1");
        env.set("http_proxy", "http://127.0.0.1:1");
        env.remove("NO_PROXY");
        env.remove("no_proxy");
        let port = unused_port();
        tokio::spawn(serve_json_version_once_after_delay(
            port,
            150,
            r#"{"webSocketDebuggerUrl":"ws://127.0.0.1:9222/"}"#,
        ));

        let mut child = Command::new("/bin/sh")
            .args(["-c", "exec sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (logs, _drainers) = start_log_drainers(&mut child).unwrap();
        let ws_url = wait_for_moli_ready(&mut child, port, &logs, MOLI_STARTUP_TIMEOUT)
            .await
            .unwrap();

        assert_eq!(ws_url, format!("ws://127.0.0.1:{}/", port));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn child_exit_surfaces_logs() {
        let port = unused_port();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "echo boom >&2; sleep 0.1; exit 23"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (logs, _drainers) = start_log_drainers(&mut child).unwrap();
        let err = wait_for_moli_ready(&mut child, port, &logs, MOLI_STARTUP_TIMEOUT)
            .await
            .unwrap_err();

        assert!(err.contains("Moli exited before CDP became ready"));
        assert!(err.contains("boom"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_reports_last_probe_error() {
        let port = unused_port();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let timeout = Duration::from_millis(300);
        let (logs, _drainers) = start_log_drainers(&mut child).unwrap();
        let err = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_moli_ready(&mut child, port, &logs, timeout),
        )
        .await
        .expect("ready wait should return before outer timeout")
        .unwrap_err();

        assert!(err.contains("Timed out after 300ms waiting for Moli CDP endpoint"));
        assert!(
            err.contains("Failed to connect to CDP") || err.contains("Timeout connecting to CDP")
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stalled_discovery_obeys_startup_deadline() {
        let listener = TokioTcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).await.unwrap();
            assert!(!request.is_empty());
        });
        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let mut process = MoliProcess::for_test(child, String::new());
        let logs = LaunchLogBuffer::default();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_moli_ready(&mut process.child, port, &logs, Duration::from_millis(100)),
        )
        .await
        .expect("discovery must not outlive its startup deadline")
        .unwrap_err();
        assert!(error.contains("Timed out after 100ms"), "{error}");
        assert!(
            error.contains("CDP discovery exceeded the startup deadline"),
            "{error}"
        );
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn test_moli_process_reports_exit() {
        let child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let mut process = MoliProcess {
            child,
            ws_url: String::new(),
            _log_drainers: Vec::new(),
        };
        process.child.wait().unwrap();
        assert!(process.has_exited());
    }

    #[tokio::test]
    async fn missing_explicit_binary_reports_launch_error() {
        let dir = tempfile::tempdir().unwrap();
        let error = launch_moli(&MoliLaunchOptions {
            executable_path: Some(
                dir.path()
                    .join("missing-moli")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        })
        .await
        .err()
        .expect("missing explicit executable must fail");
        assert!(error.contains("Failed to launch Moli"), "{error}");
        assert!(error.contains("missing-moli"), "{error}");
    }

    #[test]
    fn launch_logs_keep_only_the_last_lines() {
        let logs = LaunchLogBuffer::default();
        for index in 0..MAX_LOG_LINES + 5 {
            logs.push_stdout(index.to_string());
            logs.push_stderr(index.to_string());
        }
        let expected: Vec<_> = (5..MAX_LOG_LINES + 5)
            .map(|index| index.to_string())
            .collect();
        assert_eq!(logs.snapshot_stdout(), expected);
        assert_eq!(logs.snapshot_stderr(), expected);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelled_moli_launch_reaps_child() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("moli");
        let pid_path = dir.path().join("pid");
        std::fs::write(
            &executable,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 60\n",
                pid_path.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let options = MoliLaunchOptions {
            executable_path: Some(executable.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut launch = Box::pin(launch_moli(&options));
        tokio::select! {
            _ = &mut launch => panic!("fixture must not become ready"),
            _ = async {
                while !pid_path.exists() || std::fs::read_to_string(&pid_path).unwrap().is_empty() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => {}
        }
        let pid: i32 = std::fs::read_to_string(&pid_path).unwrap().parse().unwrap();
        drop(launch);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert_eq!(
            unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn test_moli_launch_error_no_logs() {
        let logs = LaunchLogBuffer::default();
        let msg = moli_launch_error("Moli exited", &logs, None);
        assert!(msg.contains("no stdout/stderr output"));
    }

    #[test]
    fn test_moli_launch_error_with_lines() {
        let logs = LaunchLogBuffer::default();
        logs.push_stdout("stdout line".to_string());
        logs.push_stderr("stderr line".to_string());
        let msg = moli_launch_error("Moli exited", &logs, Some("connect failed"));
        assert!(msg.contains("stdout line"));
        assert!(msg.contains("stderr line"));
        assert!(msg.contains("Last probe error: connect failed"));
    }

    #[test]
    fn moli_serve_enables_visual_automation() {
        assert_eq!(
            build_moli_serve_args(9222, &MoliLaunchOptions::default()),
            [
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                "9222",
                "--layout",
                "--image",
                "--font"
            ]
        );
    }

    #[test]
    fn moli_serve_maps_network_and_user_agent_options() {
        let options = MoliLaunchOptions {
            args: vec![
                "--timeout=30".into(),
                "--http-cache-dir".into(),
                "cache directory".into(),
            ],
            proxy: Some("http://127.0.0.1:8080".into()),
            proxy_bypass: Some("localhost,127.0.0.1".into()),
            user_agent: Some("Adapter probe".into()),
            ignore_https_errors: true,
            show_scrollbars: true,
            ..Default::default()
        };
        let args = build_moli_serve_args(9333, &options);
        assert_eq!(
            &args[8..],
            [
                "--http-proxy",
                "http://127.0.0.1:8080",
                "--http-no-proxy",
                "localhost,127.0.0.1",
                "--user-agent",
                "Adapter probe",
                "--insecure-disable-tls-host-verification",
                "--scrollbars",
                "--timeout=30",
                "--http-cache-dir",
                "cache directory"
            ]
        );
        assert_eq!(args[4], "9333");
    }

    #[test]
    fn moli_proxy_auth_preserves_escapes_and_explicit_ports() {
        for (proxy, username, password, expected) in [
            (
                "http://proxy.test:80",
                Some("user"),
                Some("p@ss:w/rd?#%"),
                "http://user:p%40ss%3Aw%2Frd%3F%23%25@proxy.test:80",
            ),
            (
                "https://[::1]:443",
                Some("user%40test"),
                Some("p%40ss%3A%2525"),
                "https://user%40test:p%40ss%3A%2525@[::1]:443",
            ),
            (
                "socks5://proxy.test:1080",
                Some("user"),
                None,
                "socks5://user:@proxy.test:1080",
            ),
            (
                "proxy.test:8080",
                None,
                Some("password"),
                "http://:password@proxy.test:8080",
            ),
            (
                "http://old:old@proxy.test:80",
                Some("user"),
                Some("pass"),
                "http://user:pass@proxy.test:80",
            ),
            (
                "http://user:p%40ss@proxy.test:80",
                None,
                None,
                "http://user:p%40ss@proxy.test:80",
            ),
        ] {
            let options = MoliLaunchOptions {
                proxy: Some(proxy.into()),
                proxy_username: username.map(String::from),
                proxy_password: password.map(String::from),
                ..Default::default()
            };
            let args = build_moli_serve_args(9222, &options);
            assert_eq!(&args[8..], ["--http-proxy", expected]);
        }
    }
}
