use crate::color;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};

const LAST_KNOWN_GOOD_URL: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

pub fn get_browsers_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agent-browser")
        .join("browsers")
}

pub fn find_installed_chrome() -> Option<PathBuf> {
    let browsers_dir = get_browsers_dir();
    let debug = std::env::var("AGENT_BROWSER_DEBUG").is_ok();

    if debug {
        let _ = writeln!(
            io::stderr(),
            "[chrome-search] home_dir={:?} browsers_dir={}",
            dirs::home_dir(),
            browsers_dir.display()
        );
    }

    if !browsers_dir.exists() {
        if debug {
            let _ = writeln!(io::stderr(), "[chrome-search] browsers_dir does not exist");
        }
        return None;
    }

    let entries = match fs::read_dir(&browsers_dir) {
        Ok(entries) => entries,
        Err(e) => {
            let _ = writeln!(
                io::stderr(),
                "Warning: cannot read Chrome cache directory {}: {}",
                browsers_dir.display(),
                e
            );
            return None;
        }
    };

    let mut versions: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            let matches = e
                .file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("chrome-"));
            if debug {
                let _ = writeln!(
                    io::stderr(),
                    "[chrome-search] entry {:?} matches={}",
                    e.file_name(),
                    matches
                );
            }
            matches
        })
        .collect();

    versions.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    for entry in versions {
        let dir = entry.path();
        if let Some(bin) = chrome_binary_in_dir(&dir) {
            let exists = bin.exists();
            if debug {
                let _ = writeln!(
                    io::stderr(),
                    "[chrome-search] candidate {} exists={}",
                    bin.display(),
                    exists
                );
            }
            if exists {
                return Some(bin);
            }
        } else if debug {
            let _ = writeln!(
                io::stderr(),
                "[chrome-search] no binary found in {}",
                dir.display()
            );
        }
    }

    if debug {
        let _ = writeln!(io::stderr(), "[chrome-search] no installed Chrome found");
    }
    None
}

fn chrome_binary_in_dir(dir: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let app =
            dir.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
        if app.exists() {
            return Some(app);
        }
        let inner = dir.join("chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing");
        if inner.exists() {
            return Some(inner);
        }
        let inner_x64 = dir.join(
            "chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        );
        if inner_x64.exists() {
            return Some(inner_x64);
        }
        None
    }

    #[cfg(target_os = "linux")]
    {
        let bin = dir.join("chrome");
        if bin.exists() {
            return Some(bin);
        }
        let inner = dir.join("chrome-linux64/chrome");
        if inner.exists() {
            return Some(inner);
        }
        None
    }

    #[cfg(target_os = "windows")]
    {
        let bin = dir.join("chrome.exe");
        if bin.exists() {
            return Some(bin);
        }
        let inner = dir.join("chrome-win64/chrome.exe");
        if inner.exists() {
            return Some(inner);
        }
        None
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}

fn platform_key() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "mac-arm64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "mac-x64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux64"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "win64"
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        // Compiles on unsupported platforms (e.g. linux aarch64) so the binary
        // can still be used for other commands like `connect`. The install path
        // guards against this at runtime before calling platform_key().
        panic!("Unsupported platform for Chrome for Testing download")
    }
}

async fn fetch_download_url() -> Result<(String, String), String> {
    let client = http_client()?;
    let resp = client
        .get(LAST_KNOWN_GOOD_URL)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch version info: {}", format_reqwest_error(&e)))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse version info: {}", e))?;

    let channel = body
        .get("channels")
        .and_then(|c| c.get("Stable"))
        .ok_or("No Stable channel found in version info")?;

    let version = channel
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or("No version string found")?
        .to_string();

    let platform = platform_key();

    let url = channel
        .get("downloads")
        .and_then(|d| d.get("chrome"))
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|entry| {
                if entry.get("platform")?.as_str()? == platform {
                    Some(entry.get("url")?.as_str()?.to_string())
                } else {
                    None
                }
            })
        })
        .ok_or_else(|| format!("No download URL found for platform: {}", platform))?;

    Ok((version, url))
}

