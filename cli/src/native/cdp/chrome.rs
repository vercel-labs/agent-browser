use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::discovery::discover_cdp_url;

pub struct ChromeProcess {
    child: Child,
    pub ws_url: String,
    temp_user_data_dir: Option<PathBuf>,
    /// On Unix, the process group ID used to kill the entire Chrome process tree.
    #[cfg(unix)]
    pgid: Option<i32>,
}

impl ChromeProcess {
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        // On Unix, kill the entire process group to ensure Chrome helper
        // processes (GPU, renderer, utility, crashpad) are also terminated.
        // This prevents orphaned Chrome processes from blocking the user's
        // normal Chrome (issue #1113).
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
        let _ = self.child.wait();
    }

    /// Returns the OS process ID of the Chrome child process.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Non-blocking check whether Chrome has exited.
    /// Returns `true` if the process has exited (and reaps it), `false` if still running.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// Wait for Chrome to exit on its own (after Browser.close CDP command),
    /// falling back to kill() if it doesn't exit within the timeout.
    /// This allows Chrome to flush cookies and other state to the user-data-dir.
    pub fn wait_or_kill(&mut self, timeout: Duration) {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(50);

        while start.elapsed() < timeout {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(poll_interval),
                Err(_) => break,
            }
        }

        self.kill();
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
                        // Use write! instead of eprintln! to avoid panicking
                        // if the daemon's stderr pipe is broken (parent dropped it).
                        let _ = writeln!(
                            std::io::stderr(),
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

#[derive(Clone)]
pub struct LaunchOptions {
    pub headless: bool,
    pub executable_path: Option<String>,
    pub proxy: Option<String>,
    pub proxy_bypass: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<String>,
    pub profile: Option<String>,
    pub args: Vec<String>,
    pub allow_file_access: bool,
    pub extensions: Option<Vec<String>>,
    pub storage_state: Option<String>,
    pub user_agent: Option<String>,
    pub ignore_https_errors: bool,
    pub color_scheme: Option<String>,
    pub download_path: Option<String>,
    /// When true, omit `--password-store=basic` and `--use-mock-keychain` so
    /// Chrome uses the real system keychain. Set automatically when launching
    /// with a copied Chrome profile.
    pub use_real_keychain: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            headless: true,
            executable_path: None,
            proxy: None,
            proxy_bypass: None,
            proxy_username: None,
            proxy_password: None,
            profile: None,
            args: Vec::new(),
            allow_file_access: false,
            extensions: None,
            storage_state: None,
            user_agent: None,
            ignore_https_errors: false,
            color_scheme: None,
            download_path: None,
            use_real_keychain: false,
        }
    }
}

struct ChromeArgs {
    args: Vec<String>,
    user_data_dir: PathBuf,
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
        "--disable-features=Translate".to_string(),
        "--enable-features=NetworkService,NetworkServiceInProcess".to_string(),
        "--metrics-recording-only".to_string(),
    ];

    if !options.use_real_keychain {
        args.push("--password-store=basic".to_string());
        args.push("--use-mock-keychain".to_string());
    }

    let has_extensions = options
        .extensions
        .as_ref()
        .is_some_and(|exts| !exts.is_empty());

    // Extensions require headed mode in native Chrome (content scripts are not
    // injected in headless mode).  Skip --headless when extensions are loaded.
    if options.headless && !has_extensions {
        args.push("--headless=new".to_string());
        // Enable SwiftShader software rendering in headless mode.  This
        // prevents silent crashes in environments where GPU drivers are
        // missing or restricted (VMs, containers, some cloud machines)
        // while preserving WebGL support.  Playwright uses the same flag.
        args.push("--enable-unsafe-swiftshader".to_string());
    }

    if let Some(ref proxy) = options.proxy {
        args.push(format!("--proxy-server={}", proxy));
    }

    if let Some(ref bypass) = options.proxy_bypass {
        args.push(format!("--proxy-bypass-list={}", bypass));
    }

    let (user_data_dir, temp_user_data_dir) = if let Some(ref profile) = options.profile {
        let expanded = expand_tilde(profile);
        let dir = PathBuf::from(&expanded);
        args.push(format!("--user-data-dir={}", expanded));
        (dir, None)
    } else {
        let dir =
            std::env::temp_dir().join(format!("agent-browser-chrome-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Failed to create temp profile dir: {}", e))?;
        args.push(format!("--user-data-dir={}", dir.display()));
        (dir.clone(), Some(dir))
    };

    if options.ignore_https_errors {
        args.push("--ignore-certificate-errors".to_string());
    }

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

    if !has_window_size && options.headless && !has_extensions {
        args.push("--window-size=1280,720".to_string());
    }

    args.extend(options.args.iter().cloned());

    if should_disable_sandbox(&args) {
        args.push("--no-sandbox".to_string());
    }

    if should_disable_dev_shm(&args) {
        args.push("--disable-dev-shm-usage".to_string());
    }

    Ok(ChromeArgs {
        args,
        user_data_dir,
        temp_user_data_dir,
    })
}

