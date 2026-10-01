//! Integration tests for the standalone dashboard lifecycle.

use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

struct DashboardCleanup<'a>(&'a TempDir);

impl Drop for DashboardCleanup<'_> {
    fn drop(&mut self) {
        let _ = run_dashboard(self.0, &["dashboard", "stop", "--json"]);
    }
}

struct RunningDashboard(Child);

impl Drop for RunningDashboard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn socket_dir(tmp: &TempDir) -> std::path::PathBuf {
    tmp.path().join("sockets")
}

fn unused_loopback_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_for_dashboard(address: SocketAddr, host: &str, origin: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
            Ok(mut stream) => {
                stream
                    .write_all(
                        format!(
                            "GET /api/sessions HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nConnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                return response;
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("dashboard did not accept {address}: {error}"),
        }
    }
}

fn seed_running_dashboard(tmp: &TempDir, port: u16, allowed_origins: &[&str]) -> RunningDashboard {
    let socket_dir = socket_dir(tmp);
    std::fs::create_dir_all(&socket_dir).unwrap();
    let child = Command::new(BIN)
        .env("AGENT_BROWSER_DASHBOARD", "1")
        .env("AGENT_BROWSER_DASHBOARD_PORT", "0")
        .env("AGENT_BROWSER_SOCKET_DIR", &socket_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to seed a running dashboard process");
    let config = serde_json::json!({
        "port": port,
        "allowed_origins": allowed_origins,
        "access_token": "a".repeat(64),
    });
    std::fs::write(
        socket_dir.join("dashboard.config"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    std::fs::write(socket_dir.join("dashboard.pid"), child.id().to_string()).unwrap();
    RunningDashboard(child)
}

fn run_dashboard(tmp: &TempDir, args: &[&str]) -> Output {
    let socket_dir = socket_dir(tmp);
    std::fs::create_dir_all(&socket_dir).unwrap();

    Command::new(BIN)
        .args(args)
        .env("AGENT_BROWSER_SOCKET_DIR", socket_dir)
        .env_remove("AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS")
        .env("NO_COLOR", "1")
        .output()
        .expect("failed to invoke agent-browser dashboard")
}

fn json_output(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout was not JSON: {error}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn dashboard_start_and_stop_return_through_mcp() {
    let tmp = TempDir::new().unwrap();
    let _cleanup = DashboardCleanup(&tmp);
    let port = unused_loopback_port();
    let mut mcp = RunningDashboard(
        Command::new(BIN)
            .args(["mcp", "--tools", "all"])
            .env("AGENT_BROWSER_SOCKET_DIR", socket_dir(&tmp))
            .env_remove("AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS")
            .env("NO_COLOR", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut stdin = mcp.0.stdin.take().unwrap();
    let stdout = mcp.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut call = |id: u64, tool: &str, arguments: Value| {
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": tool, "arguments": arguments},
            })
        )
        .unwrap();
        let line = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("MCP dashboard call did not return while the server was running");
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["isError"], false, "{response}");
        response["result"]["structuredContent"]["response"].clone()
    };

    let started = call(
        1,
        "agent_browser_dashboard_start",
        serde_json::json!({"port": port}),
    );
    assert_eq!(started["data"]["port"], port);
    assert!(socket_dir(&tmp).join("dashboard.pid").exists());
    let response = wait_for_dashboard(
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        &format!("localhost:{port}"),
        &format!("http://localhost:{port}"),
    );
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");

    let stopped = call(2, "agent_browser_dashboard_stop", serde_json::json!({}));
    assert_eq!(stopped["data"]["stopped"], true);
    assert!(!socket_dir(&tmp).join("dashboard.pid").exists());
    drop(stdin);
    assert!(mcp.0.wait().unwrap().success());
    reader.join().unwrap();
}

#[test]
fn explicit_dashboard_start_accepts_mcp_style_arguments() {
    let tmp = TempDir::new().unwrap();
    let _cleanup = DashboardCleanup(&tmp);
    let port = unused_loopback_port();
    let port_arg = port.to_string();

    let started = run_dashboard(
        &tmp,
        &[
            "dashboard",
            "start",
            "--port",
            &port_arg,
            "--allowed-origins",
            "https://dashboard.example.com",
            "--json",
        ],
    );
    assert!(
        started.status.success(),
        "dashboard start failed: {}",
        String::from_utf8_lossy(&started.stderr)
    );
    let payload = json_output(&started);
    assert_eq!(payload["data"]["port"], port);
    let access_urls = payload["data"]["access_urls"].as_array().unwrap();
    assert_eq!(access_urls.len(), 1);
    assert!(access_urls[0]
        .as_str()
        .is_some_and(|url| url.starts_with("https://dashboard.example.com/")));
    assert!(access_urls
        .iter()
        .all(|url| !url.as_str().is_some_and(|url| url.contains("localhost"))));
    let config: Value =
        serde_json::from_slice(&std::fs::read(socket_dir(&tmp).join("dashboard.config")).unwrap())
            .unwrap();
    assert!(config["access_token"]
        .as_str()
        .is_some_and(|token| token.len() == 64));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(socket_dir(&tmp).join("dashboard.config"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn dashboard_listens_on_and_authorizes_ipv6_loopback() {
    // Some minimal CI/container kernels disable IPv6 entirely. In that case
    // the production server deliberately remains available on IPv4 only.
    if TcpListener::bind((Ipv6Addr::LOCALHOST, 0)).is_err() {
        return;
    }

    let tmp = TempDir::new().unwrap();
    let _cleanup = DashboardCleanup(&tmp);
    let port = unused_loopback_port();
    let port_arg = port.to_string();

    let started = run_dashboard(&tmp, &["dashboard", "start", "--port", &port_arg, "--json"]);
    assert!(started.status.success());

    let response = wait_for_dashboard(
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        &format!("[::1]:{port}"),
        &format!("http://[::1]:{port}"),
    );
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "unexpected IPv6 dashboard response: {response}"
    );
}

#[test]
fn invalid_dashboard_options_fail_without_starting_a_server() {
    let cases: &[&[&str]] = &[
        &["dashboard", "--bogus", "--json"],
        &[
            "dashboard",
            "start",
            "--allowed-origin",
            "https://dashboard.example.com",
            "--json",
        ],
        &["dashboard", "start", "--port", "nope", "--json"],
        &["dashboard", "start", "--port", "0", "--json"],
        &["dashboard", "start", "--port", "--json"],
        &[
            "dashboard",
            "start",
            "--allowed-origins",
            "https://dashboard.example.com,invalid",
            "--json",
        ],
    ];

    for args in cases {
        let tmp = TempDir::new().unwrap();
        let output = run_dashboard(&tmp, args);
        assert!(
            !output.status.success(),
            "invalid arguments unexpectedly succeeded: {args:?}"
        );
        assert_eq!(json_output(&output)["success"], false);
        assert!(!socket_dir(&tmp).join("dashboard.pid").exists());
        assert!(!socket_dir(&tmp).join("dashboard.config").exists());
    }
}

#[test]
fn running_dashboard_rejects_configuration_changes() {
    let tmp = TempDir::new().unwrap();
    let port = unused_loopback_port();
    let _dashboard = seed_running_dashboard(
        &tmp,
        port,
        &[
            "https://dashboard.example.com",
            "https://second.example.com",
        ],
    );
    let port_arg = port.to_string();
    let start_args = [
        "dashboard",
        "--port",
        &port_arg,
        "--allowed-origins",
        "https://dashboard.example.com,https://second.example.com",
        "--json",
    ];

    let repeated = run_dashboard(&tmp, &start_args);
    assert!(repeated.status.success());
    assert_eq!(json_output(&repeated)["data"]["already_running"], true);

    let changed = run_dashboard(
        &tmp,
        &[
            "dashboard",
            "start",
            "--port",
            &port_arg,
            "--allowed-origins",
            "https://different.example.com",
            "--json",
        ],
    );
    assert!(
        !changed.status.success(),
        "changed allowlist unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&changed.stdout),
        String::from_utf8_lossy(&changed.stderr)
    );
    let payload = json_output(&changed);
    assert_eq!(payload["success"], false);
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("dashboard stop")));

    let changed_port = "1";
    let changed = run_dashboard(
        &tmp,
        &[
            "dashboard",
            "start",
            "--port",
            changed_port,
            "--allowed-origins",
            "https://dashboard.example.com,https://second.example.com",
            "--json",
        ],
    );
    assert!(
        !changed.status.success(),
        "changed port unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&changed.stdout),
        String::from_utf8_lossy(&changed.stderr)
    );
    assert!(json_output(&changed)["error"]
        .as_str()
        .is_some_and(|error| error.contains("dashboard stop")));
}
