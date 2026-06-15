use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::chrome::LaunchOptions;

const HOST_SCRIPT: &str = include_str!("patchright_host.mjs");

pub struct PatchrightProcess {
    child: Child,
    pub ws_url: String,
    temp_user_data_dir: Option<PathBuf>,
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl PatchrightProcess {
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
        let _ = self.child.wait();
    }

    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    pub fn wait_or_kill(&mut self, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        self.kill();
    }
}

impl Drop for PatchrightProcess {
    fn drop(&mut self) {
        self.kill();
        if let Some(ref dir) = self.temp_user_data_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

pub fn launch_patchright(options: &LaunchOptions) -> Result<PatchrightProcess, String> {
    if options
        .extensions
        .as_ref()
        .map(|e| !e.is_empty())
        .unwrap_or(false)
    {
        return Err("Extensions are not supported with the patchright backend".to_string());
    }
    let root = patchright_backend_dir();
    let node_modules = root.join("node_modules").join("patchright");
    if !node_modules.exists() {
        return Err(format!(
            "Patchright backend is not installed. Run `agent-browser-priv install patchright` first.\nExpected: {}",
            node_modules.display()
        ));
    }

    fs::create_dir_all(&root)
        .map_err(|e| format!("Failed to create Patchright backend dir: {}", e))?;
    let host_path = root.join("patchright-host.mjs");
    fs::write(&host_path, HOST_SCRIPT)
        .map_err(|e| format!("Failed to write Patchright host script: {}", e))?;

    let port = free_local_port()?;
    let (profile, temp_user_data_dir) = if let Some(ref profile) = options.profile {
        (expand_tilde(profile), None)
    } else {
        let dir = std::env::temp_dir().join(format!(
            "agent-browser-priv-patchright-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create temp Patchright profile dir: {}", e))?;
        (dir.display().to_string(), Some(dir))
    };

    let args_json = serde_json::to_string(&options.args)
        .map_err(|e| format!("Failed to encode Patchright launch args: {}", e))?;

    let mut cmd = Command::new("node");
    cmd.arg(&host_path)
        .arg("--profile")
        .arg(&profile)
        .arg("--port")
        .arg(port.to_string())
        .arg("--headless")
        .arg(if options.headless { "true" } else { "false" })
        .arg("--args")
        .arg(args_json)
        .current_dir(&root)
        .env("NODE_PATH", root.join("node_modules"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    if let Some(ref path) = options.executable_path {
        cmd.arg("--executable-path").arg(path);
    }
    if let Some(ref ua) = options.user_agent {
        cmd.arg("--user-agent").arg(ua);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
    }

    let mut child = cmd.spawn().map_err(|e| {
        cleanup_temp_dir(&temp_user_data_dir);
        format!(
            "Failed to launch Patchright host with Node.js: {}. Install Node.js or use --backend chrome.",
            e
        )
    })?;

    let deadline = Instant::now() + Duration::from_secs(45);
    let ws_url = match wait_for_cdp_version(port, deadline) {
        Ok(url) => url,
        Err(e) => {
            let _ = child.kill();
            let stderr = read_stderr(&mut child);
            cleanup_temp_dir(&temp_user_data_dir);
            return Err(format!("{}\nPatchright host stderr:\n{}", e, stderr));
        }
    };

    #[cfg(unix)]
    let pgid = Some(child.id() as i32);

    Ok(PatchrightProcess {
        child,
        ws_url,
        temp_user_data_dir,
        #[cfg(unix)]
        pgid,
    })
}

pub fn patchright_backend_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agent-browser-priv")
        .join("backends")
        .join("patchright")
}

fn free_local_port() -> Result<u16, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("Failed to bind local port: {}", e))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Failed to read local port: {}", e))?
        .port();
    drop(listener);
    Ok(port)
}

fn wait_for_cdp_version(port: u16, deadline: Instant) -> Result<String, String> {
    while Instant::now() <= deadline {
        match fetch_json_version(port) {
            Ok(Some(ws_url)) => return Ok(ws_url),
            Ok(None) => {}
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "Timeout waiting for Patchright CDP endpoint on 127.0.0.1:{}",
        port
    ))
}

fn fetch_json_version(port: u16) -> Result<Option<String>, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .map_err(|e| format!("CDP port not ready: {}", e))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(b"GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .map_err(|e| e.to_string())?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|e| e.to_string())?;
    let Some(body_start) = response.find("\r\n\r\n") else {
        return Ok(None);
    };
    let body = &response[body_start + 4..];
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    Ok(value
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

fn read_stderr(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else {
        return "(stderr unavailable)".to_string();
    };
    let mut buf = String::new();
    let _ = stderr.read_to_string(&mut buf);
    if buf.trim().is_empty() {
        "(empty)".to_string()
    } else {
        buf
    }
}

fn cleanup_temp_dir(dir: &Option<PathBuf>) {
    if let Some(dir) = dir {
        let _ = fs::remove_dir_all(dir);
    }
}

fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir()
            .map(|h| h.display().to_string())
            .unwrap_or_else(|| path.to_string());
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    path.to_string()
}
