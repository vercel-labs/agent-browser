//! `poppy join` (experimental): reading a Company's `poppy.json`.
//!
//! The daemon (`native::poppy`) runs discovery after policy and domain checks,
//! then the browser steps and the authorizer, so the one-time code never
//! reaches the CLI process.

use std::net::{IpAddr, SocketAddr};

use reqwest::{redirect::Policy, Client};
use serde_json::Value;
use url::Url;

use crate::native::flow::net::{is_link_local_or_metadata, is_non_public};
use crate::tls::TrustOptions;

const WELL_KNOWN: &str = "/.well-known/poppy.json";

/// A failed join with a machine-readable code.
#[derive(Debug, PartialEq, Eq)]
pub struct JoinError {
    pub code: &'static str,
    pub message: String,
}

fn discovery_failed(message: impl Into<String>) -> JoinError {
    JoinError {
        code: "discovery_failed",
        message: message.into(),
    }
}

/// Where a Company publishes `poppy.json`, before any request is made.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub origin: String,
    pub host: String,
    /// The one URL discovery requests.
    pub well_known: String,
}

/// What discovery learned about a Company.
#[derive(Debug, PartialEq, Eq)]
pub struct Discovery {
    pub origin: String,
    pub endpoint: String,
}

/// `host` plus `:port` when the port is explicit and not the scheme default.
fn host_with_port(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    Some(match url.port() {
        Some(port) => format!("{}:{}", host, port),
        None => host,
    })
}

/// HTTPS, or plain HTTP to a loopback host for local testing.
fn check_scheme(url: &Url) -> Result<(), JoinError> {
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    match url.scheme() {
        "https" => Ok(()),
        "http" if local => Ok(()),
        _ => Err(discovery_failed(format!("{} is not an HTTPS URL", url))),
    }
}

/// Whether discovery began at a target on this machine, whose hops are not
/// third-party requests.
fn started_locally(start: &Url) -> bool {
    match start.host() {
        Some(url::Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            name == "localhost" || name.ends_with(".localhost")
        }
        Some(url::Host::Ipv4(ip)) => is_non_public(IpAddr::V4(ip)),
        Some(url::Host::Ipv6(ip)) => is_non_public(IpAddr::V6(ip)),
        None => false,
    }
}

/// A hop reaches public addresses only, unless discovery started locally, and
/// never a link-local or cloud metadata address.
fn admitted(ip: IpAddr, local: bool) -> bool {
    !is_link_local_or_metadata(ip) && (local || !is_non_public(ip))
}

fn check_address(host: &str, ip: IpAddr, local: bool) -> Result<(), JoinError> {
    if admitted(ip, local) {
        return Ok(());
    }
    let ip = ip.to_canonical();
    Err(discovery_failed(if is_link_local_or_metadata(ip) {
        format!(
            "{} resolves to the link-local or cloud metadata address {}",
            host, ip
        )
    } else {
        format!("{} resolves to the non-public address {}", host, ip)
    }))
}

/// A client for one hop, pinned to the addresses that were checked, so the
/// address that passed `check_address` is the address that is used.
async fn client_for(
    url: &Url,
    local: bool,
    trust: &TrustOptions,
    deadline: tokio::time::Instant,
) -> Result<Client, JoinError> {
    let host = url
        .host_str()
        .ok_or_else(|| discovery_failed(format!("{} has no host", url)))?;
    let mut builder = crate::tls::apply_to_reqwest(Client::builder(), trust)
        .map_err(discovery_failed)?
        .redirect(Policy::none());
    match url.host() {
        Some(url::Host::Ipv4(ip)) => check_address(host, IpAddr::V4(ip), local)?,
        Some(url::Host::Ipv6(ip)) => check_address(host, IpAddr::V6(ip), local)?,
        _ => {
            let port = url.port_or_known_default().unwrap_or(443);
            let lookup = tokio::time::timeout_at(deadline, tokio::net::lookup_host((host, port)));
            let addrs: Vec<SocketAddr> = lookup
                .await
                .map_err(|_| discovery_failed(format!("Resolving {} timed out", host)))?
                .map_err(|e| discovery_failed(format!("Could not resolve {}: {}", host, e)))?
                .collect();
            if addrs.is_empty() {
                return Err(discovery_failed(format!(
                    "{} did not resolve to any address",
                    host
                )));
            }
            for addr in &addrs {
                check_address(host, addr.ip(), local)?;
            }
            builder = builder.resolve_to_addrs(host, &addrs);
        }
    }
    builder
        .build()
        .map_err(|e| discovery_failed(format!("Failed to create HTTP client: {}", e)))
}