fn format_reqwest_error(e: &reqwest::Error) -> String {
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(e);
    while let Some(cause) = source {
        msg.push_str(&format!(": {}", cause));
        source = std::error::Error::source(cause);
    }
    msg
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("agent-browser/{}", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", format_reqwest_error(&e)))
}

async fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = http_client()?;
    let max_retries = 3;
    let mut last_err = String::new();

    for attempt in 0..max_retries {
        if attempt > 0 {
            eprintln!(
                "  Retrying download (attempt {}/{})",
                attempt + 1,
                max_retries
            );
            tokio::time::sleep(std::time::Duration::from_secs(1 << attempt)).await;
        }

        let resp = match client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("Download failed: {}", format_reqwest_error(&e));
                if e.is_connect() || e.is_timeout() {
                    continue;
                }
                return Err(last_err);
            }
        };

        let status = resp.status();
        if !status.is_success() {
            last_err = format!(
                "Download failed: server returned HTTP {} for {}",
                status, url
            );
            if status.is_server_error() {
                continue;
            }
            return Err(last_err);
        }

        let total = resp.content_length();
        let mut bytes = Vec::new();
        let mut stream = resp;
        let mut downloaded: u64 = 0;
        let mut last_pct: u64 = 0;

        let mut chunk_err = None;
        loop {
            let chunk = stream
                .chunk()
                .await
                .map_err(|e| format!("Download error: {}", format_reqwest_error(&e)));
            match chunk {
                Ok(Some(data)) => {
                    downloaded += data.len() as u64;
                    bytes.extend_from_slice(&data);

                    if let Some(total) = total {
                        let pct = (downloaded * 100) / total;
                        if pct >= last_pct + 5 {
                            last_pct = pct;
                            let mb = downloaded as f64 / 1_048_576.0;
                            let total_mb = total as f64 / 1_048_576.0;
                            eprint!("\r  {:.0}/{:.0} MB ({pct}%)", mb, total_mb);
                            let _ = io::stderr().flush();
                        }
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    chunk_err = Some(e);
                    break;
                }
            }
        }

        eprintln!();

        if let Some(e) = chunk_err {
            last_err = e;
            continue;
        }

        return Ok(bytes);
    }

    Err(last_err)
}

fn extract_zip(bytes: Vec<u8>, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| format!("Failed to create directory: {}", e))?;

    let cursor = io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to read zip archive: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry: {}", e))?;

        let enclosed = match file.enclosed_name() {
            Some(name) => name.to_owned(),
            None => continue,
        };
        let raw_name = enclosed.to_string_lossy().to_string();
        // Strip the top-level "chrome-<platform>/" directory from zip entries.
        // On Windows, enclosed_name() normalizes paths to backslashes, so we
        // must split on either separator.
        let rel_path = raw_name
            .strip_prefix("chrome-")
            .and_then(|s| s.find(['/', '\\']).map(|i| &s[i + 1..]))
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or(raw_name.clone());

        if rel_path.is_empty() {
            continue;
        }

        let out_path = dest.join(&rel_path);

        // Defense-in-depth: ensure the resolved path is inside dest
        if !out_path.starts_with(dest) {
            continue;
        }

        if file.is_dir() {
            fs::create_dir_all(&out_path)
                .map_err(|e| format!("Failed to create dir {}: {}", out_path.display(), e))?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("Failed to create parent dir {}: {}", parent.display(), e)
                })?;
            }
            let mut out_file = fs::File::create(&out_path)
                .map_err(|e| format!("Failed to create file {}: {}", out_path.display(), e))?;
            io::copy(&mut file, &mut out_file)
                .map_err(|e| format!("Failed to write {}: {}", out_path.display(), e))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    let _ = fs::set_permissions(&out_path, fs::Permissions::from_mode(mode));
                }
            }
        }
    }

    Ok(())
}

