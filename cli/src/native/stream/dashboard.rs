use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::net::{Ipv4Addr, Ipv6Addr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use crate::connection::get_socket_dir;

use super::chat::{chat_status_json, handle_chat_request, handle_models_request};
use super::discovery::discover_sessions;
use super::http::serve_embedded_file;

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
const DASHBOARD_ACCESS_TOKEN_COOKIE: &str = "__Host-agent-browser-dashboard-token";

#[cfg(test)]
static EXEC_CLI_INVOCATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn build_json_error_body(error: &str) -> String {
    let escaped = serde_json::to_string(error).unwrap_or_else(|_| format!("\"{}\"", error));
    format!(r#"{{"success":false,"error":{escaped}}}"#)
}

async fn write_http_response_inner(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(body).await;
}

async fn write_http_response_no_cors(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) {
    write_http_response_inner(stream, status, content_type, body).await;
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
    request_headers(request).lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        if header_name.trim().eq_ignore_ascii_case(name) {
            Some(value.trim())
        } else {
            None
        }
    })
}

fn request_headers(request: &str) -> &str {
    request
        .find("\r\n\r\n")
        .or_else(|| request.find("\n\n"))
        .map(|header_end| &request[..header_end])
        .unwrap_or(request)
}

fn normalize_origin_authority(origin: &str) -> Option<String> {
    let url = url::Url::parse(origin).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    // `url::Url::host_str` already includes brackets around IPv6 literals.
    // Adding another pair would turn `[::1]` into `[[::1]]` and prevent the
    // documented IPv6 loopback origin from matching the Host header.
    let host = url.host_str()?.to_ascii_lowercase();
    let default_port = (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443));
    Some(match url.port() {
        Some(port) if !default_port => format!("{host}:{port}"),
        _ => host,
    })
}

fn normalize_host_authority(host: &str, scheme: Option<&str>) -> String {
    let host = host.trim().to_ascii_lowercase();
    let default_port = match scheme {
        Some("http") => Some("80"),
        Some("https") => Some("443"),
        _ => None,
    };

    if let Some(bracket_end) = host.rfind(']') {
        if bracket_end == host.len() - 1 {
            return host;
        }

        if host.as_bytes().get(bracket_end + 1) == Some(&b':') {
            let port = &host[bracket_end + 2..];
            if Some(port) == default_port {
                return host[..=bracket_end].to_string();
            }
        }

        return host;
    }

    if let Some((name, port)) = host.rsplit_once(':') {
        if !name.contains(':') && Some(port) == default_port {
            return name.to_string();
        }
    }

    host
}

fn authority_host(authority: &str) -> &str {
    if let Some(stripped) = authority.strip_prefix('[') {
        if let Some(bracket_end) = stripped.find(']') {
            return &authority[..=bracket_end + 1];
        }
    }

    if let Some((host, _port)) = authority.rsplit_once(':') {
        if !host.contains(':') {
            return host;
        }
    }

    authority
}

fn is_loopback_authority(authority: &str) -> bool {
    matches!(
        authority_host(authority),
        "localhost" | "127.0.0.1" | "::1" | "[::1]"
    )
}

fn is_loopback_dashboard_origin(origin: &str) -> bool {
    normalize_origin_authority(origin).is_some_and(|authority| is_loopback_authority(&authority))
}

fn normalized_dashboard_origin(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }

    let authority = normalize_origin_authority(value)?;
    Some(format!("{}://{authority}", url.scheme()))
}

fn normalized_dashboard_allowed_origin(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let origin = normalized_dashboard_origin(value)?;
    let authority = normalize_origin_authority(&origin)?;
    if is_loopback_authority(&authority) {
        return None;
    }
    if url.scheme() != "https" {
        return None;
    }
    Some(origin)
}