pub fn launch_chrome(options: &LaunchOptions) -> Result<ChromeProcess, String> {
    let chrome_path = match &options.executable_path {
        Some(p) => PathBuf::from(p),
        None => find_chrome().ok_or_else(|| {
            let cache_dir = crate::install::get_browsers_dir();
            format!(
                "Chrome not found. Checked:\n  \
                 - agent-browser cache: {}\n  \
                 - System Chrome installations\n  \
                 - Puppeteer browser cache\n  \
                 - Playwright browser cache\n\
                 Run `agent-browser install` to download Chrome, or use --executable-path.",
                cache_dir.display()
            )
        })?,
    };

    // Profile name preprocessing: if --profile is a Chrome profile name (not a
    // path), resolve it to a directory, copy the profile to a temp dir, and
    // rewrite options so the retry loop uses the copied profile.
    let mut resolved_options: Option<LaunchOptions> = None;
    let mut profile_temp_dir: Option<PathBuf> = None;

    if let Some(ref profile) = options.profile {
        if is_chrome_profile_name(profile) {
            let user_data_dir = find_chrome_user_data_dir().ok_or_else(|| {
                "No Chrome user data directory found. Cannot resolve profile name.\n\
                 If you meant a directory path, use a full path (e.g., /path/to/profile)."
                    .to_string()
            })?;
            let resolved = resolve_chrome_profile(&user_data_dir, profile)?;
            let temp_path = copy_chrome_profile(&user_data_dir, &resolved)?;

            let mut opts = options.clone();
            opts.profile = Some(temp_path.display().to_string());
            opts.use_real_keychain = true;
            opts.args.push(format!("--profile-directory={}", resolved));
            profile_temp_dir = Some(temp_path);
            resolved_options = Some(opts);
        }
    }

    let effective_options = resolved_options.as_ref().unwrap_or(options);

    let max_attempts = 3;
    let mut last_err = String::new();

    for attempt in 1..=max_attempts {
        match try_launch_chrome(&chrome_path, effective_options) {
            Ok(mut process) => {
                // Transfer profile temp dir ownership to ChromeProcess for cleanup on Drop.
                // The try_launch_chrome temp_user_data_dir is None here because we set profile
                // to the temp path (treated as a user-supplied path, no second temp dir).
                if let Some(ref dir) = profile_temp_dir {
                    process.temp_user_data_dir = Some(dir.clone());
                }
                return Ok(process);
            }
            Err(e) => {
                last_err = e;
                if attempt < max_attempts {
                    // Use write! instead of eprintln! to avoid panicking
                    // if the daemon's stderr pipe is broken (parent dropped it).
                    let _ = writeln!(
                        std::io::stderr(),
                        "[chrome] Launch attempt {}/{} failed, retrying in 500ms...",
                        attempt,
                        max_attempts
                    );
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    }

    // All retries failed: clean up profile temp dir if we created one
    if let Some(ref dir) = profile_temp_dir {
        let _ = std::fs::remove_dir_all(dir);
    }

    Err(last_err)
}

fn try_launch_chrome(chrome_path: &Path, options: &LaunchOptions) -> Result<ChromeProcess, String> {
    let ChromeArgs {
        args,
        user_data_dir,
        temp_user_data_dir,
    } = build_chrome_args(options)?;

    // Mitigate stale DevToolsActivePort risk (e.g., previous crash left it behind).
    // Puppeteer does similar cleanup before spawning.
    let _ = std::fs::remove_file(user_data_dir.join("DevToolsActivePort"));

    let cleanup_temp_dir = |dir: &Option<PathBuf>| {
        if let Some(ref d) = dir {
            let _ = std::fs::remove_dir_all(d);
        }
    };

    let mut cmd = Command::new(chrome_path);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    // Place Chrome in its own process group so we can kill the entire tree
    // (main process + GPU/renderer/utility/crashpad helpers) with a single
    // killpg(), preventing orphaned processes (issue #1113).
    //
    // NOTE: Do NOT use PR_SET_PDEATHSIG here. Chrome is spawned via
    // tokio::task::spawn_blocking, and PR_SET_PDEATHSIG fires when the
    // *thread* that forked the child exits, not the process. Tokio reaps
    // idle blocking threads after ~10s, which kills Chrome (issue #1157).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: pre_exec runs between fork() and exec() in the child.
        // setpgid is async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }
    }

    let mut child = cmd.spawn().map_err(|e| {
        cleanup_temp_dir(&temp_user_data_dir);
        format!("Failed to launch Chrome at {:?}: {}", chrome_path, e)
    })?;

    // Shared overall deadline so we don't double-wait (poll + stderr fallback).
    let deadline = std::time::Instant::now() + Duration::from_secs(30);

    // Primary path: use DevToolsActivePort written into user-data-dir.
    // This is more reliable on Windows than scraping stderr for "DevTools listening on ...",
    // which can be missing/empty depending on how Chrome is launched.
    let ws_url = match wait_for_devtools_active_port(&mut child, &user_data_dir, deadline) {
        Ok(url) => url,
        Err(primary_err) => {
            // Fallback: scrape stderr (legacy behavior) for better diagnostics.
            let stderr = child.stderr.take().ok_or_else(|| {
                let _ = child.kill();
                cleanup_temp_dir(&temp_user_data_dir);
                "Failed to capture Chrome stderr".to_string()
            })?;
            let reader = BufReader::new(stderr);
            match wait_for_ws_url_until(reader, deadline) {
                Ok(url) => url,
                Err(fallback_err) => {
                    let _ = child.kill();
                    cleanup_temp_dir(&temp_user_data_dir);
                    return Err(format!(
                        "{}\n(also tried parsing stderr) {}",
                        primary_err, fallback_err
                    ));
                }
            }
        }
    };

    #[cfg(unix)]
    let pgid = {
        let pid = child.id() as i32;
        // The child called setpgid(0,0) via process_group(0), so its PGID
        // equals its own PID.
        Some(pid)
    };

    Ok(ChromeProcess {
        child,
        ws_url,
        temp_user_data_dir,
        #[cfg(unix)]
        pgid,
    })
}

fn wait_for_devtools_active_port(
    child: &mut Child,
    user_data_dir: &Path,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let poll_interval = Duration::from_millis(50);

    while std::time::Instant::now() <= deadline {
        if let Ok(Some(status)) = child.try_wait() {
            // Chrome exited before writing DevToolsActivePort -- report the
            // exit code so the caller can surface it alongside stderr output.
            let code = status
                .code()
                .map(|c| format!("{}", c))
                .unwrap_or_else(|| "unknown".to_string());
            return Err(format!(
                "Chrome exited early (exit code: {}) without writing DevToolsActivePort",
                code
            ));
        }

        if let Some((port, ws_path)) = read_devtools_active_port(user_data_dir) {
            let ws_url = format!("ws://127.0.0.1:{}{}", port, ws_path);
            return Ok(ws_url);
        }

        std::thread::sleep(poll_interval);
    }

    Err("Timeout waiting for DevToolsActivePort".to_string())
}

fn wait_for_ws_url_until(
    reader: BufReader<std::process::ChildStderr>,
    deadline: std::time::Instant,
) -> Result<String, String> {
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
            return format!(
                "{} (no stderr output from Chrome)\nHint: try passing --args \"--no-sandbox\" if Chrome crashes silently in your environment",
                message
            );
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
    // 1. Check Chrome downloaded by `agent-browser install`
    if let Some(p) = crate::install::find_installed_chrome() {
        return Some(p);
    }

    // If the cache directory exists but no Chrome was found, warn -- this
    // likely means the cache is corrupted or the directory layout is unexpected.
    let cache_dir = crate::install::get_browsers_dir();
    if cache_dir.exists() {
        let _ = writeln!(
            std::io::stderr(),
            "Warning: Chrome cache directory exists ({}) but no Chrome binary found inside. \
             Falling back to system Chrome. Run `agent-browser install` to re-download.",
            cache_dir.display()
        );
    }

    // 2. Check system-installed Chrome
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ];
        for c in &candidates {
            let p = PathBuf::from(c);
            if p.exists() {
                return Some(p);
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = [
            "google-chrome",
            "google-chrome-stable",
            "chromium-browser",
            "chromium",
            "brave-browser",
            "brave-browser-stable",
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
    }

    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ];
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let chrome = PathBuf::from(&local).join(r"Google\Chrome\Application\chrome.exe");
            if chrome.exists() {
                return Some(chrome);
            }
            let brave =
                PathBuf::from(&local).join(r"BraveSoftware\Brave-Browser\Application\brave.exe");
            if brave.exists() {
                return Some(brave);
            }
        }
        for c in &candidates {
            let p = PathBuf::from(c);
            if p.exists() {
                return Some(p);
            }
        }
    }

    // 3. Fallback: check Puppeteer / Playwright browser caches
    if let Some(p) = find_puppeteer_chrome() {
        return Some(p);
    }
    if let Some(p) = find_playwright_chromium() {
        return Some(p);
    }

    None
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
            if let Ok(ws_url) = discover_cdp_url("127.0.0.1", port, None).await {
                return Ok(ws_url);
            }
            // M144+: direct WebSocket — verify the port is actually listening
            // before returning, otherwise a stale DevToolsActivePort file
            // (left behind after Chrome exits/crashes) produces a confusing
            // "connection refused" error instead of falling through.
            if is_port_reachable(port) {
                let ws_url = format!("ws://127.0.0.1:{}{}", port, ws_path);
                return Ok(ws_url);
            }
            // Port is dead — remove the stale file so future runs skip it.
            let stale = dir.join("DevToolsActivePort");
            let _ = std::fs::remove_file(&stale);
        }
    }

    // Fallback: probe common ports
    for port in [9222u16, 9229] {
        if let Ok(ws_url) = discover_cdp_url("127.0.0.1", port, None).await {
            return Ok(ws_url);
        }
    }

    Err("No running Chrome instance found. Launch Chrome with --remote-debugging-port or use --cdp.".to_string())
}