pub fn run_install(with_deps: bool) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        eprintln!(
            "{} Chrome for Testing does not provide Linux ARM64 builds.",
            color::error_indicator()
        );
        eprintln!("  Install Chromium from your system package manager instead:");
        eprintln!("    sudo apt install chromium-browser   # Debian/Ubuntu");
        eprintln!("    sudo dnf install chromium            # Fedora");
        eprintln!("  Then use: agent-browser --executable-path /usr/bin/chromium");
        exit(1);
    }

    let is_linux = cfg!(target_os = "linux");

    if is_linux {
        if with_deps {
            install_linux_deps();
        } else {
            println!(
                "{} Linux detected. If browser fails to launch, run:",
                color::warning_indicator()
            );
            println!("  agent-browser install --with-deps");
            println!();
        }
    }

    println!("{}", color::cyan("Installing Chrome..."));

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| {
            eprintln!(
                "{} Failed to create runtime: {}",
                color::error_indicator(),
                e
            );
            exit(1);
        });

    let (version, url) = match rt.block_on(fetch_download_url()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{} {}", color::error_indicator(), e);
            exit(1);
        }
    };

    let dest = get_browsers_dir().join(format!("chrome-{}", version));

    if let Some(bin) = chrome_binary_in_dir(&dest) {
        if bin.exists() {
            println!(
                "{} Chrome {} is already installed",
                color::success_indicator(),
                version
            );
            return;
        }
    }

    println!("  Downloading Chrome {} for {}", version, platform_key());
    println!("  {}", url);

    let bytes = match rt.block_on(download_bytes(&url)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{} {}", color::error_indicator(), e);
            exit(1);
        }
    };

    match extract_zip(bytes, &dest) {
        Ok(()) => {
            println!(
                "{} Chrome {} installed successfully",
                color::success_indicator(),
                version
            );
            println!("  Location: {}", dest.display());

            if is_linux && !with_deps {
                println!();
                println!(
                    "{} If you see \"shared library\" errors when running, use:",
                    color::yellow("Note:")
                );
                println!("  agent-browser install --with-deps");
            }
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&dest);
            eprintln!("{} {}", color::error_indicator(), e);
            exit(1);
        }
    }
}

fn report_install_status(status: io::Result<std::process::ExitStatus>) {
    match status {
        Ok(s) if s.success() => {
            println!(
                "{} System dependencies installed",
                color::success_indicator()
            )
        }
        Ok(_) => eprintln!(
            "{} Failed to install some dependencies. You may need to run manually with sudo.",
            color::warning_indicator()
        ),
        Err(e) => eprintln!(
            "{} Could not run install command: {}",
            color::warning_indicator(),
            e
        ),
    }
}

