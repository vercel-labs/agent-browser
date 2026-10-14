//! Opt-in coverage of owned Moli sessions through the public CLI.

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::process::Stdio;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{sleep, timeout, Duration};
use tokio_tungstenite::tungstenite::Message;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

struct Session {
    dir: TempDir,
    binary: String,
}

impl Session {
    fn new() -> Self {
        let binary = std::env::var("MOLI_BIN").expect("MOLI_BIN is required for Moli E2E tests");
        assert!(std::path::Path::new(&binary).is_file(), "invalid MOLI_BIN");
        let binary = std::fs::canonicalize(binary)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        Self { dir, binary }
    }

    fn command(&self) -> std::process::Command {
        let mut command = std::process::Command::new(BIN);
        for (key, _) in std::env::vars_os() {
            let name = key.to_string_lossy();
            if name.starts_with("AGENT_BROWSER_")
                || name.starts_with("MOLI_")
                || matches!(
                    name.to_ascii_lowercase().as_str(),
                    "http_proxy" | "https_proxy" | "all_proxy" | "no_proxy"
                )
            {
                command.env_remove(key);
            }
        }
        command
            .current_dir(self.dir.path())
            .env("AGENT_BROWSER_SOCKET_DIR", self.dir.path())
            .env("NO_COLOR", "1")
            .args(["--config", "config.json", "--session", "m", "--json"])
            .stdin(Stdio::null());
        command
    }

    async fn response(&self, args: &[&str]) -> Value {
        let output = timeout(
            Duration::from_secs(30),
            tokio::process::Command::from(self.command())
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("CLI timed out")
        .unwrap();
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            output.status.success(),
            response["success"] == true,
            "{output:?}"
        );
        response
    }

    async fn run(&self, args: &[&str]) -> Value {
        let response = self.response(args).await;
        assert_eq!(response["success"], true, "{args:?}: {response}");
        response["data"].clone()
    }

    async fn open(&self, url: &str, launch_args: &[&str]) -> Value {
        let mut args = vec!["--engine", "moli", "--executable-path", &self.binary];
        args.extend_from_slice(launch_args);
        args.extend(["open", url]);
        self.run(&args).await
    }

    async fn evaluate(&self, script: &str) -> Value {
        self.run(&["eval", script]).await["result"].clone()
    }

