use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use crate::connection::get_socket_dir;

use super::chat::{chat_status_json, handle_chat_request, handle_models_request};
use super::discovery::discover_sessions;
use super::http::{serve_embedded_file, CORS_HEADERS};

/// Dashboard same-origin proxy endpoints for session metadata and streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionProxyEndpoint {
    Tabs,
    Status,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DashboardProxyError {
    status: &'static str,
    message: String,
}

impl DashboardProxyError {
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: "404 Not Found",
            message: message.into(),
        }
    }

    fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: "502 Bad Gateway",
            message: message.into(),
        }
    }
}

const PROXY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const PROXY_MAX_RESPONSE_SIZE: u64 = 16 * 1024 * 1024;

fn build_json_error_body(error: &str) -> String {
    let escaped = serde_json::to_string(error).unwrap_or_else(|_| format!("\"{}\"", error));
    format!(r#"{{"success":false,"error":{escaped}}}"#)
}

async fn write_http_response_inner(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    include_cors: bool,
) {
    let cors_headers = if include_cors { CORS_HEADERS } else { "" };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{cors_headers}\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(body).await;
}

async fn write_http_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) {
    write_http_response_inner(stream, status, content_type, body, true).await;
}

async fn write_http_response_no_cors(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) {
    write_http_response_inner(stream, status, content_type, body, false).await;
}

async fn write_json_error_response_no_cors(
    stream: &mut tokio::net::TcpStream,
    status: &'static str,
    error: &str,
) {
    let body = build_json_error_body(error);
    write_http_response_no_cors(
        stream,
        status,
        "application/json; charset=utf-8",
        body.as_bytes(),
    )
    .await;
}

fn parse_request_method_and_path(request: &str) -> (&str, &str) {
    let first_line = request.lines().next().unwrap_or("");
    let method = first_line.split_whitespace().next().unwrap_or("GET");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");
    (method, path)
}

fn is_websocket_upgrade(request: &str) -> bool {
    request.lines().any(|line| {
        if let Some((name, value)) = line.split_once(':') {
            name.trim().eq_ignore_ascii_case("upgrade")
                && value.trim().eq_ignore_ascii_case("websocket")
        } else {
            false
        }
    })
}

fn request_header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        if header_name.trim().eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn normalize_origin_authority(origin: &str) -> Option<String> {
    let url = url::Url::parse(origin).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

fn normalize_host_authority(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();

    if let Some(bracket_end) = host.rfind(']') {
        if bracket_end == host.len() - 1 {
            return host;
        }

        if host.as_bytes().get(bracket_end + 1) == Some(&b':') {
            let port = &host[bracket_end + 2..];
            if port == "80" || port == "443" {
                return host[..=bracket_end].to_string();
            }
        }

        return host;
    }

    if let Some((name, port)) = host.rsplit_once(':') {
        if !name.contains(':') && (port == "80" || port == "443") {
            return name.to_string();
        }
    }

    host
}

fn header_matches_host(request: &str, header_name: &str) -> Option<bool> {
    let authority =
        request_header_value(request, header_name).and_then(normalize_origin_authority)?;
    let host = request_header_value(request, "host").map(normalize_host_authority)?;
    Some(authority == host)
}

/// Validates that a proxied WebSocket request either has no Origin header or
/// presents an Origin whose authority matches the request Host header.
fn is_same_origin_ws_request(request: &str) -> bool {
    match header_matches_host(request, "origin") {
        Some(matches) => matches,
        None => request_header_value(request, "origin").is_none(),
    }
}

/// Validates that an HTTP session-proxy request came from a same-origin page.
///
/// For GET requests we require either a same-origin `Origin` or a same-origin
/// `Referer` so browsers cannot hit the proxy routes via side-channel tags or
/// arbitrary cross-origin fetches.
fn is_same_origin_http_request(request: &str) -> bool {
    matches!(header_matches_host(request, "origin"), Some(true))
        || matches!(header_matches_host(request, "referer"), Some(true))
}

/// Parse a dashboard route of the form `/api/session/<port>/<endpoint>`.
fn parse_session_proxy_route(path: &str) -> Result<(u16, SessionProxyEndpoint), &'static str> {
    if !path.starts_with("/api/session/") {
        return Err("Invalid session proxy route.");
    }

    let mut parts = path.split('/');
    if parts.next() != Some("") || parts.next() != Some("api") || parts.next() != Some("session") {
        return Err("Invalid session proxy route.");
    }

    let port_str = parts.next().ok_or("Missing session proxy port.")?;
    if port_str.is_empty() {
        return Err("Missing session proxy port.");
    }

    let endpoint = match parts.next().ok_or("Missing session proxy endpoint.")? {
        "tabs" => SessionProxyEndpoint::Tabs,
        "status" => SessionProxyEndpoint::Status,
        "stream" => SessionProxyEndpoint::Stream,
        _ => return Err("Unknown session proxy endpoint."),
    };

    if parts.next().is_some() {
        return Err("Unexpected path segments in session proxy route.");
    }

    let port = port_str
        .parse::<u16>()
        .map_err(|_| "Session proxy port must be a valid TCP port.")?;
    if port == 0 {
        return Err("Session proxy port must be a valid TCP port.");
    }

    Ok((port, endpoint))
}

