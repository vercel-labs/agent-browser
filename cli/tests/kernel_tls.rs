use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio_tungstenite::tungstenite::{self, protocol::Role, Message};

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const CA: &str = include_str!("fixtures/tls/ca-a.pem");
const CERT: &str = include_str!("fixtures/tls/server-a.pem");
const KEY: &str = include_str!("fixtures/tls/server-a.key");

// A network-boundary fixture: real TLS, Kernel HTTP requests and CDP frames.
struct Kernel {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Kernel {
    fn new(cert: &str, key: &str, cdp_url: Option<String>) -> Self {
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
        let cdp_url = cdp_url.unwrap_or_else(|| format!("wss://localhost:{port}/cdp"));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (observed, stopped) = (requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            for stream in listener.incoming() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let stream = stream.unwrap();
                let (config, observed, cdp_url) =
                    (config.clone(), observed.clone(), cdp_url.clone());
                workers.push(thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
                    let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), stream);
                    let mut header = Vec::new();
                    let mut byte = [0];
                    while !header.ends_with(b"\r\n\r\n") {
                        if tls.read_exact(&mut byte).is_err() { return; }
                        header.push(byte[0]);
                    }
                    let header = String::from_utf8(header).unwrap();
                    let first = header.lines().next().unwrap().to_string();
                    observed.lock().unwrap().push(first.clone());
                    if first.starts_with("GET /cdp ") {
                        let key = header.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("sec-websocket-key").then(|| value.trim())
                        }).unwrap();
                        let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
                        write!(tls, "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").unwrap();
                        tls.flush().unwrap();
                        let mut ws = tungstenite::WebSocket::from_raw_socket(tls, Role::Server, None);
                        while let Ok(message) = ws.read() {
                            match message {
                                Message::Text(text) => {
                                    let cmd: Value = serde_json::from_str(&text).unwrap();
                                    let result = match cmd["method"].as_str().unwrap() {
                                        "Target.getTargets" => json!({"targetInfos": [{"targetId":"page", "type":"page", "url":"https://example.com/", "title":"Example", "attached":false}]}),
                                        "Target.attachToTarget" => json!({"sessionId":"page-session"}),
                                        "Runtime.evaluate" => json!({"result":{"type":"string", "value":"https://example.com/"}}),
                                        _ => json!({}),
                                    };
                                    if ws.send(Message::Text(json!({"id":cmd["id"], "result":result}).to_string())).is_err() { break; }
                                }
                                Message::Close(_) => { let _ = ws.flush(); break; }
                                _ => {}
                            }
                        }
                    } else {
                        let body_length = header.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                        }).unwrap_or(0);
                        let mut body = vec![0; body_length];
                        tls.read_exact(&mut body).unwrap();
                        let body = if first.starts_with("POST /browsers ") {
                            json!({"session_id":"test-session", "cdp_ws_url":cdp_url}).to_string()
                        } else if first.starts_with("GET /json/version ") {
                            json!({"Browser":"Chrome/157", "Protocol-Version":"1.3", "User-Agent":"fixture", "V8-Version":"1", "WebKit-Version":"1", "webSocketDebuggerUrl":format!("wss://localhost:{port}/cdp")}).to_string()
                        } else { "{}".to_string() };
                        write!(tls, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                        let _ = tls.flush();
                    }
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            port,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn url(&self) -> String {
        format!("https://localhost:{}", self.port)
    }
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Kernel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        self.worker.take().unwrap().join().unwrap();
    }
}