fn is_port_reachable(port: u16) -> bool {
    use std::net::TcpStream;
    let addr = format!("127.0.0.1:{}", port);
    TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500)).is_ok()
}

/// Returns the default Chrome user-data directory paths for the current platform.
/// Includes Chrome, Chrome Canary, Chromium, and Brave.
pub fn get_chrome_user_data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    #[cfg(target_os = "macos")]
    {
        if let Some(home) = dirs::home_dir() {
            let base = home.join("Library/Application Support");
            for name in [
                "Google/Chrome",
                "Google/Chrome Canary",
                "Chromium",
                "BraveSoftware/Brave-Browser",
            ] {
                dirs.push(base.join(name));
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(home) = dirs::home_dir() {
            let config = home.join(".config");
            for name in [
                "google-chrome",
                "google-chrome-unstable",
                "chromium",
                "BraveSoftware/Brave-Browser",
            ] {
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
                r"BraveSoftware\Brave-Browser\User Data",
            ] {
                dirs.push(base.join(name));
            }
        }
    }

    dirs
}

/// Returns true if the given string looks like a Chrome profile name rather than
/// a file path. A profile name contains no `/`, `\`, or `~` characters.
pub fn is_chrome_profile_name(s: &str) -> bool {
    !s.contains('/') && !s.contains('\\') && !s.contains('~')
}

/// Returns the first existing Chrome user-data directory that contains a
/// `Local State` file.
pub fn find_chrome_user_data_dir() -> Option<PathBuf> {
    get_chrome_user_data_dirs()
        .into_iter()
        .find(|dir| dir.join("Local State").is_file())
}

/// A Chrome profile entry parsed from `Local State`.
#[derive(Debug, Clone)]
pub struct ChromeProfile {
    /// The directory name (e.g., "Default", "Profile 1").
    pub directory: String,
    /// The user-visible display name (e.g., "Person 1").
    pub name: String,
}

/// Lists all Chrome profiles found in the given user-data directory by reading
/// the `Local State` JSON file. Returns an empty vec if the file is missing,
/// malformed, or lacks the expected `profile.info_cache` key.
pub fn list_chrome_profiles(user_data_dir: &Path) -> Vec<ChromeProfile> {
    let local_state_path = user_data_dir.join("Local State");
    let content = match std::fs::read_to_string(&local_state_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let info_cache = match json.get("profile").and_then(|p| p.get("info_cache")) {
        Some(obj) if obj.is_object() => obj.as_object().unwrap(),
        _ => return Vec::new(),
    };

    let mut profiles: Vec<ChromeProfile> = info_cache
        .iter()
        .map(|(dir_name, info)| {
            let display_name = info
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(dir_name)
                .to_string();
            ChromeProfile {
                directory: dir_name.clone(),
                name: display_name,
            }
        })
        .collect();
    profiles.sort_by(|a, b| a.directory.cmp(&b.directory));
    profiles
}

/// Resolves a profile input string to a Chrome profile directory name using
/// three-tier matching:
/// 1. Exact directory name match
/// 2. Case-insensitive display name match (error if ambiguous)
/// 3. Case-insensitive directory name match
///
/// Returns the resolved directory name, or an error with available profiles.
pub fn resolve_chrome_profile(user_data_dir: &Path, input: &str) -> Result<String, String> {
    let profiles = list_chrome_profiles(user_data_dir);

    if profiles.is_empty() {
        return Err(format!(
            "No Chrome profiles found in {}.\n\
             If you meant a directory path, use a full path (e.g., /path/to/profile).",
            user_data_dir.display()
        ));
    }

    // Tier 1: exact directory name match
    if let Some(p) = profiles.iter().find(|p| p.directory == input) {
        return Ok(p.directory.clone());
    }

    // Tier 2: case-insensitive display name match
    let input_lower = input.to_lowercase();
    let display_matches: Vec<&ChromeProfile> = profiles
        .iter()
        .filter(|p| p.name.to_lowercase() == input_lower)
        .collect();
    match display_matches.len() {
        1 => return Ok(display_matches[0].directory.clone()),
        n if n > 1 => {
            return Err(format!(
                "Ambiguous profile name \"{}\". Multiple profiles match:\n{}\n\
                 Use the directory name instead.",
                input,
                format_profile_list(&display_matches)
            ));
        }
        _ => {}
    }

    // Tier 3: case-insensitive directory name match
    if let Some(p) = profiles
        .iter()
        .find(|p| p.directory.to_lowercase() == input_lower)
    {
        return Ok(p.directory.clone());
    }

    let all_profiles: Vec<&ChromeProfile> = profiles.iter().collect();
    Err(format!(
        "Chrome profile \"{}\" not found. Available profiles:\n{}\n\
         If you meant a directory path, use a full path (e.g., /path/to/profile).",
        input,
        format_profile_list(&all_profiles)
    ))
}

fn format_profile_list(profiles: &[&ChromeProfile]) -> String {
    profiles
        .iter()
        .map(|p| format!("  {} ({})", p.directory, p.name))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Directories to exclude when copying a Chrome profile. These are large
/// non-auth directories that are not needed for reusing login state.
const PROFILE_COPY_EXCLUDE_DIRS: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "Service Worker",
    "blob_storage",
    "File System",
    "GCM Store",
    "optimization_guide",
    "ShaderCache",
    "component_crx_cache",
];

/// Copies a Chrome profile subdirectory and `Local State` to a temp directory
/// with a two-level structure suitable for `--user-data-dir`. Returns the temp
/// directory path on success.
///
/// The copy is best-effort: individual file failures (e.g., `SingletonLock`)
/// are skipped with a warning. If the source profile directory is missing or
/// the temp dir cannot be created, returns an error after cleaning up.
pub fn copy_chrome_profile(
    user_data_dir: &Path,
    profile_directory: &str,
) -> Result<PathBuf, String> {
    let temp_dir =
        std::env::temp_dir().join(format!("agent-browser-profile-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("Failed to create temp profile dir: {}", e))?;

    // Copy Local State (non-fatal if missing or unreadable)
    let local_state_src = user_data_dir.join("Local State");
    if let Err(e) = std::fs::copy(&local_state_src, temp_dir.join("Local State")) {
        let _ = writeln!(
            std::io::stderr(),
            "Warning: could not copy Local State from {}: {}",
            local_state_src.display(),
            e
        );
    }

    // Copy profile subdirectory
    let src_profile = user_data_dir.join(profile_directory);
    if !src_profile.is_dir() {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(format!(
            "Profile directory not found: {}",
            src_profile.display()
        ));
    }
    let dst_profile = temp_dir.join(profile_directory);
    if let Err(e) = copy_dir_recursive(&src_profile, &dst_profile) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(format!("Failed to copy profile: {}", e));
    }

    Ok(temp_dir)
}

/// Recursively copies a directory, skipping entries in [`PROFILE_COPY_EXCLUDE_DIRS`].
/// Individual file copy failures are logged to stderr but do not fail the operation.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create directory {}: {}", dst.display(), e))?;

    let entries = std::fs::read_dir(src)
        .map_err(|e| format!("Failed to read directory {}: {}", src.display(), e))?;

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Warning: failed to read entry in {}: {}",
                    src.display(),
                    e
                );
                continue;
            }
        };

        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let src_path = entry.path();
        let dst_path = dst.join(&name);

        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(e) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "Warning: failed to get file type for {}: {}",
                    src_path.display(),
                    e
                );
                continue;
            }
        };

        if file_type.is_dir() {
            if PROFILE_COPY_EXCLUDE_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else if let Err(e) = std::fs::copy(&src_path, &dst_path) {
            let _ = writeln!(
                std::io::stderr(),
                "Warning: failed to copy {}: {}",
                src_path.display(),
                e
            );
        }
    }

    Ok(())
}

