use std::{net::IpAddr, time::Duration};

use tokio_tungstenite::connect_async;
use url::Url;

use super::firefox::DevtoolsEndpoint;

/// A loopback Firefox remote agent: the BiDi WebSocket and the unencrypted DevTools (RDP) endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachTarget {
    pub bidi_url: String,
    pub devtools: DevtoolsEndpoint,
    /// Whether the attach URL supplied a `devtools` query parameter; ignored for owned launches.
    pub devtools_explicit: bool,
}

impl AttachTarget {
    /// Explains an attach failure without exposing BiDi credentials or query parameters.
    pub fn devtools_unreachable_error(&self) -> String {
        let (host, port) = match &self.devtools {
            DevtoolsEndpoint::Tcp { host, port } => (host, port),
            DevtoolsEndpoint::Unix(path) => {
                return format!(
                    "Firefox DevTools server is not reachable at Unix socket {}",
                    path.display()
                );
            }
        };
        let address = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let prefs = "devtools.debugger.remote-enabled=true, devtools.chrome.enabled=true and devtools.debugger.prompt-connection=false";
        if self.devtools_explicit {
            format!(
                "Firefox DevTools server is not reachable at {address}. Start Firefox with --start-debugger-server {port} and the prefs {prefs}."
            )
        } else {
            format!(
                "Firefox DevTools server is not reachable at {address} (the default). If Firefox was started with --start-debugger-server <rdp-port>, connect with: {}/session?devtools=<rdp-port>. Required prefs: {prefs}.",
                target_label(&self.bidi_url)
            )
        }
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

fn endpoint_url(target: &str) -> Result<Url, url::ParseError> {
    match target.parse::<u16>() {
        Ok(port) => Url::parse(&format!("http://127.0.0.1:{port}")),
        Err(_) => Url::parse(target),
    }
}

fn target_from_url(mut url: Url) -> Result<AttachTarget, String> {
    let bidi_scheme = if matches!(url.scheme(), "https" | "wss") {
        "wss"
    } else {
        "ws"
    };
    let host = url
        .host_str()
        .ok_or("Firefox endpoint must include a host")?
        .trim_matches(['[', ']'])
        .to_string();
    if host.trim_end_matches('.') != "localhost"
        && !host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    {
        return Err("Firefox DevTools uses an unencrypted RDP connection. Forward the BiDi and DevTools ports over an encrypted tunnel such as ssh -L, then attach to 127.0.0.1.".into());
    }
    let devtools_port = url
        .query_pairs()
        .find(|(key, _)| key == "devtools")
        .map(|(_, value)| {
            value
                .parse::<u16>()
                .map_err(|_| "Firefox endpoint has an invalid devtools port".to_string())
        })
        .transpose()?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(url.query_pairs().filter(|(key, _)| key != "devtools"))
        .finish();
    url.set_scheme(bidi_scheme)
        .map_err(|_| "Firefox endpoint has an invalid WebSocket scheme".to_string())?;
    url.set_path("/session");
    url.set_query((!query.is_empty()).then_some(query.as_str()));
    Ok(AttachTarget {
        bidi_url: url.to_string(),
        devtools: DevtoolsEndpoint::Tcp {
            host,
            port: devtools_port.unwrap_or(6000),
        },
        devtools_explicit: devtools_port.is_some(),
    })
}

pub fn normalize_attach_target(target: &str) -> String {
    endpoint_url(target)
        .map(|url| url.to_string())
        .unwrap_or_else(|_| target.into())
}

pub fn target_label(target: &str) -> String {
    match endpoint_url(target).map(|url| url.origin()) {
        Ok(url::Origin::Tuple(scheme, host, port)) => format!("{scheme}://{host}:{port}"),
        _ => "Firefox endpoint".into(),
    }
}

/// Parses a `ws://` or `wss://` URL whose path is `/session` into a target without network access.
pub fn parse_attach_url(target: &str) -> Result<Option<AttachTarget>, String> {
    let Ok(url) = Url::parse(target) else {
        return Ok(None);
    };
    if !matches!(url.scheme(), "ws" | "wss") || url.path() != "/session" {
        return Ok(None);
    }
    target_from_url(url).map(Some)
}

/// Probes a port or HTTP URL for Firefox's WebDriver BiDi WebSocket.
pub async fn probe_attach_target(target: &str) -> Result<AttachTarget, String> {
    let url = endpoint_url(target).map_err(|e| format!("invalid Firefox endpoint: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("expected an http(s)://host:port URL or a port".into());
    }
    let bidi = target_from_url(url)?;
    let connect = tokio::time::timeout(PROBE_TIMEOUT, connect_async(&bidi.bidi_url)).await;
    match connect {
        Ok(Ok((mut socket, _))) => {
            let _ = socket.close(None).await;
            Ok(bidi)
        }
        Ok(Err(error)) => Err(format!("WebSocket handshake failed: {error}")),
        Err(_) => Err("WebSocket handshake timed out after 2 seconds".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_attach_url, target_from_url, AttachTarget};
    use crate::native::bidi::firefox::DevtoolsEndpoint;
    use url::Url;

    fn target(url: &str, expected_url: &str, expected_port: u16, devtools_explicit: bool) {
        assert_eq!(
            parse_attach_url(url).unwrap(),
            Some(AttachTarget {
                bidi_url: expected_url.into(),
                devtools: DevtoolsEndpoint::Tcp {
                    host: "127.0.0.1".into(),
                    port: expected_port,
                },
                devtools_explicit,
            })
        );
    }

    #[test]
    fn parses_webdriver_bidi_attach_urls() {
        target(
            "ws://127.0.0.1:9333/session",
            "ws://127.0.0.1:9333/session",
            6000,
            false,
        );
        target(
            "ws://127.0.0.1:9333/session?devtools=6080",
            "ws://127.0.0.1:9333/session",
            6080,
            true,
        );
        target(
            "wss://127.0.0.1:9333/session?token=a&devtools=6080",
            "wss://127.0.0.1:9333/session?token=a",
            6080,
            true,
        );
        assert_eq!(parse_attach_url("ws://127.0.0.1:9333/other"), Ok(None));
        assert!(
            parse_attach_url("ws://127.0.0.1:9333/session?devtools=nope")
                .unwrap_err()
                .contains("invalid devtools port")
        );
        assert_eq!(
            parse_attach_url("ws://[::1]:9333/session").unwrap(),
            Some(AttachTarget {
                bidi_url: "ws://[::1]:9333/session".into(),
                devtools: DevtoolsEndpoint::Tcp {
                    host: "::1".into(),
                    port: 6000,
                },
                devtools_explicit: false,
            })
        );
    }

    #[test]
    fn bare_port_devtools_error_shows_the_bidi_port_and_rdp_override() {
        let target = target_from_url(super::endpoint_url("9444").unwrap()).unwrap();
        assert!(!target.devtools_explicit);
        assert_eq!(
            target.devtools_unreachable_error(),
            "Firefox DevTools server is not reachable at 127.0.0.1:6000 (the default). If Firefox was started with --start-debugger-server <rdp-port>, connect with: ws://127.0.0.1:9444/session?devtools=<rdp-port>. Required prefs: devtools.debugger.remote-enabled=true, devtools.chrome.enabled=true and devtools.debugger.prompt-connection=false."
        );
    }

    #[test]
    fn explicit_devtools_error_names_the_requested_port_without_a_default() {
        for port in [6080, 6000] {
            let target = parse_attach_url(&format!("ws://127.0.0.1:9333/session?devtools={port}"))
                .unwrap()
                .unwrap();
            assert!(target.devtools_explicit);
            let error = target.devtools_unreachable_error();
            assert_eq!(
                error,
                format!("Firefox DevTools server is not reachable at 127.0.0.1:{port}. Start Firefox with --start-debugger-server {port} and the prefs devtools.debugger.remote-enabled=true, devtools.chrome.enabled=true and devtools.debugger.prompt-connection=false.")
            );
            assert!(!error.contains("default"), "{error}");
            assert!(!error.contains("?devtools="), "{error}");
        }
    }

    #[test]
    fn default_devtools_hint_preserves_transport_and_redacts_secrets() {
        for (url, address, bidi_origin) in [
            (
                "https://user:password@localhost:9444?token=secret",
                "localhost:6000",
                "wss://localhost:9444",
            ),
            ("http://localhost", "localhost:6000", "ws://localhost:80"),
            ("https://localhost", "localhost:6000", "wss://localhost:443"),
            (
                "ws://[::1]:9555/session?token=secret",
                "[::1]:6000",
                "ws://[::1]:9555",
            ),
        ] {
            let target = target_from_url(Url::parse(url).unwrap()).unwrap();
            let error = target.devtools_unreachable_error();
            assert!(
                error.contains(&format!("at {address} (the default)")),
                "{error}"
            );
            assert!(
                error.contains(&format!("{bidi_origin}/session?devtools=<rdp-port>")),
                "{error}"
            );
            for secret in ["user", "password", "token", "secret"] {
                assert!(!error.contains(secret), "{error}");
            }
        }
    }

    #[test]
    fn unix_devtools_error_names_the_socket_without_tcp_guidance() {
        let target = AttachTarget {
            bidi_url: "ws://127.0.0.1:9333/session".into(),
            devtools: DevtoolsEndpoint::Unix("/tmp/firefox-rdp.sock".into()),
            devtools_explicit: false,
        };
        assert_eq!(
            target.devtools_unreachable_error(),
            "Firefox DevTools server is not reachable at Unix socket /tmp/firefox-rdp.sock"
        );
    }

    #[test]
    fn http_targets_keep_their_transport_security_when_probed() {
        let secure = target_from_url(Url::parse("https://127.0.0.1?token=a").unwrap()).unwrap();
        assert_eq!(secure.bidi_url, "wss://127.0.0.1/session?token=a");
        let plain = target_from_url(Url::parse("http://127.0.0.1").unwrap()).unwrap();
        assert_eq!(plain.bidi_url, "ws://127.0.0.1/session");
        for host in [
            "127.0.0.1",
            "[::1]",
            "localhost",
            "192.0.2.1",
            "example.com",
        ] {
            let result = target_from_url(Url::parse(&format!("wss://{host}/session")).unwrap());
            if matches!(host, "192.0.2.1" | "example.com") {
                assert!(result.unwrap_err().contains("encrypted tunnel"));
            } else {
                assert!(result.is_ok());
            }
        }
    }
}