/// Validate and normalize a comma-separated dashboard origin allowlist into
/// the exact set enforced by the server. Invalid entries reject the entire
/// configuration so a typo cannot silently weaken reverse-proxy protection.
/// Sorting makes equivalent configurations stable for lifecycle comparisons
/// in the parent CLI process.
pub fn normalize_dashboard_allowed_origins(value: Option<&str>) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };

    if value.trim().is_empty() {
        return Err("Dashboard allowed origins cannot be empty.".to_string());
    }

    let mut origins = Vec::new();
    for value in value.split(',') {
        let value = value.trim();
        let origin = normalized_dashboard_allowed_origin(value).ok_or_else(|| {
            format!(
                "Invalid dashboard allowed origin '{value}'. Expected an exact non-loopback HTTPS origin without a path, query, fragment, or credentials."
            )
        })?;
        origins.push(origin);
    }
    origins.sort();
    origins.dedup();
    Ok(origins)
}

fn allowed_dashboard_origins() -> Result<Vec<String>, String> {
    let configured = std::env::var("AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS").ok();
    normalize_dashboard_allowed_origins(configured.as_deref())
}

pub fn is_valid_dashboard_access_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn dashboard_access_token() -> Option<String> {
    std::env::var("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN")
        .ok()
        .filter(|token| is_valid_dashboard_access_token(token))
}

fn access_token_matches(request: &str) -> bool {
    let Ok(allowed_origins) = allowed_dashboard_origins() else {
        return false;
    };

    // Same-origin validation runs before this check. Once it has established a
    // loopback Host and loopback browser origin, no token is needed. In
    // particular, never place the external dashboard credential in a cookie on
    // plain-http localhost, where cookies are shared across ports.
    if request_header_value(request, "host")
        .map(|host| normalize_host_authority(host, None))
        .is_some_and(|host| is_loopback_authority(&host))
    {
        return true;
    }

    if allowed_origins.is_empty() {
        return true;
    }
    dashboard_access_token().as_deref().is_some_and(|expected| {
        request_access_token(request).is_some_and(|provided| constant_time_eq(expected, provided))
    })
}

fn constant_time_eq(expected: &str, provided: &str) -> bool {
    if expected.len() != provided.len() {
        return false;
    }
    expected
        .bytes()
        .zip(provided.bytes())
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn request_access_token(request: &str) -> Option<&str> {
    request_header_value(request, "cookie")?
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == DASHBOARD_ACCESS_TOKEN_COOKIE).then_some(value)
        })
}

fn header_is_trusted_dashboard_origin(request: &str, header_name: &str) -> bool {
    let Some(origin) =
        request_header_value(request, header_name).and_then(normalized_dashboard_origin)
    else {
        return false;
    };
    let Some((scheme, _)) = origin.split_once("://") else {
        return false;
    };
    let Some(host) = request_header_value(request, "host")
        .map(|host| normalize_host_authority(host, Some(scheme)))
    else {
        return false;
    };

    if normalize_origin_authority(&origin).as_deref() != Some(host.as_str()) {
        return false;
    }

    (is_loopback_authority(&host) && is_loopback_dashboard_origin(&origin))
        || allowed_dashboard_origins()
            .is_ok_and(|allowed| allowed.iter().any(|allowed| allowed == &origin))
}

/// Validates that a proxied WebSocket request came from a trusted dashboard
/// origin. Non-browser clients may omit Origin only when connecting through a
/// loopback Host, which prevents DNS rebinding from trusting an attacker host.
fn is_same_origin_ws_request(request: &str) -> bool {
    if request_header_value(request, "origin").is_some() {
        header_is_trusted_dashboard_origin(request, "origin")
    } else {
        request_header_value(request, "host")
            .map(|host| normalize_host_authority(host, None))
            .is_some_and(|host| is_loopback_authority(&host))
    }
}

/// Validates that an HTTP session-proxy request came from a same-origin page.
///
/// For GET requests we require either a same-origin `Origin` or a same-origin
/// `Referer` so browsers cannot hit the proxy routes via side-channel tags or
/// arbitrary cross-origin fetches.
fn is_same_origin_http_request(request: &str) -> bool {
    if request_header_value(request, "origin").is_some() {
        header_is_trusted_dashboard_origin(request, "origin")
    } else {
        header_is_trusted_dashboard_origin(request, "referer")
    }
}

