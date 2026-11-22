use rust_embed::Embed;
use serde_json::{json, Value};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

#[cfg(windows)]
use crate::connection::resolve_port;
#[cfg(unix)]
use crate::session_files::SessionFile;

use super::chat::{chat_status_json, handle_chat_request, handle_models_request};
use super::dashboard::spawn_session;
use super::discovery::discover_sessions;
use super::origin::{
    is_loopback_authority, normalize_host_authority, origin_matches_host, request_header_value,
};

#[derive(Embed)]
#[folder = "../packages/dashboard/out/"]
struct DashboardAssets;

pub(super) const CORS_HEADERS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n";

fn header_authority_matches_host(request: &str, header_name: &str) -> bool {
    let (Some(value), Some(host)) = (
        request_header_value(request, header_name),
        request_header_value(request, "host"),
    ) else {
        return false;
    };
    origin_matches_host(value, host) && is_loopback_authority(&normalize_host_authority(host, None))
}

/// Protects the command relay by requiring same-origin browser metadata.
fn is_same_origin_command_request(request: &str) -> bool {
    if request_header_value(request, "origin").is_some() {
        header_authority_matches_host(request, "origin")
    } else {
        header_authority_matches_host(request, "referer")
    }
}

fn has_loopback_host(request: &str) -> bool {
    request_header_value(request, "host")
        .map(|host| normalize_host_authority(host, None))
        .is_some_and(|host| is_loopback_authority(&host))
}

/// Browsers label every request with `Sec-Fetch-Site`; only requests from the
/// page's own origin, or user-initiated ones, may pass.
fn fetch_site_allows(request: &str) -> bool {
    match request_header_value(request, "sec-fetch-site") {
        None => true,
        Some(site) => site.eq_ignore_ascii_case("same-origin") || site.eq_ignore_ascii_case("none"),
    }
}

/// Reads accept same-origin browser metadata, or no `Origin`/`Referer` at all
/// from a loopback `Host` (curl and the dashboard session proxy).
fn is_same_origin_read_request(request: &str) -> bool {
    if request_header_value(request, "origin").is_some()
        || request_header_value(request, "referer").is_some()
    {
        return is_same_origin_command_request(request);
    }
    has_loopback_host(request)
}

/// Single gate for the whole `/api/` surface. Writes always require a
/// same-origin `Origin` or `Referer`; reads also accept header-less loopback
/// clients.
fn is_authorized_api_request(method: &str, request: &str) -> bool {
    if !fetch_site_allows(request) {
        return false;
    }
    if method == "GET" || method == "HEAD" {
        is_same_origin_read_request(request)
    } else {
        is_same_origin_command_request(request)
    }
}

/// CORS headers for `/api/` responses. The origin is reflected only for
/// same-origin requests; there is no wildcard.
fn api_cors_headers(request: &str) -> String {
    match request_header_value(request, "origin") {
        Some(origin) if is_same_origin_command_request(request) => format!(
            "Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nVary: Origin\r\n"
        ),
        _ => String::new(),
    }
}

async fn write_json_error_response_no_cors(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    error: &str,
) {
    let body = format!(
        r#"{{"success":false,"error":{}}}"#,
        serde_json::to_string(error).unwrap_or_else(|_| format!("\"{}\"", error))
    );
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
}

