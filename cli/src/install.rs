use crate::color;
use serde::Deserialize;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{exit, Command, Stdio};
use url::Url;

const OFFICIAL_LAST_KNOWN_GOOD_URL: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions.json";
const OFFICIAL_DOWNLOAD_BASE_URL: &str = "https://storage.googleapis.com/chrome-for-testing-public";
const CHROME_LAST_KNOWN_GOOD_URL_ENV: &str = "AGENT_BROWSER_CHROME_LAST_KNOWN_GOOD_URL";
const CHROME_DOWNLOAD_BASE_URL_ENV: &str = "AGENT_BROWSER_CHROME_DOWNLOAD_BASE_URL";

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChromeInstallSource {
    last_known_good_url: String,
    download_base_url: String,
    custom_last_known_good_url: bool,
    custom_download_base_url: bool,
}

#[derive(Debug, Deserialize)]
struct LastKnownGoodVersions {
    channels: ChromeChannels,
}

#[derive(Debug, Deserialize)]
struct ChromeChannels {
    #[serde(rename = "Stable")]
    stable: ChromeChannel,
}

#[derive(Debug, Deserialize)]
struct ChromeChannel {
    version: String,
}

pub fn get_browsers_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".agent-browser")
        .join("browsers")
}

pub fn find_installed_chrome() -> Option<PathBuf> {
    let browsers_dir = get_browsers_dir();
    if !browsers_dir.exists() {
        return None;
    }

    let mut versions: Vec<_> = fs::read_dir(&browsers_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.starts_with("chrome-"))
        })
        .collect();

    versions.sort_by_key(|b| std::cmp::Reverse(b.file_name()));

    for entry in versions {
        if let Some(bin) = chrome_binary_in_dir(&entry.path()) {
            if bin.exists() {
                return Some(bin);
            }
        }
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

/// Resolve Chrome installer env vars into Stable manifest and download base URLs.
fn resolve_install_source() -> Result<ChromeInstallSource, String> {
    let last_known_good_url = read_env_var(CHROME_LAST_KNOWN_GOOD_URL_ENV)?;
    let download_base_url = read_env_var(CHROME_DOWNLOAD_BASE_URL_ENV)?;

    let (last_known_good_url, custom_last_known_good_url) = match last_known_good_url {
        Some(value) => {
            validate_install_url(CHROME_LAST_KNOWN_GOOD_URL_ENV, &value)?;
            (value, true)
        }
        None => (OFFICIAL_LAST_KNOWN_GOOD_URL.to_string(), false),
    };

    let (download_base_url, custom_download_base_url) = match download_base_url {
        Some(value) => {
            let normalized = trim_trailing_slash(value);
            validate_install_url(CHROME_DOWNLOAD_BASE_URL_ENV, &normalized)?;
            (normalized, true)
        }
        None => (OFFICIAL_DOWNLOAD_BASE_URL.to_string(), false),
    };

    Ok(ChromeInstallSource {
        last_known_good_url,
        download_base_url,
        custom_last_known_good_url,
        custom_download_base_url,
    })
}

fn read_env_var(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Err(format!("{name} is set but empty"))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} contains non-Unicode data")),
    }
}

fn trim_trailing_slash(value: String) -> String {
    value.trim_end_matches('/').to_string()
}

fn validate_install_url(name: &str, value: &str) -> Result<(), String> {
    let parsed = Url::parse(value).map_err(|e| format!("{name} is not a valid URL: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(format!("{name} must use http or https, got {scheme}")),
    }
}

fn archive_name_for_platform(platform: &str) -> Option<&'static str> {
    match platform {
        "linux64" => Some("chrome-linux64.zip"),
        "mac-arm64" => Some("chrome-mac-arm64.zip"),
        "mac-x64" => Some("chrome-mac-x64.zip"),
        "win64" => Some("chrome-win64.zip"),
        _ => None,
    }
}

fn build_download_url(base_url: &str, version: &str, platform: &str) -> Result<String, String> {
    const DOWNLOAD_URL_LABEL: &str = "Chrome install URL";

    let archive = archive_name_for_platform(platform)
        .ok_or_else(|| format!("Unsupported platform for archive download: {platform}"))?;
    let url = format!("{base_url}/{version}/{platform}/{archive}");
    validate_install_url(DOWNLOAD_URL_LABEL, &url)?;
    Ok(url)
}

fn validate_chrome_version(version: &str) -> Result<(), String> {
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 4
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(format!(
            "Invalid Chrome version '{version}'. Expected numeric format like 146.0.7680.80"
        ));
    }
    Ok(())
}

fn is_insecure_custom_url(url: &str, custom_configured: bool) -> bool {
    if !custom_configured {
        return false;
    }

    Url::parse(url)
        .map(|parsed| parsed.scheme() == "http")
        .unwrap_or(false)
}

fn parse_stable_version(body: &[u8]) -> Result<String, String> {
    let manifest: LastKnownGoodVersions =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON: {e}"))?;
    let version = manifest.channels.stable.version.trim();
    if version.is_empty() {
        return Err("missing Stable version".to_string());
    }
    Ok(version.to_string())
}

