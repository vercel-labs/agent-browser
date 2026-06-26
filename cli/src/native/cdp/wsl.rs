//! WSL (Windows Subsystem for Linux) CDP auto-connect support.
//!
//! When running inside WSL without a display server, agent-browser cannot launch
//! a headless Linux Chromium (no GPU, no X11/Wayland).  This module detects WSL
//! and auto-connects to the Windows host's Chrome/Edge via Chrome DevTools
//! Protocol, or launches one if none is running.
//!
//! Entry point: [`wsl_auto_connect_cdp`].

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Top ports to probe on the Windows host in order of preference.
const CDP_PROBE_PORTS: &[u16] = &[9222, 9223, 9224, 9229];

/// Timeout for individual CDP discovery attempts.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

/// Time to wait after launching Chrome before probing CDP.
const LAUNCH_WAIT: Duration = Duration::from_secs(2);

/// Max retries after launch before giving up.
const LAUNCH_MAX_RETRIES: u32 = 5;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Try to auto-connect to a Windows Chrome/Edge CDP endpoint when running in WSL.
///
/// Returns `Ok(ws_url)` on success, or `Err(msg)` if WSL is not detected or
/// no suitable browser could be found/launched.
pub async fn wsl_auto_connect_cdp() -> Result<String, String> {
    if !is_wsl() {
        return Err("not running in WSL".to_string());
    }

    let host_ip = get_wsl_host_ip()?;

    // 1. Try existing CDP endpoints on known ports.
    for &port in CDP_PROBE_PORTS {
        let ws = discover_cdp_url_with_host(&host_ip, port, DISCOVERY_TIMEOUT).await;
        if let Ok(url) = ws {
            return Ok(url);
        }
    }

    // 2. No running Chrome — find and launch one.
    let chrome_exe = find_windows_chrome()?;
    let port = CDP_PROBE_PORTS[0];

    launch_windows_chrome_cdp(&chrome_exe, port)?;

    // 3. Wait for Chrome to become ready, with retries.
    for _ in 0..LAUNCH_MAX_RETRIES {
        tokio::time::sleep(LAUNCH_WAIT).await;
        if let Ok(url) = discover_cdp_url_with_host(&host_ip, port, DISCOVERY_TIMEOUT).await {
            return Ok(url);
        }
    }

    Err(format!(
        "Chrome launched but CDP did not become available on {}:{} after {} retries",
        host_ip, port, LAUNCH_MAX_RETRIES
    ))
}

// ---------------------------------------------------------------------------
// WSL detection
// ---------------------------------------------------------------------------

/// Returns `true` if we are running inside WSL.
fn is_wsl() -> bool {
    // Method 1: check /proc/version for "microsoft" or "WSL"
    if let Ok(version) = std::fs::read_to_string("/proc/version") {
        let lower = version.to_lowercase();
        if lower.contains("microsoft") || lower.contains("wsl") {
            return true;
        }
    }
    // Method 2: check for /proc/sys/fs/binfmt_misc/WSLInterop
    std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
}

// ---------------------------------------------------------------------------
// Windows host IP resolution
// ---------------------------------------------------------------------------

/// Get the Windows host IP address as seen from WSL.
///
/// Reads the nameserver from `/etc/resolv.conf` which WSL sets to the
/// Windows host's virtual network interface, then falls back to reading
/// the default gateway from `ip route`.
fn get_wsl_host_ip() -> Result<String, String> {
    // Primary: /etc/resolv.conf nameserver
    if let Ok(contents) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in contents.lines() {
            let trimmed = line.trim();
            if let Some(ip) = trimmed.strip_prefix("nameserver ") {
                let ip = ip.trim();
                if !ip.is_empty() && ip != "127.0.0.1" {
                    return Ok(ip.to_string());
                }
            }
        }
    }

    // Fallback: ip route show default
    if let Ok(output) = Command::new("sh")
        .args(["-c", "ip route show default 2>/dev/null | awk '{print $3}'"])
        .output()
    {
        let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !ip.is_empty() {
            return Ok(ip);
        }
    }

    Err("Cannot determine Windows host IP: check /etc/resolv.conf or ip route".to_string())
}