struct Session {
    tmp: TempDir,
    endpoint: String,
}
impl Session {
    fn new(kernel: &Kernel) -> Self {
        Self {
            tmp: TempDir::new().unwrap(),
            endpoint: kernel.url(),
        }
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("AGENT_BROWSER_")
                || name.starts_with("KERNEL_")
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
            .env("AGENT_BROWSER_PROVIDER", "kernel")
            .env("KERNEL_API_KEY", "fixture-key")
            .env("KERNEL_ENDPOINT", &self.endpoint)
            .env("NO_COLOR", "1")
            .args(["--session", "kernel-tls", "--json"]);
        cmd
    }
    fn ca(&self, pem: &str) -> String {
        let path = self.tmp.path().join("ca.pem");
        std::fs::write(&path, pem).unwrap();
        path.to_str().unwrap().to_string()
    }
    fn run(&self, args: &[&str]) -> Value {
        response(self.command().args(args))
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.command().arg("close").output();
    }
}
fn response(cmd: &mut Command) -> Value {
    let output = cmd.output().unwrap();
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn kernel_custom_ca_covers_creation_cdp_and_cleanup() {
    let kernel = Kernel::new(CERT, KEY, None);
    let session = Session::new(&kernel);
    let ca = session.ca(CA);
    let result = session.run(&["--ca-cert", &ca, "get", "url"]);
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["data"]["url"], "https://example.com/");
    let closed = session.run(&["close"]);
    assert_eq!(closed["success"], true, "{closed}");
    assert_eq!(
        kernel.requests(),
        [
            "POST /browsers HTTP/1.1",
            "GET /cdp HTTP/1.1",
            "DELETE /browsers/test-session HTTP/1.1"
        ]
    );
}

#[test]
fn kernel_system_ca_environment_covers_creation_cdp_and_cleanup() {
    let kernel = Kernel::new(CERT, KEY, None);
    let session = Session::new(&kernel);
    let ca = session.ca(CA);
    // --no-ca-cert disables the ambient extra-bundle path, so only native roots can succeed.
    let result = response(
        session
            .command()
            .env("SSL_CERT_FILE", &ca)
            .env("AGENT_BROWSER_USE_SYSTEM_CA", "true")
            .args(["--no-ca-cert", "get", "url"]),
    );
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(session.run(&["close"])["success"], true);
    assert!(kernel
        .requests()
        .contains(&"DELETE /browsers/test-session HTTP/1.1".to_string()));
}

#[test]
fn kernel_rejects_untrusted_expired_and_wrong_hostname_api_certificates() {
    for (cert, key, trust, wrong_host) in [
        (CERT, KEY, None, false),
        (
            CERT,
            KEY,
            Some(include_str!("fixtures/tls/ca-b.pem")),
            false,
        ),
        (
            include_str!("fixtures/tls/expired.pem"),
            KEY,
            Some(CA),
            false,
        ),
        (CERT, KEY, Some(CA), true),
    ] {
        let kernel = Kernel::new(cert, key, None);
        let mut session = Session::new(&kernel);
        if wrong_host {
            session.endpoint = format!("https://127.0.0.1:{}", kernel.port);
        }
        let ca = trust.map(|pem| session.ca(pem));
        let mut args = Vec::new();
        if let Some(ca) = ca.as_deref() {
            args.extend(["--ca-cert", ca]);
        }
        args.extend(["get", "url"]);
        let result = session.run(&args);
        assert_eq!(result["success"], false, "{result}");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("Kernel request failed"),
            "{result}"
        );
        assert!(kernel.requests().is_empty());
    }
}

#[test]
fn kernel_cdp_tls_failure_still_cleans_up_with_configured_trust() {
    for (cert, key, wrong_host) in [
        (
            include_str!("fixtures/tls/server-b.pem"),
            include_str!("fixtures/tls/server-b.key"),
            false,
        ),
        (include_str!("fixtures/tls/expired.pem"), KEY, false),
        (CERT, KEY, true),
    ] {
        let cdp = Kernel::new(cert, key, None);
        let host = if wrong_host { "127.0.0.1" } else { "localhost" };
        let kernel = Kernel::new(CERT, KEY, Some(format!("wss://{host}:{}/cdp", cdp.port)));
        let session = Session::new(&kernel);
        let ca = session.ca(CA);
        let result = session.run(&["--ca-cert", &ca, "get", "url"]);
        assert_eq!(result["success"], false, "{result}");
        assert!(
            result["error"]
                .as_str()
                .unwrap()
                .contains("connection failed"),
            "{result}"
        );
        assert_eq!(
            kernel.requests(),
            [
                "POST /browsers HTTP/1.1",
                "DELETE /browsers/test-session HTTP/1.1"
            ]
        );
        assert!(cdp.requests().is_empty());
    }
}

