use serde_json::{json, Value};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use crate::connection::get_socket_dir;

use super::chat::{chat_status_json, handle_chat_request, handle_models_request};
use super::discovery::discover_sessions;
use super::http::{serve_embedded_file, CORS_HEADERS};

pub async fn run_dashboard_server(port: u16) {
    let addr = format!("127.0.0.1:{}", port);
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind dashboard server on {}: {}", addr, e);
            return;
        }
    };

    loop {
        let Ok((stream, _addr)) = listener.accept().await else {
            break;
        };
        tokio::spawn(async move {
            handle_dashboard_connection(stream).await;
        });
    }
}

async fn handle_dashboard_connection(mut stream: tokio::net::TcpStream) {
    use tokio::io::AsyncReadExt;

    let mut buf = vec![0u8; 8192];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let header_str = std::str::from_utf8(&buf[..n]).unwrap_or("");
    let first_line = header_str.lines().next().unwrap_or("").to_string();
    let method = first_line.split_whitespace().next().unwrap_or("GET");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");
    let origin = header_str.lines().find_map(|line| {
        if line.len() > 8 && line[..8].eq_ignore_ascii_case("origin: ") {
            Some(line[8..].trim().to_string())
        } else {
            None
        }
    });

    if method == "OPTIONS" {
        let response = format!(
            "HTTP/1.1 204 No Content\r\n{CORS_HEADERS}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    if method == "POST" && path == "/api/chat" {
        let body_str = read_post_body(&mut stream, &buf, n).await;
        handle_chat_request(&mut stream, &body_str, origin.as_deref()).await;
        return;
    }

    if method == "GET" && path == "/api/models" {
        handle_models_request(&mut stream, origin.as_deref()).await;
        return;
    }

    if method == "POST" && (path == "/api/sessions" || path == "/api/exec" || path == "/api/kill") {
        let body_str = read_post_body(&mut stream, &buf, n).await;
        let result = if path == "/api/exec" {
            exec_cli(&body_str).await
        } else if path == "/api/kill" {
            kill_session(&body_str).await
        } else {
            spawn_session(&body_str).await
        };
        let (status, resp_body) = match result {
            Ok(msg) => ("200 OK", msg),
            Err(e) => (
                "400 Bad Request",
                format!(
                    r#"{{"success":false,"error":{}}}"#,
                    serde_json::to_string(&e).unwrap_or_else(|_| format!("\"{}\"", e))
                ),
            ),
        };
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
            resp_body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(resp_body.as_bytes()).await;
        return;
    }

    let (status, content_type, body): (&str, &str, Vec<u8>) = if path == "/api/sessions" {
        (
            "200 OK",
            "application/json; charset=utf-8",
            discover_sessions().into_bytes(),
        )
    } else if path == "/api/chat/status" {
        (
            "200 OK",
            "application/json; charset=utf-8",
            chat_status_json().into_bytes(),
        )
    } else {
        serve_embedded_file(path)
    };

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
        status,
        content_type,
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&body).await;
}

async fn read_post_body(stream: &mut tokio::net::TcpStream, initial: &[u8], n: usize) -> String {
    use tokio::io::AsyncReadExt;

    let header_end = initial[..n]
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| {
            initial[..n]
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| p + 2)
        });
    let Some(header_end) = header_end else {
        return String::new();
    };

    let header_str = String::from_utf8_lossy(&initial[..header_end]);
    let content_length: usize = header_str
        .lines()
        .find_map(|l| {
            if l.len() > 16 && l[..16].eq_ignore_ascii_case("content-length: ") {
                l[16..].trim().parse().ok()
            } else {
                let lower = l.to_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse().ok())
            }
        })
        .unwrap_or(0);

    if content_length == 0 {
        return String::new();
    }

    let read_body = &initial[header_end..n];
    let already_read = read_body.len().min(content_length);

    let mut body = Vec::with_capacity(content_length);
    body.extend_from_slice(&read_body[..already_read]);

    let remaining = content_length - already_read;
    if remaining > 0 {
        let mut rest = vec![0u8; remaining];
        if stream.read_exact(&mut rest).await.is_ok() {
            body.extend_from_slice(&rest);
        }
    }

    String::from_utf8(body).unwrap_or_default()
}

async fn exec_cli(body: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let args: Vec<String> = parsed
        .get("args")
        .and_then(|v| v.as_array())
        .ok_or("Missing \"args\" array")?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    if args.is_empty() {
        return Err("Empty args array".to_string());
    }

    let exe = std::env::current_exe().map_err(|e| format!("Cannot resolve executable: {}", e))?;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.args(&args)
        .arg("--json")
        .env_remove("AGENT_BROWSER_DASHBOARD")
        .env_remove("AGENT_BROWSER_DASHBOARD_PORT")
        .env_remove("AGENT_BROWSER_STREAM_PORT");

    let output = cmd
        .output()
        .await
        .map_err(|e| format!("Failed to execute: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    Ok(json!({
        "success": output.status.success(),
        "exit_code": output.status.code(),
        "stdout": stdout,
        "stderr": stderr,
    })
    .to_string())
}

async fn kill_session(body: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let session = parsed
        .get("session")
        .and_then(|v| v.as_str())
        .ok_or("Missing \"session\" field")?;

    if session.is_empty() || session.len() > 64 {
        return Err("Session name must be 1-64 characters".to_string());
    }

    let dir = get_socket_dir();
    let pid_path = dir.join(format!("{}.pid", session));

    let pid_str = std::fs::read_to_string(&pid_path)
        .map_err(|_| format!("No PID file for session '{}'", session))?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .map_err(|_| format!("Invalid PID in file: {}", pid_str.trim()))?;

    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }

    for ext in &["pid", "sock", "stream", "engine", "extensions"] {
        let _ = std::fs::remove_file(dir.join(format!("{}.{}", session, ext)));
    }

    Ok(json!({ "success": true, "killed_pid": pid }).to_string())
}

pub(super) async fn spawn_session(body: &str) -> Result<String, String> {
    let parsed: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;
    let session = parsed
        .get("session")
        .and_then(|v| v.as_str())
        .ok_or("Missing \"session\" field")?;

    if session.is_empty() || session.len() > 64 {
        return Err("Session name must be 1-64 characters".to_string());
    }

    let exe = std::env::current_exe().map_err(|e| format!("Cannot resolve executable: {}", e))?;

    let mut cmd = tokio::process::Command::new(&exe);
    cmd.arg("open")
        .arg("about:blank")
        .arg("--session")
        .arg(session);

    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());

    let status = cmd
        .status()
        .await
        .map_err(|e| format!("Failed to spawn session: {}", e))?;

    if status.success() {
        Ok(format!(
            r#"{{"success":true,"session":{}}}"#,
            serde_json::to_string(session).unwrap_or_default()
        ))
    } else {
        Err(format!("Session process exited with {}", status))
    }
}
