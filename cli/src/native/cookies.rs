use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::cdp::client::CdpClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    #[serde(default)]
    pub expires: f64,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub http_only: bool,
    #[serde(default)]
    pub secure: bool,
    #[serde(default)]
    pub session: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}

pub async fn get_all_cookies(client: &CdpClient, session_id: &str) -> Result<Vec<Cookie>, String> {
    let result = client
        .send_command_no_params("Network.getAllCookies", Some(session_id))
        .await?;

    let cookies: Vec<Cookie> = result
        .get("cookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Ok(cookies)
}

pub async fn get_cookies(
    client: &CdpClient,
    session_id: &str,
    urls: Option<Vec<String>>,
) -> Result<Vec<Cookie>, String> {
    let params = match urls {
        Some(ref u) if !u.is_empty() => json!({ "urls": u }),
        _ => json!({}),
    };

    let result = client
        .send_command("Network.getCookies", Some(params), Some(session_id))
        .await?;

    let cookies: Vec<Cookie> = result
        .get("cookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    Ok(cookies)
}

pub async fn set_cookies(
    client: &CdpClient,
    session_id: &str,
    cookies: Vec<Value>,
    current_url: Option<&str>,
) -> Result<(), String> {
    let cookies: Vec<Value> = cookies
        .into_iter()
        .map(|mut c| {
            // Auto-fill url if no domain/path/url provided
            if c.get("url").is_none() && c.get("domain").is_none() && current_url.is_some() {
                c.as_object_mut().map(|m| {
                    m.insert(
                        "url".to_string(),
                        Value::String(current_url.unwrap().to_string()),
                    )
                });
            }
            c
        })
        .collect();

    client
        .send_command(
            "Network.setCookies",
            Some(json!({ "cookies": cookies })),
            Some(session_id),
        )
        .await?;

    Ok(())
}

pub async fn clear_cookies(client: &CdpClient, session_id: &str) -> Result<(), String> {
    client
        .send_command_no_params("Network.clearBrowserCookies", Some(session_id))
        .await?;
    Ok(())
}

/// A cookie that a Device Bound Session Credentials (DBSC) session lists as
/// one of its credentials ("cookie craving"). Chrome keeps such cookies fresh
/// by signing refresh challenges with a device key that never leaves the
/// browser that registered the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundCookie {
    pub name: String,
    pub domain: String,
    pub path: String,
}

impl BoundCookie {
    fn matches(&self, cookie: &Cookie) -> bool {
        self.name == cookie.name
            && normalize_cookie_domain(&self.domain) == normalize_cookie_domain(&cookie.domain)
            && normalize_cookie_path(&self.path) == normalize_cookie_path(&cookie.path)
    }
}

fn normalize_cookie_domain(domain: &str) -> String {
    domain.trim_start_matches('.').to_ascii_lowercase()
}

fn normalize_cookie_path(path: &str) -> &str {
    if path.is_empty() {
        "/"
    } else {
        path
    }
}

/// Extracts the cookie cravings from a `Network.deviceBoundSessionsAdded`
/// event payload.
pub fn bound_cookies_from_sessions(params: &Value) -> Vec<BoundCookie> {
    let mut out = Vec::new();
    let sessions = params.get("sessions").and_then(|v| v.as_array());
    for session in sessions.into_iter().flatten() {
        let cravings = session.get("cookieCravings").and_then(|v| v.as_array());
        for craving in cravings.into_iter().flatten() {
            let Some(name) = craving.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            let field = |key: &str| {
                craving
                    .get(key)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string()
            };
            let bound = BoundCookie {
                name: name.to_string(),
                domain: field("domain"),
                path: field("path"),
            };
            if !out.contains(&bound) {
                out.push(bound);
            }
        }
    }
    out
}

/// Splits `cookies` into the ones safe to replay and the ones bound to a
/// device bound session. Replaying a bound cookie in another browser, or
/// writing an old copy back into the browser that owns the session, lets the
/// site see one device-bound session used from two places, and sites end the
/// session when that happens.
pub fn partition_bound_cookies(
    cookies: Vec<Cookie>,
    bound: &[BoundCookie],
) -> (Vec<Cookie>, Vec<Cookie>) {
    cookies
        .into_iter()
        .partition(|cookie| !bound.iter().any(|b| b.matches(cookie)))
}

/// Formats skipped bound cookies for warnings, e.g. `sid (example.com)`.
pub fn describe_cookies(cookies: &[Cookie]) -> Vec<String> {
    cookies
        .iter()
        .map(|c| format!("{} ({})", c.name, normalize_cookie_domain(&c.domain)))
        .collect()
}

/// How long to wait for Chrome to report its device bound sessions after
/// `Network.enableDeviceBoundSessions`. Chrome sends the initial set as an
/// event once it has read the session store, which takes a few milliseconds.
const BOUND_SESSIONS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// True when the error says the browser does not implement
/// `Network.enableDeviceBoundSessions`, as older Chrome and engines without
/// DBSC do. A browser that does not implement the command has no bound
/// sessions to report. Any other error means the session list is unknown.
fn dbsc_is_unsupported(error: &str) -> bool {
    error.contains("Network.enableDeviceBoundSessions")
        && (error.contains("wasn't found")
            || error.contains("Method not found")
            || error.contains("-32601"))
}

/// Lists the cookies that the browser's device bound sessions are bound to.
///
/// Returns an empty list when the browser does not support DBSC (older Chrome,
/// Linux builds without DBSC, other engines). An empty list also means "no
/// bound cookies", so a session list that cannot be read is an error rather
/// than an empty result: the caller must not export or overwrite bound cookies
/// just because enumeration failed.
pub async fn get_device_bound_cookies(
    client: &CdpClient,
    session_id: &str,
) -> Result<Vec<BoundCookie>, String> {
    let mut events = client.subscribe_methods(&["Network.deviceBoundSessionsAdded"]);
    let enabled = client
        .send_command(
            "Network.enableDeviceBoundSessions",
            Some(json!({ "enable": true })),
            Some(session_id),
        )
        .await;
    if let Err(error) = enabled {
        if dbsc_is_unsupported(&error) {
            return Ok(Vec::new());
        }
        return Err(format!(
            "Could not list the browser's device-bound sessions: {}",
            error
        ));
    }
    let mut bound = Vec::new();
    let deadline = tokio::time::Instant::now() + BOUND_SESSIONS_TIMEOUT;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, events.recv()).await {
        if event.session_id.as_deref() == Some(session_id) {
            bound = bound_cookies_from_sessions(&event.params);
            break;
        }
    }
    let _ = client
        .send_command(
            "Network.enableDeviceBoundSessions",
            Some(json!({ "enable": false })),
            Some(session_id),
        )
        .await;
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(name: &str, domain: &str, path: &str) -> Cookie {
        Cookie {
            name: name.to_string(),
            value: format!("{name}-value"),
            domain: domain.to_string(),
            path: path.to_string(),
            expires: 0.0,
            size: 0,
            http_only: true,
            secure: true,
            session: false,
            same_site: Some("Lax".to_string()),
        }
    }

    // Shape of a `Network.deviceBoundSessionsAdded` event from Chrome 158.
    fn sessions_event() -> Value {
        json!({
            "sessions": [
                {
                    "key": { "site": "https://google.com", "id": "s1" },
                    "refreshUrl": "https://accounts.google.com/RotateBoundCookies",
                    "inclusionRules": { "origin": "https://google.com", "includeSite": true, "urlRules": [] },
                    "cookieCravings": [
                        { "name": "__Secure-1PSIDTS", "domain": ".google.com", "path": "/", "secure": true, "httpOnly": true, "sameSite": "Lax" },
                        { "name": "__Secure-3PSIDTS", "domain": ".google.com", "path": "/", "secure": true, "httpOnly": true, "sameSite": "None" }
                    ],
                    "expiryDate": 1829669803.75,
                    "allowedRefreshInitiators": []
                },
                {
                    "key": { "site": "https://localhost", "id": "s2" },
                    "refreshUrl": "https://localhost:8787/dbsc/refresh",
                    "inclusionRules": { "origin": "https://localhost:8787", "includeSite": false, "urlRules": [] },
                    "cookieCravings": [
                        { "name": "bound", "domain": "localhost", "path": "/", "secure": true, "httpOnly": true, "sameSite": "Lax" }
                    ],
                    "expiryDate": 1829669803.75,
                    "allowedRefreshInitiators": []
                }
            ]
        })
    }

    #[test]
    fn bound_cookies_come_from_session_cravings() {
        let bound = bound_cookies_from_sessions(&sessions_event());
        let names: Vec<&str> = bound.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["__Secure-1PSIDTS", "__Secure-3PSIDTS", "bound"]);
        assert!(bound_cookies_from_sessions(&json!({ "sessions": [] })).is_empty());
        assert!(bound_cookies_from_sessions(&json!({})).is_empty());
    }

    #[test]
    fn partition_skips_only_cookies_a_session_is_bound_to() {
        let bound = bound_cookies_from_sessions(&sessions_event());
        let cookies = vec![
            cookie("__Secure-1PSIDTS", ".google.com", "/"),
            cookie("__Secure-1PSID", ".google.com", "/"),
            cookie("__Secure-3PSIDTS", "google.com", "/"),
            // Same name on an unrelated site is not bound.
            cookie("__Secure-1PSIDTS", ".example.com", "/"),
            cookie("bound", "LOCALHOST", ""),
            cookie("sid", "localhost", "/"),
            // Same name and site with another path is a different cookie.
            cookie("bound", "localhost", "/admin"),
        ];

        let (kept, skipped) = partition_bound_cookies(cookies, &bound);

        let label = |c: &Cookie| format!("{}@{}{}", c.name, c.domain, c.path);
        assert_eq!(
            kept.iter().map(label).collect::<Vec<_>>(),
            [
                "__Secure-1PSID@.google.com/",
                "__Secure-1PSIDTS@.example.com/",
                "sid@localhost/",
                "bound@localhost/admin",
            ]
        );
        assert_eq!(
            describe_cookies(&skipped),
            [
                "__Secure-1PSIDTS (google.com)",
                "__Secure-3PSIDTS (google.com)",
                "bound (localhost)",
            ]
        );
    }

    #[test]
    fn only_a_missing_dbsc_command_counts_as_no_bound_sessions() {
        assert!(dbsc_is_unsupported(
            "CDP error (Network.enableDeviceBoundSessions): 'Network.enableDeviceBoundSessions' wasn't found"
        ));
        assert!(dbsc_is_unsupported(
            "CDP error (Network.enableDeviceBoundSessions): Method not found (-32601)"
        ));
        // Any other failure leaves the session list unknown, so it must not
        // pass for "the browser has no bound sessions".
        assert!(!dbsc_is_unsupported(
            "CDP error (Network.enableDeviceBoundSessions): Internal error"
        ));
        assert!(!dbsc_is_unsupported(
            "CDP command timed out: Network.enableDeviceBoundSessions"
        ));
    }

    #[test]
    fn partition_keeps_everything_without_bound_sessions() {
        let cookies = vec![
            cookie("a", ".example.com", "/"),
            cookie("b", "localhost", "/"),
        ];
        let (kept, skipped) = partition_bound_cookies(cookies, &[]);
        assert_eq!(kept.len(), 2);
        assert!(skipped.is_empty());
    }
}
