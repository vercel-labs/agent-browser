//! The MCP wrapper must return while the browser daemon remains alive.
#![cfg(windows)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_agent-browser");
const SESSION: &str = "mcp-stdio-lifetime";

struct Server {
    child: Child,
    sockets: TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Closing via another CLI must also clean up after an MCP timeout.
        let _ = Command::new(BIN)
            .args(["--session", SESSION, "close"])
            .env("AGENT_BROWSER_SOCKET_DIR", self.sockets.path())
            .env_remove("AGENT_BROWSER_NAMESPACE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "launches real Chrome; set AGENT_BROWSER_EXECUTABLE_PATH if needed"]
fn mcp_open_returns_before_the_browser_is_closed() {
    let sockets = TempDir::new().unwrap();
    let child = Command::new(BIN)
        .args(["mcp", "--tools", "all"])
        .env("AGENT_BROWSER_SOCKET_DIR", sockets.path())
        .env_remove("AGENT_BROWSER_NAMESPACE")
        .env_remove("AGENT_BROWSER_DAEMON")
        .env_remove("AGENT_BROWSER_SESSION")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server { child, sockets };
    let output = server.child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(output).lines() {
            let Ok(line) = line else { break };
            let message = serde_json::from_str::<Value>(&line).unwrap();
            if tx.send(message).is_err() {
                break;
            }
        }
    });
    let input = server.child.stdin.as_mut().unwrap();
    writeln!(
        input,
        "{}",
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion":"2025-11-25", "capabilities":{},
                "clientInfo":{"name":"mcp-lifetime-test","version":"1"}}
        })
    )
    .unwrap();
    let initialized = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(initialized["id"], 1);
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(input, "{}", json!({
        "jsonrpc":"2.0","id":2,"method":"tools/call",
        "params":{"name":"agent_browser_open","arguments":{"url":"about:blank","session":SESSION}}
    })).unwrap();
    let opened = rx
        .recv_timeout(Duration::from_secs(45))
        .expect("MCP open waited for the browser daemon's inherited output handles to close");
    assert_eq!(opened["id"], 2);
    assert_eq!(opened["result"]["isError"], false, "{opened}");
    assert_eq!(
        opened["result"]["structuredContent"]["response"]["data"]["url"],
        "about:blank"
    );
    writeln!(
        input,
        "{}",
        json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"agent_browser_close","arguments":{"session":SESSION}}
        })
    )
    .unwrap();
    let closed = rx.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_eq!(closed["id"], 3);
    assert_eq!(closed["result"]["isError"], false, "{closed}");
}