    async fn wait_for(&self, script: &str, expected: Value) {
        timeout(Duration::from_secs(10), async {
            loop {
                if self.evaluate(script).await == expected {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("page did not reach expected state");
    }

    async fn close(&self, port: u16) {
        assert_eq!(self.run(&["close"]).await["closed"], true);
        timeout(Duration::from_secs(5), async {
            while self.dir.path().join("m.pid").exists() || self.dir.path().join("m.sock").exists()
            {
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("daemon did not clean up");
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.dir.path().join("m.pid").exists() {
            let _ = self.command().arg("close").output();
        }
    }
}

struct Fixture {
    url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new(body: &'static str) -> Self {
        Self::with_proxy_auth(body, None).await
    }

    async fn with_proxy_auth(body: &'static str, credentials: Option<&str>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded_requests = requests.clone();
        let authorization = credentials.map(|value| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(value)
            )
        });
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 8192];
                let count = stream.read(&mut request).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&request[..count]).into_owned();
                let authorized = authorization.as_ref().is_none_or(|expected| {
                    request.lines().any(|line| {
                        line.split_once(':').is_some_and(|(name, value)| {
                            name.eq_ignore_ascii_case("proxy-authorization")
                                && value.trim() == expected
                        })
                    })
                });
                recorded_requests.lock().unwrap().push(request);
                let (status, body) = if authorized {
                    ("200 OK", body)
                } else {
                    ("407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"moli-test\"", "Proxy authentication required")
                };
                let response = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Self {
            url,
            requests,
            task,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type Stream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn message(stream: &mut Stream, matches: impl Fn(&Value) -> bool) -> Value {
    timeout(Duration::from_secs(15), async {
        while let Some(Ok(Message::Text(text))) = stream.next().await {
            let value: Value = serde_json::from_str(&text).unwrap();
            if matches(&value) {
                return value;
            }
        }
        panic!("stream closed");
    })
    .await
    .expect("stream message timed out")
}

#[tokio::test]
#[ignore = "requires MOLI_BIN"]
async fn e2e_moli_launch_args() {
    let session = Session::new();
    let fixture = Fixture::new(
        "<!doctype html><script>globalThis.initialUserAgent = navigator.userAgent</script>",
    )
    .await;
    for separator in [",", "\n"] {
        let args = ["--user-agent=Moli extra", "--http-timeout=30000"].join(separator);
        session.open(&fixture.url, &["--args", &args]).await;
        assert_eq!(
            session.evaluate("globalThis.initialUserAgent").await,
            "Moli extra"
        );
        let port = session.run(&["stream", "status"]).await["port"]
            .as_u64()
            .unwrap() as u16;
        session.close(port).await;
    }
    for (args, expected) in [
        ("--not-a-moli-option", "unexpected argument"),
        ("--port=0", "cannot be used multiple times"),
        ("--layout", "cannot be used multiple times"),
    ] {
        let response = session
            .response(&[
                "--engine",
                "moli",
                "--executable-path",
                &session.binary,
                "--args",
                args,
                "open",
                &fixture.url,
            ])
            .await;
        assert_eq!(response["success"], false, "{response}");
        assert!(
            response["error"].as_str().unwrap().contains(expected),
            "{response}"
        );
        let port = session.run(&["stream", "status"]).await["port"]
            .as_u64()
            .unwrap() as u16;
        session.close(port).await;
    }
}

#[tokio::test]
#[ignore = "requires MOLI_BIN"]
async fn e2e_moli_proxy_authentication() {
    let session = Session::new();
    let proxy = Fixture::with_proxy_auth(
        "<!doctype html><title>Authenticated proxy</title>",
        Some("user name:p@ss:w/rd?#%"),
    )
    .await;
    for credentials in [
        "user name:p@ss:w/rd?#%",
        "user%20name:p%40ss%3Aw%2Frd%3F%23%25",
    ] {
        let proxy_url = proxy
            .url
            .replacen("http://", &format!("http://{credentials}@"), 1);
        let opened = session
            .open("http://moli-proxy.test/", &["--proxy", &proxy_url])
            .await;
        assert_eq!(opened["title"], "Authenticated proxy");
        let port = session.run(&["stream", "status"]).await["port"]
            .as_u64()
            .unwrap() as u16;
        session.close(port).await;
    }
    assert!(proxy
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.starts_with("GET http://moli-proxy.test/ ")));
}

#[tokio::test]
#[ignore = "requires MOLI_BIN"]
async fn e2e_moli_profile_persists_local_storage_across_restart() {
    let session = Session::new();
    let fixture = Fixture::new(
        "<!doctype html><script>window.initialStorage = localStorage.getItem('login')</script>",
    )
    .await;
    let profile = session.dir.path().join("profile with spaces");
    let other_profile = session.dir.path().join("other profile");
    session
        .open(&fixture.url, &["--profile", profile.to_str().unwrap()])
        .await;
    let port = session.run(&["stream", "status"]).await["port"]
        .as_u64()
        .unwrap() as u16;
    assert_eq!(session.evaluate("window.initialStorage").await, Value::Null);
    session
        .evaluate("localStorage.setItem('login', 'profile-token')")
        .await;
    session.close(port).await;
    assert!(profile.is_dir());

    for (args, expected) in [
        (
            vec!["--profile", profile.to_str().unwrap()],
            json!("profile-token"),
        ),
        (
            vec!["--profile", other_profile.to_str().unwrap()],
            Value::Null,
        ),
        (vec![], Value::Null),
    ] {
        session.open(&fixture.url, &args).await;
        let port = session.run(&["stream", "status"]).await["port"]
            .as_u64()
            .unwrap() as u16;
        assert_eq!(session.evaluate("window.initialStorage").await, expected);
        session.close(port).await;
    }
}

#[tokio::test]
#[ignore = "requires MOLI_BIN"]
async fn e2e_moli_state_is_loaded_before_first_navigation() {
    let session = Session::new();
    let fixture = Fixture::new("<!doctype html><script>window.initialStorage = [localStorage.getItem('login'), sessionStorage.getItem('tab')]</script>").await;
    let state_path = session.dir.path().join("auth.json");
    std::fs::write(&state_path, json!({
        "cookies": [{"name": "login", "value": "state-token", "domain": "127.0.0.1", "path": "/", "expires": -1, "httpOnly": true}],
        "origins": [{"origin": fixture.url, "localStorage": [{"name":"login", "value":"state-token"}], "sessionStorage": [{"name":"tab", "value":"restored"}]}]
    }).to_string()).unwrap();
    session
        .open(
            &format!("{}/account", fixture.url),
            &["--state", state_path.to_str().unwrap()],
        )
        .await;
    let port = session.run(&["stream", "status"]).await["port"]
        .as_u64()
        .unwrap() as u16;
    assert_eq!(
        session.evaluate("window.initialStorage").await,
        json!(["state-token", "restored"])
    );
    {
        let requests = fixture.requests.lock().unwrap();
        let account: Vec<_> = requests
            .iter()
            .filter(|r| r.starts_with("GET /account "))
            .collect();
        assert_eq!(account.len(), 1, "{requests:?}");
        assert!(
            account[0]
                .to_ascii_lowercase()
                .contains("cookie: login=state-token"),
            "{}",
            account[0]
        );
    }
    let saved_path = session.dir.path().join("saved.json");
    session
        .run(&["state", "save", saved_path.to_str().unwrap()])
        .await;
    session.close(port).await;

    session
        .open(
            &format!("{}/account", fixture.url),
            &["--state", saved_path.to_str().unwrap()],
        )
        .await;
    let port = session.run(&["stream", "status"]).await["port"]
        .as_u64()
        .unwrap() as u16;
    assert_eq!(
        session.evaluate("window.initialStorage").await,
        json!(["state-token", "restored"])
    );
    let cookies = session.run(&["cookies"]).await;
    assert!(cookies["cookies"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["name"] == "login" && c["value"] == "state-token" && c["httpOnly"] == true));
    session.close(port).await;

    session.open(&fixture.url, &[]).await;
    let port = session.run(&["stream", "status"]).await["port"]
        .as_u64()
        .unwrap() as u16;
    assert_eq!(
        session.evaluate("window.initialStorage").await,
        json!([null, null])
    );
    assert_eq!(session.run(&["cookies"]).await["cookies"], json!([]));
    session.close(port).await;

    std::fs::write(&state_path, "not JSON").unwrap();
    let failed = session
        .response(&[
            "--engine",
            "moli",
            "--executable-path",
            &session.binary,
            "--state",
            state_path.to_str().unwrap(),
            "open",
            &fixture.url,
        ])
        .await;
    assert_eq!(failed["success"], false);
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("Invalid state file"),
        "{failed}"
    );
    let info = session.run(&["session", "info"]).await;
    assert_eq!(info["runtime"]["browserLaunched"], false, "{info}");
    let port = session.run(&["stream", "status"]).await["port"]
        .as_u64()
        .unwrap() as u16;
    session.close(port).await;
}

#[tokio::test]
#[ignore = "requires MOLI_BIN"]
async fn e2e_moli_dashboard_stream_input_tabs_and_url() {
    let session = Session::new();
    let fixture = Fixture::new(include_str!("fixtures/moli-dashboard.html")).await;
    let opened = session.open(&fixture.url, &[]).await;
    assert_eq!(opened["lifecycle"]["effectiveLaunch"]["engine"], "moli");
    let port = session.run(&["stream", "status"]).await["port"]
        .as_u64()
        .unwrap() as u16;
    let (mut stream, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    message(&mut stream, |v| {
        v["type"] == "status" && v["engine"] == "moli" && v["screencasting"] == true
    })
    .await;
    let first = message(&mut stream, |v| v["type"] == "frame").await;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(first["data"].as_str().unwrap())
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap();
    assert!(image.width() > 0 && image.height() > 0);

    for (x, y) in [(50, 40), (50, 100)] {
        for event in ["mousePressed", "mouseReleased"] {
            stream
                .send(Message::Text(
                    json!({"type":"input_mouse", "eventType":event,
                "x":x, "y":y, "button":"left", "clickCount":1})
                    .to_string(),
                ))
                .await
                .unwrap();
        }
    }
    for event in ["keyDown", "keyUp"] {
        stream
            .send(Message::Text(
                json!({"type":"input_keyboard", "eventType":event,
            "key":"a", "code":"KeyA", "text":"a", "windowsVirtualKeyCode":65})
                .to_string(),
            ))
            .await
            .unwrap();
    }
    session
        .wait_for(
            "[document.querySelector('button').textContent, document.querySelector('input').value]",
            json!(["Clicked", "a"]),
        )
        .await;
    message(&mut stream, |v| {
        v["type"] == "frame" && v["data"] != first["data"]
    })
    .await;

    for (script, suffix) in [
        ("history.pushState({}, '', '/spa')", "/spa"),
        ("history.replaceState({}, '', '/replace')", "/replace"),
        ("location.hash = 'section'", "/replace#section"),
    ] {
        session.evaluate(script).await;
        message(&mut stream, |v| {
            v["type"] == "url" && v["url"] == format!("{}{suffix}", fixture.url)
        })
        .await;
    }
    session
        .run(&["tab", "new", &format!("{}/second", fixture.url)])
        .await;
    message(&mut stream, |v| {
        v["type"] == "tabs"
            && v["tabs"].as_array().is_some_and(|tabs| {
                tabs.iter().any(|tab| {
                    tab["active"] == true && tab["url"] == format!("{}/second", fixture.url)
                })
            })
    })
    .await;
    session.run(&["tab", "t1"]).await;
    message(&mut stream, |v| {
        v["type"] == "tabs"
            && v["tabs"].as_array().is_some_and(|tabs| {
                tabs.iter()
                    .any(|tab| tab["active"] == true && tab["tabId"] == "t1")
            })
    })
    .await;
    session
        .evaluate("history.pushState({}, '', '/after-switch')")
        .await;
    message(&mut stream, |v| {
        v["type"] == "url" && v["url"] == format!("{}/after-switch", fixture.url)
    })
    .await;
    message(&mut stream, |v| v["type"] == "frame").await;
    stream.close(None).await.unwrap();
    session.close(port).await;
}
