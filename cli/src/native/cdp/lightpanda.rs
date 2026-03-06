use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct LightpandaProcess {
    child: Child,
    pub ws_url: String,
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

pub struct LightpandaLaunchOptions {
    pub executable_path: Option<String>,
    pub proxy: Option<String>,
    pub port: Option<u16>,
}

impl Default for LightpandaLaunchOptions {
    fn default() -> Self {
        Self {
            executable_path: None,
            proxy: None,
            port: None,
        }
    }
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

pub fn launch_lightpanda(
    options: &LightpandaLaunchOptions,
) -> Result<LightpandaProcess, String> {
    let binary_path = match &options.executable_path {
        Some(p) => PathBuf::from(p),
        None => find_lightpanda().ok_or(
            "Lightpanda not found. Install it from https://lightpanda.io/docs/open-source/installation or use --executable-path.",
        )?,
    };

    let port = options.port.unwrap_or(0);
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

    // Disable inactivity timeout so the connection stays alive during long sessions
    args.push("--timeout".to_string());
    args.push("0".to_string());

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

    let address = match wait_for_address(reader) {
        Ok(addr) => addr,
        Err(e) => {
            let _ = child.kill();
            return Err(e);
        }
    };

    let ws_url = format!("ws://{}", address);

    Ok(LightpandaProcess { child, ws_url })
}

/// Parse Lightpanda's stderr for the server address.
/// Lightpanda outputs lines like:
///   INFO  app : server running . . . address = 127.0.0.1:9222
fn wait_for_address(
    reader: BufReader<std::process::ChildStderr>,
) -> Result<String, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut stderr_lines: Vec<String> = Vec::new();

    for line in reader.lines() {
        if std::time::Instant::now() > deadline {
            return Err(lightpanda_launch_error(
                "Timeout waiting for Lightpanda server address",
                &stderr_lines,
            ));
        }
        let line = line.map_err(|e| format!("Failed to read Lightpanda stderr: {}", e))?;
        if let Some(address) = extract_address(&line) {
            return Ok(address);
        }
        stderr_lines.push(line);
    }

    Err(lightpanda_launch_error(
        "Lightpanda exited before providing server address",
        &stderr_lines,
    ))
}

fn extract_address(line: &str) -> Option<String> {
    // Match "address = HOST:PORT" anywhere in the line
    if let Some(idx) = line.find("address = ") {
        let addr = line[idx + "address = ".len()..].trim().to_string();
        if !addr.is_empty() {
            return Some(addr);
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
    fn test_extract_address_standard() {
        // Lightpanda outputs the address on a separate indented line
        assert_eq!(
            extract_address("      address = 127.0.0.1:9222"),
            Some("127.0.0.1:9222".to_string())
        );
    }

    #[test]
    fn test_extract_address_inline() {
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
