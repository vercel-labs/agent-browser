#![cfg(unix)]

use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");

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
                        thread::spawn(move || {
                            let mut request = [0u8; 8192];
                            let read = stream.read(&mut request).unwrap_or(0);
                            let request = String::from_utf8_lossy(&request[..read]);
                            let path = request.split_whitespace().nth(1).unwrap_or("/");
                            let body =
                                format!("<!doctype html><title>{path}</title><main>{path}</main>");
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = stream.write_all(response.as_bytes());
                            let _ = stream.flush();
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
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

    fn url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}{}", self.port, path)
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

struct Sessions {
    socket_dir: TempDir,
    names: Vec<&'static str>,
}

impl Sessions {
    fn new() -> Self {
        Self {
            socket_dir: TempDir::new().unwrap(),
            names: vec![
                "isolate-stress-host",
                "isolate-stress-agent-a",
                "isolate-stress-agent-b",
            ],
        }
    }

    fn command(&self, session: &str) -> Command {
        let mut command = Command::new(BIN);
        command
            .args(["--session", session])
            .env("AGENT_BROWSER_SOCKET_DIR", self.socket_dir.path())
            .env_remove("XDG_RUNTIME_DIR")
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .env_remove("AGENT_BROWSER_PIN_TAB")
            .env_remove("AGENT_BROWSER_ISOLATE_CONTEXT")
            .env("NO_COLOR", "1");
        command
    }

    fn run_json(&self, session: &str, args: &[&str]) -> Value {
        let output = self
            .command(session)
            .args(args)
            .arg("--json")
            .output()
            .expect("failed to run agent-browser");
        assert!(
            output.status.success(),
            "command failed for {session}: {args:?}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout was not JSON for {session}: {args:?}: {error}\n{}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn pid(&self, session: &str) -> i32 {
        std::fs::read_to_string(self.socket_dir.path().join(format!("{session}.pid")))
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn stop_daemon(&self, session: &str) {
        let pid = self.pid(session);
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if unsafe { libc::kill(pid, 0) } != 0 {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("daemon {session} did not stop after SIGTERM");
    }
}

impl Drop for Sessions {
    fn drop(&mut self) {
        for session in &self.names {
            let _ = self.command(session).arg("close").output();
        }
    }
}

fn set_probe(sessions: &Sessions, session: &str, value: &str) {
    let script = format!(
        r#"(async () => {{
            document.cookie = "stress_cookie={value}; Path=/; SameSite=Lax";
            localStorage.setItem("stress_value", "{value}");
            sessionStorage.setItem("stress_value", "{value}");
            await new Promise((resolve, reject) => {{
                const request = indexedDB.open("stress_db", 1);
                request.onupgradeneeded = () => request.result.createObjectStore("values");
                request.onerror = () => reject(request.error);
                request.onsuccess = () => {{
                    const transaction = request.result.transaction("values", "readwrite");
                    transaction.objectStore("values").put("{value}", "stress_value");
                    transaction.oncomplete = resolve;
                    transaction.onerror = () => reject(transaction.error);
                }};
            }});
            const cache = await caches.open("stress_cache");
            await cache.put("/stress-value", new Response("{value}"));
            return true;
        }})()"#
    );
    sessions.run_json(session, &["eval", &script]);
}

fn read_probe(sessions: &Sessions, session: &str) -> Value {
    let response = sessions.run_json(
        session,
        &[
            "eval",
            r#"(async () => {
                const indexedDb = await new Promise((resolve, reject) => {
                    const request = indexedDB.open("stress_db", 1);
                    request.onerror = () => reject(request.error);
                    request.onsuccess = () => {
                        const transaction = request.result.transaction("values", "readonly");
                        const get = transaction.objectStore("values").get("stress_value");
                        get.onsuccess = () => resolve(get.result ?? null);
                        get.onerror = () => reject(get.error);
                    };
                });
                const cached = await (await caches.open("stress_cache")).match("/stress-value");
                return {
                    cookie: document.cookie,
                    local: localStorage.getItem("stress_value"),
                    session: sessionStorage.getItem("stress_value"),
                    indexedDb,
                    cached: cached ? await cached.text() : null
                };
            })()"#,
        ],
    );
    response["data"]["result"].clone()
}

fn assert_probe(value: &Value, expected: &str, context: &str) {
    assert!(
        value["cookie"]
            .as_str()
            .is_some_and(|cookie| cookie.contains(&format!("stress_cookie={expected}"))),
        "{context}: cookie mismatch: {value}"
    );
    assert_eq!(value["local"], expected, "{context}: localStorage");
    assert_eq!(value["session"], expected, "{context}: sessionStorage");
    assert_eq!(value["indexedDb"], expected, "{context}: IndexedDB");
    assert_eq!(value["cached"], expected, "{context}: Cache API");
}

fn tabs(sessions: &Sessions, session: &str) -> Vec<Value> {
    sessions.run_json(session, &["tab", "list"])["data"]["tabs"]
        .as_array()
        .unwrap()
        .clone()
}

