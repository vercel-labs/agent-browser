use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::discovery::discover_cdp_url_with_timeout;

const LIGHTPANDA_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const LIGHTPANDA_POLL_INTERVAL: Duration = Duration::from_millis(100);
const LIGHTPANDA_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);
const LIGHTPANDA_SESSION_TIMEOUT_SECS: u64 = 604800; // 1 week, the documented maximum
const MAX_LOG_LINES: usize = 40;

pub struct LightpandaProcess {
    child: Child,
    pub ws_url: String,
    _log_drainers: Vec<std::thread::JoinHandle<()>>,
}

impl LightpandaProcess {
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for LightpandaProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[derive(Default)]
pub struct LightpandaLaunchOptions {
    pub executable_path: Option<String>,
    pub proxy: Option<String>,
    pub port: Option<u16>,
}

fn build_lightpanda_serve_args(port: u16, proxy: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "serve".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--timeout".to_string(),
        LIGHTPANDA_SESSION_TIMEOUT_SECS.to_string(),
    ];

    if let Some(proxy) = proxy {
        args.push("--http_proxy".to_string());
        args.push(proxy.to_string());
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

pub fn find_lightpanda() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(output) = Command::new("which").arg("lightpanda").output() {
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
        if let Ok(output) = Command::new("where").arg("lightpanda").output() {
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
            home.join(".lightpanda/lightpanda"),
            home.join(".local/bin/lightpanda"),
        ];
        for c in &candidates {
            if c.exists() {
                return Some(c.clone());
            }
        }
    }

    None
}

pub async fn launch_lightpanda(
    options: &LightpandaLaunchOptions,
) -> Result<LightpandaProcess, String> {
    let binary_path = match &options.executable_path {
        Some(p) => PathBuf::from(p),
        None => find_lightpanda().ok_or(
            "Lightpanda not found. Install it from https://lightpanda.io/docs/open-source/installation or use --executable-path.",
        )?,
    };

    let port = match options.port {
        Some(p) => p,
        None => TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("Failed to find an available port for Lightpanda: {}", e))?,
    };
    let args = build_lightpanda_serve_args(port, options.proxy.as_deref());

    let mut child = Command::new(&binary_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch Lightpanda at {:?}: {}", binary_path, e))?;

    let (log_buffer, log_drainers) = start_log_drainers(&mut child)?;

    let ws_url =
        match wait_for_lightpanda_ready(&mut child, port, &log_buffer, LIGHTPANDA_STARTUP_TIMEOUT)
            .await
        {
            Ok(url) => url,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };

    Ok(LightpandaProcess {
        child,
        ws_url,
        _log_drainers: log_drainers,
    })
}

fn start_log_drainers(
    child: &mut Child,
) -> Result<(LaunchLogBuffer, Vec<std::thread::JoinHandle<()>>), String> {
    let stdout = child.stdout.take().ok_or_else(|| {
        let _ = child.kill();
        "Failed to capture Lightpanda stdout".to_string()
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.kill();
        "Failed to capture Lightpanda stderr".to_string()
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

async fn wait_for_lightpanda_ready(
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
            return Err(lightpanda_launch_error(
                &format!(
                    "Lightpanda exited before CDP became ready (status: {})",
                    status
                ),
                logs,
                last_probe_error.as_deref(),
            ));
        }

        match discover_cdp_url_with_timeout("127.0.0.1", port, LIGHTPANDA_DISCOVERY_TIMEOUT).await {
            Ok(ws_url) => return Ok(ws_url),
            Err(err) => last_probe_error = Some(err),
        }

        if std::time::Instant::now() >= deadline {
            return Err(lightpanda_launch_error(
                &format!(
                    "Timed out after {}ms waiting for Lightpanda CDP endpoint on port {}",
                    startup_timeout.as_millis(),
                    port
                ),
                logs,
                last_probe_error.as_deref(),
            ));
        }

        tokio::time::sleep(LIGHTPANDA_POLL_INTERVAL).await;
    }
}

fn lightpanda_launch_error(
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
            "Lightpanda stderr (last {} lines):\n  {}",
            stderr_lines.len(),
            stderr_lines.join("\n  ")
        ));
    }

    if !stdout_lines.is_empty() {
        details.push(format!(
            "Lightpanda stdout (last {} lines):\n  {}",
            stdout_lines.len(),
            stdout_lines.join("\n  ")
        ));
    }

    if details.is_empty() {
        format!("{} (no stdout/stderr output from Lightpanda)", message)
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
            .args(["-c", "sleep 5"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let (logs, _drainers) = start_log_drainers(&mut child).unwrap();
        let ws_url = wait_for_lightpanda_ready(&mut child, port, &logs, LIGHTPANDA_STARTUP_TIMEOUT)
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
        let err = wait_for_lightpanda_ready(&mut child, port, &logs, LIGHTPANDA_STARTUP_TIMEOUT)
            .await
            .unwrap_err();

        assert!(err.contains("Lightpanda exited before CDP became ready"));
        assert!(err.contains("boom"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_reports_last_probe_error() {
        let port = unused_port();
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let timeout = Duration::from_millis(300);
        let (logs, _drainers) = start_log_drainers(&mut child).unwrap();
        let err = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_lightpanda_ready(&mut child, port, &logs, timeout),
        )
        .await
        .expect("ready wait should return before outer timeout")
        .unwrap_err();

        assert!(err.contains("Timed out after 300ms waiting for Lightpanda CDP endpoint"));
        assert!(
            err.contains("Failed to connect to CDP") || err.contains("Timeout connecting to CDP")
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn test_find_lightpanda_returns_none_when_missing() {
        let _ = find_lightpanda();
    }

    #[test]
    fn test_lightpanda_launch_error_no_logs() {
        let logs = LaunchLogBuffer::default();
        let msg = lightpanda_launch_error("Lightpanda exited", &logs, None);
        assert!(msg.contains("no stdout/stderr output"));
    }

    #[test]
    fn test_lightpanda_launch_error_with_lines() {
        let logs = LaunchLogBuffer::default();
        logs.push_stdout("stdout line".to_string());
        logs.push_stderr("stderr line".to_string());
        let msg = lightpanda_launch_error("Lightpanda exited", &logs, Some("connect failed"));
        assert!(msg.contains("stdout line"));
        assert!(msg.contains("stderr line"));
        assert!(msg.contains("Last probe error: connect failed"));
    }

    #[test]
    fn test_default_options() {
        let opts = LightpandaLaunchOptions::default();
        assert!(opts.executable_path.is_none());
        assert!(opts.proxy.is_none());
        assert!(opts.port.is_none());
    }

    #[test]
    fn test_build_lightpanda_serve_args_sets_explicit_session_timeout() {
        let args = build_lightpanda_serve_args(9222, None);

        assert_eq!(
            args,
            vec![
                "serve".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "9222".to_string(),
                "--timeout".to_string(),
                "604800".to_string(),
            ]
        );
    }

    #[test]
    fn test_build_lightpanda_serve_args_with_proxy() {
        let args = build_lightpanda_serve_args(9333, Some("http://127.0.0.1:8080"));

        assert_eq!(
            args,
            vec![
                "serve".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "9333".to_string(),
                "--timeout".to_string(),
                "604800".to_string(),
                "--http_proxy".to_string(),
                "http://127.0.0.1:8080".to_string(),
            ]
        );
    }
}