fn sessions_json_has_active_port(sessions_json: &str, port: u16) -> Result<bool, String> {
    let sessions: Vec<Value> = serde_json::from_str(sessions_json)
        .map_err(|e| format!("Failed to parse active sessions: {e}"))?;
    Ok(sessions.iter().any(|session| {
        session
            .get("port")
            .and_then(|value| value.as_u64())
            .map(|value| value == u64::from(port))
            .unwrap_or(false)
    }))
}

fn require_active_session_port(port: u16) -> Result<(), DashboardProxyError> {
    let sessions_json = discover_sessions();
    let is_active = sessions_json_has_active_port(&sessions_json, port)
        .map_err(DashboardProxyError::bad_gateway)?;
    if is_active {
        Ok(())
    } else {
        Err(DashboardProxyError::not_found(format!(
            "No active session is listening on port {port}."
        )))
    }
}

fn split_http_response(response: &[u8]) -> Result<(&[u8], &[u8]), String> {
    if let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") {
        let body_start = header_end + 4;
        return Ok((&response[..header_end], &response[body_start..]));
    }

    if let Some(header_end) = response.windows(2).position(|window| window == b"\n\n") {
        let body_start = header_end + 2;
        return Ok((&response[..header_end], &response[body_start..]));
    }

    Err("Upstream response was missing an HTTP header terminator.".to_string())
}

fn parse_upstream_http_response(response: &[u8]) -> Result<(String, String, Vec<u8>), String> {
    let (header_bytes, body) = split_http_response(response)?;
    let header_str = std::str::from_utf8(header_bytes)
        .map_err(|e| format!("Upstream response headers were not valid UTF-8: {e}"))?;

    let mut lines = header_str.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| "Upstream response was missing a status line.".to_string())?;
    let status = status_line
        .split_once(' ')
        .map(|(_, status)| status.trim().to_string())
        .filter(|status| !status.is_empty())
        .ok_or_else(|| "Upstream response status line was malformed.".to_string())?;
    let content_type = lines
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-type") {
                Some(value.trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "application/json; charset=utf-8".to_string());

    Ok((status, content_type, body.to_vec()))
}

/// Proxy dashboard-origin HTTP requests for session tabs or status to the loopback session server.
async fn proxy_session_http_route(
    port: u16,
    endpoint: SessionProxyEndpoint,
) -> Result<(String, String, Vec<u8>), DashboardProxyError> {
    debug_assert!(matches!(
        endpoint,
        SessionProxyEndpoint::Tabs | SessionProxyEndpoint::Status
    ));

    require_active_session_port(port)?;

    let upstream_path = match endpoint {
        SessionProxyEndpoint::Tabs => "/api/tabs",
        SessionProxyEndpoint::Status => "/api/status",
        SessionProxyEndpoint::Stream => unreachable!("stream routes use the WebSocket proxy"),
    };
    let request = format!(
        "GET {upstream_path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );

    tokio::time::timeout(PROXY_TIMEOUT, async {
        let mut upstream = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| {
                DashboardProxyError::bad_gateway(format!(
                    "Failed to connect to session {port}: {e}"
                ))
            })?;
        upstream.write_all(request.as_bytes()).await.map_err(|e| {
            DashboardProxyError::bad_gateway(format!(
                "Failed to proxy request to session {port}: {e}"
            ))
        })?;

        let mut response = Vec::new();
        (&mut upstream)
            .take(PROXY_MAX_RESPONSE_SIZE + 1)
            .read_to_end(&mut response)
            .await
            .map_err(|e| {
                DashboardProxyError::bad_gateway(format!(
                    "Failed to read session {port} response: {e}"
                ))
            })?;
        if response.len() as u64 > PROXY_MAX_RESPONSE_SIZE {
            return Err(DashboardProxyError::bad_gateway(format!(
                "Session {port} response exceeded {PROXY_MAX_RESPONSE_SIZE} bytes."
            )));
        }

        parse_upstream_http_response(&response).map_err(DashboardProxyError::bad_gateway)
    })
    .await
    .map_err(|_| {
        DashboardProxyError::bad_gateway(format!(
            "Session {port} proxy request timed out after {}s.",
            PROXY_TIMEOUT.as_secs()
        ))
    })?
}

