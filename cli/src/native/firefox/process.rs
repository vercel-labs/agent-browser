use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use crate::native::cdp::chrome::LaunchOptions;
#[cfg(windows)]
use crate::native::cdp::windows_process::Child;
#[cfg(not(windows))]
use std::process::Child;

const DOWNLOAD_LINK: &str = "https://www.mozilla.org/firefox/new/";
const MIN_MAJOR: u32 = 150;
const STDERR_TAIL_LINES: usize = 40;
const BIDI_PREFIX: &str = "WebDriver BiDi listening on ";
const FIXED_PATHS: &[&str] = &[
    "/Applications/Firefox.app/Contents/MacOS/firefox",
    r"C:\Program Files\Mozilla Firefox\firefox.exe",
    "/usr/lib/firefox/firefox",
    "/opt/firefox/firefox",
];

/// Where Firefox's DevTools (RDP) server listens: a unix socket inside the
/// private profile dir, or a loopback port on Windows.
#[derive(Debug, Clone, PartialEq)]
pub enum DevtoolsEndpoint {
    Unix(PathBuf),
    Port(u16),
}

/// An owned Firefox child, its temp profile and its stderr tail. Dropping it
/// kills the process tree and deletes the profile.
pub struct FirefoxProcess {
    child: Child,
    pub bidi_url: String,
    pub devtools: DevtoolsEndpoint,
    pub profile_dir: PathBuf,
    socket_dir: Option<PathBuf>,
    tail: Arc<Mutex<VecDeque<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

/// Rejects options Firefox cannot honour. `cdp`, `auto_connect` and `provider`
/// are resolved from flags, env and config by the caller.
pub fn validate_firefox_options(
    options: &LaunchOptions,
    cdp: Option<&str>,
    auto_connect: bool,
    provider: Option<&str>,
) -> Result<(), String> {
    let unsupported = |flag: &str| Err(format!("{flag} is not supported with --engine firefox"));
    if cdp.is_some() {
        return unsupported("--cdp");
    }
    if auto_connect {
        return unsupported("--auto-connect");
    }
    if provider.is_some() {
        return unsupported("--provider");
    }
    if options.extensions.as_ref().is_some_and(|e| !e.is_empty()) {
        return unsupported("--extension");
    }
    if options.profile.is_some() {
        return unsupported("--profile");
    }
    if !options.args.is_empty() {
        return unsupported("--args");
    }
    if options.allow_file_access {
        return unsupported("--allow-file-access");
    }
    if options.webgpu {
        return unsupported("--webgpu");
    }
    if options.ca_cert.is_some() {
        return unsupported("--ca-cert");
    }
    Ok(())
}

/// Locates Firefox: `explicit`, then `AGENT_BROWSER_EXECUTABLE_PATH`, `PATH`, then standard install paths.
pub fn find_firefox(explicit: Option<&str>) -> Result<PathBuf, String> {
    let env = std::env::var("AGENT_BROWSER_EXECUTABLE_PATH").ok();
    let explicit = explicit.or(env.as_deref()).filter(|p| !p.is_empty());
    find_firefox_with(explicit, std::env::var_os("PATH"), FIXED_PATHS)
}

fn find_firefox_with(
    explicit: Option<&str>,
    path: Option<OsString>,
    fixed: &[&str],
) -> Result<PathBuf, String> {
    let exe = if cfg!(windows) {
        "firefox.exe"
    } else {
        "firefox"
    };
    let found = match explicit {
        Some(p) if Path::new(p).is_file() || is_snap(Path::new(p)) => PathBuf::from(p),
        Some(p) => return Err(format!("Firefox executable not found at {p}")),
        None => path
            .iter()
            .flat_map(std::env::split_paths)
            .map(|d| d.join(exe))
            .chain(fixed.iter().map(PathBuf::from))
            .find(|p| p.is_file())
            .ok_or_else(|| {
                format!("Firefox not found. Install Firefox {MIN_MAJOR}+ from {DOWNLOAD_LINK} or pass --executable-path")
            })?,
    };
    if is_snap(&found) {
        return Err(format!(
            "Snap-packaged Firefox cannot use temporary profiles. Install the Mozilla tarball from {DOWNLOAD_LINK} and pass --executable-path"
        ));
    }
    Ok(found)
}

fn is_snap(p: &Path) -> bool {
    let snap = |p: &Path| p.starts_with("/snap") || p.file_name().is_some_and(|n| n == "snap");
    snap(p) || std::fs::canonicalize(p).is_ok_and(|c| snap(&c))
}

/// Extracts the URL from a `WebDriver BiDi listening on ws://...` stderr line.
pub fn parse_bidi_url(line: &str) -> Option<String> {
    let url = line.trim().strip_prefix(BIDI_PREFIX)?.trim();
    url.starts_with("ws://").then(|| url.to_string())
}

/// Fails unless `version` (the BiDi `browserVersion`) is Firefox 150 or newer.
pub fn check_firefox_version(version: &str) -> Result<(), String> {
    match version
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
    {
        Some(m) if m >= MIN_MAJOR => Ok(()),
        _ => Err(format!(
            "Firefox {version} is not supported, need {MIN_MAJOR} or newer. Download it from {DOWNLOAD_LINK}"
        )),
    }
}

fn user_js(download_path: Option<&str>, restrict_webrtc: bool) -> String {
    let mut js = String::from(
        "user_pref(\"devtools.debugger.remote-enabled\", true);\n\
         user_pref(\"devtools.chrome.enabled\", true);\n\
         user_pref(\"devtools.debugger.prompt-connection\", false);\n\
         user_pref(\"dom.w3c_touch_events.enabled\", 1);\n\
         user_pref(\"browser.download.useDownloadDir\", true);\n\
         user_pref(\"browser.download.always_ask_before_handling_new_types\", false);\n\
         user_pref(\"browser.download.manager.showWhenStarting\", false);\n",
    );
    if restrict_webrtc {
        js.push_str("user_pref(\"media.peerconnection.enabled\", false);\n");
    }
    if let Some(dir) = download_path {
        let dir = serde_json::to_string(dir).unwrap_or_default();
        js.push_str(&format!(
            "user_pref(\"browser.download.folderList\", 2);\nuser_pref(\"browser.download.dir\", {dir});\n"
        ));
    }
    js
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

#[cfg(windows)]
fn devtools_endpoint(
    _profile_dir: &Path,
) -> Result<(DevtoolsEndpoint, String, Option<PathBuf>), String> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map_err(|e| format!("Failed to reserve a DevTools port: {e}"))?
        .port();
    Ok((DevtoolsEndpoint::Port(port), port.to_string(), None))
}

#[cfg(not(windows))]
fn devtools_endpoint(
    profile_dir: &Path,
) -> Result<(DevtoolsEndpoint, String, Option<PathBuf>), String> {
    let id = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let dir = Path::new("/tmp").join(format!("abff-{id}"));
    if create_private_dir(&dir).is_ok() {
        let sock = dir.join("s");
        let arg = sock.to_string_lossy().into_owned();
        return Ok((DevtoolsEndpoint::Unix(sock), arg, Some(dir)));
    }
    let sock = profile_dir.join("rdp.sock");
    let arg = sock.to_string_lossy().into_owned();
    Ok((DevtoolsEndpoint::Unix(sock), arg, None))
}

fn prepare(
    path: &Path,
    headless: bool,
    profile_dir: &Path,
    download_path: Option<&str>,
    restrict_webrtc: bool,
) -> Result<(DevtoolsEndpoint, Option<PathBuf>, Child), String> {
    std::fs::write(
        profile_dir.join("user.js"),
        user_js(download_path, restrict_webrtc),
    )
    .map_err(|e| format!("Failed to write Firefox profile: {e}"))?;
    let (devtools, rdp, socket_dir) = devtools_endpoint(profile_dir)?;
    let mut args: Vec<String> = Vec::new();
    if headless {
        args.push("--headless".into());
    }
    if cfg!(target_os = "macos") {
        args.push("--foreground".into());
    }
    if cfg!(windows) {
        args.push("--wait-for-browser".into());
    }
    args.extend(["--remote-debugging-port", "0", "--start-debugger-server"].map(String::from));
    args.push(rdp);
    args.extend([
        "-profile".into(),
        profile_dir.to_string_lossy().into_owned(),
    ]);
    args.extend(["-no-remote".into(), "about:blank".into()]);
    let child = spawn(path, &args, headless).inspect_err(|_| {
        if let Some(dir) = &socket_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    })?;
    Ok((devtools, socket_dir, child))
}

impl FirefoxProcess {
    /// Spawns Firefox with a fresh temp profile and waits for the BiDi URL until `timeout`.
    pub fn launch(
        path: &Path,
        headless: bool,
        download_path: Option<&str>,
        restrict_webrtc: bool,
        timeout: Duration,
    ) -> Result<Self, String> {
        let id = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let profile_dir = std::env::temp_dir().join(format!("agent-browser-firefox-{id}"));
        create_private_dir(&profile_dir)
            .map_err(|e| format!("Failed to create Firefox profile dir: {e}"))?;
        let (devtools, socket_dir, mut child) =
            prepare(path, headless, &profile_dir, download_path, restrict_webrtc).inspect_err(
                |_| {
                    let _ = std::fs::remove_dir_all(&profile_dir);
                },
            )?;
        let stderr: Option<Box<dyn Read + Send>> = child.stderr.take().map(|s| Box::new(s) as _);
        let tail = Arc::new(Mutex::new(VecDeque::new()));
        let (tx, rx) = mpsc::channel();
        let reader = stderr.map(|s| {
            let tail = tail.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(s).lines().map_while(Result::ok) {
                    if let Some(url) = parse_bidi_url(&line) {
                        let _ = tx.send(url);
                    }
                    let mut t = tail.lock().unwrap();
                    if t.len() == STDERR_TAIL_LINES {
                        t.pop_front();
                    }
                    t.push_back(line);
                }
            })
        });
        let mut p = FirefoxProcess {
            child,
            bidi_url: String::new(),
            devtools,
            profile_dir,
            socket_dir,
            tail,
            reader,
        };
        let deadline = Instant::now() + timeout;
        let failure = loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(url) => {
                    p.bidi_url = url;
                    return Ok(p);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break "Firefox exited before it was ready"
                }
                Err(mpsc::RecvTimeoutError::Timeout) if p.has_exited() => {
                    break "Firefox exited before it was ready"
                }
                Err(_) if Instant::now() >= deadline => {
                    break "Timed out waiting for Firefox to start"
                }
                Err(_) => {}
            }
        };
        p.kill();
        if let Some(r) = p.reader.take() {
            let _ = r.join();
        }
        Err(format!("{failure}\nstderr:\n{}", p.stderr_tail()))
    }

    /// Returns true if the process has exited.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)) | Err(_))
    }

    /// Kills the process tree.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }

    /// Last lines Firefox wrote to stderr.
    pub fn stderr_tail(&self) -> String {
        let t = self.tail.lock().unwrap();
        t.iter().cloned().collect::<Vec<_>>().join("\n")
    }
}

