//! Browser provider connections for remote CDP sessions.
//!
//! Supports Browserbase, Browserless, Browser Use, and Kernel providers.
//! Each provider returns a CDP WebSocket URL for connecting via BrowserManager.

use serde_json::{json, Value};
use std::env;

/// Provider session info for cleanup on failure.
pub struct ProviderSession {
    pub provider: String,
    pub session_id: String,
}

pub struct ProviderConnection {
    pub ws_url: String,
    pub session: Option<ProviderSession>,
    /// If true, the WebSocket IS the page session (no Target.* commands).
    pub direct_page: bool,
}

/// Connects to the specified browser provider and returns a CDP WebSocket URL
/// along with session info for cleanup on failure.
pub async fn connect_provider(provider_name: &str) -> Result<ProviderConnection, String> {
    match provider_name.to_lowercase().as_str() {
        "browserbase" => {
            let (url, session) = connect_browserbase().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
            })
        }
        "browserless" => {
            let (url, session) = connect_browserless().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
            })
        }
        "browser-use" | "browseruse" => {
            let (url, session) = connect_browser_use().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
            })
        }
        "kernel" => {
            let (url, session) = connect_kernel().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
            })
        }
        _ => Err(format!(
            "Unknown provider '{}'. Supported: browserbase, browserless, browser-use, kernel",
            provider_name
        )),
    }
}

/// Close a provider session (call on CDP connect failure).
pub async fn close_provider_session(session: &ProviderSession) {
    let client = reqwest::Client::new();
    match session.provider.as_str() {
        "browserbase" => {
            if let Ok(api_key) = env::var("BROWSERBASE_API_KEY") {
                let _ = client
                    .post(format!(
                        "https://api.browserbase.com/v1/sessions/{}",
                        session.session_id
                    ))
                    .header("Content-Type", "application/json")
                    .header("X-BB-API-Key", &api_key)
                    .json(&serde_json::json!({ "status": "REQUEST_RELEASE" }))
                    .send()
                    .await;
            }
        }
        "browser-use" => {
            if let Ok(api_key) = env::var("BROWSER_USE_API_KEY") {
                let _ = client
                    .patch(format!(
                        "https://api.browser-use.com/api/v2/browsers/{}",
                        session.session_id
                    ))
                    .header("X-Browser-Use-API-Key", &api_key)
                    .header("Content-Type", "application/json")
                    .json(&json!({ "action": "stop" }))
                    .send()
                    .await;
            }
        }
        "browserless" => {
            // session_id holds the stop URL for browserless
            let _ = client.delete(&session.session_id).send().await;
        }
        "kernel" => {
            if let Ok(api_key) = env::var("KERNEL_API_KEY") {
                let endpoint = env::var("KERNEL_ENDPOINT")
                    .unwrap_or_else(|_| "https://api.onkernel.com".to_string());
                let _ = client
                    .delete(format!(
                        "{}/browsers/{}",
                        endpoint.trim_end_matches('/'),
                        session.session_id
                    ))
                    .header("Authorization", format!("Bearer {}", api_key))
                    .send()
                    .await;
            }
        }
        _ => {}
    }
}

async fn connect_browserbase() -> Result<(String, Option<ProviderSession>), String> {
    let api_key = env::var("BROWSERBASE_API_KEY")
        .map_err(|_| "BROWSERBASE_API_KEY environment variable is not set")?;

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.browserbase.com/v1/sessions")
        .header("content-type", "application/json")
        .header("x-bb-api-key", &api_key)
        .body("{}")
        .send()
        .await
        .map_err(|e| format!("Browserbase request failed: {}", e))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read Browserbase response: {}", e))?;

    if !status.is_success() {
        return Err(format!(
            "Browserbase API error ({}): {}",
            status.as_u16(),
            body
        ));
    }

    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid Browserbase response: {}", e))?;

    let session_id = json
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let ws_url = json
        .get("connectUrl")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "Browserbase response missing connectUrl".to_string())?;

    Ok((
        ws_url,
        Some(ProviderSession {
            provider: "browserbase".to_string(),
            session_id,
        }),
    ))
}