/// Reads a `/.well-known/poppy.json` response for the Company at `host`.
fn interpret(host: &str, status: u16, body: &str) -> Result<String, JoinError> {
    if status == 404 || status == 410 {
        return Err(JoinError {
            code: "not_supported",
            message: format!("{} does not publish poppy.json", host),
        });
    }
    if status != 200 {
        return Err(discovery_failed(format!(
            "poppy.json returned status {}",
            status
        )));
    }
    let doc: Value =
        serde_json::from_str(body).map_err(|_| discovery_failed("poppy.json is not valid JSON"))?;
    let version = doc
        .get("protocol_version")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let compatible = version.split_once('.').is_some_and(|(major, minor)| {
        major == "1" && !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit())
    });
    if !compatible {
        return Err(discovery_failed(format!(
            "Unsupported protocol_version '{}'",
            version
        )));
    }
    let domain = doc
        .pointer("/organization/domain")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !domain.eq_ignore_ascii_case(host) {
        return Err(discovery_failed(format!(
            "organization.domain '{}' does not match {}",
            domain, host
        )));
    }
    if doc.pointer("/web/disabled").and_then(Value::as_bool) == Some(true) {
        return Err(JoinError {
            code: "web_disabled",
            message: "The Company asks Personal Agents not to browse its website".to_string(),
        });
    }
    let endpoint = doc
        .pointer("/web/browser_session_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| JoinError {
            code: "no_browser_session_endpoint",
            message: "The Company does not list web.browser_session_endpoint".to_string(),
        })?;
    let url = Url::parse(endpoint)
        .map_err(|e| discovery_failed(format!("Invalid browser_session_endpoint: {}", e)))?;
    check_scheme(&url)?;
    Ok(endpoint.to_string())
}

/// Resolves the discovery URL for `raw_url` without touching the network.
pub fn target(raw_url: &str) -> Result<Target, JoinError> {
    let url = crate::read::normalize_url(raw_url).map_err(discovery_failed)?;
    check_scheme(&url)?;
    let host =
        host_with_port(&url).ok_or_else(|| discovery_failed(format!("{} has no host", raw_url)))?;
    let origin = format!("{}://{}", url.scheme(), host);
    let well_known = format!("{}{}", origin, WELL_KNOWN);
    Ok(Target {
        origin,
        host,
        well_known,
    })
}

