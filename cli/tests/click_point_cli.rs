use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

// A control whose top edge sits inside the viewport and whose centre lies
// below it. The document listener records what a click really hit.
const PAGE: &str = r#"<!DOCTYPE html><html><head><meta charset="utf-8"><title>fold</title>
<style>body{margin:0}
#next{height:50px;display:block;position:absolute;top:calc(100vh - 10px)}</style></head>
<body><h1>Fold</h1><button id="next">Next</button><div style="height:2000px"></div>
<script>document.addEventListener('click', e => { window.__last = e.target.id || e.target.tagName; });</script>
</body></html>"#;

struct TestServer {
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TestServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let server_stop = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            while !server_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request = [0u8; 2048];
                        let _ = stream.read(&mut request).unwrap_or(0);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            PAGE.len(),
                            PAGE
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            port,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/fold", self.port)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Session {
    socket_dir: TempDir,
    name: &'static str,
}

impl Session {
    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(["--session", self.name])
            .env("AGENT_BROWSER_SOCKET_DIR", self.socket_dir.path())
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env("NO_COLOR", "1");
        cmd
    }

    fn run_json(&self, args: &[&str]) -> serde_json::Value {
        let output = self
            .command()
            .args(args)
            .arg("--json")
            .output()
            .expect("failed to run agent-browser");
        assert!(
            output.status.success(),
            "command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout was not JSON: {}\n{}",
                e,
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.command().arg("close").output();
    }
}

/// A selector click on a control that straddles the bottom edge of the viewport
/// must land on that control. The element intersects the viewport, so the old
/// "in view" test skipped the scroll, the click point (the centre) lay below
/// the viewport, and the input landed on the document while the command still
/// reported success.
#[test]
#[ignore]
fn click_lands_on_a_control_that_straddles_the_viewport_edge() {
    let server = TestServer::start();
    let session = Session {
        socket_dir: TempDir::new().unwrap(),
        name: "click-point-cli",
    };
    session.run_json(&["open", &server.url()]);
    let geometry = session.run_json(&[
        "eval",
        "(() => { const r = document.getElementById('next').getBoundingClientRect(); \
         return JSON.stringify({top: r.top, cy: r.top + r.height / 2, ih: innerHeight}); })()",
    ]);
    let geometry: serde_json::Value =
        serde_json::from_str(geometry["data"]["result"].as_str().unwrap()).unwrap();
    let (top, cy, ih) = (
        geometry["top"].as_f64().unwrap(),
        geometry["cy"].as_f64().unwrap(),
        geometry["ih"].as_f64().unwrap(),
    );
    assert!(
        top < ih && cy > ih,
        "the premise: the control straddles the fold ({geometry})"
    );

    session.run_json(&["click", "#next"]);

    let hit = session.run_json(&["eval", "window.__last || ''"]);
    assert_eq!(
        hit["data"]["result"].as_str().unwrap(),
        "next",
        "the click must land on the control, not on the document below the fold"
    );
}