/// Returns true if Chrome's sandbox should be disabled because the environment
/// doesn't support it (containers, VMs, CI runners, running as root).
fn should_disable_sandbox(existing_args: &[String]) -> bool {
    if existing_args.iter().any(|a| a == "--no-sandbox") {
        return false; // already set by user
    }

    // CI environments (GitHub Actions, GitLab CI, etc.) often lack user namespace
    // support due to AppArmor or kernel restrictions.
    if std::env::var("CI").is_ok() {
        return true;
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
            if cgroup.contains("docker") || cgroup.contains("kubepods") || cgroup.contains("lxc") {
                return true;
            }
        }
    }

    false
}

/// Returns true if Chrome should use disk instead of /dev/shm for shared memory.
/// On CI runners and containers, /dev/shm is often too small (64MB default),
/// which causes Chrome to crash mid-session.
fn should_disable_dev_shm(existing_args: &[String]) -> bool {
    if existing_args.iter().any(|a| a == "--disable-dev-shm-usage") {
        return false;
    }

    if std::env::var("CI").is_ok() {
        return true;
    }

    #[cfg(unix)]
    {
        if unsafe { libc::geteuid() } == 0 {
            return true;
        }
        if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
            return true;
        }
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if cgroup.contains("docker") || cgroup.contains("kubepods") || cgroup.contains("lxc") {
                return true;
            }
        }
    }

    false
}

