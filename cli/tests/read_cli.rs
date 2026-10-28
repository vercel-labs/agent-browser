use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

struct ReadSession(tempfile::TempDir);

impl Drop for ReadSession {
    fn drop(&mut self) {
        if self.0.path().join("sockets/read-fences.pid").exists() {
            let _ = cli_command(self.0.path(), &["close"]).output();
        }
    }
}

fn cli_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent-browser"));
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("AGENT_BROWSER_")
            || matches!(
                name.to_ascii_uppercase().as_str(),
                "HTTP_PROXY" | "HTTPS_PROXY" | "ALL_PROXY" | "NO_PROXY"
            )
        {
            command.env_remove(key);
        }
    }
    command
        .current_dir(root)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("AGENT_BROWSER_SOCKET_DIR", root.join("sockets"))
        .env("AGENT_BROWSER_SESSION", "read-fences")
        .env("NO_COLOR", "1")
        .args(args);
    command
}

fn run_cli(root: &Path, args: &[&str], input: &str) -> Value {
    let mut child = cli_command(root, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| panic!("{error}: {output:?}"))
}

#[test]
fn cli_and_mcp_preserve_fenced_sections_and_exclude_code_from_outlines() {
    let session = ReadSession(tempfile::tempdir().unwrap());
    let root = &session.0;
    for fence in ["```", "~~~"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/guide", listener.local_addr().unwrap());
        let body = format!("# Guide\n\n## Authentication\nUse token.\n\n{fence}sh\n# configure token\nexport TOKEN=abc\n{fence}\n\nAfter code instructions.\n\n## Other\nNo match.\n");
        let worker = std::thread::spawn(move || {
            for stream in listener.incoming().take(8) {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = BufReader::new(&mut stream);
                loop {
                    let mut line = String::new();
                    assert!(request.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/markdown\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        for (flags, options, outline) in [
            (
                vec!["--filter", "Authentication"],
                json!({"filter": "Authentication"}),
                false,
            ),
            (
                vec!["--filter", "After code instructions"],
                json!({"filter": "After code instructions"}),
                false,
            ),
            (
                vec!["--llms", "full", "--filter", "Authentication"],
                json!({"llms": "full", "filter": "Authentication"}),
                false,
            ),
            (vec!["--outline"], json!({"outline": true}), true),
        ] {
            let mut args = vec!["read", &url, "--json", "--timeout", "2000"];
            args.extend(flags);
            let cli = run_cli(root.path(), &args, "");
            assert_eq!(cli["success"], true, "{cli}");
            let mut arguments = options;
            arguments["url"] = json!(url);
            arguments["readTimeoutMs"] = json!(2000);
            let request = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "agent_browser_read", "arguments": arguments}});
            let mcp = run_cli(
                root.path(),
                &["mcp", "--tools", "all"],
                &format!("{request}\n"),
            );
            assert_eq!(mcp["result"]["isError"], false, "{mcp}");
            assert_eq!(mcp["result"]["structuredContent"]["response"], cli, "{mcp}");
            let content = cli["data"]["content"].as_str().unwrap();
            if outline {
                assert_eq!(
                    content,
                    format!("# Outline\n\nSource: {url}\n\n- Guide\n  - Authentication\n  - Other")
                );
            } else {
                assert!(content.starts_with("## Authentication\n"), "{content}");
                assert!(
                    content.contains("# configure token\nexport TOKEN=abc"),
                    "{content}"
                );
                assert!(content.ends_with("After code instructions."), "{content}");
                assert!(!content.contains("## Other"), "{content}");
            }
        }
        worker.join().unwrap();
    }
}
