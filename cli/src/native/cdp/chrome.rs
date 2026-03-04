use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::types::BrowserVersionInfo;

pub struct ChromeProcess {
    child: Child,
    pub ws_url: String,
    temp_user_data_dir: Option<PathBuf>,
}

impl ChromeProcess {
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChromeProcess {
    fn drop(&mut self) {
        self.kill();
        if let Some(ref dir) = self.temp_user_data_dir {
            for attempt in 0..3 {
                match std::fs::remove_dir_all(dir) {
                    Ok(()) => break,
                    Err(_) if attempt < 2 => {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: failed to clean up temp profile {}: {}",
                            dir.display(),
                            e
                        );
                    }
                }
            }
        }
    }
}

pub struct LaunchOptions {
    pub headless: bool,
    pub executable_path: Option<String>,
    pub proxy: Option<String>,
    pub proxy_bypass: Option<String>,
    pub profile: Option<String>,
    pub args: Vec<String>,
    pub allow_file_access: bool,
    pub extensions: Option<Vec<String>>,
    pub storage_state: Option<String>,
    pub user_agent: Option<String>,
    pub ignore_https_errors: bool,
    pub color_scheme: Option<String>,
    pub download_path: Option<String>,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            headless: true,
            executable_path: None,
            proxy: None,
            proxy_bypass: None,
            profile: None,
            args: Vec::new(),
            allow_file_access: false,
            extensions: None,
            storage_state: None,
            user_agent: None,
            ignore_https_errors: false,
            color_scheme: None,
            download_path: None,
        }
    }
}

struct ChromeArgs {
    args: Vec<String>,
    temp_user_data_dir: Option<PathBuf>,
}

fn build_chrome_args(options: &LaunchOptions) -> Result<ChromeArgs, String> {
    let mut args = vec![
        "--remote-debugging-port=0".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-backgrounding-occluded-windows".to_string(),
        "--disable-component-update".to_string(),
        "--disable-default-apps".to_string(),
        "--disable-hang-monitor".to_string(),
        "--disable-popup-blocking".to_string(),
        "--disable-prompt-on-repost".to_string(),
        "--disable-sync".to_string(),
        "--enable-features=NetworkService,NetworkServiceInProcess".to_string(),
        "--metrics-recording-only".to_string(),
        "--password-store=basic".to_string(),
        "--use-mock-keychain".to_string(),
    ];

    if options.headless {
        args.push("--headless=new".to_string());
    }

    if let Some(ref proxy) = options.proxy {
        args.push(format!("--proxy-server={}", proxy));
    }

    if let Some(ref bypass) = options.proxy_bypass {
        args.push(format!("--proxy-bypass-list={}", bypass));
    }

    let temp_user_data_dir = if let Some(ref profile) = options.profile {
        let expanded = expand_tilde(profile);
        args.push(format!("--user-data-dir={}", expanded));
        None
    } else {
        let dir = std::env::temp_dir()
            .join(format!("agent-browser-chrome-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create temp profile dir: {}", e))?;
        args.push(format!("--user-data-dir={}", dir.display()));
        Some(dir)
    };

    if options.allow_file_access {
        args.push("--allow-file-access-from-files".to_string());
        args.push("--allow-file-access".to_string());
    }

    if let Some(ref exts) = options.extensions {
        if !exts.is_empty() {
            let ext_list = exts.join(",");
            args.push(format!("--load-extension={}", ext_list));
            args.push(format!("--disable-extensions-except={}", ext_list));
        }
    }

    let has_window_size = options
        .args
        .iter()
        .any(|a| a.starts_with("--start-maximized") || a.starts_with("--window-size="));

    if !has_window_size && options.headless {
        args.push("--window-size=1280,720".to_string());
    }

    args.extend(options.args.iter().cloned());

    if should_disable_sandbox(&args) {
        args.push("--no-sandbox".to_string());
    }

    Ok(ChromeArgs {
        args,
        temp_user_data_dir,
    })
}

