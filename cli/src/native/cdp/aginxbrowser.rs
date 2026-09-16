use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::launch_log::{start_log_drainers, wait_for_cdp_ready};

const AGINXBROWSER_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

pub struct AginxBrowserProcess {
    child: Child,
    pub ws_url: String,
    _log_drainers: Vec<std::thread::JoinHandle<()>>,
}

impl AginxBrowserProcess {
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Non-blocking check whether the engine process has exited (crashed or
    /// terminated), reaping the zombie when it has.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for AginxBrowserProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

#[derive(Default)]
pub struct AginxBrowserLaunchOptions {
    pub executable_path: Option<String>,
    pub proxy: Option<String>,
    pub port: Option<u16>,
    pub allow_file_access: bool,
}

/// AginxBrowser serves CDP on a loopback port chosen at launch; the proxy is
/// configured through the `AGINXBROWSER_PROXY` environment variable.
fn build_aginxbrowser_args(port: u16, allow_file_access: bool) -> Vec<String> {
    let mut args = vec!["--cdp-port".to_string(), port.to_string()];

    if allow_file_access {
        args.push("--allow-file-access".to_string());
    }

    args
}

pub fn find_aginxbrowser() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Ok(output) = Command::new("which").arg("aginxbrowser").output() {
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
        if let Ok(output) = Command::new("where").arg("aginxbrowser").output() {
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
        let candidates = [home.join(".local/bin/aginxbrowser")];
        for c in &candidates {
            if c.exists() {
                return Some(c.clone());
            }
        }
    }

    None
}

pub async fn launch_aginxbrowser(
    options: &AginxBrowserLaunchOptions,
) -> Result<AginxBrowserProcess, String> {
    let binary_path = match &options.executable_path {
        Some(p) => PathBuf::from(p),
        None => find_aginxbrowser().ok_or(
            "AginxBrowser not found. Install it from https://github.com/yinnho/aginxbrowser or use --executable-path.",
        )?,
    };

    let port = match options.port {
        Some(p) => p,
        None => TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .map_err(|e| format!("Failed to find an available port for AginxBrowser: {}", e))?,
    };
    let args = build_aginxbrowser_args(port, options.allow_file_access);

    let mut command = Command::new(&binary_path);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(proxy) = &options.proxy {
        command.env("AGINXBROWSER_PROXY", proxy);
    }

    let mut child = command
        .spawn()
        .map_err(|e| format!("Failed to launch AginxBrowser at {:?}: {}", binary_path, e))?;

    let (log_buffer, log_drainers) = start_log_drainers(&mut child, "AginxBrowser")?;

    let ws_url = match wait_for_cdp_ready(
        &mut child,
        port,
        &log_buffer,
        AGINXBROWSER_STARTUP_TIMEOUT,
        "AginxBrowser",
    )
    .await
    {
        Ok(url) => url,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };

    Ok(AginxBrowserProcess {
        child,
        ws_url,
        _log_drainers: log_drainers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener as TokioTcpListener;
    use tokio::time::Duration;

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

        let (logs, _drainers) = start_log_drainers(&mut child, "AginxBrowser").unwrap();
        let ws_url = wait_for_cdp_ready(
            &mut child,
            port,
            &logs,
            AGINXBROWSER_STARTUP_TIMEOUT,
            "AginxBrowser",
        )
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

        let (logs, _drainers) = start_log_drainers(&mut child, "AginxBrowser").unwrap();
        let err = wait_for_cdp_ready(
            &mut child,
            port,
            &logs,
            AGINXBROWSER_STARTUP_TIMEOUT,
            "AginxBrowser",
        )
        .await
        .unwrap_err();

        assert!(err.contains("AginxBrowser exited before CDP became ready"));
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
        let (logs, _drainers) = start_log_drainers(&mut child, "AginxBrowser").unwrap();
        let err = tokio::time::timeout(
            Duration::from_secs(2),
            wait_for_cdp_ready(&mut child, port, &logs, timeout, "AginxBrowser"),
        )
        .await
        .expect("ready wait should return before outer timeout")
        .unwrap_err();

        assert!(err.contains("Timed out after 300ms waiting for AginxBrowser CDP endpoint"));
        assert!(
            err.contains("Failed to connect to CDP") || err.contains("Timeout connecting to CDP")
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn test_find_aginxbrowser_returns_none_when_missing() {
        let _ = find_aginxbrowser();
    }

    #[test]
    fn test_default_options() {
        let opts = AginxBrowserLaunchOptions::default();
        assert!(opts.executable_path.is_none());
        assert!(opts.proxy.is_none());
        assert!(opts.port.is_none());
        assert!(!opts.allow_file_access);
    }

    #[test]
    fn test_build_aginxbrowser_args_uses_supported_options() {
        let args = build_aginxbrowser_args(9222, false);

        assert_eq!(args, vec!["--cdp-port".to_string(), "9222".to_string(),]);
    }

    #[test]
    fn test_build_aginxbrowser_args_with_file_access() {
        let args = build_aginxbrowser_args(9333, true);

        assert_eq!(
            args,
            vec![
                "--cdp-port".to_string(),
                "9333".to_string(),
                "--allow-file-access".to_string(),
            ]
        );
    }
}
