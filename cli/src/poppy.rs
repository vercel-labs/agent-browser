//! `poppy join` (experimental): discovery, on the CLI side.
//!
//! The browser steps and the authorizer run in the daemon (`native::poppy`),
//! so the one-time code never reaches this process.

use std::time::Duration;

use reqwest::{redirect::Policy, Client};
use serde_json::{json, Value};
use url::Url;

use crate::color;
use crate::tls::TrustOptions;

const WELL_KNOWN: &str = "/.well-known/poppy.json";
/// The documented `AGENT_BROWSER_DEFAULT_TIMEOUT` default, in ms.
pub const DEFAULT_TIMEOUT_MS: u64 = 25_000;

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

async fn discover_async(
    raw_url: &str,
    timeout_ms: u64,
    trust: &TrustOptions,
) -> Result<Discovery, JoinError> {
    let target = crate::read::normalize_url(raw_url).map_err(discovery_failed)?;
    check_scheme(&target)?;
    let host = host_with_port(&target)
        .ok_or_else(|| discovery_failed(format!("{} has no host", raw_url)))?;
    let origin = format!("{}://{}", target.scheme(), host);

    let client = crate::tls::apply_to_reqwest(Client::builder(), trust)
        .map_err(discovery_failed)?
        .redirect(Policy::none())
        .timeout(Duration::from_millis(timeout_ms))
        .build()
        .map_err(|e| discovery_failed(format!("Failed to create HTTP client: {}", e)))?;
    let url = format!("{}{}", origin, WELL_KNOWN);
    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .map_err(|e| discovery_failed(format!("Request to {} failed: {}", url, e)))?;
    let status = response.status().as_u16();
    if status == 404 || status == 410 {
        return Err(interpret(&host, status, "").unwrap_err());
    }
    let body = response
        .text()
        .await
        .map_err(|e| discovery_failed(format!("Could not read {}: {}", url, e)))?;
    let endpoint = interpret(&host, status, &body)?;
    Ok(Discovery { origin, endpoint })
}

/// Fetches the Company's `poppy.json` and returns its origin and browser session endpoint.
pub fn discover(
    raw_url: &str,
    timeout_ms: u64,
    trust: &TrustOptions,
) -> Result<Discovery, JoinError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| discovery_failed(format!("Failed to start runtime: {}", e)))?
        .block_on(discover_async(raw_url, timeout_ms, trust))
}

pub fn print_failure(json: bool, error: &JoinError) {
    if json {
        println!(
            "{}",
            json!({
                "success": false,
                "error": error.message,
                "code": error.code,
                "data": { "experimental": true, "joined": false },
            })
        );
    } else {
        eprintln!(
            "{} {}: {}",
            color::error_indicator(),
            error.code,
            error.message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn plain_http_is_only_for_loopback() {
        assert!(check_scheme(&Url::parse("http://127.0.0.1:3000/").unwrap()).is_ok());
        assert!(check_scheme(&Url::parse("http://localhost/").unwrap()).is_ok());
        assert!(check_scheme(&Url::parse("http://example.com/").unwrap()).is_err());
        assert!(check_scheme(&Url::parse("https://example.com/").unwrap()).is_ok());
    }
}