pub fn launch_chrome(options: &LaunchOptions) -> Result<ChromeProcess, String> {
    let chrome_path = match &options.executable_path {
        Some(p) => PathBuf::from(p),
        None => {
            find_chrome().ok_or("Chrome not found. Install Chrome or use --executable-path.")?
        }
    };

    let ChromeArgs {
        args,
        temp_user_data_dir,
    } = build_chrome_args(options)?;

    let cleanup_temp_dir = |dir: &Option<PathBuf>| {
        if let Some(ref d) = dir {
            let _ = std::fs::remove_dir_all(d);
        }
    };

    let mut child = Command::new(&chrome_path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            cleanup_temp_dir(&temp_user_data_dir);
            format!("Failed to launch Chrome at {:?}: {}", chrome_path, e)
        })?;

    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| {
            let _ = child.kill();
            cleanup_temp_dir(&temp_user_data_dir);
            "Failed to capture Chrome stderr".to_string()
        })?;
    let reader = BufReader::new(stderr);

    let ws_url = match wait_for_ws_url(reader) {
        Ok(url) => url,
        Err(e) => {
            let _ = child.kill();
            cleanup_temp_dir(&temp_user_data_dir);
            return Err(e);
        }
    };

    Ok(ChromeProcess {
        child,
        ws_url,
        temp_user_data_dir,
    })
}

fn wait_for_ws_url(reader: BufReader<std::process::ChildStderr>) -> Result<String, String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let prefix = "DevTools listening on ";
    let mut stderr_lines: Vec<String> = Vec::new();

    for line in reader.lines() {
        if std::time::Instant::now() > deadline {
            return Err(chrome_launch_error(
                "Timeout waiting for Chrome DevTools URL",
                &stderr_lines,
            ));
        }
        let line = line.map_err(|e| format!("Failed to read Chrome stderr: {}", e))?;
        if let Some(url) = line.strip_prefix(prefix) {
            return Ok(url.trim().to_string());
        }
        stderr_lines.push(line);
    }

    Err(chrome_launch_error(
        "Chrome exited before providing DevTools URL",
        &stderr_lines,
    ))
}

fn chrome_launch_error(message: &str, stderr_lines: &[String]) -> String {
    let relevant: Vec<&String> = stderr_lines
        .iter()
        .filter(|l| {
            let lower = l.to_lowercase();
            lower.contains("error")
                || lower.contains("fatal")
                || lower.contains("sandbox")
                || lower.contains("namespace")
                || lower.contains("permission")
                || lower.contains("cannot")
                || lower.contains("failed")
                || lower.contains("abort")
        })
        .collect();

    if relevant.is_empty() {
        if stderr_lines.is_empty() {
            return format!("{} (no stderr output from Chrome)", message);
        }
        let last_lines: Vec<&String> = stderr_lines.iter().rev().take(5).collect();
        return format!(
            "{}\nChrome stderr (last {} lines):\n  {}",
            message,
            last_lines.len(),
            last_lines
                .into_iter()
                .rev()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    let hint = if relevant.iter().any(|l| {
        let lower = l.to_lowercase();
        lower.contains("sandbox") || lower.contains("namespace")
    }) {
        "\nHint: try --args \"--no-sandbox\" (required in containers, VMs, and some Linux setups)"
    } else {
        ""
    };

    format!(
        "{}\nChrome stderr:\n  {}{}",
        message,
        relevant
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
        hint
    )
}

pub fn find_chrome() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ];
        for c in &candidates {
            let p = PathBuf::from(c);
            if p.exists() {
                return Some(p);
            }
        }

        if let Some(p) = find_playwright_chromium() {
            return Some(p);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = [
            "google-chrome",
            "google-chrome-stable",
            "chromium-browser",
            "chromium",
        ];
        for name in &candidates {
            if let Ok(output) = Command::new("which").arg(name).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some(PathBuf::from(path));
                    }
                }
            }
        }

        if let Some(p) = find_playwright_chromium() {
            return Some(p);
        }
    }

    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ];
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let p = PathBuf::from(&local).join(r"Google\Chrome\Application\chrome.exe");
            if p.exists() {
                return Some(p);
            }
        }
        for c in &candidates {
            let p = PathBuf::from(c);
            if p.exists() {
                return Some(p);
            }
        }
    }

    None
}