async fn resolve_version(source: &ChromeInstallSource) -> Result<String, String> {
    let url = &source.last_known_good_url;

    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("Failed to fetch version info from {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Failed to fetch version info from {url}: {e}"))?;

    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("Failed to read version info from {url}: {e}"))?;

    parse_stable_version(&body).map_err(|e| format!("Failed to parse version info from {url}: {e}"))
}

async fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| format!("Download failed for {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("Download failed for {url}: {e}"))?;

    let total = resp.content_length();
    let mut bytes = Vec::new();
    let mut stream = resp;
    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = 0;

    loop {
        let chunk = stream
            .chunk()
            .await
            .map_err(|e| format!("Download error: {}", e))?;
        match chunk {
            Some(data) => {
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
            None => break,
        }
    }

    eprintln!();
    Ok(bytes)
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
        let rel_path = raw_name
            .strip_prefix("chrome-")
            .and_then(|s| s.split_once('/'))
            .map(|(_, rest)| rest.to_string())
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

fn exit_install_error(message: String) -> ! {
    eprintln!("{} {}", color::error_indicator(), message);
    exit(1);
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

    let source = match resolve_install_source() {
        Ok(source) => source,
        Err(e) => {
            eprintln!("{} {}", color::error_indicator(), e);
            exit(1);
        }
    };

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

    let version = match rt.block_on(resolve_version(&source)) {
        Ok(version) => version,
        Err(e) => exit_install_error(e),
    };
    if let Err(e) = validate_chrome_version(&version) {
        exit_install_error(e);
    }
    let url = match build_download_url(&source.download_base_url, &version, platform_key()) {
        Ok(url) => url,
        Err(e) => exit_install_error(e),
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
    if source.custom_last_known_good_url {
        println!("  Stable manifest override: {}", source.last_known_good_url);
        if is_insecure_custom_url(&source.last_known_good_url, true) {
            println!(
                "  {} Using insecure HTTP Stable manifest override; only use this on a trusted internal network.",
                color::warning_indicator()
            );
        }
    }
    if source.custom_download_base_url {
        println!("  Download base override: {}", source.download_base_url);
        if is_insecure_custom_url(&source.download_base_url, true) {
            println!(
                "  {} Using insecure HTTP download base override; only use this on a trusted internal network.",
                color::warning_indicator()
            );
        }
    }
    println!("  {}", url);

    let bytes = match rt.block_on(download_bytes(&url)) {
        Ok(b) => b,
        Err(e) => exit_install_error(e),
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

fn install_linux_deps() {
    println!("{}", color::cyan("Installing system dependencies..."));

    let (pkg_mgr, deps) = if which_exists("apt-get") {
        let libasound = if package_exists_apt("libasound2t64") {
            "libasound2t64"
        } else {
            "libasound2"
        };

        (
            "apt-get",
            vec![
                "libxcb-shm0",
                "libx11-xcb1",
                "libx11-6",
                "libxcb1",
                "libxext6",
                "libxrandr2",
                "libxcomposite1",
                "libxcursor1",
                "libxdamage1",
                "libxfixes3",
                "libxi6",
                "libgtk-3-0",
                "libpangocairo-1.0-0",
                "libpango-1.0-0",
                "libatk1.0-0",
                "libcairo-gobject2",
                "libcairo2",
                "libgdk-pixbuf-2.0-0",
                "libxrender1",
                libasound,
                "libfreetype6",
                "libfontconfig1",
                "libdbus-1-3",
                "libnss3",
                "libnspr4",
                "libatk-bridge2.0-0",
                "libdrm2",
                "libxkbcommon0",
                "libatspi2.0-0",
                "libcups2",
                "libxshmfence1",
                "libgbm1",
            ],
        )
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
            ],
        )
    } else {
        eprintln!(
            "{} No supported package manager found (apt-get, dnf, or yum)",
            color::error_indicator()
        );
        exit(1);
    };

    let install_cmd = match pkg_mgr {
        "apt-get" => {
            format!(
                "sudo apt-get update && sudo apt-get install -y {}",
                deps.join(" ")
            )
        }
        _ => format!("sudo {} install -y {}", pkg_mgr, deps.join(" ")),
    };

    println!("Running: {}", install_cmd);
    let status = Command::new("sh").arg("-c").arg(&install_cmd).status();

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
    use crate::test_utils::EnvGuard;

    #[test]
    fn resolve_install_source_uses_official_defaults() {
        let _guard = EnvGuard::new(&[CHROME_LAST_KNOWN_GOOD_URL_ENV, CHROME_DOWNLOAD_BASE_URL_ENV]);

        let source = resolve_install_source().unwrap();

        assert!(!source.custom_last_known_good_url);
        assert!(!source.custom_download_base_url);
        assert_eq!(
            source.last_known_good_url,
            OFFICIAL_LAST_KNOWN_GOOD_URL.to_string()
        );
        assert_eq!(
            source.download_base_url,
            OFFICIAL_DOWNLOAD_BASE_URL.to_string()
        );
    }

    #[test]
    fn resolve_install_source_overrides_last_known_good_url() {
        let guard = EnvGuard::new(&[CHROME_LAST_KNOWN_GOOD_URL_ENV, CHROME_DOWNLOAD_BASE_URL_ENV]);
        guard.set(
            CHROME_LAST_KNOWN_GOOD_URL_ENV,
            "https://mirror.example.com/cft/last-known-good-versions.json",
        );

        let source = resolve_install_source().unwrap();

        assert!(source.custom_last_known_good_url);
        assert!(!source.custom_download_base_url);
        assert_eq!(
            source.last_known_good_url,
            "https://mirror.example.com/cft/last-known-good-versions.json".to_string()
        );
        assert_eq!(
            source.download_base_url,
            OFFICIAL_DOWNLOAD_BASE_URL.to_string()
        );
    }

    #[test]
    fn resolve_install_source_overrides_download_base_url() {
        let guard = EnvGuard::new(&[CHROME_LAST_KNOWN_GOOD_URL_ENV, CHROME_DOWNLOAD_BASE_URL_ENV]);
        guard.set(
            CHROME_DOWNLOAD_BASE_URL_ENV,
            "https://mirror.example.com/chrome-for-testing/",
        );

        let source = resolve_install_source().unwrap();

        assert!(!source.custom_last_known_good_url);
        assert!(source.custom_download_base_url);
        assert_eq!(
            source.last_known_good_url,
            OFFICIAL_LAST_KNOWN_GOOD_URL.to_string()
        );
        assert_eq!(
            source.download_base_url,
            "https://mirror.example.com/chrome-for-testing".to_string()
        );
    }

    #[test]
    fn resolve_install_source_allows_http_overrides() {
        let guard = EnvGuard::new(&[CHROME_LAST_KNOWN_GOOD_URL_ENV, CHROME_DOWNLOAD_BASE_URL_ENV]);
        guard.set(
            CHROME_LAST_KNOWN_GOOD_URL_ENV,
            "http://mirror.internal/chrome-for-testing/last-known-good-versions.json",
        );
        guard.set(
            CHROME_DOWNLOAD_BASE_URL_ENV,
            "http://mirror.internal/chrome-for-testing",
        );

        let source = resolve_install_source().unwrap();

        assert!(source.custom_last_known_good_url);
        assert!(source.custom_download_base_url);
        assert!(is_insecure_custom_url(
            &source.last_known_good_url,
            source.custom_last_known_good_url
        ));
        assert!(is_insecure_custom_url(
            &source.download_base_url,
            source.custom_download_base_url
        ));
    }

    #[test]
    fn resolve_install_source_rejects_empty_last_known_good_url() {
        let guard = EnvGuard::new(&[CHROME_LAST_KNOWN_GOOD_URL_ENV, CHROME_DOWNLOAD_BASE_URL_ENV]);
        guard.set(CHROME_LAST_KNOWN_GOOD_URL_ENV, "   ");

        let err = resolve_install_source().unwrap_err();

        assert!(err.contains("is set but empty"));
    }

    #[test]
    fn resolve_install_source_rejects_empty_download_base_url() {
        let guard = EnvGuard::new(&[CHROME_LAST_KNOWN_GOOD_URL_ENV, CHROME_DOWNLOAD_BASE_URL_ENV]);
        guard.set(CHROME_DOWNLOAD_BASE_URL_ENV, "   ");

        let err = resolve_install_source().unwrap_err();

        assert!(err.contains("is set but empty"));
    }

    #[test]
    fn build_download_url_uses_base_url() {
        let url = build_download_url(
            "https://mirror.example.com/chrome-for-testing",
            "146.0.7680.80",
            "win64",
        )
        .unwrap();

        assert_eq!(
            url,
            "https://mirror.example.com/chrome-for-testing/146.0.7680.80/win64/chrome-win64.zip"
        );
    }

    #[test]
    fn build_download_url_accepts_http_base_url() {
        let url = build_download_url(
            "http://mirror.internal/chrome-for-testing",
            "146.0.7680.80",
            "win64",
        )
        .unwrap();

        assert_eq!(
            url,
            "http://mirror.internal/chrome-for-testing/146.0.7680.80/win64/chrome-win64.zip"
        );
    }

    #[test]
    fn parse_stable_version_reads_manifest() {
        let version = parse_stable_version(
            br#"{
                "channels": {
                    "Stable": {
                        "version": "146.0.7680.80"
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(version, "146.0.7680.80");
    }

    #[test]
    fn validate_chrome_version_accepts_cft_format() {
        validate_chrome_version("146.0.7680.80").unwrap();
    }

    #[test]
    fn validate_chrome_version_rejects_path_traversal() {
        let err = validate_chrome_version("146.0.7680.80/../../evil").unwrap_err();
        assert!(err.contains("Invalid Chrome version"));
    }

    #[test]
    fn validate_chrome_version_rejects_non_numeric_suffix() {
        let err = validate_chrome_version("146.0.7680.80-beta").unwrap_err();
        assert!(err.contains("Invalid Chrome version"));
    }
}
