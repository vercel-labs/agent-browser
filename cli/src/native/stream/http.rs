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

fn parse_origin(peeked: &[u8]) -> Option<String> {
    let header_str = std::str::from_utf8(peeked).ok()?;
    for line in header_str.lines() {
        if line.len() > 8 && line[..8].eq_ignore_ascii_case("origin: ") {
            return Some(line[8..].trim().to_string());
        }
    }
    None
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
        let response = format!(
            "HTTP/1.1 204 No Content\r\n{CORS_HEADERS}Access-Control-Max-Age: 86400\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
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
            let response = format!(
                "HTTP/1.1 413 Payload Too Large\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
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
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{CORS_HEADERS}\r\n",
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
