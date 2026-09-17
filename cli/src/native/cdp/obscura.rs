use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::discovery::discover_cdp_url_with_timeout;

const OBSCURA_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const OBSCURA_POLL_INTERVAL: Duration = Duration::from_millis(100);
const OBSCURA_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_LOG_LINES: usize = 40;

pub struct ObscuraProcess {
    child: Child,
    pub ws_url: String,
    _log_drainers: Vec<std::thread::JoinHandle<()>>,
}

impl ObscuraProcess {
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

impl Drop for ObscuraProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[derive(Default)]
pub struct ObscuraLaunchOptions {
    pub executable_path: Option<String>,
    pub proxy: Option<String>,
    pub port: Option<u16>,
    /// Run Obscura with `--stealth` (privacy-first consistent browser
    /// fingerprint and tracker blocking). Requires a stealth-enabled engine build.
    pub stealth: bool,
}

/// Obscura's stealth mode is opt-in via the `AGENT_BROWSER_OBSCURA_STEALTH`
/// environment variable. Accepts `1`, `true`, `yes`, or `on`.
pub fn stealth_from_env() -> bool {
    std::env::var("AGENT_BROWSER_OBSCURA_STEALTH")
        .map(|v| stealth_flag_enabled(&v))
        .unwrap_or(false)
}

fn stealth_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn build_obscura_serve_args(port: u16, proxy: Option<&str>, stealth: bool) -> Vec<String> {
    let mut args = vec![
        "serve".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
    ];

    if let Some(proxy) = proxy {
        args.push("--proxy".to_string());
        args.push(proxy.to_string());
    }

    if stealth {
        args.push("--stealth".to_string());
    }

    args
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

pub fn find_obscura() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(output) = Command::new("which").arg("obscura").output() {
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
        if let Ok(output) = Command::new("where").arg("obscura").output() {
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

    if let Some(home) = dirs::home_dir() {
        let candidates = [
            home.join(".obscura/obscura"),
            home.join(".local/bin/obscura"),
            home.join(".cargo/bin/obscura"),
        ];
        for c in &candidates {
            if c.exists() {
                return Some(c.clone());
            }
        }
    }

    None
}

pub async fn launch_obscura(options: &ObscuraLaunchOptions) -> Result<ObscuraProcess, String> {
    let binary_path = match &options.executable_path {
        Some(p) => PathBuf::from(p),
        None => find_obscura().ok_or(
            "Obscura not found. Install it from https://github.com/h4ckf0r0day/obscura or use --executable-path.",
        )?,
    };

    let port = match options.port {
        Some(p) => p,
        None => TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("Failed to find an available port for Obscura: {}", e))?,
    };
    let args = build_obscura_serve_args(port, options.proxy.as_deref(), options.stealth);

    let mut child = Command::new(&binary_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch Obscura at {:?}: {}", binary_path, e))?;

    let (log_buffer, log_drainers) = start_log_drainers(&mut child)?;

    let mut process = ObscuraProcess {
        child,
        ws_url: String::new(),
        _log_drainers: log_drainers,
    };
    process.ws_url = wait_for_obscura_ready(
        &mut process.child,
        port,
        &log_buffer,
        OBSCURA_STARTUP_TIMEOUT,
    )
    .await?;

    Ok(process)
}

fn start_log_drainers(
    child: &mut Child,
) -> Result<(LaunchLogBuffer, Vec<std::thread::JoinHandle<()>>), String> {
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "Failed to capture Obscura stdout".to_string()
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.kill();
        let _ = child.wait();
        "Failed to capture Obscura stderr".to_string()
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

async fn wait_for_obscura_ready(
    child: &mut Child,
    port: u16,
    logs: &LaunchLogBuffer,
    startup_timeout: Duration,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + startup_timeout;
    let mut last_probe_error = None;

    loop {
        if let Ok(Some(status)) = child.try_wait() {
            // Give the drainer threads a brief window to flush the last log lines
            // before we snapshot them.  This is best-effort: lines written just
            // before exit may still be missing, but the most useful output (early
            // startup errors) will already be in the buffer.
            tokio::time::sleep(Duration::from_millis(25)).await;
            return Err(obscura_launch_error(
                &format!(
                    "Obscura exited before CDP became ready (status: {})",
                    status
                ),
                logs,
                last_probe_error.as_deref(),
            ));
        }

        if std::time::Instant::now() >= deadline {
            return Err(obscura_launch_error(
                &format!(
                    "Timed out after {}ms waiting for Obscura CDP endpoint on port {}",
                    startup_timeout.as_millis(),
                    port
                ),
                logs,
                last_probe_error.as_deref(),
            ));
        }

        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        match tokio::time::timeout(
            remaining,
            discover_cdp_url_with_timeout("127.0.0.1", port, None, OBSCURA_DISCOVERY_TIMEOUT),
        )
        .await
        {
            Ok(Ok(ws_url)) => return Ok(ws_url),
            Ok(Err(err)) => last_probe_error = Some(err),
            Err(_) => last_probe_error = Some("CDP discovery exceeded the startup deadline".into()),
        }

        tokio::time::sleep(
            OBSCURA_POLL_INTERVAL
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        )
        .await;
    }
}

fn obscura_launch_error(
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
            "Obscura stderr (last {} lines):\n  {}",
            stderr_lines.len(),
            stderr_lines.join("\n  ")
        ));
    }

    if !stdout_lines.is_empty() {
        details.push(format!(
            "Obscura stdout (last {} lines):\n  {}",
            stdout_lines.len(),
            stdout_lines.join("\n  ")
        ));
    }

    if details.is_empty() {
        format!("{} (no stdout/stderr output from Obscura)", message)
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
        let ws_url = wait_for_obscura_ready(&mut child, port, &logs, OBSCURA_STARTUP_TIMEOUT)
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
        let err = wait_for_obscura_ready(&mut child, port, &logs, OBSCURA_STARTUP_TIMEOUT)
            .await
            .unwrap_err();

        assert!(err.contains("Obscura exited before CDP became ready"));
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
            wait_for_obscura_ready(&mut child, port, &logs, timeout),
        )
        .await
        .expect("ready wait should return before outer timeout")
        .unwrap_err();

        assert!(err.contains("Timed out after 300ms waiting for Obscura CDP endpoint"));
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
        let mut process = ObscuraProcess::for_test(child, String::new());
        let logs = LaunchLogBuffer::default();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_obscura_ready(&mut process.child, port, &logs, Duration::from_millis(100)),
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
    fn test_obscura_process_reports_exit() {
        let child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let mut process = ObscuraProcess {
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
        let error = launch_obscura(&ObscuraLaunchOptions {
            executable_path: Some(
                dir.path()
                    .join("missing-obscura")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ..Default::default()
        })
        .await
        .err()
        .expect("missing explicit executable must fail");
        assert!(error.contains("Failed to launch Obscura"), "{error}");
        assert!(error.contains("missing-obscura"), "{error}");
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
    async fn cancelled_obscura_launch_reaps_child() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("obscura");
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
        let options = ObscuraLaunchOptions {
            executable_path: Some(executable.to_string_lossy().into_owned()),
            ..Default::default()
        };
        let mut launch = Box::pin(launch_obscura(&options));
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
    fn test_obscura_launch_error_no_logs() {
        let logs = LaunchLogBuffer::default();
        let msg = obscura_launch_error("Obscura exited", &logs, None);
        assert!(msg.contains("no stdout/stderr output"));
    }

    #[test]
    fn test_obscura_launch_error_with_lines() {
        let logs = LaunchLogBuffer::default();
        logs.push_stdout("stdout line".to_string());
        logs.push_stderr("stderr line".to_string());
        let msg = obscura_launch_error("Obscura exited", &logs, Some("connect failed"));
        assert!(msg.contains("stdout line"));
        assert!(msg.contains("stderr line"));
        assert!(msg.contains("Last probe error: connect failed"));
    }

    #[test]
    fn test_default_options() {
        let opts = ObscuraLaunchOptions::default();
        assert!(opts.executable_path.is_none());
        assert!(opts.proxy.is_none());
        assert!(opts.port.is_none());
        assert!(!opts.stealth);
    }

    #[test]
    fn test_build_obscura_serve_args_minimal() {
        let args = build_obscura_serve_args(9222, None, false);

        assert_eq!(
            args,
            vec![
                "serve".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "9222".to_string(),
            ]
        );
    }

    #[test]
    fn test_build_obscura_serve_args_with_proxy() {
        let args = build_obscura_serve_args(9333, Some("http://127.0.0.1:8080"), false);

        assert_eq!(
            args,
            vec![
                "serve".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "9333".to_string(),
                "--proxy".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ]
        );
    }

    #[test]
    fn test_build_obscura_serve_args_with_stealth() {
        let args = build_obscura_serve_args(9222, None, true);

        assert_eq!(
            args,
            vec![
                "serve".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "9222".to_string(),
                "--stealth".to_string(),
            ]
        );
    }

    #[test]
    fn test_stealth_flag_enabled_parsing() {
        for v in ["1", "true", "TRUE", "yes", "on", " on "] {
            assert!(stealth_flag_enabled(v), "{v:?} should enable stealth");
        }
        for v in ["0", "false", "no", "off", ""] {
            assert!(!stealth_flag_enabled(v), "{v:?} should not enable stealth");
        }
    }
}
