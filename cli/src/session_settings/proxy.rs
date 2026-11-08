//! Proxy URL normalization: the server without its credentials, plus the credentials.

pub struct ParsedProxy {
    pub server: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Decodes proxy credential escapes, preserving the raw value if decoding fails.
fn decode_proxy_credential(credential: &str) -> String {
    urlencoding::decode(credential)
        .map(|decoded| decoded.into_owned())
        .unwrap_or_else(|_| credential.to_string())
}

pub fn parse_proxy(proxy_str: &str) -> ParsedProxy {
    let (protocol, rest) = match proxy_str.find("://") {
        Some(protocol_end) => (
            Some(&proxy_str[..protocol_end + 3]),
            &proxy_str[protocol_end + 3..],
        ),
        None => (None, proxy_str),
    };

    let Some(at_pos) = rest.rfind('@') else {
        return ParsedProxy {
            server: proxy_str.to_string(),
            username: None,
            password: None,
        };
    };

    let creds = &rest[..at_pos];
    let server_part = &rest[at_pos + 1..];
    let server = match protocol {
        Some(protocol) => format!("{protocol}{server_part}"),
        None => server_part.to_string(),
    };

    let (username, password) = match creds.find(':') {
        Some(colon_pos) => {
            let u = &creds[..colon_pos];
            let p = &creds[colon_pos + 1..];
            (
                if u.is_empty() {
                    None
                } else {
                    Some(decode_proxy_credential(u))
                },
                if p.is_empty() {
                    None
                } else {
                    Some(decode_proxy_credential(p))
                },
            )
        }
        None => (
            if creds.is_empty() {
                None
            } else {
                Some(decode_proxy_credential(creds))
            },
            None,
        ),
    };

    ParsedProxy {
        server,
        username,
        password,
    }
}