pub async fn discover_cdp_url(port: u16) -> Result<String, String> {
    let url = format!("http://127.0.0.1:{}/json/version", port);

    let body = tokio::time::timeout(Duration::from_secs(2), async {
        reqwest_get_string(&url).await
    })
    .await
    .map_err(|_| format!("Timeout connecting to CDP on port {}", port))?
    .map_err(|e| format!("Failed to connect to CDP on port {}: {}", port, e))?;

    let info: BrowserVersionInfo = serde_json::from_str(&body)
        .map_err(|e| format!("Invalid /json/version response: {}", e))?;

    info.web_socket_debugger_url
        .ok_or_else(|| format!("No webSocketDebuggerUrl in /json/version on port {}", port))
}

async fn reqwest_get_string(url: &str) -> Result<String, String> {
    let client = tokio::net::TcpStream::connect(
        url.strip_prefix("http://")
            .unwrap_or(url)
            .split('/')
            .next()
            .unwrap_or("127.0.0.1:9222"),
    )
    .await
    .map_err(|e| e.to_string())?;

    let path = url
        .find('/')
        .and_then(|i| url[i..].find('/').map(|j| &url[i + j..]))
        .unwrap_or("/json/version");

    let host = url
        .strip_prefix("http://")
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("127.0.0.1");

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        path, host
    );

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut client = client;
    client
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;

    let mut response = Vec::new();
    client
        .read_to_end(&mut response)
        .await
        .map_err(|e| e.to_string())?;

    let response_str = String::from_utf8_lossy(&response);
    let body = response_str
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or("")
        .to_string();

    Ok(body)
}

pub fn read_devtools_active_port(user_data_dir: &Path) -> Option<(u16, String)> {
    let path = user_data_dir.join("DevToolsActivePort");
    let content = std::fs::read_to_string(&path).ok()?;
    let mut lines = content.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let ws_path = lines
        .next()
        .unwrap_or("/devtools/browser")
        .trim()
        .to_string();
    Some((port, ws_path))
}

pub async fn auto_connect_cdp() -> Result<String, String> {
    let user_data_dirs = get_chrome_user_data_dirs();

    for dir in &user_data_dirs {
        if let Some((port, ws_path)) = read_devtools_active_port(dir) {
            // Try HTTP endpoint first (pre-M144)
            if let Ok(ws_url) = discover_cdp_url(port).await {
                return Ok(ws_url);
            }
            // M144+: direct WebSocket
            let ws_url = format!("ws://127.0.0.1:{}{}", port, ws_path);
            return Ok(ws_url);
        }
    }

    // Fallback: probe common ports
    for port in [9222u16, 9229] {
        if let Ok(ws_url) = discover_cdp_url(port).await {
            return Ok(ws_url);
        }
    }

    Err("No running Chrome instance found. Launch Chrome with --remote-debugging-port or use --cdp.".to_string())
}

fn get_chrome_user_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let base = home.join("Library/Application Support");
            for name in ["Google/Chrome", "Google/Chrome Canary", "Chromium"] {
                dirs.push(base.join(name));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            let config = home.join(".config");
            for name in ["google-chrome", "google-chrome-unstable", "chromium"] {
                dirs.push(config.join(name));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let base = PathBuf::from(local);
            for name in [
                r"Google\Chrome\User Data",
                r"Google\Chrome SxS\User Data",
                r"Chromium\User Data",
            ] {
                dirs.push(base.join(name));
            }
        }
    }

    dirs
}