/// Redirect statuses that carry a `Location` to follow.
fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Fetches the Company's `poppy.json`, following redirects one hop at a time.
///
/// `check` runs on every URL before it is requested, so a hop can be refused
/// before any DNS lookup or connection, and the address check in `client_for`
/// runs before the connection too. A hop fetches only the well-known path with
/// no query, so a file uploaded elsewhere on a host cannot speak for it. The
/// document's `organization.domain` must match the host that finally serves it.
/// Everything stops at `deadline`.
pub async fn fetch(
    target: Target,
    deadline: tokio::time::Instant,
    trust: &TrustOptions,
    check: impl Fn(&Url) -> Result<(), JoinError>,
) -> Result<Discovery, JoinError> {
    let mut url = Url::parse(&target.well_known)
        .map_err(|e| discovery_failed(format!("Invalid discovery URL: {}", e)))?;
    let local = started_locally(&url);
    let mut seen: Vec<Url> = Vec::new();
    let (response, host, origin) = loop {
        check_scheme(&url)?;
        // Only the well-known path speaks for a host; an upload elsewhere on it must not.
        if url.path() != WELL_KNOWN || url.query().is_some() {
            return Err(discovery_failed(format!(
                "Discovery would request {}, not {}",
                url, WELL_KNOWN
            )));
        }
        check(&url)?;
        if seen.contains(&url) {
            return Err(discovery_failed(format!("Redirect loop at {}", url)));
        }
        seen.push(url.clone());
        let client = client_for(&url, local, trust, deadline).await?;
        let request = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .send();
        let response = tokio::time::timeout_at(deadline, request)
            .await
            .map_err(|_| discovery_failed(format!("Request to {} timed out", url)))?
            .map_err(|e| discovery_failed(format!("Request to {} failed: {}", url, e)))?;
        let status = response.status().as_u16();
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok());
        if let (true, Some(location)) = (is_redirect(status), location) {
            url = url
                .join(location)
                .map_err(|e| discovery_failed(format!("Invalid redirect Location: {}", e)))?;
            continue;
        }
        let host =
            host_with_port(&url).ok_or_else(|| discovery_failed(format!("{} has no host", url)))?;
        let origin = format!("{}://{}", url.scheme(), host);
        break (response, host, origin);
    };
    let status = response.status().as_u16();
    if status == 404 || status == 410 {
        return Err(interpret(&host, status, "").unwrap_err());
    }
    let body = tokio::time::timeout_at(deadline, response.text())
        .await
        .map_err(|_| discovery_failed(format!("Reading {} timed out", url)))?
        .map_err(|e| discovery_failed(format!("Could not read {}: {}", url, e)))?;
    let endpoint = interpret(&host, status, &body)?;
    Ok(Discovery { origin, endpoint })
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn doc(extra: Value) -> String {
        let mut doc = json!({
            "protocol_version": "1.0",
            "organization": { "domain": "example.com" },
            "web": { "browser_session_endpoint": "https://example.com/join" },
        });
        for (key, value) in extra.as_object().unwrap() {
            doc[key] = value.clone();
        }
        doc.to_string()
    }

    fn code(status: u16, body: &str) -> &'static str {
        interpret("example.com", status, body).unwrap_err().code
    }

    #[test]
    fn a_valid_document_yields_the_endpoint() {
        assert_eq!(
            interpret("example.com", 200, &doc(json!({}))).unwrap(),
            "https://example.com/join"
        );
        let minor = doc(json!({ "protocol_version": "1.7" }));
        assert!(interpret("example.com", 200, &minor).is_ok());
    }

    #[test]
    fn missing_documents_are_not_supported() {
        assert_eq!(code(404, ""), "not_supported");
        assert_eq!(code(410, ""), "not_supported");
    }

    #[test]
    fn other_failures_are_discovery_failed() {
        assert_eq!(code(500, ""), "discovery_failed");
        assert_eq!(code(200, "not json"), "discovery_failed");
        for version in ["2.0", "1.invalid", "1", "1.", "1.+7"] {
            assert_eq!(
                code(200, &doc(json!({ "protocol_version": version }))),
                "discovery_failed",
                "{version}"
            );
        }
        assert_eq!(
            code(
                200,
                &doc(json!({ "organization": { "domain": "other.example" } }))
            ),
            "discovery_failed"
        );
        let plain = doc(json!({ "web": { "browser_session_endpoint": "http://example.com/j" } }));
        assert_eq!(code(200, &plain), "discovery_failed");
    }

    #[test]
    fn web_settings_are_reported() {
        assert_eq!(
            code(200, &doc(json!({ "web": { "disabled": true } }))),
            "web_disabled"
        );
        assert_eq!(
            code(200, &doc(json!({ "web": {} }))),
            "no_browser_session_endpoint"
        );
    }

    #[test]
    fn the_target_is_the_well_known_url_of_the_origin() {
        let target = target("http://127.0.0.1:3000/some/page").unwrap();
        assert_eq!(target.origin, "http://127.0.0.1:3000");
        assert_eq!(
            target.well_known,
            "http://127.0.0.1:3000/.well-known/poppy.json"
        );
        assert_eq!(target.host, "127.0.0.1:3000");
        assert!(super::target("http://example.com/").is_err());
    }

    #[test]
    fn plain_http_is_only_for_loopback() {
        assert!(check_scheme(&Url::parse("http://127.0.0.1:3000/").unwrap()).is_ok());
        assert!(check_scheme(&Url::parse("http://localhost/").unwrap()).is_ok());
        assert!(check_scheme(&Url::parse("http://example.com/").unwrap()).is_err());
        assert!(check_scheme(&Url::parse("https://example.com/").unwrap()).is_ok());
    }

    #[test]
    fn a_public_start_reaches_public_addresses_only() {
        assert!(super::started_locally(
            &Url::parse("http://127.0.0.1:3000/").unwrap()
        ));
        assert!(super::started_locally(
            &Url::parse("http://localhost:3000/").unwrap()
        ));
        assert!(super::started_locally(
            &Url::parse("https://[::1]/").unwrap()
        ));
        assert!(!super::started_locally(
            &Url::parse("https://example.com/").unwrap()
        ));

        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let private: IpAddr = "10.0.0.7".parse().unwrap();
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let metadata: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(admitted(public, false));
        assert!(!admitted(loopback, false));
        assert!(!admitted(private, false));
        assert!(!admitted(metadata, false));
        assert!(admitted(loopback, true));
        assert!(admitted(private, true));
        // A link-local address is never reached, local target or not.
        assert!(!admitted(metadata, true));
    }

    #[test]
    fn a_refused_hop_says_which_address_it_reached() {
        let err = check_address("evil.example", "10.0.0.7".parse().unwrap(), false).unwrap_err();
        assert_eq!(err.code, "discovery_failed");
        assert!(
            err.message.contains("non-public address 10.0.0.7"),
            "{err:?}"
        );
        let err =
            check_address("evil.example", "169.254.169.254".parse().unwrap(), true).unwrap_err();
        assert!(err.message.contains("link-local"), "{err:?}");
    }
}
