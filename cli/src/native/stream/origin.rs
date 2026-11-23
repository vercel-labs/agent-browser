//! Origin trust rules shared by the stream server, its WebSocket upgrade and
//! the dashboard. Every comparison of an `Origin` against a `Host` goes
//! through this module so the surfaces cannot drift apart.

/// Value of the first header named `name` in the request head. Header-like
/// lines in the body are ignored.
pub(super) fn request_header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
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

/// `host[:port]` of an origin, lowercased, with scheme-default ports dropped.
/// Origins carrying credentials do not normalize.
pub(super) fn normalize_origin_authority(origin: &str) -> Option<String> {
    let url = url::Url::parse(origin).ok()?;
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    // `host_str` already brackets IPv6 literals.
    let host = url.host_str()?.to_ascii_lowercase();
    let default_port = (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443));
    Some(match url.port() {
        Some(port) if !default_port => format!("{host}:{port}"),
        _ => host,
    })
}

/// `Host` header value, lowercased, with the default port of `scheme` dropped.
pub(super) fn normalize_host_authority(host: &str, scheme: Option<&str>) -> String {
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

pub(super) fn is_loopback_authority(authority: &str) -> bool {
    matches!(
        authority_host(authority),
        "localhost" | "127.0.0.1" | "::1" | "[::1]"
    )
}

/// `scheme://authority` for an http(s) origin, `None` for anything else
/// (`null`, `file://`, credentials, unparsable values).
pub(super) fn normalized_http_origin(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let authority = normalize_origin_authority(value)?;
    Some(format!("{}://{authority}", url.scheme()))
}

/// Whether an http(s) `origin` has the same authority as the `Host` header
/// value. A different port is a different origin.
pub(super) fn origin_matches_host(origin: &str, host: &str) -> bool {
    let Some(origin) = normalized_http_origin(origin) else {
        return false;
    };
    let Some((scheme, _)) = origin.split_once("://") else {
        return false;
    };
    normalize_origin_authority(&origin)
        .is_some_and(|authority| authority == normalize_host_authority(host, Some(scheme)))
}

/// WebSocket upgrade policy for the stream server. A browser upgrade is
/// allowed only from the stream's own origin on a loopback `Host`, which also
/// rules out DNS rebinding.
///
/// A missing `Origin` is allowed: browsers always send one on a WebSocket
/// upgrade, so its absence means a non-browser client (a library, the
/// dashboard proxy, a test) that is not subject to cross-origin attacks.
pub(crate) fn is_allowed_ws_upgrade(origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(host) = host else {
        return false;
    };
    origin_matches_host(origin, host)
        && is_loopback_authority(&normalize_host_authority(host, None))
}

/// Shared fixtures so every surface is held to the same invariant: a page
/// from another port, a `file://` page, `null` and credentialed origins are
/// refused; the stream's own origin is allowed.
#[cfg(test)]
pub(super) mod invariant {
    pub(in crate::native::stream) fn host(port: u16) -> String {
        format!("127.0.0.1:{port}")
    }

    pub(in crate::native::stream) fn same_origin(port: u16) -> String {
        format!("http://127.0.0.1:{port}")
    }

    pub(in crate::native::stream) fn refused_origins(port: u16) -> Vec<String> {
        let other = port.wrapping_add(1);
        vec![
            format!("http://localhost:{other}"),
            format!("http://127.0.0.1:{other}"),
            format!("http://[::1]:{other}"),
            "file://".to_string(),
            "file:///tmp/viewer.html".to_string(),
            "null".to_string(),
            format!("http://user@127.0.0.1:{port}"),
            "https://evil.example".to_string(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::invariant::{host, refused_origins, same_origin};
    use super::*;
    use crate::native::stream::http::handle_http_request;
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::RwLock;

    const PORT: u16 = 38000;

    #[test]
    fn upgrade_policy_refuses_every_foreign_origin() {
        let host = host(PORT);
        for origin in refused_origins(PORT) {
            assert!(
                !is_allowed_ws_upgrade(Some(&origin), Some(&host)),
                "{origin} must be refused"
            );
        }
    }

    #[test]
    fn upgrade_policy_allows_same_origin_and_non_browser_clients() {
        let host = host(PORT);
        assert!(is_allowed_ws_upgrade(Some(&same_origin(PORT)), Some(&host)));
        assert!(is_allowed_ws_upgrade(None, Some(&host)));
        assert!(is_allowed_ws_upgrade(None, None));
        assert!(is_allowed_ws_upgrade(
            Some("http://[::1]:38000"),
            Some("[::1]:38000")
        ));
        assert!(is_allowed_ws_upgrade(
            Some("http://localhost:38000"),
            Some("LOCALHOST:38000")
        ));
    }

    #[test]
    fn upgrade_policy_refuses_a_rebinding_host_with_a_matching_origin() {
        assert!(!is_allowed_ws_upgrade(
            Some("http://evil.example:38000"),
            Some("evil.example:38000")
        ));
        assert!(!is_allowed_ws_upgrade(Some("http://127.0.0.1:38000"), None));
    }

    #[test]
    fn ipv6_loopback_authority_is_not_double_bracketed() {
        assert_eq!(
            normalize_origin_authority("http://[::1]:7777").as_deref(),
            Some("[::1]:7777")
        );
    }

    #[test]
    fn credentials_do_not_normalize() {
        assert_eq!(normalize_origin_authority("http://u:p@127.0.0.1:1"), None);
        assert_eq!(normalize_origin_authority("http://u@127.0.0.1:1"), None);
    }

    #[test]
    fn default_ports_compare_equal_to_a_bare_host() {
        assert!(origin_matches_host(
            "https://example.com",
            "example.com:443"
        ));
        assert!(origin_matches_host("http://example.com:80", "example.com"));
        assert!(!origin_matches_host(
            "https://example.com",
            "example.com:80"
        ));
    }

    async fn http_status(request: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let peeked = request.as_bytes().to_vec();
        let last_tabs: Arc<RwLock<Vec<Value>>> = Arc::new(RwLock::new(Vec::new()));
        let last_engine = Arc::new(RwLock::new("chrome".to_string()));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_http_request(stream, &peeked, &last_tabs, &last_engine, "origin-test").await;
        });
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client.write_all(request.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        server.await.unwrap();
        String::from_utf8(response)
            .unwrap()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn command_post(origin: &str) -> String {
        let body = r#"{"action":"tabs"}"#;
        format!(
            "POST /api/command HTTP/1.1\r\nHost: {}\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            host(PORT),
            body.len()
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn http_gate_refuses_every_foreign_origin_and_allows_same_origin() {
        for origin in refused_origins(PORT) {
            let status = http_status(&command_post(&origin)).await;
            assert!(
                status.starts_with("HTTP/1.1 403"),
                "{origin} must be refused: {status}"
            );
        }
        let status = http_status(&command_post(&same_origin(PORT))).await;
        assert!(
            !status.starts_with("HTTP/1.1 403"),
            "same origin must pass the gate: {status}"
        );
    }
}
