use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::cdp::chrome::LaunchOptions;

const CAMOUFOX_ADAPTER_SCRIPT: &str = include_str!("../../scripts/camoufox_adapter.py");

pub struct CamoufoxAdapter {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    script_path: PathBuf,
    next_id: u64,
}

impl CamoufoxAdapter {
    pub async fn launch(cmd: &Value) -> Result<Self, String> {
        let script = write_adapter_script()?;
        let python = std::env::var("AGENT_BROWSER_CAMOUFOX_PYTHON")
            .unwrap_or_else(|_| "python3".to_string());

        let mut child = Command::new(&python)
            .arg(&script)
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                let _ = fs::remove_file(&script);
                format!(
                    "Failed to start Camoufox adapter with `{}`: {}. \
                     Set AGENT_BROWSER_CAMOUFOX_PYTHON to a Python executable with camoufox installed.",
                    python, e
                )
            })?;

        let stdin = match child.stdin.take() {
            Some(stdin) => stdin,
            None => {
                let _ = child.kill().await;
                let _ = fs::remove_file(&script);
                return Err("Failed to open Camoufox adapter stdin".to_string());
            }
        };
        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill().await;
                let _ = fs::remove_file(&script);
                return Err("Failed to open Camoufox adapter stdout".to_string());
            }
        };

        let mut adapter = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            script_path: script,
            next_id: 1,
        };
        adapter.send("launch", cmd).await?;
        Ok(adapter)
    }

    pub async fn launch_from_options(options: &LaunchOptions) -> Result<Self, String> {
        let cmd = launch_command_from_options(options);
        Self::launch(&cmd).await
    }

    pub async fn send(&mut self, action: &str, cmd: &Value) -> Result<Value, String> {
        if let Some(status) = self.has_exited_status() {
            return Err(format!(
                "Camoufox adapter exited before `{}` ({})",
                action, status
            ));
        }

        let id = self.next_id.to_string();
        self.next_id += 1;

        let request = json!({
            "id": id,
            "action": action,
            "cmd": cmd,
        });
        let line = serde_json::to_string(&request)
            .map_err(|e| format!("Failed to encode Camoufox adapter request: {}", e))?;

        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to Camoufox adapter: {}", e))?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| format!("Failed to write to Camoufox adapter: {}", e))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("Failed to flush Camoufox adapter request: {}", e))?;

        let mut response = String::new();
        let n = self
            .stdout
            .read_line(&mut response)
            .await
            .map_err(|e| format!("Failed to read Camoufox adapter response: {}", e))?;
        if n == 0 {
            return Err("Camoufox adapter closed its output pipe".to_string());
        }

        let parsed: Value = serde_json::from_str(response.trim()).map_err(|e| {
            format!(
                "Camoufox adapter returned non-JSON response `{}`: {}",
                response.trim(),
                e
            )
        })?;

        if !parsed
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Err(parsed
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Camoufox adapter command failed")
                .to_string());
        }

        Ok(parsed.get("data").cloned().unwrap_or(Value::Null))
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let _ = self.send("close", &json!({})).await;
        let _ = self.child.kill().await;
        Ok(())
    }

    pub fn has_exited(&mut self) -> bool {
        self.has_exited_status().is_some()
    }

    fn has_exited_status(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.to_string()),
            Ok(None) => None,
            Err(e) => Some(format!("status check failed: {}", e)),
        }
    }
}

fn launch_command_from_options(options: &LaunchOptions) -> Value {
    let mut proxy = Value::Null;
    if let Some(ref server) = options.proxy {
        proxy = json!({
            "server": server,
            "username": options.proxy_username,
            "password": options.proxy_password,
        });
    }

    json!({
        "headless": options.headless,
        "executablePath": options.executable_path,
        "args": options.args,
        "proxy": proxy,
        "userAgent": options.user_agent,
        "storageState": options.storage_state,
        "ignoreHTTPSErrors": options.ignore_https_errors,
    })
}

impl Drop for CamoufoxAdapter {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = fs::remove_file(&self.script_path);
    }
}

fn write_adapter_script() -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!(
        "agent-browser-camoufox-adapter-{}.py",
        uuid::Uuid::new_v4()
    ));
    fs::write(&path, CAMOUFOX_ADAPTER_SCRIPT).map_err(|e| {
        format!(
            "Failed to write embedded Camoufox adapter script to {}: {}",
            path.display(),
            e
        )
    })?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_command_from_options_preserves_storage_state() {
        let options = LaunchOptions {
            storage_state: Some("/tmp/auth.json".to_string()),
            ignore_https_errors: true,
            user_agent: Some("agent-browser-test".to_string()),
            ..Default::default()
        };

        let cmd = launch_command_from_options(&options);

        assert_eq!(cmd["storageState"], "/tmp/auth.json");
        assert_eq!(cmd["ignoreHTTPSErrors"], true);
        assert_eq!(cmd["userAgent"], "agent-browser-test");
    }

    #[test]
    fn write_adapter_script_materializes_embedded_script() {
        let path = write_adapter_script().unwrap();
        let content = fs::read_to_string(&path).unwrap();

        assert!(path.starts_with(std::env::temp_dir()));
        assert!(content.contains("Camoufox sidecar for agent-browser"));

        let _ = fs::remove_file(path);
    }
}
