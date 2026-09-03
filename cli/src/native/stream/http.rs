use rust_embed::Embed;
use serde_json::{json, Value};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

use crate::connection::get_socket_dir;
#[cfg(windows)]
use crate::connection::resolve_port;

use super::chat::{chat_status_json, handle_chat_request, handle_models_request};
use super::dashboard::spawn_session;
use super::discovery::discover_sessions;

#[derive(Embed)]
#[folder = "../packages/dashboard/out/"]
struct DashboardAssets;

pub(super) const CORS_HEADERS: &str = "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n";

/// Build CORS headers that reflect the request origin only when it passes
/// `is_allowed_origin`. Used for sensitive endpoints (chat, models) so the
/// API key is not accessible from arbitrary web pages.
pub(super) fn cors_headers_for_origin(origin: Option<&str>) -> String {
    let allowed_origin = match origin {
        Some(o) if super::is_allowed_origin(Some(o)) => o,
        _ => "http://localhost",
    };
    format!(
        "Access-Control-Allow-Origin: {}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\n",
        allowed_origin
    )
}

fn request_headers(request: &str) -> &str {
    request
        .find("\r\n\r\n")
        .or_else(|| request.find("\n\n"))
        .map(|header_end| &request[..header_end])
        .unwrap_or(request)
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

fn parse_origin(peeked: &[u8]) -> Option<String> {
    let header_str = std::str::from_utf8(peeked).ok()?;
    request_header_value(header_str, "origin").map(ToString::to_string)
}

fn normalize_origin_authority(origin: &str) -> Option<String> {
    let url = url::Url::parse(origin).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let default_port = (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443));
    Some(match url.port() {
        Some(port) if !default_port => format!("{host}:{port}"),
        _ => host,
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

fn header_authority_matches_host(request: &str, header_name: &str) -> bool {
    let Some(authority) =
        request_header_value(request, header_name).and_then(normalize_origin_authority)
    else {
        return false;
    };
    let Some(host) = request_header_value(request, "host").map(normalize_host_authority) else {
        return false;
    };
    authority == host && is_loopback_authority(&authority) && is_loopback_authority(&host)
}

/// Protects the command relay by requiring same-origin browser metadata.
fn is_same_origin_command_request(request: &str) -> bool {
    if request_header_value(request, "origin").is_some() {
        header_authority_matches_host(request, "origin")
    } else {
        header_authority_matches_host(request, "referer")
    }
}

fn command_cors_headers(request: &str) -> String {
    match request_header_value(request, "origin") {
        Some(origin) if is_same_origin_command_request(request) => format!(
            "Access-Control-Allow-Origin: {origin}\r\nAccess-Control-Allow-Methods: POST, OPTIONS\r\nAccess-Control-Allow-Headers: Content-Type\r\nVary: Origin\r\n"
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
    let origin = parse_origin(peeked);

    if method == "OPTIONS" {
        if path == "/api/command" {
            if !is_same_origin_command_request(&request) {
                write_json_error_response_no_cors(
                    &mut stream,
                    "403 Forbidden",
                    "Origin or Referer does not match Host header.",
                )
                .await;
                return;
            }

            let cors_headers = command_cors_headers(&request);
            let response = format!(
                "HTTP/1.1 204 No Content\r\n{cors_headers}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }

        let response = format!(
            "HTTP/1.1 204 No Content\r\n{CORS_HEADERS}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = stream.write_all(response.as_bytes()).await;
        return;
    }

    if method == "POST" {
        if path == "/api/command" && !is_same_origin_command_request(&request) {
            write_json_error_response_no_cors(
                &mut stream,
                "403 Forbidden",
                "Origin or Referer does not match Host header.",
            )
            .await;
            return;
        }

        let full_body = read_full_body(&mut stream, peeked).await;
        if full_body.is_none()
            && (path == "/api/chat" || path == "/api/sessions" || path == "/api/command")
        {
            let body = r#"{"error":"Request body too large"}"#;
            let cors_headers = if path == "/api/command" {
                command_cors_headers(&request)
            } else {
                CORS_HEADERS.to_string()
            };
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
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
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
            let cors_headers = command_cors_headers(&request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{cors_headers}\r\n",
                resp_body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(resp_body.as_bytes()).await;
            return;
        }

        if path == "/api/chat" {
            handle_chat_request(&mut stream, body_str, origin.as_deref()).await;
            return;
        }
    }

    if method == "GET" && path == "/api/models" {
        handle_models_request(&mut stream, origin.as_deref()).await;
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

    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
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

fn parse_content_length_bytes(headers: &[u8]) -> Option<usize> {
    let header_str = std::str::from_utf8(headers).ok()?;
    for line in header_str.lines() {
        if line.len() > 16 && line[..16].eq_ignore_ascii_case("content-length: ") {
            return line[16..].trim().parse().ok();
        }
    }
    None
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
        let socket_path = get_socket_dir().join(format!("{}.sock", session_name));
        tokio::net::UnixStream::connect(&socket_path)
            .await
            .map_err(|e| format!("Failed to connect to daemon: {}", e))?
    };

    #[cfg(windows)]
    let stream = {
        let port = resolve_port(session_name);
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
    let clean = url_path.trim_start_matches('/');
    let key = if clean.is_empty() {
        "index.html"
    } else {
        clean
    };

    let file = DashboardAssets::get(key).or_else(|| DashboardAssets::get("index.html"));

    match file {
        Some(content) => {
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
    use crate::test_utils::EnvGuard;
    use std::sync::Arc;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

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
}