/// Search Puppeteer's browser cache for a Chrome binary.
/// Puppeteer v19+ stores Chrome in ~/.cache/puppeteer/chrome/<platform>-<version>/
fn find_puppeteer_chrome() -> Option<PathBuf> {
    let mut search_dirs = Vec::new();

    if let Ok(custom) = std::env::var("PUPPETEER_CACHE_DIR") {
        search_dirs.push(PathBuf::from(custom).join("chrome"));
    }

    if let Some(home) = dirs::home_dir() {
        search_dirs.push(home.join(".cache/puppeteer/chrome"));
    }

    for dir in &search_dirs {
        if !dir.is_dir() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut matches: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| {
                    let candidate = build_puppeteer_binary_path(&e.path());
                    if candidate.exists() {
                        Some(candidate)
                    } else {
                        None
                    }
                })
                .collect();
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
fn build_puppeteer_binary_path(version_dir: &Path) -> PathBuf {
    version_dir.join("chrome-linux64/chrome")
}

#[cfg(target_os = "macos")]
fn build_puppeteer_binary_path(version_dir: &Path) -> PathBuf {
    // Puppeteer uses chrome-mac-arm64 or chrome-mac-x64 depending on arch
    let arm = version_dir.join(
        "chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    );
    if arm.exists() {
        return arm;
    }
    version_dir.join(
        "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
    )
}

#[cfg(target_os = "windows")]
fn build_puppeteer_binary_path(version_dir: &Path) -> PathBuf {
    version_dir.join(r"chrome-win64\chrome.exe")
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn build_puppeteer_binary_path(version_dir: &Path) -> PathBuf {
    version_dir.join("chrome")
}

/// Search Playwright's browser cache for a Chromium binary.
/// Legacy fallback for users who previously installed Chromium via Playwright.
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

    #[cfg(unix)]
    fn spawn_noop_child() -> Child {
        Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(windows)]
    fn spawn_noop_child() -> Child {
        Command::new("cmd.exe")
            .args(["/C", "exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

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
        assert!(msg.contains("Hint:"));
        assert!(msg.contains("--no-sandbox"));
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
        let lines = vec!["info line".to_string(), "another info line".to_string()];
        let msg = chrome_launch_error("Chrome exited", &lines);
        assert!(msg.contains("last 2 lines"));
    }

    #[test]
    fn test_find_playwright_chromium_nonexistent() {
        let guard = EnvGuard::new(&["PLAYWRIGHT_BROWSERS_PATH", "HOME", "USERPROFILE"]);
        guard.set("PLAYWRIGHT_BROWSERS_PATH", "/nonexistent/path");

        let temp_home = std::env::temp_dir().join(format!(
            "agent-browser-test-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock should be after unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp_home).expect("temp home should be created");
        let temp_home = temp_home.to_string_lossy().to_string();
        guard.set("HOME", &temp_home);
        guard.set("USERPROFILE", &temp_home);

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
            .any(|a| a == "--enable-unsafe-swiftshader"));
        assert!(result.args.iter().any(|a| a == "--window-size=1280,720"));
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
        assert!(!result
            .args
            .iter()
            .any(|a| a == "--enable-unsafe-swiftshader"));
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
        assert!(!result.args.iter().any(|a| a == "--window-size=1280,720"));
        assert!(result.args.iter().any(|a| a == "--window-size=1920,1080"));
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
    fn test_build_args_disables_translate() {
        let opts = LaunchOptions::default();
        let result = build_chrome_args(&opts).unwrap();
        assert!(result
            .args
            .iter()
            .any(|a| a.contains("--disable-features") && a.contains("Translate")));
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_build_args_headless_with_extensions_skips_headless_flag() {
        let opts = LaunchOptions {
            headless: true,
            extensions: Some(vec!["/tmp/my-ext".to_string()]),
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(
            !result.args.iter().any(|a| a.contains("--headless")),
            "headless flag should be omitted when extensions are present"
        );
        assert!(
            !result.args.iter().any(|a| a.contains("--window-size")),
            "window-size should be omitted when extensions force headed mode"
        );
        assert!(result
            .args
            .iter()
            .any(|a| a.starts_with("--load-extension=")));
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_build_args_headed_with_extensions_no_headless_flag() {
        let opts = LaunchOptions {
            headless: false,
            extensions: Some(vec!["/tmp/my-ext".to_string()]),
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(
            !result.args.iter().any(|a| a.contains("--headless")),
            "headless flag should not be present in headed mode"
        );
        assert!(result
            .args
            .iter()
            .any(|a| a.starts_with("--load-extension=")));
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_build_args_ignore_https_errors_includes_flag() {
        let opts = LaunchOptions {
            ignore_https_errors: true,
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(result
            .args
            .iter()
            .any(|a| a == "--ignore-certificate-errors"));
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_build_args_ignore_https_errors_default_no_flag() {
        let opts = LaunchOptions::default();
        let result = build_chrome_args(&opts).unwrap();
        assert!(!result
            .args
            .iter()
            .any(|a| a == "--ignore-certificate-errors"));
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
            let child = spawn_noop_child();
            let _process = ChromeProcess {
                child,
                ws_url: String::new(),
                temp_user_data_dir: Some(dir.clone()),
                #[cfg(unix)]
                pgid: None,
            };
            // _process dropped here
        }

        assert!(!dir.exists(), "Temp dir should be cleaned up on drop");
    }

    #[test]
    fn test_is_chrome_profile_name_simple() {
        assert!(is_chrome_profile_name("Default"));
        assert!(is_chrome_profile_name("Profile 1"));
        assert!(is_chrome_profile_name(""));
    }

    #[test]
    fn test_is_chrome_profile_name_paths() {
        assert!(!is_chrome_profile_name("/tmp/dir"));
        assert!(!is_chrome_profile_name("~/my-profile"));
        assert!(!is_chrome_profile_name("C:\\Users\\foo"));
        assert!(!is_chrome_profile_name("relative/path"));
    }

    /// Helper to create a fake Chrome user-data dir with a `Local State` file.
    fn create_fake_local_state(base: &Path, profiles: &[(&str, &str)]) {
        let mut info_cache = serde_json::Map::new();
        for (dir_name, display_name) in profiles {
            let mut entry = serde_json::Map::new();
            entry.insert(
                "name".to_string(),
                serde_json::Value::String(display_name.to_string()),
            );
            info_cache.insert(dir_name.to_string(), serde_json::Value::Object(entry));
        }

        let local_state = serde_json::json!({
            "profile": {
                "info_cache": info_cache
            }
        });

        std::fs::create_dir_all(base).unwrap();
        std::fs::write(
            base.join("Local State"),
            serde_json::to_string_pretty(&local_state).unwrap(),
        )
        .unwrap();
    }

    /// RAII guard that removes the temp directory on drop (even on panic).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            Self(std::env::temp_dir().join(format!(
                "agent-browser-test-{}-{}-{}",
                name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    impl std::ops::Deref for TempDir {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    #[test]
    fn test_list_chrome_profiles_valid() {
        let dir = TempDir::new("list-profiles");
        create_fake_local_state(&dir, &[("Default", "Person 1"), ("Profile 1", "Work")]);

        let profiles = list_chrome_profiles(&dir);
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].directory, "Default");
        assert_eq!(profiles[0].name, "Person 1");
        assert_eq!(profiles[1].directory, "Profile 1");
        assert_eq!(profiles[1].name, "Work");
    }

    #[test]
    fn test_list_chrome_profiles_missing_local_state() {
        let dir = TempDir::new("list-profiles-missing");
        std::fs::create_dir_all(&*dir).unwrap();
        let profiles = list_chrome_profiles(&dir);
        assert!(profiles.is_empty());
    }

    #[test]
    fn test_list_chrome_profiles_malformed_json() {
        let dir = TempDir::new("list-profiles-malformed");
        std::fs::create_dir_all(&*dir).unwrap();
        std::fs::write(dir.join("Local State"), "not json").unwrap();
        let profiles = list_chrome_profiles(&dir);
        assert!(profiles.is_empty());
    }

    #[test]
    fn test_list_chrome_profiles_missing_info_cache() {
        let dir = TempDir::new("list-profiles-no-cache");
        std::fs::create_dir_all(&*dir).unwrap();
        std::fs::write(dir.join("Local State"), r#"{"profile": {}}"#).unwrap();
        let profiles = list_chrome_profiles(&dir);
        assert!(profiles.is_empty());
    }

    #[test]
    fn test_resolve_chrome_profile_exact_directory() {
        let dir = TempDir::new("resolve-exact");
        create_fake_local_state(&dir, &[("Default", "Person 1"), ("Profile 1", "Work")]);

        let result = resolve_chrome_profile(&dir, "Default");
        assert_eq!(result.unwrap(), "Default");
    }

    #[test]
    fn test_resolve_chrome_profile_display_name_case_insensitive() {
        let dir = TempDir::new("resolve-display");
        create_fake_local_state(&dir, &[("Default", "Person 1"), ("Profile 1", "Work")]);

        let result = resolve_chrome_profile(&dir, "work");
        assert_eq!(result.unwrap(), "Profile 1");
    }

    #[test]
    fn test_resolve_chrome_profile_directory_name_case_insensitive() {
        let dir = TempDir::new("resolve-dir-ci");
        create_fake_local_state(&dir, &[("Default", "Person 1"), ("Profile 1", "Work")]);

        let result = resolve_chrome_profile(&dir, "default");
        assert_eq!(result.unwrap(), "Default");
    }

    #[test]
    fn test_resolve_chrome_profile_not_found() {
        let dir = TempDir::new("resolve-notfound");
        create_fake_local_state(&dir, &[("Default", "Person 1")]);

        let result = resolve_chrome_profile(&dir, "Nonexistent");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("not found"));
        assert!(err.contains("Default"));
        assert!(err.contains("full path"));
    }

    #[test]
    fn test_resolve_chrome_profile_ambiguous_display_name() {
        let dir = TempDir::new("resolve-ambiguous");
        create_fake_local_state(&dir, &[("Default", "Work"), ("Profile 1", "Work")]);

        let result = resolve_chrome_profile(&dir, "Work");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Ambiguous"));
        assert!(err.contains("Default"));
        assert!(err.contains("Profile 1"));
    }

    /// Helper to create a fake Chrome profile directory with some files.
    fn create_fake_profile(user_data_dir: &Path, profile_dir: &str) {
        let profile_path = user_data_dir.join(profile_dir);
        std::fs::create_dir_all(profile_path.join("Local Storage/leveldb")).unwrap();
        std::fs::write(profile_path.join("Cookies"), "fake-cookies").unwrap();
        std::fs::write(
            profile_path.join("Local Storage/leveldb/CURRENT"),
            "fake-leveldb",
        )
        .unwrap();
        // Create an excluded directory to verify it's skipped
        std::fs::create_dir_all(profile_path.join("Cache")).unwrap();
        std::fs::write(profile_path.join("Cache/data_0"), "cache-data").unwrap();
    }

    #[test]
    fn test_copy_chrome_profile_structure() {
        let src = TempDir::new("copy-src");
        create_fake_local_state(&src, &[("Default", "Person 1")]);
        create_fake_profile(&src, "Default");

        let temp_path = copy_chrome_profile(&src, "Default").unwrap();
        let temp = TempDir(temp_path);

        assert!(temp.join("Local State").is_file());
        assert!(temp.join("Default/Cookies").is_file());
        assert!(temp.join("Default/Local Storage/leveldb/CURRENT").is_file());
        assert_eq!(
            std::fs::read_to_string(temp.join("Default/Cookies")).unwrap(),
            "fake-cookies"
        );
        assert_eq!(
            std::fs::read_to_string(temp.join("Default/Local Storage/leveldb/CURRENT")).unwrap(),
            "fake-leveldb"
        );
        assert!(!temp.join("Default/Cache").exists());
    }

    #[test]
    fn test_copy_chrome_profile_missing_source() {
        let src = TempDir::new("copy-missing-src");
        std::fs::create_dir_all(&*src).unwrap();

        let result = copy_chrome_profile(&src, "Nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Profile directory not found"));
    }

    #[test]
    fn test_copy_chrome_profile_missing_local_state() {
        let src = TempDir::new("copy-no-ls");
        let profile_path = src.join("Default");
        std::fs::create_dir_all(&profile_path).unwrap();
        std::fs::write(profile_path.join("Cookies"), "data").unwrap();

        let temp_path = copy_chrome_profile(&src, "Default").unwrap();
        let temp = TempDir(temp_path);

        assert!(!temp.join("Local State").exists());
        assert!(temp.join("Default/Cookies").is_file());
    }

    #[test]
    fn test_copy_dir_recursive_excludes() {
        let src = TempDir::new("copy-excludes-src");
        let dst = TempDir::new("copy-excludes-dst");
        std::fs::create_dir_all(src.join("keep")).unwrap();
        std::fs::write(src.join("keep/data"), "keep-data").unwrap();
        for excluded in PROFILE_COPY_EXCLUDE_DIRS {
            std::fs::create_dir_all(src.join(excluded)).unwrap();
            std::fs::write(src.join(excluded).join("file"), "excluded").unwrap();
        }

        copy_dir_recursive(&src, &dst).unwrap();

        assert!(dst.join("keep/data").is_file());
        for excluded in PROFILE_COPY_EXCLUDE_DIRS {
            assert!(
                !dst.join(excluded).exists(),
                "{} should be excluded",
                excluded
            );
        }
    }

    #[test]
    fn test_build_args_use_real_keychain_true() {
        let opts = LaunchOptions {
            use_real_keychain: true,
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(
            !result.args.iter().any(|a| a == "--password-store=basic"),
            "should NOT have --password-store=basic when use_real_keychain is true"
        );
        assert!(
            !result.args.iter().any(|a| a == "--use-mock-keychain"),
            "should NOT have --use-mock-keychain when use_real_keychain is true"
        );
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_build_args_use_real_keychain_false_default() {
        let opts = LaunchOptions::default();
        let result = build_chrome_args(&opts).unwrap();
        assert!(
            result.args.iter().any(|a| a == "--password-store=basic"),
            "should have --password-store=basic by default"
        );
        assert!(
            result.args.iter().any(|a| a == "--use-mock-keychain"),
            "should have --use-mock-keychain by default"
        );
        if let Some(ref dir) = result.temp_user_data_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn test_build_args_profile_path_preserves_keychain_flags() {
        let opts = LaunchOptions {
            profile: Some("/tmp/my-profile".to_string()),
            ..Default::default()
        };
        let result = build_chrome_args(&opts).unwrap();
        assert!(result
            .args
            .iter()
            .any(|a| a == "--user-data-dir=/tmp/my-profile"));
        assert!(
            result.args.iter().any(|a| a == "--password-store=basic"),
            "profile path should keep keychain flags"
        );
    }
}
