use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct LightpandaProcess {
    child: Child,
    pub ws_url: String,
    _stderr_drain: Option<std::thread::JoinHandle<()>>,
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

pub fn find_lightpanda() -> Option<PathBuf> {
    // Check PATH via `which`
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

    // Common install locations
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

    // npm package binary: @lightpanda/browser installs to node_modules/.bin
    // Not checked here since the user would typically have it in PATH.

    None
}

pub fn launch_lightpanda(options: &LightpandaLaunchOptions) -> Result<LightpandaProcess, String> {
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
    let port_str = port.to_string();

    let mut args = vec![
        "serve".to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port_str,
    ];

    if let Some(ref proxy) = options.proxy {
        args.push("--http_proxy".to_string());
        args.push(proxy.clone());
    }

    // Use the maximum inactivity timeout (1 week) so the connection stays alive
    // during long sessions. Lightpanda treats 0 as "timeout immediately" rather
    // than "no timeout".
    args.push("--timeout".to_string());
    args.push("604800".to_string());

    // Lightpanda defaults to --log_level warn, which suppresses the "server
    // running" info message we need to detect the listen address.
    args.push("--log_level".to_string());
    args.push("info".to_string());

    let mut child = Command::new(&binary_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to launch Lightpanda at {:?}: {}", binary_path, e))?;

    // Lightpanda logs to stderr
    let stderr = child.stderr.take().ok_or_else(|| {
        let _ = child.kill();
        "Failed to capture Lightpanda stderr".to_string()
    })?;
    let reader = BufReader::new(stderr);

    let (address, reader) = match wait_for_address(reader) {
        Ok(result) => result,
        Err(e) => {
            let _ = child.kill();
            return Err(e);
        }
    };

    let ws_url = format!("ws://{}", address);

    let drain = std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = String::new();
        loop {
            buf.clear();
            match reader.read_line(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });

    Ok(LightpandaProcess {
        child,
        ws_url,
        _stderr_drain: Some(drain),
    })
}

/// Parse Lightpanda's stderr for the server address.
/// Lightpanda outputs lines like:
///   INFO  app : server running . . . address = 127.0.0.1:9222
///
/// Returns the address and the reader so the caller can keep the pipe alive.
fn wait_for_address(
    mut reader: BufReader<std::process::ChildStderr>,
) -> Result<(String, BufReader<std::process::ChildStderr>), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut stderr_lines: Vec<String> = Vec::new();
    let mut buf = String::new();

    loop {
        if std::time::Instant::now() > deadline {
            return Err(lightpanda_launch_error(
                "Timeout waiting for Lightpanda server address",
                &stderr_lines,
            ));
        }
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => {
                return Err(lightpanda_launch_error(
                    "Lightpanda exited before providing server address",
                    &stderr_lines,
                ));
            }
            Ok(_) => {
                let line = buf.trim_end().to_string();
                if let Some(address) = extract_address(&line) {
                    return Ok((address, reader));
                }
                stderr_lines.push(line);
            }
            Err(e) => {
                return Err(format!("Failed to read Lightpanda stderr: {}", e));
            }
        }
    }
}

fn extract_address(line: &str) -> Option<String> {
    // Lightpanda uses logfmt (`address=...`) in release, pretty (`address = ...`) in debug.
    for pattern in &["address=", "address = "] {
        if let Some(idx) = line.find(pattern) {
            let addr = line[idx + pattern.len()..].trim().to_string();
            // logfmt lines may have subsequent key=value pairs
            let addr = addr.split_whitespace().next().unwrap_or("").to_string();
            if !addr.is_empty() {
                return Some(addr);
            }
        }
    }
    None
}

fn lightpanda_launch_error(message: &str, stderr_lines: &[String]) -> String {
    if stderr_lines.is_empty() {
        return format!("{} (no stderr output from Lightpanda)", message);
    }

    let last_lines: Vec<&String> = stderr_lines.iter().rev().take(5).collect();
    format!(
        "{}\nLightpanda stderr (last {} lines):\n  {}",
        message,
        last_lines.len(),
        last_lines
            .into_iter()
            .rev()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_address_pretty_debug_build() {
        assert_eq!(
            extract_address("      address = 127.0.0.1:9222"),
            Some("127.0.0.1:9222".to_string())
        );
    }

    #[test]
    fn test_extract_address_logfmt_release_build() {
        assert_eq!(
            extract_address(
                "$time=1234 $scope=app $level=info $msg=\"server running\" address=127.0.0.1:9222"
            ),
            Some("127.0.0.1:9222".to_string())
        );
    }

    #[test]
    fn test_extract_address_pretty_inline() {
        assert_eq!(
            extract_address("INFO  app : server running address = 127.0.0.1:4567"),
            Some("127.0.0.1:4567".to_string())
        );
    }

    #[test]
    fn test_extract_address_no_match() {
        assert_eq!(extract_address("INFO  app : starting up..."), None);
    }

    #[test]
    fn test_find_lightpanda_returns_none_when_missing() {
        // On most CI/dev machines Lightpanda won't be installed
        // Just verify the function doesn't panic
        let _ = find_lightpanda();
    }

    #[test]
    fn test_lightpanda_launch_error_no_stderr() {
        let msg = lightpanda_launch_error("Lightpanda exited", &[]);
        assert!(msg.contains("no stderr output"));
    }

    #[test]
    fn test_lightpanda_launch_error_with_lines() {
        let lines = vec![
            "INFO starting up".to_string(),
            "ERROR bind failed: address in use".to_string(),
        ];
        let msg = lightpanda_launch_error("Lightpanda exited", &lines);
        assert!(msg.contains("bind failed"));
        assert!(msg.contains("last 2 lines"));
    }

    #[test]
    fn test_default_options() {
        let opts = LightpandaLaunchOptions::default();
        assert!(opts.executable_path.is_none());
        assert!(opts.proxy.is_none());
        assert!(opts.port.is_none());
    }
}