/// Bridge a dashboard-origin WebSocket upgrade to the loopback session stream.
async fn proxy_session_stream(mut stream: tokio::net::TcpStream, port: u16) {
    let upstream_url = format!("ws://127.0.0.1:{port}");
    let (upstream_ws, _) = match tokio_tungstenite::connect_async(&upstream_url).await {
        Ok(ws) => ws,
        Err(error) => {
            write_json_error_response_no_cors(
                &mut stream,
                "502 Bad Gateway",
                &format!("Failed to connect to session {port}: {error}"),
            )
            .await;
            return;
        }
    };
    let client_ws = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(_) => return,
    };

    let (mut client_tx, mut client_rx) = client_ws.split();
    let (mut upstream_tx, mut upstream_rx) = upstream_ws.split();

    loop {
        tokio::select! {
            message = client_rx.next() => {
                match message {
                    Some(Ok(message)) => {
                        let is_close = matches!(message, Message::Close(_));
                        if upstream_tx.send(message).await.is_err() {
                            break;
                        }
                        if is_close {
                            break;
                        }
                    }
                    Some(Err(_)) | None => {
                        let _ = upstream_tx.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
            message = upstream_rx.next() => {
                match message {
                    Some(Ok(message)) => {
                        let is_close = matches!(message, Message::Close(_));
                        if client_tx.send(message).await.is_err() {
                            break;
                        }
                        if is_close {
                            break;
                        }
                    }
                    Some(Err(_)) | None => {
                        let _ = client_tx.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        }
    }
}

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
    let mut buf = vec![0u8; 8192];
    let peeked_len = match stream.peek(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let peeked_request = String::from_utf8_lossy(&buf[..peeked_len]);
    let (peeked_method, peeked_path) = parse_request_method_and_path(&peeked_request);

    if peeked_path.starts_with("/api/session/") {
        let (port, endpoint) = match parse_session_proxy_route(peeked_path) {
            Ok(route) => route,
            Err(error) => {
                write_json_error_response_no_cors(&mut stream, "400 Bad Request", error).await;
                return;
            }
        };

        match endpoint {
            SessionProxyEndpoint::Stream => {
                if peeked_method != "GET" {
                    write_json_error_response_no_cors(
                        &mut stream,
                        "400 Bad Request",
                        "Session stream proxy only supports GET WebSocket upgrades.",
                    )
                    .await;
                    return;
                }
                if !is_websocket_upgrade(&peeked_request) {
                    write_json_error_response_no_cors(
                        &mut stream,
                        "400 Bad Request",
                        "Session stream proxy requires a WebSocket upgrade request.",
                    )
                    .await;
                    return;
                }
                if !is_same_origin_ws_request(&peeked_request) {
                    write_json_error_response_no_cors(
                        &mut stream,
                        "403 Forbidden",
                        "Origin does not match Host header.",
                    )
                    .await;
                    return;
                }
                if let Err(error) = require_active_session_port(port) {
                    write_json_error_response_no_cors(&mut stream, error.status, &error.message)
                        .await;
                    return;
                }
                proxy_session_stream(stream, port).await;
                return;
            }
            SessionProxyEndpoint::Tabs | SessionProxyEndpoint::Status => {
                if peeked_method != "GET" {
                    write_json_error_response_no_cors(
                        &mut stream,
                        "400 Bad Request",
                        "Session proxy routes only support GET requests.",
                    )
                    .await;
                    return;
                }
            }
        }
    }

    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let (method, path) = parse_request_method_and_path(&request);
    let origin = request_header_value(&request, "origin").map(|value| value.to_string());

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
            Err(e) => ("400 Bad Request", build_json_error_body(&e)),
        };
        write_http_response(
            &mut stream,
            status,
            "application/json; charset=utf-8",
            resp_body.as_bytes(),
        )
        .await;
        return;
    }

    if path.starts_with("/api/session/") {
        let (port, endpoint) = match parse_session_proxy_route(path) {
            Ok(route) => route,
            Err(error) => {
                write_json_error_response_no_cors(&mut stream, "400 Bad Request", error).await;
                return;
            }
        };

        match endpoint {
            SessionProxyEndpoint::Tabs | SessionProxyEndpoint::Status => {
                if !is_same_origin_http_request(&request) {
                    write_json_error_response_no_cors(
                        &mut stream,
                        "403 Forbidden",
                        "Origin or Referer does not match Host header.",
                    )
                    .await;
                    return;
                }

                match proxy_session_http_route(port, endpoint).await {
                    Ok((status, content_type, body)) => {
                        write_http_response_no_cors(&mut stream, &status, &content_type, &body)
                            .await;
                    }
                    Err(error) => {
                        write_json_error_response_no_cors(
                            &mut stream,
                            error.status,
                            &error.message,
                        )
                        .await;
                    }
                }
                return;
            }
            SessionProxyEndpoint::Stream => {
                write_json_error_response_no_cors(
                    &mut stream,
                    "400 Bad Request",
                    "Session stream proxy requires a WebSocket upgrade request.",
                )
                .await;
                return;
            }
        }
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

    write_http_response(&mut stream, status, content_type, &body).await;
}

async fn read_post_body(stream: &mut tokio::net::TcpStream, initial: &[u8], n: usize) -> String {
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
                l[16..].trim().parse::<usize>().ok()
            } else {
                let lower = l.to_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .and_then(|v| v.trim().parse::<usize>().ok())
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
        // SAFETY: The PID came from the daemon-managed pidfile and is only used
        // to send standard termination signals to that process.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        // SAFETY: A signal value of 0 performs an existence check on the same pid.
        if unsafe { libc::kill(pid as i32, 0) } == 0 {
            // SAFETY: The process still exists after SIGTERM, so escalate to SIGKILL.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_same_origin_ws_request_matching() {
        let req = "GET /api/session/9222/stream HTTP/1.1\r\nHost: localhost:4848\r\nOrigin: http://localhost:4848\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_same_origin_ws_request(req));
    }

    #[test]
    fn test_same_origin_ws_request_proxied() {
        let req = "GET /api/session/9222/stream HTTP/1.1\r\nHost: dashboard.agent-browser.localhost\r\nOrigin: https://dashboard.agent-browser.localhost\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_same_origin_ws_request(req));
    }

    #[test]
    fn test_normalize_origin_authority_https_without_port() {
        assert_eq!(
            normalize_origin_authority("https://dashboard.agent-browser.localhost"),
            Some("dashboard.agent-browser.localhost".to_string())
        );
    }

    #[test]
    fn test_same_origin_ws_request_default_https_port() {
        let req = "GET /api/session/9222/stream HTTP/1.1\r\nHost: dashboard.agent-browser.localhost:443\r\nOrigin: https://dashboard.agent-browser.localhost\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_same_origin_ws_request(req));
    }

    #[test]
    fn test_same_origin_http_request_matching_origin() {
        let req = "GET /api/session/9222/tabs HTTP/1.1\r\nHost: localhost:4848\r\nOrigin: http://localhost:4848\r\n\r\n";
        assert!(is_same_origin_http_request(req));
    }

    #[test]
    fn test_same_origin_http_request_matching_referer() {
        let req = "GET /api/session/9222/tabs HTTP/1.1\r\nHost: dashboard.agent-browser.localhost:443\r\nReferer: https://dashboard.agent-browser.localhost/sessions\r\n\r\n";
        assert!(is_same_origin_http_request(req));
    }

    #[test]
    fn test_same_origin_http_request_rejects_missing_origin_and_referer() {
        let req = "GET /api/session/9222/tabs HTTP/1.1\r\nHost: localhost:4848\r\n\r\n";
        assert!(!is_same_origin_http_request(req));
    }

    #[test]
    fn test_same_origin_http_request_rejects_cross_origin_referer() {
        let req = "GET /api/session/9222/tabs HTTP/1.1\r\nHost: localhost:4848\r\nReferer: https://evil.com/path\r\n\r\n";
        assert!(!is_same_origin_http_request(req));
    }

    #[test]
    fn test_same_origin_ws_request_coder() {
        let req = "GET /api/session/9222/stream HTTP/1.1\r\nHost: workspace.coder.com\r\nOrigin: https://workspace.coder.com\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_same_origin_ws_request(req));
    }

    #[test]
    fn test_cross_origin_ws_request_rejected() {
        let req = "GET /api/session/9222/stream HTTP/1.1\r\nHost: localhost:4848\r\nOrigin: https://evil.com\r\nUpgrade: websocket\r\n\r\n";
        assert!(!is_same_origin_ws_request(req));
    }

    #[test]
    fn test_no_origin_header_allowed() {
        let req = "GET /api/session/9222/stream HTTP/1.1\r\nHost: localhost:4848\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_same_origin_ws_request(req));
    }

    #[test]
    fn test_parse_session_proxy_route_valid() {
        assert_eq!(
            parse_session_proxy_route("/api/session/9222/tabs"),
            Ok((9222, SessionProxyEndpoint::Tabs))
        );
        assert_eq!(
            parse_session_proxy_route("/api/session/1337/status"),
            Ok((1337, SessionProxyEndpoint::Status))
        );
        assert_eq!(
            parse_session_proxy_route("/api/session/65535/stream"),
            Ok((65535, SessionProxyEndpoint::Stream))
        );
    }

    #[test]
    fn test_parse_session_proxy_route_invalid() {
        assert!(parse_session_proxy_route("/api/session/0/tabs").is_err());
        assert!(parse_session_proxy_route("/api/session/not-a-port/tabs").is_err());
        assert!(parse_session_proxy_route("/api/session/70000/tabs").is_err());
        assert!(parse_session_proxy_route("/api/session/9222").is_err());
        assert!(parse_session_proxy_route("/api/session/9222/unknown").is_err());
        assert!(parse_session_proxy_route("/api/session/9222/tabs/extra").is_err());
    }

    #[test]
    fn test_parse_session_proxy_route_path_traversal() {
        assert!(parse_session_proxy_route("/api/session/9222/tabs/..").is_err());
        assert!(parse_session_proxy_route("/api/session/9222/tabs/../status").is_err());
        assert!(parse_session_proxy_route("/api/session/9222/../../etc/passwd").is_err());
        assert!(parse_session_proxy_route("/api/session/../session/9222/tabs").is_err());
    }

    #[test]
    fn test_parse_session_proxy_route_double_slashes() {
        assert!(parse_session_proxy_route("/api/session//9222/tabs").is_err());
        assert!(parse_session_proxy_route("/api//session/9222/tabs").is_err());
        assert!(parse_session_proxy_route("//api/session/9222/tabs").is_err());
    }

    #[test]
    fn test_parse_session_proxy_route_trailing_slash() {
        assert!(parse_session_proxy_route("/api/session/9222/tabs/").is_err());
        assert!(parse_session_proxy_route("/api/session/9222/status/").is_err());
        assert!(parse_session_proxy_route("/api/session/9222/stream/").is_err());
    }

    #[test]
    fn test_parse_session_proxy_route_encoded_paths() {
        assert!(parse_session_proxy_route("/api/session/9222/tabs%20extra").is_err());
        assert!(parse_session_proxy_route("/api/session/%39%32%32%32/tabs").is_err());
    }

    #[test]
    fn test_sessions_json_has_active_port() {
        let sessions_json = r#"[
            {"session":"alpha","port":9222,"engine":"chrome"},
            {"session":"beta","port":9333,"engine":"chrome"}
        ]"#;

        assert_eq!(sessions_json_has_active_port(sessions_json, 9222), Ok(true));
        assert_eq!(
            sessions_json_has_active_port(sessions_json, 9444),
            Ok(false)
        );
    }

    #[test]
    fn test_sessions_json_has_active_port_invalid_json() {
        assert!(sessions_json_has_active_port("{", 9222).is_err());
    }

    #[test]
    fn test_parse_upstream_http_response() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nConnection: close\r\n\r\n{\"ok\":true}";
        let parsed = parse_upstream_http_response(response).expect("response should parse");

        assert_eq!(parsed.0, "200 OK");
        assert_eq!(parsed.1, "application/json; charset=utf-8");
        assert_eq!(parsed.2, b"{\"ok\":true}".to_vec());
    }
}