/// Returns true if Chrome's sandbox should be disabled because the environment
/// doesn't support it (containers, VMs, running as root).
fn should_disable_sandbox(existing_args: &[String]) -> bool {
    if existing_args.iter().any(|a| a == "--no-sandbox") {
        return false; // already set by user
    }

    #[cfg(unix)]
    {
        // Root user -- standard container default, Chrome sandbox requires non-root
        if unsafe { libc::geteuid() } == 0 {
            return true;
        }

        // Docker container
        if Path::new("/.dockerenv").exists() {
            return true;
        }

        // Podman container
        if Path::new("/run/.containerenv").exists() {
            return true;
        }

        // Generic container detection: cgroup contains docker/kubepods/lxc
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("docker")
                || cgroup.contains("kubepods")
                || cgroup.contains("lxc")
            {
                return true;
            }
        }
    }

    false
}

/// Search Playwright's browser cache for a Chromium binary.
/// This is where `agent-browser install` (via `npx playwright install chromium`) puts it.
fn find_playwright_chromium() -> Option<PathBuf> {
    let mut search_dirs = Vec::new();

    if let Ok(custom) = std::env::var("PLAYWRIGHT_BROWSERS_PATH") {
        search_dirs.push(PathBuf::from(custom));
    }

    if let Some(home) = dirs::home_dir() {
        search_dirs.push(home.join(".cache/ms-playwright"));
    }

    for dir in &search_dirs {
        if !dir.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut matches: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.starts_with("chromium-"))
                        .unwrap_or(false)
                })
                .filter_map(|e| {
                    let candidate = build_playwright_binary_path(&e.path());
                    if candidate.exists() {
                        Some(candidate)
                    } else {
                        None
                    }
                })
                .collect();
            // Sort descending so the newest version wins
            matches.sort();
            matches.reverse();
            if let Some(p) = matches.into_iter().next() {
                return Some(p);
            }
        }
    }

    None
}

#[cfg(target_os = "linux")]
fn build_playwright_binary_path(chromium_dir: &Path) -> PathBuf {
    chromium_dir.join("chrome-linux64/chrome")
}

#[cfg(target_os = "macos")]
fn build_playwright_binary_path(chromium_dir: &Path) -> PathBuf {
    chromium_dir.join("chrome-mac/Chromium.app/Contents/MacOS/Chromium")
}