async fn connect_browserless() -> Result<(String, Option<ProviderSession>), String> {
    let api_key = env::var("BROWSERLESS_API_KEY")
        .map_err(|_| "BROWSERLESS_API_KEY environment variable is not set")?;

    let api_url = env::var("BROWSERLESS_API_URL")
        .unwrap_or_else(|_| "https://production-sfo.browserless.io".to_string());
    let browser_type =
        env::var("BROWSERLESS_BROWSER_TYPE").unwrap_or_else(|_| "chromium".to_string());

    let supported = ["chromium", "chrome"];
    if !supported.contains(&browser_type.as_str()) {
        return Err(format!(
            "BROWSERLESS_BROWSER_TYPE \"{}\" is not supported. Only {} are allowed.",
            browser_type,
            supported.join(", ")
        ));
    }

    let ttl: u64 = env::var("BROWSERLESS_TTL")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300000);
    let stealth = env::var("BROWSERLESS_STEALTH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);

    let url = format!("{}/session", api_url.trim_end_matches('/'));

    let client = reqwest::Client::new();
    let response = client
        .post(&url)
        .query(&[("token", &api_key)])
        .header("Content-Type", "application/json")
        .json(&json!({
            "ttl": ttl,
            "stealth": stealth,
            "browser": browser_type,
        }))
        .send()
        .await
        .map_err(|e| format!("Browserless request failed: {}", e))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read Browserless response: {}", e))?;

    if !status.is_success() {
        return Err(format!(
            "Browserless API error ({}): {}",
            status.as_u16(),
            body
        ));
    }

    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid Browserless response: {}", e))?;

    let connect_url = json
        .get("connect")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "Browserless response missing 'connect' URL".to_string())?;

    let stop_url = json
        .get("stop")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "Browserless response missing 'stop' URL".to_string())?;

    Ok((
        connect_url,
        Some(ProviderSession {
            provider: "browserless".to_string(),
            // Store the stop URL as the session_id for cleanup
            session_id: stop_url,
        }),
    ))
}

async fn connect_browser_use() -> Result<(String, Option<ProviderSession>), String> {
    let api_key = env::var("BROWSER_USE_API_KEY")
        .map_err(|_| "BROWSER_USE_API_KEY environment variable is not set")?;

    let ws_url = format!("wss://connect.browser-use.com?apiKey={}", api_key);

    Ok((ws_url, None))
}

async fn connect_kernel() -> Result<(String, Option<ProviderSession>), String> {
    let api_key = env::var("KERNEL_API_KEY").ok();
    let endpoint =
        env::var("KERNEL_ENDPOINT").unwrap_or_else(|_| "https://api.onkernel.com".to_string());

    let url = format!("{}/browsers", endpoint.trim_end_matches('/'));

    let headless = env::var("KERNEL_HEADLESS")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let stealth = env::var("KERNEL_STEALTH")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);
    let timeout_seconds = env::var("KERNEL_TIMEOUT_SECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(300);

    let mut body = json!({
        "headless": headless,
        "stealth": stealth,
        "timeout_seconds": timeout_seconds,
    });

    if let Ok(profile) = env::var("KERNEL_PROFILE_NAME") {
        if !profile.is_empty() {
            body.as_object_mut()
                .unwrap()
                .insert("profile".to_string(), json!(profile));
        }
    }

    let client = reqwest::Client::new();
    let mut request = client.post(&url).header("Content-Type", "application/json");
    if let Some(ref key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }
    let response = request
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Kernel request failed: {}", e))?;

    let status = response.status();
    let resp_body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read Kernel response: {}", e))?;

    if !status.is_success() {
        return Err(format!(
            "Kernel API error ({}): {}",
            status.as_u16(),
            resp_body
        ));
    }

    let json: Value =
        serde_json::from_str(&resp_body).map_err(|e| format!("Invalid Kernel response: {}", e))?;

    let session_id = json
        .get("session_id")
        .or_else(|| json.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let ws_url = json
        .get("cdp_ws_url")
        .or_else(|| json.get("connectUrl"))
        .or_else(|| json.get("connect_url"))
        .or_else(|| json.get("cdpUrl"))
        .or_else(|| json.get("cdp_url"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| {
            "Kernel response missing cdp_ws_url, connectUrl, connect_url, cdpUrl, or cdp_url"
                .to_string()
        })?;

    Ok((
        ws_url,
        Some(ProviderSession {
            provider: "kernel".to_string(),
            session_id,
        }),
    ))
}