// ---------------------------------------------------------------------------
// Windows browser discovery
// ---------------------------------------------------------------------------

/// Known paths for Chrome and Edge on Windows, ordered by preference.
const WINDOWS_BROWSER_PATHS: &[&str] = &[
    // Chrome stable
    "/mnt/c/Program Files/Google/Chrome/Application/chrome.exe",
    // Chrome stable (32-bit)
    "/mnt/c/Program Files (x86)/Google/Chrome/Application/chrome.exe",
    // Edge stable
    "/mnt/c/Program Files/Microsoft/Edge/Application/msedge.exe",
    // Edge stable (32-bit)
    "/mnt/c/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
];

/// Find an installed Windows Chrome or Edge executable via WSL filesystem mounts.
fn find_windows_chrome() -> Result<String, String> {
    for path in WINDOWS_BROWSER_PATHS {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(path.to_string());
        }
    }
    Err(
        "No Windows Chrome or Edge installation found. Checked:\n  ".to_string()
            + &WINDOWS_BROWSER_PATHS.join("\n  "),
    )
}

// ---------------------------------------------------------------------------
// Chrome launch
// ---------------------------------------------------------------------------

/// Launch a Windows Chrome/Edge instance with remote debugging enabled.
///
/// Uses a separate `--user-data-dir` under `C:\temp` to avoid conflicts
/// with the user's normal Chrome session.
fn launch_windows_chrome_cdp(exe_path: &str, port: u16) -> Result<(), String> {
    // Use a Windows-native temp profile so Chrome writes to NTFS, not the
    // WSL virtual filesystem (which can cause file-locking issues).
    let user_data_dir = format!("C:\\temp\\agent-browser-cdp-{}", port);

    let mut cmd = Command::new(exe_path);
    cmd.arg(format!("--remote-debugging-port={}", port))
        .arg("--remote-debugging-address=0.0.0.0") // allow connections from WSL
        .arg(format!("--user-data-dir={}", user_data_dir))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-session-crashed-bubble")
        .arg("--disable-features=TranslateUI")
        .arg("--disable-sync")
        .arg("--no-service-autorun")
        // Don't open a visible window — headless on the Windows side
        // keeps things tidy for background automation.
        .arg("--headless=new")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let _child = cmd.spawn().map_err(|e| {
        format!(
            "Failed to launch {} (is WSL interop enabled?): {}",
            exe_path, e
        )
    })?;

    // We intentionally detach — Chrome will keep running in the background.
    // The caller polls for CDP readiness and connects via WebSocket.

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Discover CDP WebSocket URL on a specific host (not just localhost).
///
/// Wraps [`discover_cdp_url`] but connects to the given host instead of
/// the default `127.0.0.1`.  This is needed because Chrome on the Windows
/// host is reachable at the WSL gateway IP, not localhost.
async fn discover_cdp_url_with_host(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<String, String> {
    // Fetch /json/version and rewrite the WebSocket host.
    let version_url = format!("http://{}:{}/json/version", host, port);

    let body = tokio::time::timeout(timeout, async {
        reqwest::get(&version_url)
            .await
            .map_err(|e| format!("{}", e))?
            .text()
            .await
            .map_err(|e| format!("{}", e))
    })
    .await
    .map_err(|_| "timeout".to_string())?
    .map_err(|e| format!("HTTP error on {}: {}", version_url, e))?;

    let info: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Bad JSON from /json/version: {}", e))?;

    let ws_url = info
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "No webSocketDebuggerUrl in /json/version".to_string())?;

    // Rewrite 127.0.0.1 → actual host IP so the WebSocket URL is reachable.
    if let Ok(mut parsed) = url::Url::parse(ws_url) {
        let _ = parsed.set_host(Some(host));
        let _ = parsed.set_port(Some(port));
        Ok(parsed.to_string())
    } else {
        Ok(ws_url.to_string())
    }
}