/// Protect dashboard API requests from cross-origin forms and DNS rebinding.
///
/// Local dashboard origins are trusted by default. A dashboard exposed through
/// a reverse proxy must explicitly opt in its browser origin with
/// `AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS` and present the generated access
/// token, since direct clients can forge Origin and Host headers.
fn is_same_origin_dashboard_request(request: &str) -> bool {
    is_same_origin_http_request(request) && access_token_matches(request)
}

fn is_authorized_dashboard_websocket_request(request: &str) -> bool {
    is_same_origin_ws_request(request) && access_token_matches(request)
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
    let ipv4_addr = (Ipv4Addr::LOCALHOST, port);
    let ipv4_listener = match TcpListener::bind(ipv4_addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind dashboard server on 127.0.0.1:{port}: {e}");
            return;
        }
    };

    // Port zero is useful for internal tests. Bind IPv6 to the same selected
    // port so both loopback families reach one dashboard instance.
    let bound_port = ipv4_listener
        .local_addr()
        .map(|address| address.port())
        .unwrap_or(port);
    let ipv6_listener = match TcpListener::bind((Ipv6Addr::LOCALHOST, bound_port)).await {
        Ok(listener) => Some(listener),
        Err(error) => {
            eprintln!("Failed to bind dashboard server on [::1]:{bound_port}: {error}");
            None
        }
    };

    if let Some(ipv6_listener) = ipv6_listener {
        tokio::join!(
            run_dashboard_listener(ipv4_listener),
            run_dashboard_listener(ipv6_listener)
        );
    } else {
        run_dashboard_listener(ipv4_listener).await;
    }
}