impl Drop for FirefoxProcess {
    fn drop(&mut self) {
        self.kill();
        remove_private_dir(&self.profile_dir);
        if let Some(dir) = &self.socket_dir {
            remove_private_dir(dir);
        }
    }
}

fn remove_private_dir(dir: &Path) {
    for _ in 0..3 {
        if std::fs::remove_dir_all(dir).is_ok() || !dir.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn spawn(path: &Path, args: &[String], headless: bool) -> Result<Child, String> {
    Child::spawn(path, args, headless)
        .map_err(|e| format!("Failed to launch Firefox at {path:?}: {e}"))
}

#[cfg(not(windows))]
fn spawn(path: &Path, args: &[String], _headless: bool) -> Result<Child, String> {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(path);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }
    cmd.spawn()
        .map_err(|e| format!("Failed to launch Firefox at {path:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> LaunchOptions {
        LaunchOptions::default()
    }

    fn err_of(o: &LaunchOptions) -> String {
        validate_firefox_options(o, None, false, None).unwrap_err()
    }

    #[test]
    fn validate_accepts_plain_options() {
        assert!(validate_firefox_options(&ok(), None, false, None).is_ok());
    }

    #[test]
    fn validate_rejects_each_option_naming_it() {
        let mut o = ok();
        o.extensions = Some(vec!["x".into()]);
        assert!(err_of(&o).contains("--extension"));
        let mut o = ok();
        o.profile = Some("p".into());
        assert!(err_of(&o).contains("--profile"));
        let mut o = ok();
        o.args = vec!["--a".into()];
        assert!(err_of(&o).contains("--args"));
        let mut o = ok();
        o.allow_file_access = true;
        assert!(err_of(&o).contains("--allow-file-access"));
        let mut o = ok();
        o.webgpu = true;
        assert!(err_of(&o).contains("--webgpu"));
        let mut o = ok();
        o.ca_cert = Some("c".into());
        assert!(err_of(&o).contains("--ca-cert"));
        let e = validate_firefox_options(&ok(), Some("9222"), false, None).unwrap_err();
        assert!(e.contains("--cdp"));
        let e = validate_firefox_options(&ok(), None, true, None).unwrap_err();
        assert!(e.contains("--auto-connect"));
        let e = validate_firefox_options(&ok(), None, false, Some("browserbase")).unwrap_err();
        assert!(e.contains("--provider"));
    }

    #[test]
    fn parses_bidi_url_and_ignores_noise() {
        assert_eq!(
            parse_bidi_url("WebDriver BiDi listening on ws://127.0.0.1:53123").as_deref(),
            Some("ws://127.0.0.1:53123")
        );
        assert_eq!(parse_bidi_url("console.warn: something ws://x"), None);
        assert_eq!(
            parse_bidi_url("WebDriver BiDi listening on ws://[::1]:9\r").as_deref(),
            Some("ws://[::1]:9")
        );
    }

    #[test]
    fn webrtc_is_disabled_only_when_domains_are_restricted() {
        assert!(user_js(None, true).contains("media.peerconnection.enabled\", false"));
        assert!(!user_js(None, false).contains("peerconnection"));
    }

    #[test]
    fn version_gate() {
        let e = check_firefox_version("149.0").unwrap_err();
        assert!(e.contains("149.0") && e.contains(DOWNLOAD_LINK), "{e}");
        assert!(check_firefox_version("150.0").is_ok());
        assert!(check_firefox_version("158.0.1").is_ok());
        assert!(check_firefox_version("garbage").is_err());
    }

    #[test]
    fn find_prefers_explicit_and_rejects_snap() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("firefox");
        std::fs::write(&bin, "").unwrap();
        let on_path = find_firefox_with(None, Some(dir.path().into()), &[]).unwrap();
        assert_eq!(on_path, bin);
        let other = dir.path().join("other");
        std::fs::write(&other, "").unwrap();
        let got = find_firefox_with(other.to_str(), Some(dir.path().into()), &[]).unwrap();
        assert_eq!(got, other);
        let e = find_firefox_with(Some("/snap/bin/firefox"), None, &[]).unwrap_err();
        assert!(e.contains("Snap") && e.contains(DOWNLOAD_LINK), "{e}");
        assert!(find_firefox_with(None, None, &[])
            .unwrap_err()
            .contains(DOWNLOAD_LINK));
    }

    #[cfg(unix)]
    fn fake(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("fake-firefox");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn launch_parses_url_and_drop_removes_profile() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake(
            dir.path(),
            "echo noise >&2\necho 'WebDriver BiDi listening on ws://127.0.0.1:4242' >&2\nexec sleep 30",
        );
        let p = FirefoxProcess::launch(&bin, true, None, false, Duration::from_secs(10)).unwrap();
        assert_eq!(p.bidi_url, "ws://127.0.0.1:4242");
        let profile = p.profile_dir.clone();
        assert!(profile.join("user.js").exists());
        assert!(profile
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("agent-browser-firefox-"));
        drop(p);
        assert!(!profile.exists());
    }

    #[cfg(unix)]
    #[test]
    fn long_tmpdir_profile_yields_short_socket_path() {
        let dir = tempfile::tempdir().unwrap();
        let long_profile = dir.path().join("t".repeat(150)).join("profile");
        std::fs::create_dir_all(&long_profile).unwrap();
        let (endpoint, _arg, socket_dir) = devtools_endpoint(&long_profile).unwrap();
        let DevtoolsEndpoint::Unix(sock) = endpoint else {
            panic!("expected a unix socket endpoint");
        };
        assert!(
            sock.as_os_str().len() < 100,
            "socket path too long: {}",
            sock.display()
        );
        let socket_dir = socket_dir.unwrap();
        assert!(socket_dir.starts_with("/tmp"));
        assert!(socket_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("abff-"));
        std::fs::remove_dir_all(socket_dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn deadline_kills_child_removes_profile_and_reports_tail() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("profile-path");
        let body = format!(
            "echo 'boom on stderr' >&2\nprev=\nfor a in \"$@\"; do [ \"$prev\" = -profile ] && echo \"$a\" > {}; prev=\"$a\"; done\nexec sleep 30",
            log.display()
        );
        let bin = fake(dir.path(), &body);
        let e = FirefoxProcess::launch(&bin, true, None, false, Duration::from_millis(800))
            .err()
            .unwrap();
        assert!(e.contains("boom on stderr"), "{e}");
        let profile = std::fs::read_to_string(&log).unwrap();
        assert!(!Path::new(profile.trim()).exists());
    }

    #[cfg(unix)]
    #[test]
    fn early_exit_fails_fast_with_tail() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake(dir.path(), "echo 'bad profile' >&2\nexit 3");
        let start = Instant::now();
        let e = FirefoxProcess::launch(&bin, true, None, false, Duration::from_secs(20))
            .err()
            .unwrap();
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(e.contains("bad profile"), "{e}");
    }

    #[test]
    #[ignore = "requires FIREFOX_BIN"]
    fn real_firefox_smoke() {
        let bin = std::env::var("FIREFOX_BIN").expect("FIREFOX_BIN");
        let p = FirefoxProcess::launch(Path::new(&bin), true, None, false, Duration::from_secs(30))
            .unwrap();
        assert!(p.bidi_url.starts_with("ws://127.0.0.1:"), "{}", p.bidi_url);
        let profile = p.profile_dir.clone();
        drop(p);
        assert!(!profile.exists());
    }
}
