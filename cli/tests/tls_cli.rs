use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::Value;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const CA_A: &str = include_str!("fixtures/tls/ca-a.pem");
const CA_B: &str = include_str!("fixtures/tls/ca-b.pem");

struct TlsServer {
    port: u16,
    requests: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl TlsServer {
    fn new(cert: &str, key: &str) -> Self {
        Self::with_response(cert, key, false, "TLS works\n")
    }

    fn with_response(cert: &str, key: &str, proxy: bool, body: &str) -> Self {
        let response = Arc::new(format!("HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()));
        let certs = rustls_pemfile::certs(&mut cert.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let key = rustls_pemfile::private_key(&mut key.as_bytes())
            .unwrap()
            .unwrap();
        let config = Arc::new(
            ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let (count, stop) = (requests.clone(), stopped.clone());
        let worker = thread::spawn(move || {
            let mut connections = Vec::new();
            for stream in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = stream else { break };
                let (config, count, response) = (config.clone(), count.clone(), response.clone());
                connections.push(thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    if proxy {
                        let mut connect = Vec::new();
                        let mut byte = [0];
                        while !connect.ends_with(b"\r\n\r\n") {
                            if stream.read_exact(&mut byte).is_err() {
                                return;
                            }
                            connect.push(byte[0]);
                        }
                        assert!(connect.starts_with(b"CONNECT googlechromelabs.github.io:443 "));
                        if stream
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .is_err()
                        {
                            return;
                        }
                    }
                    let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), stream);
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while !request.ends_with(b"\r\n\r\n") {
                        if tls.read_exact(&mut byte).is_err() {
                            return;
                        }
                        request.push(byte[0]);
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = tls.write_all(response.as_bytes());
                    let _ = tls.flush();
                }));
            }
            for connection in connections {
                let _ = connection.join();
            }
        });
        Self {
            port,
            requests,
            stopped,
            worker: Some(worker),
        }
    }

    fn a() -> Self {
        Self::new(
            include_str!("fixtures/tls/server-a.pem"),
            include_str!("fixtures/tls/server-a.key"),
        )
    }
    fn b() -> Self {
        Self::new(
            include_str!("fixtures/tls/server-b.pem"),
            include_str!("fixtures/tls/server-b.key"),
        )
    }
    fn url(&self) -> String {
        format!("https://localhost:{}", self.port)
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}

struct Session {
    tmp: TempDir,
    system_bundle: Option<PathBuf>,
}

impl Session {
    fn new() -> Self {
        Self {
            tmp: TempDir::new().unwrap(),
            system_bundle: None,
        }
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("AGENT_BROWSER_")
                || matches!(
                    name.to_ascii_uppercase().as_str(),
                    "SSL_CERT_FILE"
                        | "SSL_CERT_DIR"
                        | "HTTP_PROXY"
                        | "HTTPS_PROXY"
                        | "ALL_PROXY"
                        | "NO_PROXY"
                )
            {
                cmd.env_remove(key);
            }
        }
        cmd.current_dir(self.tmp.path())
            .env("HOME", self.tmp.path())
            .env("USERPROFILE", self.tmp.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.tmp.path().join("sockets"))
            .env("NO_COLOR", "1")
            .args(["--session", "tls", "--json"]);
        if let Some(path) = &self.system_bundle {
            cmd.env("SSL_CERT_FILE", path);
        }
        cmd
    }
    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn run(&self, args: &[&str]) -> Value {
        let out = self.output(args);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            panic!(
                "args={args:?} stdout={} stderr={}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }
    fn pid(&self) -> String {
        std::fs::read_to_string(self.tmp.path().join("sockets/tls.pid")).unwrap()
    }
    fn ca(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.tmp.path().join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self
            .command()
            .env_remove("SSL_CERT_FILE")
            .arg("close")
            .output();
    }
}

fn read(session: &Session, url: &str, options: &[&str], succeeds: bool) -> Value {
    let mut args = options.to_vec();
    args.extend(["read", url, "--timeout", "2000"]);
    let response = session.run(&args);
    assert_eq!(response["success"], succeeds, "{args:?}: {response}");
    if succeeds {
        assert_eq!(
            response["data"]["lifecycle"]["effectiveLaunch"]["browserLaunched"], false,
            "{response}"
        );
    }
    response
}

#[test]
fn read_trust_changes_rotate_and_clear_without_browser_or_daemon_restart() {
    let (a, b) = (TlsServer::a(), TlsServer::b());
    let session = Session::new();
    let ca = session.ca("ca.pem", CA_A);
    read(&session, &a.url(), &[], false);
    let pid = session.pid();
    read(
        &session,
        &a.url(),
        &["--ca-cert", ca.to_str().unwrap()],
        true,
    );
    read(&session, &a.url(), &[], true);
    read(&session, &b.url(), &[], false);
    std::fs::write(&ca, CA_B).unwrap();
    read(&session, &b.url(), &[], true);
    read(&session, &a.url(), &[], false);
    std::fs::write(&ca, "invalid certificate").unwrap();
    read(&session, &b.url(), &[], false);
    std::fs::write(&ca, CA_B).unwrap();
    read(&session, &b.url(), &[], true);
    let same = session.ca("equivalent.pem", &format!("{CA_B}{CA_B}"));
    read(
        &session,
        &b.url(),
        &["--ca-cert", same.to_str().unwrap()],
        true,
    );
    read(&session, &b.url(), &["--no-ca-cert"], false);
    assert_eq!(session.pid(), pid);
    let other = Session::new();
    let other_ca = other.ca("ca.pem", CA_B);
    read(
        &other,
        &b.url(),
        &["--ca-cert", other_ca.to_str().unwrap()],
        true,
    );
    read(&session, &b.url(), &[], false);
}

#[test]
fn trust_preserves_hostname_and_expiry_validation() {
    let a = TlsServer::a();
    let expired = TlsServer::new(
        include_str!("fixtures/tls/expired.pem"),
        include_str!("fixtures/tls/server-a.key"),
    );
    let session = Session::new();
    let ca = session.ca("ca.pem", CA_A);
    read(
        &session,
        &a.url(),
        &["--ca-cert", ca.to_str().unwrap()],
        true,
    );
    let count = a.requests.load(Ordering::SeqCst);
    read(
        &session,
        &format!("https://127.0.0.1:{}", a.port),
        &[],
        false,
    );
    read(&session, &expired.url(), &[], false);
    assert_eq!(a.requests.load(Ordering::SeqCst), count);
    assert_eq!(expired.requests.load(Ordering::SeqCst), 0);
}

#[test]
fn system_roots_refresh_and_explicit_false_overrides_environment() {
    let (a, b) = (TlsServer::a(), TlsServer::b());
    let mut session = Session::new();
    let ca = session.ca("native.pem", CA_A);
    session.system_bundle = Some(ca.clone());
    read(
        &session,
        &a.url(),
        &["--no-ca-cert", "--use-system-ca"],
        true,
    );
    let pid = session.pid();
    std::fs::write(&ca, CA_B).unwrap();
    read(&session, &b.url(), &[], true);
    read(&session, &a.url(), &[], false);
    let out = session
        .command()
        .env("AGENT_BROWSER_USE_SYSTEM_CA", "1")
        .args(["--use-system-ca", "false", "read", &b.url()])
        .output()
        .unwrap();
    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(response["success"], false, "{response}");
    assert_eq!(session.pid(), pid);
    read(&session, &b.url(), &[], false);
}

#[test]
fn wss_uses_refreshed_trust_on_the_same_daemon() {
    let (a, b) = (TlsServer::a(), TlsServer::b());
    let session = Session::new();
    let ca = session.ca("ca.pem", CA_A);
    for (server, flags, accepted) in [
        (&a, vec!["--ca-cert", ca.to_str().unwrap()], true),
        (&b, vec![], false),
    ] {
        let mut args = flags;
        let endpoint = format!("wss://localhost:{}", server.port);
        args.extend(["--cdp", &endpoint, "snapshot"]);
        let response = session.run(&args);
        assert_eq!(
            response["success"], false,
            "test server intentionally declines websocket upgrade: {response}"
        );
        assert_eq!(
            server.requests.load(Ordering::SeqCst) > 0,
            accepted,
            "{response}"
        );
    }
    let pid = session.pid();
    std::fs::write(&ca, CA_B).unwrap();
    let endpoint = format!("wss://localhost:{}", b.port);
    let response = session.run(&["--cdp", &endpoint, "snapshot"]);
    assert_eq!(response["success"], false);
    assert!(b.requests.load(Ordering::SeqCst) > 0, "{response}");
    assert_eq!(session.pid(), pid);
}

#[test]
fn install_rejects_an_invalid_explicit_ca_before_network_or_daemon() {
    let session = Session::new();
    let ca = session.ca("invalid.pem", "invalid certificate");
    let out = session.output(&["--ca-cert", ca.to_str().unwrap(), "install"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid.pem"));
    assert!(!session.tmp.path().join("sockets/tls.pid").exists());
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64")))]
#[test]
fn install_fetches_manifest_with_explicit_and_native_trust() {
    let cert = include_str!("fixtures/tls/install.pem");
    let proxy = TlsServer::with_response(
        cert,
        include_str!("fixtures/tls/install.key"),
        true,
        r#"{"channels":{"Stable":{"version":"0.0.0","downloads":{"chrome":[]}}}}"#,
    );
    let session = Session::new();
    let ca = session.ca("installer.pem", cert);
    let url = format!("http://127.0.0.1:{}", proxy.port);
    for (args, trusted) in [
        (vec!["install"], false),
        (vec!["--ca-cert", ca.to_str().unwrap(), "install"], true),
        (vec!["--no-ca-cert", "--use-system-ca", "install"], true),
    ] {
        let mut cmd = session.command();
        cmd.env("HTTPS_PROXY", &url);
        if args.contains(&"--use-system-ca") {
            cmd.env("SSL_CERT_FILE", &ca);
        }
        let out = cmd.args(&args).output().unwrap();
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "fixture has no download URL");
        assert_eq!(
            error.contains("No download URL found"),
            trusted,
            "{args:?}: {error}"
        );
    }
    assert_eq!(proxy.requests.load(Ordering::SeqCst), 2);
    assert!(!session.tmp.path().join("sockets/tls.pid").exists());
}

#[cfg(unix)]
#[test]
fn read_follows_ca_symlink_rotation() {
    let (a, b) = (TlsServer::a(), TlsServer::b());
    let session = Session::new();
    let path_a = session.ca("a.pem", CA_A);
    let path_b = session.ca("b.pem", CA_B);
    let selected = session.tmp.path().join("current.pem");
    std::os::unix::fs::symlink(&path_a, &selected).unwrap();
    read(
        &session,
        &a.url(),
        &["--ca-cert", selected.to_str().unwrap()],
        true,
    );
    let pid = session.pid();
    std::fs::remove_file(&selected).unwrap();
    std::os::unix::fs::symlink(&path_b, &selected).unwrap();
    read(&session, &b.url(), &[], true);
    read(&session, &a.url(), &[], false);
    assert_eq!(session.pid(), pid);
}

#[test]
#[ignore = "requires AGENT_BROWSER_TEST_CHROME pointing to a local Chrome binary"]
fn tls_changes_preserve_a_live_browser_page() {
    let executable = std::env::var("AGENT_BROWSER_TEST_CHROME").expect("set Chrome path");
    let (a, b) = (TlsServer::a(), TlsServer::b());
    let session = Session::new();
    let ca = session.ca("live.pem", CA_A);
    let initial = session.run(&[
        "--executable-path",
        &executable,
        "open",
        "data:text/html,<title>TLS continuity</title><p>proof</p>",
    ]);
    assert_eq!(initial["success"], true, "{initial}");
    let marker = session.run(&["eval", "globalThis.tlsContinuity = 'same-page'"]);
    assert_eq!(marker["success"], true, "{marker}");
    let tabs_before = session.run(&["tab", "list"]);
    let pid = session.pid();
    for (server, options) in [(&a, vec!["--ca-cert", ca.to_str().unwrap()]), (&b, vec![])] {
        if server.port == b.port {
            std::fs::write(&ca, CA_B).unwrap();
        }
        let mut args = options;
        let url = server.url();
        args.extend(["read", &url]);
        let response = session.run(&args);
        assert_eq!(response["success"], true, "{response}");
        let lifecycle = &response["data"]["lifecycle"];
        assert_eq!(lifecycle["restartedBackground"], false, "{response}");
        assert_eq!(lifecycle["relaunchedBrowser"], false, "{response}");
    }
    let marker = session.run(&["eval", "globalThis.tlsContinuity"]);
    assert_eq!(marker["data"]["result"], "same-page", "{marker}");
    let tabs_after = session.run(&["tab", "list"]);
    assert_eq!(tabs_before["data"]["tabs"], tabs_after["data"]["tabs"]);
    assert_eq!(session.pid(), pid);
}

#[test]
fn trust_option_transition_matrix_preserves_effective_roots() {
    let (a, b) = (TlsServer::a(), TlsServer::b());
    for prior_bundle in [None, Some('a'), Some('b')] {
        for prior_native in [false, true] {
            for change in ["omit", "a", "b", "clear", "native-off", "native-on"] {
                let mut session = Session::new();
                let ca_a = session.ca("explicit-a.pem", CA_A);
                let ca_b = session.ca("explicit-b.pem", CA_B);
                session.system_bundle = Some(session.ca("native-b.pem", CA_B));
                read(
                    &session,
                    &a.url(),
                    &[
                        "--no-ca-cert",
                        "--use-system-ca",
                        if prior_native { "true" } else { "false" },
                    ],
                    false,
                );
                let pid = session.pid();
                if let Some(bundle) = prior_bundle {
                    let path = if bundle == 'a' { &ca_a } else { &ca_b };
                    read(
                        &session,
                        &a.url(),
                        &["--ca-cert", path.to_str().unwrap()],
                        bundle == 'a',
                    );
                }
                let (bundle, native, options) = match change {
                    "a" => (
                        Some('a'),
                        prior_native,
                        vec!["--ca-cert", ca_a.to_str().unwrap()],
                    ),
                    "b" => (
                        Some('b'),
                        prior_native,
                        vec!["--ca-cert", ca_b.to_str().unwrap()],
                    ),
                    "clear" => (None, prior_native, vec!["--no-ca-cert"]),
                    "native-off" => (prior_bundle, false, vec!["--use-system-ca", "false"]),
                    "native-on" => (prior_bundle, true, vec!["--use-system-ca", "true"]),
                    _ => (prior_bundle, prior_native, vec![]),
                };
                read(&session, &a.url(), &options, bundle == Some('a'));
                read(&session, &b.url(), &[], native || bundle == Some('b'));
                assert_eq!(
                    session.pid(),
                    pid,
                    "{prior_bundle:?}/{prior_native}/{change}"
                );
            }
        }
    }
}