async fn run_dashboard_listener(listener: TcpListener) {
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
                if !is_authorized_dashboard_websocket_request(&peeked_request) {
                    write_json_error_response_no_cors(
                        &mut stream,
                        "403 Forbidden",
                        "Origin or dashboard access token is invalid.",
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

    if path.starts_with("/api/") && !is_same_origin_dashboard_request(&request) {
        write_json_error_response_no_cors(
            &mut stream,
            "403 Forbidden",
            "Origin, Referer, or dashboard access token is invalid.",
        )
        .await;
        return;
    }

    if method == "OPTIONS" {
        let response = "HTTP/1.1 204 No Content\r\nAccess-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
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
        write_http_response_no_cors(
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

    write_http_response_no_cors(&mut stream, status, content_type, &body).await;
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

    #[cfg(test)]
    EXEC_CLI_INVOCATIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

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
    use crate::test_utils::EnvGuard;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn send_request_to_dashboard_handler(request: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_dashboard_connection(stream).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        server.await.unwrap();

        String::from_utf8(response).unwrap()
    }

    fn dashboard_request(method: &str, path: &str, headers: &str, body: &str) -> String {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:4848\r\n{headers}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cross_origin_text_plain_exec_is_rejected_without_execution() {
        let _guard = EnvGuard::new(&[]);
        EXEC_CLI_INVOCATIONS.store(0, std::sync::atomic::Ordering::SeqCst);
        let body = r#"{"args":["--version"],"z":"="}"#;
        let request = dashboard_request(
            "POST",
            "/api/exec",
            "Origin: https://evil.example\r\nContent-Type: text/plain\r\n",
            body,
        );

        let response = send_request_to_dashboard_handler(&request).await;

        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden"),
            "unexpected response: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin"),
            "forbidden exec response exposed CORS: {response}"
        );
        assert_eq!(
            EXEC_CLI_INVOCATIONS.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "cross-origin request reached exec_cli"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_mutations_reject_cross_origin_requests_without_wildcard_cors() {
        for (method, path) in [
            ("POST", "/api/exec"),
            ("POST", "/api/kill"),
            ("POST", "/api/sessions"),
            ("POST", "/api/chat"),
            ("GET", "/api/sessions"),
        ] {
            let request = dashboard_request(
                method,
                path,
                "Origin: https://evil.example\r\nContent-Type: application/json\r\n",
                "{}",
            );
            let response = send_request_to_dashboard_handler(&request).await;

            assert!(
                response.starts_with("HTTP/1.1 403 Forbidden"),
                "unexpected response for {path}: {response}"
            );
            assert!(
                !response.contains("Access-Control-Allow-Origin"),
                "forbidden {path} response exposed CORS: {response}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_sensitive_routes_reject_cross_origin_preflight_without_cors() {
        for path in [
            "/api/exec",
            "/api/kill",
            "/api/sessions",
            "/api/chat",
            "/api/models",
        ] {
            let request = dashboard_request(
                "OPTIONS",
                path,
                "Origin: https://evil.example\r\nAccess-Control-Request-Method: POST\r\n",
                "",
            );
            let response = send_request_to_dashboard_handler(&request).await;

            assert!(
                response.starts_with("HTTP/1.1 403 Forbidden"),
                "unexpected response for {path}: {response}"
            );
            assert!(
                !response.contains("Access-Control-Allow-Origin"),
                "forbidden {path} preflight exposed CORS: {response}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_exec_rejects_missing_origin_or_cross_origin_referer_without_execution() {
        let _guard = EnvGuard::new(&[]);
        EXEC_CLI_INVOCATIONS.store(0, std::sync::atomic::Ordering::SeqCst);
        let body = r#"{"args":["--version"]}"#;

        for headers in [
            "Content-Type: application/json\r\n",
            "Referer: https://evil.example/\r\nContent-Type: application/json\r\n",
        ] {
            let request = dashboard_request("POST", "/api/exec", headers, body);
            let response = send_request_to_dashboard_handler(&request).await;

            assert!(
                response.starts_with("HTTP/1.1 403 Forbidden"),
                "unexpected response: {response}"
            );
            assert!(
                !response.contains("Access-Control-Allow-Origin"),
                "forbidden exec response exposed CORS: {response}"
            );
        }

        assert_eq!(
            EXEC_CLI_INVOCATIONS.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "untrusted requests reached exec_cli"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn same_origin_dashboard_exec_runs_without_cors() {
        let _guard = EnvGuard::new(&[]);
        EXEC_CLI_INVOCATIONS.store(0, std::sync::atomic::Ordering::SeqCst);
        let body = r#"{"args":["--version"]}"#;
        let request = dashboard_request(
            "POST",
            "/api/exec",
            "Origin: http://localhost:4848\r\nContent-Type: application/json\r\n",
            body,
        );

        let response = send_request_to_dashboard_handler(&request).await;

        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "unexpected response: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin"),
            "dashboard exec response exposed CORS: {response}"
        );
        assert_eq!(
            EXEC_CLI_INVOCATIONS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "same-origin request did not reach exec_cli"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reverse_proxy_dashboard_requires_an_access_token() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "a".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.example.com",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);
        EXEC_CLI_INVOCATIONS.store(0, std::sync::atomic::Ordering::SeqCst);
        let body = r#"{"args":["--version"]}"#;

        let without_token = format!(
            "POST /api/exec HTTP/1.1\r\nHost: dashboard.example.com\r\nOrigin: https://dashboard.example.com\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len(),
        );
        let response = send_request_to_dashboard_handler(&without_token).await;
        assert!(response.starts_with("HTTP/1.1 403 Forbidden"));
        assert_eq!(
            EXEC_CLI_INVOCATIONS.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "tokenless reverse-proxy request reached exec_cli"
        );

        let with_token = format!(
            "POST /api/exec HTTP/1.1\r\nHost: dashboard.example.com\r\nOrigin: https://dashboard.example.com\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len(),
        );
        let response = send_request_to_dashboard_handler(&with_token).await;
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert_eq!(
            EXEC_CLI_INVOCATIONS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "authenticated reverse-proxy request did not reach exec_cli"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn loopback_dashboard_does_not_require_external_access_token() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.example.com",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &"a".repeat(64));
        EXEC_CLI_INVOCATIONS.store(0, std::sync::atomic::Ordering::SeqCst);
        let body = r#"{"args":["--version"]}"#;
        let request = format!(
            "POST /api/exec HTTP/1.1\r\nHost: localhost:4848\r\nOrigin: http://localhost:4848\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len(),
        );

        let response = send_request_to_dashboard_handler(&request).await;
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "unexpected response: {response}"
        );
        assert_eq!(
            EXEC_CLI_INVOCATIONS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "tokenless loopback request did not reach exec_cli"
        );

        let websocket = "GET /api/session/9222/stream HTTP/1.1\r\nHost: localhost:4848\r\nOrigin: http://localhost:4848\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_authorized_dashboard_websocket_request(websocket));
    }

    #[test]
    fn reverse_proxy_with_a_missing_access_token_fails_closed() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.example.com",
        );
        guard.remove("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN");
        let request = "POST /api/exec HTTP/1.1\r\nHost: dashboard.example.com\r\nOrigin: https://dashboard.example.com\r\n\r\n";

        assert!(!is_same_origin_dashboard_request(request));
    }

    #[test]
    fn reverse_proxy_websocket_requires_an_access_token() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "b".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.example.com",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);

        let without_token = "GET /api/session/9222/stream HTTP/1.1\r\nHost: dashboard.example.com\r\nOrigin: https://dashboard.example.com\r\nUpgrade: websocket\r\n\r\n";
        assert!(!is_authorized_dashboard_websocket_request(without_token));

        let with_token = format!(
            "GET /api/session/9222/stream HTTP/1.1\r\nHost: dashboard.example.com\r\nOrigin: https://dashboard.example.com\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\nUpgrade: websocket\r\n\r\n"
        );
        assert!(is_authorized_dashboard_websocket_request(&with_token));
    }

    #[test]
    fn dashboard_allowed_origin_enables_a_matching_proxy_origin() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "a".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.agent-browser.localhost",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);
        let request = format!(
            "POST /api/exec HTTP/1.1\r\nHost: dashboard.agent-browser.localhost\r\nOrigin: https://dashboard.agent-browser.localhost\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\n\r\n"
        );

        assert!(is_same_origin_dashboard_request(&request));
    }

    #[test]
    fn dashboard_rejects_dns_rebinding_host_even_when_origin_matches() {
        let request = "POST /api/exec HTTP/1.1\r\nHost: attacker.example:4848\r\nOrigin: http://attacker.example:4848\r\n\r\n";

        assert!(!is_same_origin_dashboard_request(request));
    }

    #[test]
    fn dashboard_rejects_header_like_body_lines() {
        let request = "POST /api/exec HTTP/1.1\r\nHost: localhost:4848\r\nContent-Type: text/plain\r\n\r\nOrigin: http://localhost:4848\r\n{\"args\":[\"--version\"]}";

        assert!(!is_same_origin_dashboard_request(request));
    }

    #[test]
    fn test_same_origin_ws_request_matching() {
        let req = "GET /api/session/9222/stream HTTP/1.1\r\nHost: localhost:4848\r\nOrigin: http://localhost:4848\r\nUpgrade: websocket\r\n\r\n";
        assert!(is_same_origin_ws_request(req));
    }

    #[test]
    fn test_same_origin_ws_request_proxied() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "a".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.agent-browser.localhost",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);
        let req = format!(
            "GET /api/session/9222/stream HTTP/1.1\r\nHost: dashboard.agent-browser.localhost\r\nOrigin: https://dashboard.agent-browser.localhost\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\nUpgrade: websocket\r\n\r\n"
        );
        assert!(is_authorized_dashboard_websocket_request(&req));
    }

    #[test]
    fn test_normalize_origin_authority_https_without_port() {
        assert_eq!(
            normalize_origin_authority("https://dashboard.agent-browser.localhost"),
            Some("dashboard.agent-browser.localhost".to_string())
        );
    }

    #[test]
    fn test_normalize_origin_authority_ignores_default_ports_and_rejects_credentials() {
        assert_eq!(
            normalize_origin_authority("https://dashboard.agent-browser.localhost:443"),
            Some("dashboard.agent-browser.localhost".to_string())
        );
        assert_eq!(
            normalize_origin_authority("http://localhost:80"),
            Some("localhost".to_string())
        );
        assert_eq!(
            normalize_origin_authority("http://attacker@localhost:4848"),
            None
        );
        assert_eq!(
            normalize_origin_authority("http://[::1]:4848"),
            Some("[::1]:4848".to_string())
        );
    }

    #[test]
    fn test_normalize_dashboard_allowed_origins_canonicalizes_a_set() {
        assert_eq!(
            normalize_dashboard_allowed_origins(Some(
                "https://second.example.com, https://dashboard.example.com:443,https://second.example.com"
            )).unwrap(),
            vec![
                "https://dashboard.example.com".to_string(),
                "https://second.example.com".to_string()
            ]
        );
    }

    #[test]
    fn test_normalize_dashboard_allowed_origins_rejects_every_invalid_entry() {
        for value in [
            "",
            "invalid",
            "http://dashboard.example.com",
            "https://dashboard.example.com/path",
            "https://dashboard.example.com,invalid",
            "http://localhost:4848",
        ] {
            assert!(
                normalize_dashboard_allowed_origins(Some(value)).is_err(),
                "unexpectedly accepted {value}"
            );
        }
    }

    #[test]
    fn ipv6_loopback_dashboard_request_is_authorized() {
        let _guard = EnvGuard::new(&[]);
        let request =
            "GET /api/sessions HTTP/1.1\r\nHost: [::1]:4848\r\nOrigin: http://[::1]:4848\r\n\r\n";

        assert!(is_same_origin_dashboard_request(request));
    }

    #[test]
    fn dashboard_access_token_comparison_rejects_prefixes_and_suffixes() {
        let token = "a".repeat(64);
        assert!(constant_time_eq(&token, &token));
        assert!(!constant_time_eq(&token, &format!("{token}a")));
        assert!(!constant_time_eq(&token, &token[1..]));
    }

    #[test]
    fn test_same_origin_ws_request_default_https_port() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "a".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.agent-browser.localhost",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);
        let req = format!(
            "GET /api/session/9222/stream HTTP/1.1\r\nHost: dashboard.agent-browser.localhost:443\r\nOrigin: https://dashboard.agent-browser.localhost\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\nUpgrade: websocket\r\n\r\n"
        );
        assert!(is_authorized_dashboard_websocket_request(&req));
    }

    #[test]
    fn test_same_origin_requests_preserve_non_default_https_port() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "a".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.agent-browser.localhost:80",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);

        let http = format!(
            "GET /api/sessions HTTP/1.1\r\nHost: dashboard.agent-browser.localhost:80\r\nOrigin: https://dashboard.agent-browser.localhost:80\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\n\r\n"
        );
        assert!(is_same_origin_dashboard_request(&http));

        let websocket = format!(
            "GET /api/session/9222/stream HTTP/1.1\r\nHost: dashboard.agent-browser.localhost:80\r\nOrigin: https://dashboard.agent-browser.localhost:80\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\nUpgrade: websocket\r\n\r\n"
        );
        assert!(is_authorized_dashboard_websocket_request(&websocket));
    }

    #[test]
    fn test_same_origin_http_request_matching_origin() {
        let req = "GET /api/session/9222/tabs HTTP/1.1\r\nHost: localhost:4848\r\nOrigin: http://localhost:4848\r\n\r\n";
        assert!(is_same_origin_http_request(req));
    }

    #[test]
    fn test_same_origin_http_request_matching_referer() {
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "a".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://dashboard.agent-browser.localhost",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);
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
        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN",
        ]);
        let token = "a".repeat(64);
        guard.set(
            "AGENT_BROWSER_DASHBOARD_ALLOWED_ORIGINS",
            "https://workspace.coder.com",
        );
        guard.set("AGENT_BROWSER_DASHBOARD_ACCESS_TOKEN", &token);
        let req = format!(
            "GET /api/session/9222/stream HTTP/1.1\r\nHost: workspace.coder.com\r\nOrigin: https://workspace.coder.com\r\nCookie: {DASHBOARD_ACCESS_TOKEN_COOKIE}={token}\r\nUpgrade: websocket\r\n\r\n"
        );
        assert!(is_authorized_dashboard_websocket_request(&req));
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