#[test]
fn kernel_ca_environment_and_config_work_in_batches() {
    for from_environment in [true, false] {
        let kernel = Kernel::new(CERT, KEY, None);
        let session = Session::new(&kernel);
        let ca = session.ca(CA);
        let mut cmd = session.command();
        if from_environment {
            cmd.env("AGENT_BROWSER_CA_CERT", &ca);
        } else {
            let config = session.tmp.path().join("config.json");
            std::fs::write(
                &config,
                json!({"provider":"kernel", "caCert":ca}).to_string(),
            )
            .unwrap();
            cmd.args(["--config", config.to_str().unwrap()]);
        }
        let result = response(cmd.args(["batch", "get url", "close"]));
        assert_eq!(result[0]["success"], true, "{result}");
        assert_eq!(result[1]["success"], true, "{result}");
        assert!(kernel
            .requests()
            .contains(&"DELETE /browsers/test-session HTTP/1.1".to_string()));
    }
}

#[test]
fn kernel_explicit_system_ca_false_overrides_environment() {
    let kernel = Kernel::new(CERT, KEY, None);
    let session = Session::new(&kernel);
    let ca = session.ca(CA);
    let result = response(
        session
            .command()
            .env("SSL_CERT_FILE", &ca)
            .env("AGENT_BROWSER_USE_SYSTEM_CA", "true")
            .args(["--no-ca-cert", "--use-system-ca", "false", "get", "url"]),
    );
    assert_eq!(result["success"], false, "{result}");
    assert!(kernel.requests().is_empty());
}

#[test]
fn kernel_cleanup_rejects_a_ca_removed_after_creation() {
    let kernel = Kernel::new(CERT, KEY, None);
    let session = Session::new(&kernel);
    let ca = session.ca(CA);
    let result = session.run(&["--ca-cert", &ca, "get", "url"]);
    assert_eq!(result["success"], true, "{result}");
    session.ca(include_str!("fixtures/tls/ca-b.pem"));
    // Cleanup must not send an authenticated DELETE after trust has been revoked.
    session.run(&["close"]);
    assert_eq!(
        kernel.requests(),
        ["POST /browsers HTTP/1.1", "GET /cdp HTTP/1.1"]
    );
}

#[test]
fn kernel_https_cdp_discovery_uses_selected_trust() {
    let cdp = Kernel::new(CERT, KEY, None);
    let kernel = Kernel::new(CERT, KEY, Some(cdp.url()));
    let session = Session::new(&kernel);
    let ca = session.ca(CA);
    let result = session.run(&["--ca-cert", &ca, "get", "url"]);
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(session.run(&["close"])["success"], true);
    assert_eq!(
        cdp.requests(),
        ["GET /json/version HTTP/1.1", "GET /cdp HTTP/1.1"]
    );
}

#[test]
fn kernel_mcp_uses_the_same_ca_configuration_as_cli() {
    let kernel = Kernel::new(CERT, KEY, None);
    let session = Session::new(&kernel);
    let ca = session.ca(CA);
    let arguments = json!({"session":"kernel-tls", "caCert":ca}).to_string();
    let result =
        response(
            session
                .command()
                .args(["mcp", "call", "agent_browser_get_url", &arguments]),
        );
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(
        result["structuredContent"]["response"]["data"]["url"],
        "https://example.com/"
    );
    assert_eq!(session.run(&["close"])["success"], true);
    assert!(kernel
        .requests()
        .contains(&"DELETE /browsers/test-session HTTP/1.1".to_string()));
}