pub(super) async fn handle_http_request(
    mut stream: tokio::net::TcpStream,
    peeked: &[u8],
    last_tabs: &Arc<RwLock<Vec<Value>>>,
    last_engine: &Arc<RwLock<String>>,
    session_name: &str,
) {
    let peeked_len = peeked.len();
    let mut discard = vec![0u8; peeked_len];
    let _ = stream.read_exact(&mut discard).await;

    let request = String::from_utf8_lossy(peeked);
    let first_line = request.lines().next().unwrap_or("");
    let method = first_line.split_whitespace().next().unwrap_or("GET");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    if path.starts_with("/api/") && !is_authorized_api_request(method, &request) {
        write_json_error_response_no_cors(
            &mut stream,
            "403 Forbidden",
            "Origin or Referer does not match Host header.",
        )
        .await;
        return;
    }

    if method == "OPTIONS" {
        let cors_headers = if path.starts_with("/api/") {
            api_cors_headers(&request)
        } else {
            CORS_HEADERS.to_string()
        };
        let response = format!(
            "HTTP/1.1 204 No Content\r\n{cors_headers}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    if method == "POST" {
        let full_body = read_full_body(&mut stream, peeked).await;
        if full_body.is_none()
            && (path == "/api/chat" || path == "/api/sessions" || path == "/api/command")
        {
            let body = r#"{"error":"Request body too large"}"#;
            let cors_headers = api_cors_headers(&request);
            let response = format!(
                "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{cors_headers}\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
            return;
        }
        let body_str = full_body.as_deref().unwrap_or("");

        if path == "/api/sessions" {
            let result = spawn_session(body_str).await;
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
            let cors_headers = api_cors_headers(&request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{cors_headers}\r\n",
                resp_body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(resp_body.as_bytes()).await;
            return;
        }

        if path == "/api/command" {
            let result = relay_command_to_daemon(session_name, body_str).await;
            let (status, resp_body) = match result {
                Ok(resp) => ("200 OK", resp),
                Err(e) => (
                    "502 Bad Gateway",
                    format!(
                        r#"{{"success":false,"error":{}}}"#,
                        serde_json::to_string(&e).unwrap_or_else(|_| format!("\"{}\"", e))
                    ),
                ),
            };
            let cors_headers = api_cors_headers(&request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{cors_headers}\r\n",
                resp_body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(resp_body.as_bytes()).await;
            return;
        }

        if path == "/api/chat" {
            handle_chat_request(&mut stream, body_str).await;
            return;
        }
    }

    if method == "GET" && path == "/api/models" {
        handle_models_request(&mut stream).await;
        return;
    }

    let (status, content_type, body): (&str, &str, Vec<u8>) = if path == "/api/sessions" {
        (
            "200 OK",
            "application/json; charset=utf-8",
            discover_sessions().into_bytes(),
        )
    } else if path == "/api/tabs" {
        let tabs = last_tabs.read().await;
        (
            "200 OK",
            "application/json; charset=utf-8",
            serde_json::to_string(&*tabs)
                .unwrap_or_else(|_| "[]".to_string())
                .into_bytes(),
        )
    } else if path == "/api/status" {
        let engine = last_engine.read().await;
        (
            "200 OK",
            "application/json; charset=utf-8",
            format!(r#"{{"engine":"{}"}}"#, *engine).into_bytes(),
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

    let cors_headers = if path.starts_with("/api/") {
        api_cors_headers(&request)
    } else {
        CORS_HEADERS.to_string()
    };
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{cors_headers}\r\n",
        status,
        content_type,
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.write_all(&body).await;
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|p| p + 2))
}

pub(super) fn parse_content_length(headers: &str) -> Option<usize> {
    request_header_value(headers, "content-length")?
        .parse()
        .ok()
}

fn parse_content_length_bytes(headers: &[u8]) -> Option<usize> {
    parse_content_length(std::str::from_utf8(headers).ok()?)
}

const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

async fn read_full_body(stream: &mut tokio::net::TcpStream, peeked: &[u8]) -> Option<String> {
    let body_offset = find_header_end(peeked)?;
    let content_length = parse_content_length_bytes(&peeked[..body_offset])?;
    if content_length == 0 {
        return Some(String::new());
    }
    if content_length > MAX_BODY_SIZE {
        return None;
    }

    let peeked_body = &peeked[body_offset..];
    let peeked_body_len = peeked_body.len().min(content_length);

    let mut body = Vec::with_capacity(content_length);
    body.extend_from_slice(&peeked_body[..peeked_body_len]);

    let remaining = content_length - peeked_body_len;
    if remaining > 0 {
        let mut rest = vec![0u8; remaining];
        if stream.read_exact(&mut rest).await.is_err() {
            return String::from_utf8(body).ok();
        }
        body.extend_from_slice(&rest);
    }

    String::from_utf8(body).ok()
}

pub(super) async fn relay_command_to_daemon(
    session_name: &str,
    body: &str,
) -> Result<String, String> {
    let mut cmd: Value = serde_json::from_str(body).map_err(|e| format!("Invalid JSON: {}", e))?;

    if cmd.get("id").is_none() {
        let id = format!(
            "dash-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        cmd["id"] = json!(id);
    }

    let mut json_str = serde_json::to_string(&cmd).map_err(|e| e.to_string())?;
    json_str.push('\n');

    #[cfg(unix)]
    let stream = {
        let socket_path = SessionFile::Socket.path(session_name);
        tokio::net::UnixStream::connect(&socket_path)
            .await
            .map_err(|e| format!("Failed to connect to daemon: {}", e))?
    };

    #[cfg(windows)]
    let stream = {
        let port = resolve_port(session_name)
            .ok_or_else(|| "Failed to connect to daemon: no port file".to_string())?;
        tokio::net::TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .map_err(|e| format!("Failed to connect to daemon: {}", e))?
    };

    let (reader, mut writer) = tokio::io::split(stream);

    writer
        .write_all(json_str.as_bytes())
        .await
        .map_err(|e| format!("Failed to send command: {}", e))?;

    let mut buf_reader = tokio::io::BufReader::new(reader);
    let mut response_line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut buf_reader, &mut response_line)
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    Ok(response_line.trim().to_string())
}

pub(super) fn serve_embedded_file(url_path: &str) -> (&'static str, &'static str, Vec<u8>) {
    let clean = url_path
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_start_matches('/');
    let key = if clean.is_empty() {
        "index.html"
    } else {
        clean
    };

    let file = DashboardAssets::get(key)
        .map(|file| (key, file))
        .or_else(|| DashboardAssets::get("index.html").map(|file| ("index.html", file)));

    match file {
        Some((key, content)) => {
            let ext = key.rsplit('.').next().unwrap_or("");
            let ct = match ext {
                "html" => "text/html; charset=utf-8",
                "js" => "application/javascript; charset=utf-8",
                "css" => "text/css; charset=utf-8",
                "json" => "application/json; charset=utf-8",
                "svg" => "image/svg+xml",
                "png" => "image/png",
                "ico" => "image/x-icon",
                "woff2" => "font/woff2",
                "woff" => "font/woff",
                "txt" => "text/plain; charset=utf-8",
                _ => "application/octet-stream",
            };
            ("200 OK", ct, content.data.to_vec())
        }
        None => (
            "404 Not Found",
            "text/html; charset=utf-8",
            b"<html><body><p>404 Not Found</p></body></html>".to_vec(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::stream::origin::normalize_origin_authority;
    use crate::test_utils::EnvGuard;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    #[test]
    fn parse_content_length_handles_spacing_case_and_unicode_header_values() {
        let headers = "POST / HTTP/1.1\r\nX-Label: 123456é\r\ncOnTeNt-LeNgTh:\t5 \r\n\r\n";

        assert_eq!(parse_content_length(headers), Some(5));
    }

    #[test]
    fn test_serve_embedded_file_query_string() {
        let (_, expected_content_type, expected_body) = serve_embedded_file("/");
        let (status, content_type, body) = serve_embedded_file("/?port=35511");

        assert_eq!(status, "200 OK");
        assert_eq!(content_type, "text/html; charset=utf-8");
        assert_eq!(content_type, expected_content_type);
        assert_eq!(body, expected_body);
    }

    #[test]
    fn test_serve_embedded_file_missing_route_falls_back_to_index() {
        let (_, expected_content_type, expected_body) = serve_embedded_file("/");
        let (status, content_type, body) = serve_embedded_file("/missing-route.js");

        assert_eq!(status, "200 OK");
        assert_eq!(content_type, "text/html; charset=utf-8");
        assert_eq!(content_type, expected_content_type);
        assert_eq!(body, expected_body);
    }

    async fn send_request_to_handler(request: &str, session_name: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let peeked = request.as_bytes().to_vec();
        let last_tabs = Arc::new(RwLock::new(Vec::new()));
        let last_engine = Arc::new(RwLock::new("chrome".to_string()));
        let session_name = session_name.to_string();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_http_request(stream, &peeked, &last_tabs, &last_engine, &session_name).await;
        });

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        server.await.unwrap();

        String::from_utf8(response).unwrap()
    }

    #[cfg(unix)]
    async fn spawn_fake_daemon(
        socket_dir: &std::path::Path,
        session_name: &str,
    ) -> oneshot::Receiver<String> {
        let socket_path = socket_dir.join(format!("{session_name}.sock"));
        let _ = std::fs::remove_file(&socket_path);
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let (tx, rx) = oneshot::channel();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();

            let mut stream = reader.into_inner();
            stream
                .write_all(br#"{"success":true,"data":{"ok":true}}"#)
                .await
                .unwrap();
            stream.write_all(b"\n").await.unwrap();
            let _ = tx.send(line);
        });

        rx
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cross_origin_command_post_is_rejected_without_relaying_to_daemon() {
        let temp_parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("t");
        std::fs::create_dir_all(&temp_parent).unwrap();
        let socket_dir = tempfile::Builder::new()
            .prefix("ab-")
            .tempdir_in(temp_parent)
            .unwrap();
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            socket_dir.path().to_str().unwrap(),
        );
        guard.remove("XDG_RUNTIME_DIR");

        let session_name = "x";
        let daemon_command = spawn_fake_daemon(socket_dir.path(), session_name).await;
        let body = r#"{"action":"tabs"}"#;
        let request = format!(
            "POST /api/command HTTP/1.1\r\nHost: localhost:7777\r\nOrigin: https://evil.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = send_request_to_handler(&request, session_name).await;

        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden"),
            "unexpected response: {response}"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), daemon_command)
                .await
                .is_err(),
            "cross-origin request reached daemon command relay"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cross_origin_command_preflight_is_rejected_without_wildcard_cors() {
        let request = concat!(
            "OPTIONS /api/command HTTP/1.1\r\n",
            "Host: localhost:7777\r\n",
            "Origin: https://evil.example\r\n",
            "Access-Control-Request-Method: POST\r\n",
            "Access-Control-Request-Headers: content-type\r\n",
            "\r\n"
        );

        let response = send_request_to_handler(request, "x").await;

        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden"),
            "unexpected response: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin: *"),
            "forbidden command preflight exposed wildcard CORS: {response}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_post_without_origin_or_referer_is_rejected() {
        let body = r#"{"action":"tabs"}"#;
        let request = format!(
            "POST /api/command HTTP/1.1\r\nHost: localhost:7777\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = send_request_to_handler(&request, "x").await;

        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden"),
            "unexpected response: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin: *"),
            "forbidden command response exposed wildcard CORS: {response}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_post_with_dns_rebinding_host_is_rejected() {
        let body = r#"{"action":"tabs"}"#;
        let request = format!(
            "POST /api/command HTTP/1.1\r\nHost: attacker.example:7777\r\nOrigin: http://attacker.example:7777\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = send_request_to_handler(&request, "x").await;

        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden"),
            "unexpected response: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin: *"),
            "forbidden command response exposed wildcard CORS: {response}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn command_post_ignores_header_like_body_lines() {
        let body = "Referer: http://localhost:7777\r\n{\"action\":\"tabs\"}";
        let request = format!(
            "POST /api/command HTTP/1.1\r\nHost: localhost:7777\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = send_request_to_handler(&request, "x").await;

        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden"),
            "unexpected response: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin: *"),
            "forbidden command response exposed wildcard CORS: {response}"
        );
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn same_origin_command_post_relays_without_wildcard_cors() {
        let temp_parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("t");
        std::fs::create_dir_all(&temp_parent).unwrap();
        let socket_dir = tempfile::Builder::new()
            .prefix("ab-")
            .tempdir_in(temp_parent)
            .unwrap();
        let guard = EnvGuard::new(&["AGENT_BROWSER_SOCKET_DIR", "XDG_RUNTIME_DIR"]);
        guard.set(
            "AGENT_BROWSER_SOCKET_DIR",
            socket_dir.path().to_str().unwrap(),
        );
        guard.remove("XDG_RUNTIME_DIR");

        let session_name = "x";
        let daemon_command = spawn_fake_daemon(socket_dir.path(), session_name).await;
        let body = r#"{"action":"tabs"}"#;
        let request = format!(
            "POST /api/command HTTP/1.1\r\nHost: localhost:7777\r\nOrigin: http://localhost:7777\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let response = send_request_to_handler(&request, session_name).await;

        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "unexpected response: {response}"
        );
        assert!(
            response.contains("Access-Control-Allow-Origin: http://localhost:7777"),
            "same-origin command response did not reflect origin: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin: *"),
            "same-origin command response exposed wildcard CORS: {response}"
        );

        let relayed = tokio::time::timeout(std::time::Duration::from_secs(1), daemon_command)
            .await
            .unwrap()
            .unwrap();
        assert!(relayed.contains(r#""action":"tabs""#), "{relayed}");
    }

    fn assert_forbidden_without_cors(response: &str) {
        assert!(
            response.starts_with("HTTP/1.1 403 Forbidden"),
            "unexpected response: {response}"
        );
        assert!(
            !response.contains("Access-Control-Allow-Origin"),
            "forbidden response carried CORS headers: {response}"
        );
    }

    fn api_request(method: &str, path: &str, headers: &str) -> String {
        let body = if method == "POST" { "{}" } else { "" };
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost:7777\r\n{headers}Content-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    const API_ROUTES: [(&str, &str); 8] = [
        ("GET", "/api/sessions"),
        ("GET", "/api/tabs"),
        ("GET", "/api/status"),
        ("GET", "/api/models"),
        ("GET", "/api/chat/status"),
        ("POST", "/api/sessions"),
        ("POST", "/api/chat"),
        ("POST", "/api/command"),
    ];

    #[tokio::test(flavor = "current_thread")]
    async fn cross_origin_requests_are_rejected_on_every_api_route() {
        for (method, path) in API_ROUTES {
            let request = api_request(method, path, "Origin: https://evil.example\r\n");
            assert_forbidden_without_cors(&send_request_to_handler(&request, "x").await);

            let request = api_request(method, path, "Referer: https://evil.example/page\r\n");
            assert_forbidden_without_cors(&send_request_to_handler(&request, "x").await);

            let preflight = api_request("OPTIONS", path, "Origin: https://evil.example\r\n");
            assert_forbidden_without_cors(&send_request_to_handler(&preflight, "x").await);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn other_loopback_port_is_cross_origin_on_every_api_route() {
        for (method, path) in API_ROUTES {
            let request = api_request(method, path, "Origin: http://localhost:3000\r\n");
            assert_forbidden_without_cors(&send_request_to_handler(&request, "x").await);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_without_origin_or_referer_are_rejected() {
        for (method, path) in API_ROUTES.iter().filter(|(m, _)| *m == "POST") {
            let request = api_request(method, path, "");
            assert_forbidden_without_cors(&send_request_to_handler(&request, "x").await);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fetch_metadata_cross_site_is_rejected_even_without_origin() {
        for site in ["cross-site", "same-site"] {
            let request = api_request("GET", "/api/tabs", &format!("Sec-Fetch-Site: {site}\r\n"));
            assert_forbidden_without_cors(&send_request_to_handler(&request, "x").await);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rebinding_host_is_rejected_without_origin() {
        let request = "GET /api/tabs HTTP/1.1\r\nHost: attacker.example:7777\r\n\r\n";
        assert_forbidden_without_cors(&send_request_to_handler(request, "x").await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn same_origin_and_dashboard_proxy_reads_succeed_without_wildcard_cors() {
        let same_origin = api_request(
            "GET",
            "/api/tabs",
            "Origin: http://localhost:7777\r\nSec-Fetch-Site: same-origin\r\n",
        );
        let response = send_request_to_handler(&same_origin, "x").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(
            response.contains("Access-Control-Allow-Origin: http://localhost:7777"),
            "{response}"
        );

        // The dashboard session proxy sends no Origin and no Referer.
        let proxy = "GET /api/status HTTP/1.1\r\nHost: 127.0.0.1:7777\r\nConnection: close\r\n\r\n";
        let response = send_request_to_handler(proxy, "x").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(
            !response.contains("Access-Control-Allow-Origin: *"),
            "{response}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn same_origin_session_post_reaches_handler() {
        // `{}` passes the gate and fails inside `spawn_session`, so no browser starts.
        let request = api_request("POST", "/api/sessions", "Origin: http://localhost:7777\r\n");
        let response = send_request_to_handler(&request, "x").await;
        assert!(
            response.starts_with("HTTP/1.1 400 Bad Request"),
            "{response}"
        );
        assert!(response.contains("Missing"), "{response}");
        assert!(
            !response.contains("Access-Control-Allow-Origin: *"),
            "{response}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dashboard_assets_are_served_cross_origin() {
        let request = "GET /index.html HTTP/1.1\r\nHost: localhost:7777\r\nOrigin: https://evil.example\r\n\r\n";
        let response = send_request_to_handler(request, "x").await;
        assert!(!response.starts_with("HTTP/1.1 403"), "{response}");
    }

    #[test]
    fn ipv6_loopback_origin_matches_host_authority() {
        assert_eq!(
            normalize_origin_authority("http://[::1]:7777").as_deref(),
            Some("[::1]:7777")
        );
        let request =
            "GET /api/tabs HTTP/1.1\r\nHost: [::1]:7777\r\nOrigin: http://[::1]:7777\r\n\r\n";
        assert!(is_authorized_api_request("GET", request));
        assert!(is_authorized_api_request("POST", request));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unlisted_api_paths_are_gated_by_prefix() {
        for method in ["GET", "POST", "OPTIONS"] {
            let request = api_request(method, "/api/anything", "Origin: https://evil.example\r\n");
            assert_forbidden_without_cors(&send_request_to_handler(&request, "x").await);
        }
    }
}