#[cfg(target_os = "windows")]
fn build_playwright_binary_path(chromium_dir: &Path) -> PathBuf {
    chromium_dir.join("chrome-win/chrome.exe")
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        if let Some(home) = dirs::home_dir() {
            return home
                .join(rest.strip_prefix('/').unwrap_or(rest))
                .to_string_lossy()
                .to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;

    #[test]
    fn test_find_chrome_returns_some_on_host() {
        // This test only makes sense on systems with Chrome installed
        if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
            let result = find_chrome();
            // Don't assert Some -- CI may not have Chrome
            if let Some(path) = result {
                assert!(path.exists());
            }
        }
    }

    #[test]
    fn test_expand_tilde() {
        let expanded = expand_tilde("~/test/path");
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("test/path"));
    }

    #[test]
    fn test_expand_tilde_no_tilde() {
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
    }

    #[test]
    fn test_read_devtools_active_port_missing() {
        let result = read_devtools_active_port(Path::new("/nonexistent"));
        assert!(result.is_none());
    }

    #[test]
    fn test_should_disable_sandbox_skips_if_already_set() {
        let args = vec!["--headless=new".to_string(), "--no-sandbox".to_string()];
        assert!(!should_disable_sandbox(&args));
    }

    #[test]
    fn test_chrome_launch_error_no_stderr() {
        let msg = chrome_launch_error("Chrome exited", &[]);
        assert!(msg.contains("no stderr output"));
    }

    #[test]
    fn test_chrome_launch_error_with_sandbox_hint() {
        let lines = vec![
            "some log line".to_string(),
            "Failed to move to new namespace: sandbox error".to_string(),
        ];
        let msg = chrome_launch_error("Chrome exited", &lines);
        assert!(msg.contains("sandbox error"));
        assert!(msg.contains("Hint:"));
        assert!(msg.contains("--no-sandbox"));
    }

    #[test]
    fn test_chrome_launch_error_generic() {
        let lines = vec![
            "info line".to_string(),
            "another info line".to_string(),
        ];
        let msg = chrome_launch_error("Chrome exited", &lines);
        assert!(msg.contains("last 2 lines"));
    }

    #[test]
    fn test_find_playwright_chromium_nonexistent() {
        let _guard = EnvGuard::new(&["PLAYWRIGHT_BROWSERS_PATH"]);
        _guard.set("PLAYWRIGHT_BROWSERS_PATH", "/nonexistent/path");
        let result = find_playwright_chromium();
        assert!(result.is_none());
    }

    #[test]
    fn test_build_args_headless_includes_headless_flag() {
        let opts = LaunchOptions {
            headless: true,
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(result.args.iter().any(|a| a == "--headless=new"));
        assert!(result
            .args
            .iter()
            .any(|a| a == "--window-size=1280,720"));
        // Temp dir created when no profile
        assert!(result.temp_user_data_dir.is_some());
        let dir = result.temp_user_data_dir.unwrap();
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_args_headed_no_headless_flag() {
        let opts = LaunchOptions {
            headless: false,
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(!result.args.iter().any(|a| a.contains("--headless")));
        assert!(!result.args.iter().any(|a| a.starts_with("--window-size=")));
        // Temp dir created when no profile
        assert!(result.temp_user_data_dir.is_some());
        let dir = result.temp_user_data_dir.unwrap();
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_build_args_temp_user_data_dir_created() {
        let opts = LaunchOptions::default();
        let result = build_chrome_args(&opts).unwrap();
        let dir = result.temp_user_data_dir.as_ref().unwrap();
        assert!(dir.exists());
        assert!(result
            .args
            .iter()
            .any(|a| a.starts_with("--user-data-dir=")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn test_build_args_profile_no_temp_dir() {
        let opts = LaunchOptions {
            profile: Some("/tmp/my-profile".to_string()),
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(result.temp_user_data_dir.is_none());
        assert!(result
            .args
            .iter()
            .any(|a| a == "--user-data-dir=/tmp/my-profile"));
    }

    #[test]
    fn test_build_args_custom_window_size_not_overridden() {
        let opts = LaunchOptions {
            headless: true,
            args: vec!["--window-size=1920,1080".to_string()],
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(!result
            .args
            .iter()
            .any(|a| a == "--window-size=1280,720"));
        assert!(result
            .args
            .iter()
            .any(|a| a == "--window-size=1920,1080"));
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_build_args_start_maximized_suppresses_default_window_size() {
        let opts = LaunchOptions {
            headless: true,
            args: vec!["--start-maximized".to_string()],
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(!result.args.iter().any(|a| a == "--window-size=1280,720"));
        assert!(result.args.iter().any(|a| a == "--start-maximized"));
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_chrome_process_drop_cleans_temp_dir() {
        let dir = std::env::temp_dir().join(format!(
            "agent-browser-chrome-drop-test-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::create_dir_all(&dir);
        assert!(dir.exists());

        {
            // Simulate a ChromeProcess with a temp dir but a dummy child.
            // We can't actually spawn Chrome here, but we can verify the Drop
            // logic by creating a small helper process.
            let child = Command::new("echo")
                .arg("test")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let _process = ChromeProcess {
                child,
                ws_url: String::new(),
                temp_user_data_dir: Some(dir.clone()),
            };
            // _process dropped here
        }

        assert!(!dir.exists(), "Temp dir should be cleaned up on drop");
    }
}