fn target_for_url(sessions: &Sessions, session: &str, url: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(target) = tabs(sessions, session)
            .into_iter()
            .find(|tab| tab["url"] == url)
            .and_then(|tab| tab["targetId"].as_str().map(str::to_string))
        {
            return target;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{session} did not discover {url}");
}

fn assert_target_isolation(sessions: &Sessions, agent_a: &str, agent_b: &str, round: usize) {
    let targets_a: HashSet<String> = tabs(sessions, agent_a)
        .into_iter()
        .filter_map(|tab| tab["targetId"].as_str().map(str::to_string))
        .collect();
    let targets_b: HashSet<String> = tabs(sessions, agent_b)
        .into_iter()
        .filter_map(|tab| tab["targetId"].as_str().map(str::to_string))
        .collect();
    assert!(
        targets_a.is_disjoint(&targets_b),
        "round {round}: isolated sessions share targets: {:?}",
        targets_a.intersection(&targets_b).collect::<Vec<_>>()
    );
}

fn wait_for_host_target_partition(
    sessions: &Sessions,
    host: &str,
    present: &str,
    absent: &str,
    context: &str,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let urls: Vec<String> = tabs(sessions, host)
            .into_iter()
            .filter_map(|tab| tab["url"].as_str().map(str::to_string))
            .collect();
        let has_present = present.is_empty() || urls.iter().any(|url| url.contains(present));
        let has_absent = !absent.is_empty() && urls.iter().any(|url| url.contains(absent));
        if has_present && !has_absent {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("{context}: host target partition did not settle");
}

fn assert_saved_state(
    path: &std::path::Path,
    expected: &[&str],
    forbidden: &[&str],
    context: &str,
) {
    let state = std::fs::read_to_string(path).unwrap();
    for value in expected {
        assert!(
            state.contains(value),
            "{context}: saved state omitted {value}: {state}"
        );
    }
    for value in forbidden {
        assert!(
            !state.contains(value),
            "{context}: saved state leaked {value}: {state}"
        );
    }
}

fn stress_rounds() -> usize {
    std::env::var("AGENT_BROWSER_ISOLATION_STRESS_ROUNDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(6)
}

#[test]
#[ignore]
fn shared_chrome_real_world_isolation_stress() {
    let server = TestServer::start();
    let sessions = Sessions::new();
    let host = "isolate-stress-host";
    let agent_a = "isolate-stress-agent-a";
    let agent_b = "isolate-stress-agent-b";
    let base_url = server.url("127.0.0.1", "/app");
    let alias_base = server.url("localhost", "/popup");

    sessions.run_json(host, &["open", &base_url]);
    let cdp = sessions.run_json(host, &["get", "cdp-url"]);
    let ws_url = cdp["data"]["cdpUrl"].as_str().unwrap().to_string();

    thread::scope(|scope| {
        let a = scope.spawn(|| {
            sessions.run_json(
                agent_a,
                &[
                    "--cdp",
                    &ws_url,
                    "--isolate-context",
                    "--pin-tab",
                    "open",
                    &base_url,
                ],
            )
        });
        let b = scope.spawn(|| {
            sessions.run_json(
                agent_b,
                &[
                    "--cdp",
                    &ws_url,
                    "--isolate-context",
                    "--pin-tab",
                    "open",
                    &base_url,
                ],
            )
        });
        a.join().unwrap();
        b.join().unwrap();
    });

    set_probe(&sessions, agent_a, "A-INITIAL");
    set_probe(&sessions, agent_b, "B-INITIAL");
    assert_target_isolation(&sessions, agent_a, agent_b, 0);

    let mut previous_a: Option<String> = None;
    let mut previous_b: Option<String> = None;
    let rounds = stress_rounds();

    for round in 0..rounds {
        let work_a = server.url("127.0.0.1", &format!("/work?agent=a&round={round}"));
        let work_b = server.url("127.0.0.1", &format!("/work?agent=b&round={round}"));

        let (target_a, target_b) = thread::scope(|scope| {
            let a = scope.spawn(|| {
                if round % 2 == 0 {
                    sessions.run_json(agent_a, &["tab", "new", &work_a])["data"]["targetId"]
                        .as_str()
                        .unwrap()
                        .to_string()
                } else {
                    sessions.run_json(agent_a, &["window", "new"]);
                    sessions.run_json(agent_a, &["open", &work_a]);
                    target_for_url(&sessions, agent_a, &work_a)
                }
            });
            let b = scope.spawn(|| {
                if round % 2 == 0 {
                    sessions.run_json(agent_b, &["window", "new"]);
                    sessions.run_json(agent_b, &["open", &work_b]);
                    target_for_url(&sessions, agent_b, &work_b)
                } else {
                    sessions.run_json(agent_b, &["tab", "new", &work_b])["data"]["targetId"]
                        .as_str()
                        .unwrap()
                        .to_string()
                }
            });
            (a.join().unwrap(), b.join().unwrap())
        });

        if let Some(target) = previous_a.take() {
            sessions.run_json(agent_a, &["tab", "close", &target]);
        }
        if let Some(target) = previous_b.take() {
            sessions.run_json(agent_b, &["tab", "close", &target]);
        }

        let value_a = format!("A-{round}");
        let value_b = format!("B-{round}");
        thread::scope(|scope| {
            let a = scope.spawn(|| set_probe(&sessions, agent_a, &value_a));
            let b = scope.spawn(|| set_probe(&sessions, agent_b, &value_b));
            a.join().unwrap();
            b.join().unwrap();
        });

        let popup_a = format!("{alias_base}?agent=a&round={round}");
        let popup_b = format!("{alias_base}?agent=b&round={round}");
        thread::scope(|scope| {
            let a = scope.spawn(|| {
                let script = format!(
                    "window.open({}); true",
                    serde_json::to_string(&popup_a).unwrap()
                );
                sessions.run_json(agent_a, &["eval", &script]);
            });
            let b = scope.spawn(|| {
                let script = format!(
                    "window.open({}); true",
                    serde_json::to_string(&popup_b).unwrap()
                );
                sessions.run_json(agent_b, &["eval", &script]);
            });
            a.join().unwrap();
            b.join().unwrap();
        });

        let popup_target_a = target_for_url(&sessions, agent_a, &popup_a);
        let popup_target_b = target_for_url(&sessions, agent_b, &popup_b);
        assert_target_isolation(&sessions, agent_a, agent_b, round);

        sessions.run_json(agent_a, &["tab", &popup_target_a]);
        sessions.run_json(agent_b, &["tab", &popup_target_b]);
        let alias_a = format!("A-ALIAS-{round}");
        let alias_b = format!("B-ALIAS-{round}");
        set_probe(&sessions, agent_a, &alias_a);
        set_probe(&sessions, agent_b, &alias_b);

        sessions.run_json(agent_a, &["tab", &target_a]);
        sessions.run_json(agent_b, &["tab", &target_b]);
        assert_probe(
            &read_probe(&sessions, agent_a),
            &value_a,
            &format!("round {round} agent A"),
        );
        assert_probe(
            &read_probe(&sessions, agent_b),
            &value_b,
            &format!("round {round} agent B"),
        );

        let state_a = sessions
            .socket_dir
            .path()
            .join(format!("state-a-{round}.json"));
        let state_b = sessions
            .socket_dir
            .path()
            .join(format!("state-b-{round}.json"));
        thread::scope(|scope| {
            let a = scope.spawn(|| {
                sessions.run_json(agent_a, &["state", "save", state_a.to_str().unwrap()])
            });
            let b = scope.spawn(|| {
                sessions.run_json(agent_b, &["state", "save", state_b.to_str().unwrap()])
            });
            a.join().unwrap();
            b.join().unwrap();
        });
        assert_saved_state(
            &state_a,
            &[&value_a, &alias_a],
            &[&value_b, &alias_b],
            &format!("round {round} agent A"),
        );
        assert_saved_state(
            &state_b,
            &[&value_b, &alias_b],
            &[&value_a, &alias_a],
            &format!("round {round} agent B"),
        );

        sessions.run_json(agent_a, &["tab", "close", &popup_target_a]);
        sessions.run_json(agent_b, &["tab", "close", &popup_target_b]);
        sessions.run_json(agent_a, &["tab", &target_a]);
        sessions.run_json(agent_b, &["tab", &target_b]);

        if round == rounds / 2 {
            thread::scope(|scope| {
                let stop = scope.spawn(|| sessions.stop_daemon(agent_a));
                let active = scope.spawn(|| {
                    set_probe(&sessions, agent_b, &format!("B-DURING-RESTART-{round}"));
                    read_probe(&sessions, agent_b)
                });
                stop.join().unwrap();
                assert_probe(
                    &active.join().unwrap(),
                    &format!("B-DURING-RESTART-{round}"),
                    "agent B while agent A restarts",
                );
            });
            sessions.run_json(agent_a, &["--cdp", &ws_url, "get", "url"]);
            assert_probe(
                &read_probe(&sessions, agent_a),
                &value_a,
                &format!("round {round} agent A after daemon restart"),
            );
        }

        previous_a = Some(target_a);
        previous_b = Some(target_b);
    }

    sessions.run_json(agent_a, &["close"]);
    assert!(
        !sessions
            .socket_dir
            .path()
            .join(format!("{agent_a}.target"))
            .exists(),
        "agent A close left its isolated binding behind"
    );
    wait_for_host_target_partition(&sessions, host, "agent=b", "agent=a", "after agent A close");
    let final_b = read_probe(&sessions, agent_b);
    assert!(
        final_b["local"]
            .as_str()
            .is_some_and(|value| value.starts_with("B-")),
        "agent B lost state after agent A close: {final_b}"
    );
    sessions.run_json(agent_b, &["close"]);
    assert!(
        !sessions
            .socket_dir
            .path()
            .join(format!("{agent_b}.target"))
            .exists(),
        "agent B close left its isolated binding behind"
    );
    wait_for_host_target_partition(&sessions, host, "", "agent=b", "after agent B close");
    sessions.run_json(host, &["close"]);
}