fn install_linux_deps() {
    println!("{}", color::cyan("Installing system dependencies..."));

    let (pkg_mgr, deps) = if which_exists("apt-get") {
        // On Ubuntu 24.04+, many libraries were renamed with a t64 suffix as
        // part of the 64-bit time_t transition. Using the old names can cause
        // apt to propose removing hundreds of system packages to resolve
        // conflicts. We check for the t64 variant first to avoid this.
        let apt_deps: Vec<&str> = vec![
            ("libxcb-shm0", None),
            ("libx11-xcb1", None),
            ("libx11-6", None),
            ("libxcb1", None),
            ("libxext6", None),
            ("libxrandr2", None),
            ("libxcomposite1", None),
            ("libxcursor1", None),
            ("libxdamage1", None),
            ("libxfixes3", None),
            ("libxi6", None),
            ("libgtk-3-0", Some("libgtk-3-0t64")),
            ("libpangocairo-1.0-0", Some("libpangocairo-1.0-0t64")),
            ("libpango-1.0-0", Some("libpango-1.0-0t64")),
            ("libatk1.0-0", Some("libatk1.0-0t64")),
            ("libcairo-gobject2", Some("libcairo-gobject2t64")),
            ("libcairo2", Some("libcairo2t64")),
            ("libgdk-pixbuf-2.0-0", Some("libgdk-pixbuf-2.0-0t64")),
            ("libxrender1", None),
            ("libasound2", Some("libasound2t64")),
            ("libfreetype6", None),
            ("libfontconfig1", None),
            ("libdbus-1-3", Some("libdbus-1-3t64")),
            ("libnss3", None),
            ("libnspr4", None),
            ("libatk-bridge2.0-0", Some("libatk-bridge2.0-0t64")),
            ("libdrm2", None),
            ("libxkbcommon0", None),
            ("libatspi2.0-0", Some("libatspi2.0-0t64")),
            ("libcups2", Some("libcups2t64")),
            ("libxshmfence1", None),
            ("libgbm1", None),
            // Fonts: without actual font files, pages render with missing glyphs
            // (tofu). This is especially visible for CJK and emoji characters.
            ("fonts-noto-color-emoji", None),
            ("fonts-noto-cjk", None),
            ("fonts-freefont-ttf", None),
        ]
        .into_iter()
        .map(|(base, t64_variant)| {
            if let Some(t64) = t64_variant {
                if package_exists_apt(t64) {
                    return t64;
                }
            }
            base
        })
        .collect();

        ("apt-get", apt_deps)
    } else if which_exists("dnf") {
        (
            "dnf",
            vec![
                "nss",
                "nspr",
                "atk",
                "at-spi2-atk",
                "cups-libs",
                "libdrm",
                "libXcomposite",
                "libXdamage",
                "libXrandr",
                "mesa-libgbm",
                "pango",
                "alsa-lib",
                "libxkbcommon",
                "libxcb",
                "libX11-xcb",
                "libX11",
                "libXext",
                "libXcursor",
                "libXfixes",
                "libXi",
                "gtk3",
                "cairo-gobject",
                // Fonts
                "google-noto-cjk-fonts",
                "google-noto-emoji-color-fonts",
                "liberation-fonts",
            ],
        )
    } else if which_exists("yum") {
        (
            "yum",
            vec![
                "nss",
                "nspr",
                "atk",
                "at-spi2-atk",
                "cups-libs",
                "libdrm",
                "libXcomposite",
                "libXdamage",
                "libXrandr",
                "mesa-libgbm",
                "pango",
                "alsa-lib",
                "libxkbcommon",
                // Fonts
                "google-noto-cjk-fonts",
                "liberation-fonts",
            ],
        )
    } else {
        eprintln!(
            "{} No supported package manager found (apt-get, dnf, or yum)",
            color::error_indicator()
        );
        exit(1);
    };

    if pkg_mgr == "apt-get" {
        // Run apt-get update first
        println!("Running: sudo apt-get update");
        let update_status = Command::new("sudo").args(["apt-get", "update"]).status();

        match update_status {
            Ok(s) if !s.success() => {
                eprintln!(
                    "{} apt-get update failed. Continuing with existing package lists.",
                    color::warning_indicator()
                );
            }
            Err(e) => {
                eprintln!(
                    "{} Could not run apt-get update: {}",
                    color::warning_indicator(),
                    e
                );
            }
            _ => {}
        }

        // Simulate the install first to detect if apt would remove any
        // packages. This prevents the catastrophic scenario where installing
        // these libraries triggers removal of hundreds of system packages
        // due to dependency conflicts (e.g. on Ubuntu 24.04 with the
        // t64 transition).
        println!("Checking for conflicts...");
        let sim_output = Command::new("sudo")
            .args(["apt-get", "install", "--simulate"])
            .args(&deps)
            .output();

        match sim_output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let combined = format!("{}\n{}", stdout, stderr);

                // Count packages that would be removed
                let removals: Vec<&str> = combined
                    .lines()
                    .filter(|line| line.starts_with("Remv "))
                    .collect();

                if !removals.is_empty() {
                    eprintln!(
                        "{} Aborting: apt would remove {} package(s) to install these dependencies.",
                        color::error_indicator(),
                        removals.len()
                    );
                    eprintln!(
                        "  This usually means some package names have changed on your system"
                    );
                    eprintln!("  (e.g. Ubuntu 24.04 renamed libraries with a t64 suffix).");
                    eprintln!();
                    eprintln!("  Packages that would be removed:");
                    for line in removals.iter().take(20) {
                        eprintln!("    {}", line);
                    }
                    if removals.len() > 20 {
                        eprintln!("    ... and {} more", removals.len() - 20);
                    }
                    eprintln!();
                    eprintln!("  To install dependencies manually, run:");
                    eprintln!("    sudo apt-get install {}", deps.join(" "));
                    eprintln!();
                    eprintln!("  Review the apt output carefully before confirming.");
                    exit(1);
                }
            }
            Err(e) => {
                eprintln!(
                    "{} Could not simulate install ({}). Proceeding with caution.",
                    color::warning_indicator(),
                    e
                );
            }
        }

        // Safe to proceed: no removals detected
        let install_cmd = format!("sudo apt-get install -y {}", deps.join(" "));
        println!("Running: {}", install_cmd);
        let status = Command::new("sudo")
            .args(["apt-get", "install", "-y"])
            .args(&deps)
            .status();

        report_install_status(status);
    } else {
        // dnf / yum path — these package managers do not remove packages
        // during install, so the simulate-first guard is not needed.
        let install_cmd = format!("sudo {} install -y {}", pkg_mgr, deps.join(" "));
        println!("Running: {}", install_cmd);
        let status = Command::new("sh").arg("-c").arg(&install_cmd).status();

        report_install_status(status);
    }
}

fn which_exists(cmd: &str) -> bool {
    #[cfg(unix)]
    {
        Command::new("which")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        Command::new("where")
            .arg(cmd)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn package_exists_apt(pkg: &str) -> bool {
    Command::new("apt-cache")
        .arg("show")
        .arg(pkg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn http_response(status: u16, reason: &str, body: &[u8]) -> Vec<u8> {
        let header = format!(
            "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status,
            reason,
            body.len()
        );
        let mut resp = header.into_bytes();
        resp.extend_from_slice(body);
        resp
    }

    async fn accept_once(listener: &TcpListener, response: &[u8]) {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf).await;
        s.write_all(response).await.unwrap();
    }

    async fn accept_with_ua_check(listener: &TcpListener, response: &[u8]) -> String {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        s.write_all(response).await.unwrap();
        request
    }

    #[tokio::test]
    async fn download_bytes_returns_body_on_200() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = b"fake-zip-content";
        let resp = http_response(200, "OK", body);

        let server = tokio::spawn(async move {
            accept_once(&listener, &resp).await;
        });

        let url = format!("http://127.0.0.1:{}/test.zip", port);
        let result = download_bytes(&url).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), body);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn download_bytes_returns_error_on_404() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let resp = http_response(404, "Not Found", b"not found");

        let server = tokio::spawn(async move {
            accept_once(&listener, &resp).await;
        });

        let url = format!("http://127.0.0.1:{}/test.zip", port);
        let result = download_bytes(&url).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("HTTP 404"),
            "expected HTTP 404 in error, got: {}",
            err
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn download_bytes_retries_on_500() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            // First two attempts: 500
            let r500 = http_response(500, "Internal Server Error", b"error");
            accept_once(&listener, &r500).await;
            accept_once(&listener, &r500).await;
            // Third attempt: 200
            let r200 = http_response(200, "OK", b"ok-data");
            accept_once(&listener, &r200).await;
        });

        let url = format!("http://127.0.0.1:{}/test.zip", port);
        let result = download_bytes(&url).await;
        assert!(
            result.is_ok(),
            "expected success after retries: {:?}",
            result
        );
        assert_eq!(result.unwrap(), b"ok-data");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn download_bytes_gives_up_after_max_retries() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let r500 = http_response(500, "Internal Server Error", b"error");
            // All 3 attempts get 500
            accept_once(&listener, &r500).await;
            accept_once(&listener, &r500).await;
            accept_once(&listener, &r500).await;
        });

        let url = format!("http://127.0.0.1:{}/test.zip", port);
        let result = download_bytes(&url).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("HTTP 500"),
            "expected HTTP 500 in error, got: {}",
            err
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn download_bytes_does_not_retry_on_403() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let resp = http_response(403, "Forbidden", b"forbidden");

        let server = tokio::spawn(async move {
            // Only one request should arrive (no retries for 4xx)
            accept_once(&listener, &resp).await;
        });

        let url = format!("http://127.0.0.1:{}/test.zip", port);
        let result = download_bytes(&url).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("HTTP 403"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_client_sends_user_agent() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let resp = http_response(200, "OK", b"ok");

        let server = tokio::spawn(async move {
            let req = accept_with_ua_check(&listener, &resp).await;
            req
        });

        let client = http_client().unwrap();
        let url = format!("http://127.0.0.1:{}/test", port);
        let _ = client.get(&url).send().await;
        let request_text = server.await.unwrap();
        let expected_ua = format!("agent-browser/{}", env!("CARGO_PKG_VERSION"));
        assert!(
            request_text.contains(&expected_ua),
            "expected User-Agent '{}' in request:\n{}",
            expected_ua,
            request_text
        );
    }

    #[test]
    fn download_bytes_connection_refused_includes_details() {
        // Use a port that nothing is listening on
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(download_bytes("http://127.0.0.1:1/test.zip"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        // The new code should include the root cause (connection refused)
        // not just the vague "error sending request for url"
        assert!(
            err.contains("Connection refused")
                || err.contains("connection refused")
                || err.contains("actively refused it"),
            "expected 'connection refused' in error, got: {}",
            err
        );
    }
}
