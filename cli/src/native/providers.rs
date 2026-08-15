//! Browser provider connections for remote CDP sessions.
//!
//! Supports AgentCore, Browserbase, Browserless, Browser Use, and Kernel providers.
//! Each provider returns a CDP WebSocket URL for connecting via BrowserManager.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::env;
use std::time::Duration;

const BROWSER_USE_API_BASE: &str = "https://api.browser-use.com";
// Browser creation in Cloud can legitimately take tens of seconds. The CLI
// grants Browser Use launch commands a longer IPC budget so this non-idempotent
// request is not retried while the daemon is still waiting for its response.
const BROWSER_USE_API_TIMEOUT: Duration = Duration::from_secs(60);
const BROWSER_USE_CDP_TIMEOUT: Duration = Duration::from_secs(10);
const BROWSER_USE_STOP_TIMEOUT: Duration = Duration::from_secs(7);
const BROWSER_USE_STOP_ATTEMPTS: usize = 3;

/// Provider session info for cleanup on failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSession {
    pub provider: String,
    pub session_id: String,
}

#[derive(Debug)]
pub struct ProviderConnection {
    pub ws_url: String,
    pub session: Option<ProviderSession>,
    /// If true, the WebSocket IS the page session (no Target.* commands).
    pub direct_page: bool,
    pub metadata: Option<Value>,
}

/// Connects to the specified browser provider and returns a CDP WebSocket URL
/// along with session info for cleanup on failure.
pub async fn connect_provider(provider_name: &str) -> Result<ProviderConnection, String> {
    let plugins = crate::plugins::plugins_from_env();
    connect_provider_with_plugins(provider_name, &plugins).await
}

/// Connects to a built-in provider or a plugin provider from the supplied
/// registry. Callers that already loaded config must use this helper so policy
/// checks and provider execution consult the same plugin list.
pub async fn connect_provider_with_plugins(
    provider_name: &str,
    plugins: &[crate::plugins::PluginConfig],
) -> Result<ProviderConnection, String> {
    connect_provider_with_plugins_and_options(provider_name, plugins, None).await
}

/// Connects to a built-in provider or plugin provider with launch options
/// supplied by the command that requested the provider. Built-in providers keep
/// their existing environment-based behavior; plugin providers receive these
/// options in the stdio protocol request.
pub async fn connect_provider_with_plugins_and_options(
    provider_name: &str,
    plugins: &[crate::plugins::PluginConfig],
    launch_options: Option<Value>,
) -> Result<ProviderConnection, String> {
    match provider_name.to_lowercase().as_str() {
        "browserbase" => {
            let (url, session) = connect_browserbase().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
                metadata: None,
            })
        }
        "browserless" => {
            let (url, session) = connect_browserless().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
                metadata: None,
            })
        }
        "browser-use" | "browseruse" => {
            let (url, session) = connect_browser_use().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
                metadata: None,
            })
        }
        "kernel" => {
            let (url, session) = connect_kernel().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
                metadata: None,
            })
        }
        "agentcore" => {
            let (url, session) = connect_agentcore().await?;
            Ok(ProviderConnection {
                ws_url: url,
                session,
                direct_page: false,
                metadata: None,
            })
        }
        _ => {
            connect_plugin_provider_with_plugins_and_options(provider_name, plugins, launch_options)
                .await
        }
    }
}

/// Close a provider session (call on CDP connect failure).
pub async fn close_provider_session(session: &ProviderSession) {
    let plugins = crate::plugins::plugins_from_env();
    let _ = close_provider_session_with_plugins(session, &plugins).await;
}

/// Close a provider session with the plugin registry that created it.
pub async fn close_provider_session_with_plugins(
    session: &ProviderSession,
    plugins: &[crate::plugins::PluginConfig],
) -> Result<(), String> {
    if let Some(plugin_name) = session.provider.strip_prefix("plugin:") {
        if let Ok(cleanup) = serde_json::from_str::<Value>(&session.session_id) {
            let _ =
                crate::plugins::close_browser_provider_with_plugins(plugin_name, plugins, cleanup)
                    .await;
        }
        return Ok(());
    }

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
            let api_key = env::var("BROWSER_USE_API_KEY")
                .map_err(|_| "BROWSER_USE_API_KEY is required to stop the Cloud session")?;
            stop_browser_use_session(&client, BROWSER_USE_API_BASE, &api_key, &session.session_id)
                .await?;
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
        "agentcore" => {
            // AgentCore session cleanup is handled via signed DELETE request
            let _ = close_agentcore_session(&session.session_id).await;
        }
        _ => {}
    }

    Ok(())
}

pub async fn connect_plugin_provider_with_plugins(
    provider_name: &str,
    plugins: &[crate::plugins::PluginConfig],
) -> Result<ProviderConnection, String> {
    connect_plugin_provider_with_plugins_and_options(provider_name, plugins, None).await
}

pub async fn connect_plugin_provider_with_plugins_and_options(
    provider_name: &str,
    plugins: &[crate::plugins::PluginConfig],
    launch_options: Option<Value>,
) -> Result<ProviderConnection, String> {
    if crate::plugins::find_plugin(plugins, provider_name).is_none() {
        return Err(format!(
            "Unknown provider '{}'. Supported: browserbase, browserless, browser-use, kernel, agentcore, or a configured plugin with browser.provider",
            provider_name
        ));
    }

    let mut plugin_launch_options = serde_json::Map::new();
    plugin_launch_options.insert(
        "headed".to_string(),
        json!(env_var_is_truthy("AGENT_BROWSER_HEADED")),
    );
    plugin_launch_options.insert(
        "engine".to_string(),
        json!(env::var("AGENT_BROWSER_ENGINE").unwrap_or_else(|_| "chrome".to_string())),
    );
    plugin_launch_options.insert(
        "userAgent".to_string(),
        json!(env::var("AGENT_BROWSER_USER_AGENT").ok()),
    );
    plugin_launch_options.insert(
        "colorScheme".to_string(),
        json!(env::var("AGENT_BROWSER_COLOR_SCHEME").ok()),
    );

    if let Some(Value::Object(command_options)) = launch_options {
        for (key, value) in command_options {
            plugin_launch_options.insert(key, value);
        }
    }

    let request = json!({
        "provider": provider_name,
        "session": env::var("AGENT_BROWSER_SESSION").unwrap_or_else(|_| "default".to_string()),
        "launchOptions": Value::Object(plugin_launch_options),
    });
    let browser =
        crate::plugins::connect_browser_provider_with_plugins(provider_name, plugins, request)
            .await?;
    let session = browser.cleanup.as_ref().map(|cleanup| ProviderSession {
        provider: format!("plugin:{}", provider_name),
        session_id: serde_json::to_string(cleanup).unwrap_or_else(|_| "{}".to_string()),
    });
    Ok(ProviderConnection {
        ws_url: browser.cdp_url,
        session,
        direct_page: browser.direct_page,
        metadata: browser.metadata,
    })
}

fn env_var_is_truthy(name: &str) -> bool {
    match env::var(name) {
        Ok(val) => !matches!(val.to_ascii_lowercase().as_str(), "0" | "false" | "no" | ""),
        Err(_) => false,
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

async fn stop_browser_use_session(
    client: &reqwest::Client,
    api_base: &str,
    api_key: &str,
    session_id: &str,
) -> Result<(), String> {
    let mut last_error = String::new();
    for attempt in 1..=BROWSER_USE_STOP_ATTEMPTS {
        let response = client
            .patch(format!(
                "{}/api/v4/browsers/{}",
                api_base.trim_end_matches('/'),
                session_id
            ))
            .header("X-Browser-Use-API-Key", api_key)
            .header(
                reqwest::header::USER_AGENT,
                format!("agent-browser/{}", env!("CARGO_PKG_VERSION")),
            )
            .header("Content-Type", "application/json")
            .json(&json!({ "action": "stop" }))
            .timeout(BROWSER_USE_STOP_TIMEOUT)
            .send()
            .await;

        let retryable = match response {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                let status = response.status();
                let retryable =
                    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "failed to read response body".to_string());
                last_error = format!("Browser Use stop API error ({}): {}", status.as_u16(), body);
                retryable
            }
            Err(error) => {
                last_error = format!("Browser Use stop request failed: {error}");
                true
            }
        };

        if !retryable || attempt == BROWSER_USE_STOP_ATTEMPTS {
            return Err(last_error);
        }
        tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
    }

    Err(last_error)
}

async fn browser_use_error_with_cleanup(
    client: &reqwest::Client,
    api_base: &str,
    api_key: &str,
    session_id: &str,
    error: String,
) -> String {
    match stop_browser_use_session(client, api_base, api_key, session_id).await {
        Ok(()) => error,
        Err(cleanup_error) => format!(
            "{error}; additionally failed to stop Browser Use session {session_id}: {cleanup_error}"
        ),
    }
}

fn parse_browser_use_bool(name: &str, value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "{name} must be one of: true, false, 1, 0, yes, no, on, off"
        )),
    }
}

/// Build the Browser Use Cloud V4 create body from provider-specific environment variables.
/// Custom proxies are intentionally not exposed by this integration.
fn browser_use_create_body_from_lookup<F>(mut lookup: F) -> Result<Value, String>
where
    F: FnMut(&str) -> Option<String>,
{
    let mut body = serde_json::Map::new();

    if let Some(profile_id) = lookup("BROWSER_USE_PROFILE_ID") {
        let profile_id = profile_id.trim();
        if profile_id.is_empty() {
            return Err("BROWSER_USE_PROFILE_ID cannot be empty".to_string());
        }
        let profile_id = uuid::Uuid::parse_str(profile_id)
            .map_err(|_| "BROWSER_USE_PROFILE_ID must be a UUID".to_string())?;
        body.insert("profileId".to_string(), json!(profile_id.to_string()));
    }

    if let Some(proxy_country) = lookup("BROWSER_USE_PROXY_COUNTRY") {
        let proxy_country = proxy_country.trim().to_ascii_lowercase();
        match proxy_country.as_str() {
            "none" | "direct" => {
                body.insert("proxyCountryCode".to_string(), Value::Null);
            }
            country if country.len() == 2 && country.chars().all(|c| c.is_ascii_alphabetic()) => {
                body.insert("proxyCountryCode".to_string(), json!(country));
            }
            _ => {
                return Err(
                    "BROWSER_USE_PROXY_COUNTRY must be a two-letter country code, 'none', or 'direct'; Browser Use Cloud validates country availability"
                        .to_string(),
                );
            }
        }
    }

    if let Some(timeout) = lookup("BROWSER_USE_TIMEOUT_MINUTES") {
        let timeout = timeout.trim().parse::<u64>().map_err(|_| {
            "BROWSER_USE_TIMEOUT_MINUTES must be an integer from 1 to 240".to_string()
        })?;
        if !(1..=240).contains(&timeout) {
            return Err("BROWSER_USE_TIMEOUT_MINUTES must be an integer from 1 to 240".to_string());
        }
        body.insert("timeout".to_string(), json!(timeout));
    }

    let screen_width = lookup("BROWSER_USE_SCREEN_WIDTH");
    let screen_height = lookup("BROWSER_USE_SCREEN_HEIGHT");
    match (screen_width, screen_height) {
        (Some(width), Some(height)) => {
            let width = width.trim().parse::<u64>().map_err(|_| {
                "BROWSER_USE_SCREEN_WIDTH must be an integer from 320 to 6144".to_string()
            })?;
            let height = height.trim().parse::<u64>().map_err(|_| {
                "BROWSER_USE_SCREEN_HEIGHT must be an integer from 320 to 3456".to_string()
            })?;
            if !(320..=6144).contains(&width) {
                return Err(
                    "BROWSER_USE_SCREEN_WIDTH must be an integer from 320 to 6144".to_string(),
                );
            }
            if !(320..=3456).contains(&height) {
                return Err(
                    "BROWSER_USE_SCREEN_HEIGHT must be an integer from 320 to 3456".to_string(),
                );
            }
            body.insert("browserScreenWidth".to_string(), json!(width));
            body.insert("browserScreenHeight".to_string(), json!(height));
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(
                "BROWSER_USE_SCREEN_WIDTH and BROWSER_USE_SCREEN_HEIGHT must be set together"
                    .to_string(),
            );
        }
        (None, None) => {}
    }

    for (env_name, field_name) in [
        ("BROWSER_USE_ALLOW_RESIZING", "allowResizing"),
        ("BROWSER_USE_ENABLE_RECORDING", "enableRecording"),
    ] {
        if let Some(value) = lookup(env_name) {
            body.insert(
                field_name.to_string(),
                json!(parse_browser_use_bool(env_name, &value)?),
            );
        }
    }

    Ok(Value::Object(body))
}

fn browser_use_create_body_from_env() -> Result<Value, String> {
    browser_use_create_body_from_lookup(|name| env::var(name).ok())
}

async fn resolve_browser_use_ws_url(
    client: &reqwest::Client,
    cdp_url: &str,
) -> Result<String, String> {
    if cdp_url.starts_with("ws://") || cdp_url.starts_with("wss://") {
        return Ok(cdp_url.to_string());
    }

    let fallback_url = if let Some(rest) = cdp_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = cdp_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return Err(format!("Invalid Browser Use CDP URL: {cdp_url}"));
    };

    let discovered_url = match client
        .get(format!("{}/json/version", cdp_url.trim_end_matches('/')))
        .timeout(BROWSER_USE_CDP_TIMEOUT)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            response.json::<Value>().await.ok().and_then(|json| {
                json.get("webSocketDebuggerUrl")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
        }
        _ => None,
    };
    let ws_url = discovered_url.unwrap_or(fallback_url);

    if cdp_url.starts_with("https://") {
        return Ok(ws_url
            .strip_prefix("ws://")
            .map(|rest| format!("wss://{rest}"))
            .unwrap_or(ws_url));
    }

    Ok(ws_url)
}

async fn connect_browser_use_with_client(
    client: &reqwest::Client,
    api_base: &str,
    api_key: &str,
    create_body: &Value,
) -> Result<(String, Option<ProviderSession>), String> {
    let response = client
        .post(format!(
            "{}/api/v4/browsers",
            api_base.trim_end_matches('/')
        ))
        .header("X-Browser-Use-API-Key", api_key)
        .header(
            reqwest::header::USER_AGENT,
            format!("agent-browser/{}", env!("CARGO_PKG_VERSION")),
        )
        .header("Content-Type", "application/json")
        .json(create_body)
        .timeout(BROWSER_USE_API_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Browser Use request failed: {}", e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read Browser Use response: {}", e))?;

    if !status.is_success() {
        return Err(format!(
            "Browser Use API error ({}): {}",
            status.as_u16(),
            body
        ));
    }

    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid Browser Use response: {}", e))?;
    let session_id = json
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| "Browser Use response missing id".to_string())?
        .to_string();
    let cdp_url = match json.get("cdpUrl").and_then(Value::as_str) {
        Some(cdp_url) => cdp_url,
        None => {
            return Err(browser_use_error_with_cleanup(
                client,
                api_base,
                api_key,
                &session_id,
                "Browser Use response missing cdpUrl".to_string(),
            )
            .await);
        }
    };

    match resolve_browser_use_ws_url(client, cdp_url).await {
        Ok(ws_url) => Ok((
            ws_url,
            Some(ProviderSession {
                provider: "browser-use".to_string(),
                session_id,
            }),
        )),
        Err(error) => {
            Err(browser_use_error_with_cleanup(client, api_base, api_key, &session_id, error).await)
        }
    }
}

async fn connect_browser_use() -> Result<(String, Option<ProviderSession>), String> {
    let api_key = env::var("BROWSER_USE_API_KEY")
        .map_err(|_| "BROWSER_USE_API_KEY environment variable is not set")?;
    let create_body = browser_use_create_body_from_env()?;
    let client = reqwest::Client::new();

    connect_browser_use_with_client(&client, BROWSER_USE_API_BASE, &api_key, &create_body).await
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

// ============================================================================
// AgentCore Provider (AWS Bedrock AgentCore Browser)
// ============================================================================

mod agentcore {
    use super::*;

    /// AgentCore-specific session info for Live View URL
    pub struct AgentCoreSessionInfo {
        pub session_id: String,
        pub browser_identifier: String,
        pub region: String,
        pub live_view_url: String,
    }

    thread_local! {
        static AGENTCORE_INFO: std::cell::RefCell<Option<AgentCoreSessionInfo>> = const { std::cell::RefCell::new(None) };
        static AGENTCORE_WS_HEADERS: std::cell::RefCell<Option<Vec<(String, String)>>> = const { std::cell::RefCell::new(None) };
    }

    pub fn set_agentcore_info(info: AgentCoreSessionInfo) {
        AGENTCORE_INFO.with(|cell| *cell.borrow_mut() = Some(info));
    }

    pub fn get_agentcore_info() -> Option<AgentCoreSessionInfo> {
        AGENTCORE_INFO.with(|cell| {
            cell.borrow().as_ref().map(|i| AgentCoreSessionInfo {
                session_id: i.session_id.clone(),
                browser_identifier: i.browser_identifier.clone(),
                region: i.region.clone(),
                live_view_url: i.live_view_url.clone(),
            })
        })
    }

    pub fn set_agentcore_ws_headers(headers: Vec<(String, String)>) {
        AGENTCORE_WS_HEADERS.with(|cell| *cell.borrow_mut() = Some(headers));
    }

    pub fn take_agentcore_ws_headers() -> Option<Vec<(String, String)>> {
        AGENTCORE_WS_HEADERS.with(|cell| cell.borrow_mut().take())
    }

    pub async fn connect() -> Result<(String, Option<ProviderSession>), String> {
        let region = env::var("AGENTCORE_REGION")
            .or_else(|_| env::var("AWS_REGION"))
            .or_else(|_| env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());
        let browser_id =
            env::var("AGENTCORE_BROWSER_ID").unwrap_or_else(|_| "aws.browser.v1".to_string());
        let timeout_secs: u64 = env::var("AGENTCORE_SESSION_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);

        let host = format!("bedrock-agentcore.{}.amazonaws.com", region);
        let path = format!(
            "/browsers/{}/sessions/start",
            urlencoding::encode(&browser_id)
        );
        let url = format!("https://{}{}", host, path);

        // Generate a unique session name
        let session_name = format!("agent-browser-{}", &uuid::Uuid::new_v4().to_string()[..8]);

        let mut body_json = json!({
            "name": session_name,
            "sessionTimeoutSeconds": timeout_secs
        });
        if let Ok(profile_id) = env::var("AGENTCORE_PROFILE_ID") {
            if !profile_id.is_empty() {
                body_json.as_object_mut().unwrap().insert(
                    "profileConfiguration".to_string(),
                    json!({ "profileIdentifier": profile_id }),
                );
            }
        }
        let body = serde_json::to_string(&body_json)
            .map_err(|e| format!("Failed to serialize request body: {}", e))?;

        let signed_headers = sign_request("PUT", &url, &region, Some(&body)).await?;

        let client = reqwest::Client::new();
        let mut req = client.put(&url).body(body.clone());
        for (key, value) in &signed_headers {
            req = req.header(key.as_str(), value.as_str());
        }

        let response = req
            .send()
            .await
            .map_err(|e| format!("AgentCore request failed: {}", e))?;

        let status = response.status();
        let resp_body = response
            .text()
            .await
            .map_err(|e| format!("Failed to read AgentCore response: {}", e))?;

        if !status.is_success() {
            return Err(format!(
                "AgentCore API error ({}): {}",
                status.as_u16(),
                resp_body
            ));
        }

        let json: Value = serde_json::from_str(&resp_body)
            .map_err(|e| format!("Invalid AgentCore response: {}", e))?;

        let session_id = json
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "AgentCore response missing sessionId".to_string())?
            .to_string();

        let browser_identifier = json
            .get("browserIdentifier")
            .and_then(|v| v.as_str())
            .unwrap_or(&browser_id)
            .to_string();

        let live_view_url = format!(
            "https://{}.console.aws.amazon.com/bedrock-agentcore/browser/{}/session/{}#",
            region, browser_identifier, session_id
        );

        set_agentcore_info(AgentCoreSessionInfo {
            session_id: session_id.clone(),
            browser_identifier: browser_identifier.clone(),
            region: region.clone(),
            live_view_url: live_view_url.clone(),
        });

        eprintln!("Session: {}", session_id);
        eprintln!("Live View: {}", live_view_url);

        let ws_path = format!(
            "/browser-streams/{}/sessions/{}/automation",
            browser_identifier, session_id
        );
        let ws_url = format!("wss://{}{}", host, ws_path);

        let ws_headers = sign_request(
            "GET",
            &format!("https://{}{}", host, ws_path),
            &region,
            None,
        )
        .await?;
        set_agentcore_ws_headers(ws_headers);

        Ok((
            ws_url,
            Some(ProviderSession {
                provider: "agentcore".to_string(),
                session_id,
            }),
        ))
    }

    /// Get AWS credentials from environment variables or AWS CLI
    fn get_aws_credentials() -> Result<(String, String, Option<String>), String> {
        // First try environment variables
        if let (Ok(access_key), Ok(secret_key)) = (
            env::var("AWS_ACCESS_KEY_ID"),
            env::var("AWS_SECRET_ACCESS_KEY"),
        ) {
            return Ok((access_key, secret_key, env::var("AWS_SESSION_TOKEN").ok()));
        }

        // Fall back to AWS CLI
        let mut cmd = std::process::Command::new("aws");
        cmd.args(["configure", "export-credentials", "--format", "env"]);

        // Honor AWS_PROFILE
        if let Ok(profile) = env::var("AWS_PROFILE") {
            cmd.args(["--profile", &profile]);
        }

        let output = cmd.output()
            .map_err(|e| format!("Failed to run aws CLI: {}. Install AWS CLI or set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "AWS CLI failed: {}. Run 'aws sso login' or set credentials",
                stderr.trim()
            ));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut access_key = None;
        let mut secret_key = None;
        let mut session_token = None;

        for line in stdout.lines() {
            if let Some(val) = line.strip_prefix("export AWS_ACCESS_KEY_ID=") {
                access_key = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("export AWS_SECRET_ACCESS_KEY=") {
                secret_key = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("export AWS_SESSION_TOKEN=") {
                session_token = Some(val.to_string());
            }
        }

        match (access_key, secret_key) {
            (Some(ak), Some(sk)) => Ok((ak, sk, session_token)),
            _ => Err("Failed to parse credentials from AWS CLI output".to_string()),
        }
    }

    async fn sign_request(
        method: &str,
        url: &str,
        region: &str,
        body: Option<&str>,
    ) -> Result<Vec<(String, String)>, String> {
        use hmac::{Hmac, Mac};
        use sha2::{Digest, Sha256};

        // Get credentials from environment or AWS CLI
        let (access_key, secret_key, session_token) = get_aws_credentials()?;

        let parsed_url = url::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;
        let host = parsed_url.host_str().unwrap_or("");

        // Get current time
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        // Create canonical request
        let payload_hash = if let Some(b) = body {
            let mut hasher = Sha256::new();
            hasher.update(b.as_bytes());
            hex::encode(hasher.finalize())
        } else {
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string()
            // empty string hash
        };

        let canonical_uri = parsed_url.path();
        let canonical_querystring = parsed_url.query().unwrap_or("");

        let mut signed_headers = "content-type;host;x-amz-date".to_string();
        let mut canonical_headers = format!(
            "content-type:application/json\nhost:{}\nx-amz-date:{}\n",
            host, amz_date
        );

        if let Some(ref token) = session_token {
            signed_headers = "content-type;host;x-amz-date;x-amz-security-token".to_string();
            canonical_headers = format!(
                "content-type:application/json\nhost:{}\nx-amz-date:{}\nx-amz-security-token:{}\n",
                host, amz_date, token
            );
        }

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            canonical_uri,
            canonical_querystring,
            canonical_headers,
            signed_headers,
            payload_hash
        );

        // Create string to sign
        let algorithm = "AWS4-HMAC-SHA256";
        let credential_scope = format!("{}/{}/bedrock-agentcore/aws4_request", date_stamp, region);

        let mut hasher = Sha256::new();
        hasher.update(canonical_request.as_bytes());
        let canonical_request_hash = hex::encode(hasher.finalize());

        let string_to_sign = format!(
            "{}\n{}\n{}\n{}",
            algorithm, amz_date, credential_scope, canonical_request_hash
        );

        // Calculate signature
        type HmacSha256 = Hmac<Sha256>;

        let k_date = HmacSha256::new_from_slice(format!("AWS4{}", secret_key).as_bytes())
            .unwrap()
            .chain_update(date_stamp.as_bytes())
            .finalize()
            .into_bytes();

        let k_region = HmacSha256::new_from_slice(&k_date)
            .unwrap()
            .chain_update(region.as_bytes())
            .finalize()
            .into_bytes();

        let k_service = HmacSha256::new_from_slice(&k_region)
            .unwrap()
            .chain_update(b"bedrock-agentcore")
            .finalize()
            .into_bytes();

        let k_signing = HmacSha256::new_from_slice(&k_service)
            .unwrap()
            .chain_update(b"aws4_request")
            .finalize()
            .into_bytes();

        let signature = hex::encode(
            HmacSha256::new_from_slice(&k_signing)
                .unwrap()
                .chain_update(string_to_sign.as_bytes())
                .finalize()
                .into_bytes(),
        );

        // Build authorization header
        let authorization = format!(
            "{} Credential={}/{}, SignedHeaders={}, Signature={}",
            algorithm, access_key, credential_scope, signed_headers, signature
        );

        let mut headers = vec![
            ("host".to_string(), host.to_string()),
            ("content-type".to_string(), "application/json".to_string()),
            ("x-amz-date".to_string(), amz_date),
            ("authorization".to_string(), authorization),
        ];

        if let Some(token) = session_token {
            headers.push(("x-amz-security-token".to_string(), token));
        }

        Ok(headers)
    }

    pub async fn close_session(session_id: &str) -> Result<(), String> {
        let info = get_agentcore_info();
        let (region, browser_id) = match &info {
            Some(i) => (i.region.clone(), i.browser_identifier.clone()),
            None => {
                let region = env::var("AGENTCORE_REGION")
                    .or_else(|_| env::var("AWS_REGION"))
                    .or_else(|_| env::var("AWS_DEFAULT_REGION"))
                    .unwrap_or_else(|_| "us-east-1".to_string());
                let browser_id = env::var("AGENTCORE_BROWSER_ID")
                    .unwrap_or_else(|_| "aws.browser.v1".to_string());
                (region, browser_id)
            }
        };

        let host = format!("bedrock-agentcore.{}.amazonaws.com", region);
        let path = format!(
            "/browsers/{}/sessions/stop",
            urlencoding::encode(&browser_id)
        );
        let url = format!("https://{}{}", host, path);

        let body = serde_json::to_string(&json!({ "sessionId": session_id }))
            .map_err(|e| format!("Failed to serialize close request: {}", e))?;

        let signed_headers = sign_request("PUT", &url, &region, Some(&body)).await?;

        let client = reqwest::Client::new();
        let mut req = client.put(&url).body(body);
        for (key, value) in &signed_headers {
            req = req.header(key.as_str(), value.as_str());
        }

        let _ = req.send().await;
        Ok(())
    }
}

pub use agentcore::{get_agentcore_info, take_agentcore_ws_headers};

async fn connect_agentcore() -> Result<(String, Option<ProviderSession>), String> {
    agentcore::connect().await
}

async fn close_agentcore_session(session_id: &str) -> Result<(), String> {
    agentcore::close_session(session_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;

    async fn serve_mock_http(
        listener: tokio::net::TcpListener,
        responses: Vec<(u16, String)>,
    ) -> Vec<String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0_u8; 1024];
                let bytes_read = stream.read(&mut chunk).await.unwrap();
                if bytes_read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..bytes_read]);

                if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_end = header_end + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8_lossy(&request).to_string());
            let reason = match status {
                200 => "OK",
                201 => "Created",
                429 => "Too Many Requests",
                500 => "Internal Server Error",
                _ => "Response",
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        }
        requests
    }

    #[tokio::test]
    async fn test_browser_use_v4_connection_and_cleanup() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_mock_http(
            listener,
            vec![
                (
                    201,
                    format!(r#"{{"id":"browser-123","cdpUrl":"{api_base}/cdp"}}"#),
                ),
                (
                    200,
                    r#"{"webSocketDebuggerUrl":"wss://browser.example/devtools/browser/123"}"#
                        .to_string(),
                ),
                (200, "{}".to_string()),
            ],
        ));
        let client = reqwest::Client::new();

        let create_body = json!({
            "profileId": "00000000-0000-0000-0000-000000000123",
            "proxyCountryCode": "de",
            "timeout": 120,
            "browserScreenWidth": 1440,
            "browserScreenHeight": 900,
            "allowResizing": true,
            "enableRecording": true,
        });
        let (ws_url, session) =
            connect_browser_use_with_client(&client, &api_base, "test-key", &create_body)
                .await
                .unwrap();
        let session = session.unwrap();
        assert_eq!(ws_url, "wss://browser.example/devtools/browser/123");
        assert_eq!(session.provider, "browser-use");
        assert_eq!(session.session_id, "browser-123");

        stop_browser_use_session(&client, &api_base, "test-key", &session.session_id)
            .await
            .unwrap();
        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /api/v4/browsers HTTP/1.1"));
        assert!(requests[0]
            .to_ascii_lowercase()
            .contains("x-browser-use-api-key: test-key"));
        assert!(requests[0]
            .to_ascii_lowercase()
            .contains("user-agent: agent-browser/"));
        assert!(requests[0].contains(r#""profileId":"00000000-0000-0000-0000-000000000123""#));
        assert!(requests[0].contains(r#""proxyCountryCode":"de""#));
        assert!(requests[0].contains(r#""timeout":120"#));
        assert!(requests[0].contains(r#""browserScreenWidth":1440"#));
        assert!(requests[0].contains(r#""browserScreenHeight":900"#));
        assert!(requests[0].contains(r#""allowResizing":true"#));
        assert!(requests[0].contains(r#""enableRecording":true"#));
        assert!(!requests[0].contains("customProxy"));
        assert!(requests[1].starts_with("GET /cdp/json/version HTTP/1.1"));
        assert!(requests[2].starts_with("PATCH /api/v4/browsers/browser-123 HTTP/1.1"));
        assert!(requests[2].contains(r#"{"action":"stop"}"#));
    }

    #[tokio::test]
    async fn test_browser_use_stop_retries_transient_failure() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_mock_http(
            listener,
            vec![
                (500, r#"{"detail":"temporary"}"#.to_string()),
                (200, "{}".to_string()),
            ],
        ));

        stop_browser_use_session(
            &reqwest::Client::new(),
            &api_base,
            "test-key",
            "browser-123",
        )
        .await
        .unwrap();

        let requests = server.await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests
            .iter()
            .all(|request| request.starts_with("PATCH /api/v4/browsers/browser-123 HTTP/1.1")));
    }

    #[test]
    fn test_browser_use_create_body_exposes_supported_v4_options() {
        let values = std::collections::HashMap::from([
            (
                "BROWSER_USE_PROFILE_ID",
                "00000000-0000-0000-0000-000000000123",
            ),
            ("BROWSER_USE_PROXY_COUNTRY", "DE"),
            ("BROWSER_USE_TIMEOUT_MINUTES", "120"),
            ("BROWSER_USE_SCREEN_WIDTH", "1440"),
            ("BROWSER_USE_SCREEN_HEIGHT", "900"),
            ("BROWSER_USE_ALLOW_RESIZING", "true"),
            ("BROWSER_USE_ENABLE_RECORDING", "1"),
        ]);

        let body = browser_use_create_body_from_lookup(|name| {
            values.get(name).map(|value| (*value).to_string())
        })
        .unwrap();

        assert_eq!(
            body,
            json!({
                "profileId": "00000000-0000-0000-0000-000000000123",
                "proxyCountryCode": "de",
                "timeout": 120,
                "browserScreenWidth": 1440,
                "browserScreenHeight": 900,
                "allowResizing": true,
                "enableRecording": true,
            })
        );
        assert!(body.get("customProxy").is_none());
    }

    #[test]
    fn test_browser_use_create_body_supports_direct_connection() {
        let body = browser_use_create_body_from_lookup(|name| {
            (name == "BROWSER_USE_PROXY_COUNTRY").then(|| "direct".to_string())
        })
        .unwrap();

        assert_eq!(body, json!({ "proxyCountryCode": null }));
    }

    #[test]
    fn test_browser_use_create_body_validates_options_before_provisioning() {
        let error = browser_use_create_body_from_lookup(|name| {
            (name == "BROWSER_USE_TIMEOUT_MINUTES").then(|| "241".to_string())
        })
        .unwrap_err();
        assert_eq!(
            error,
            "BROWSER_USE_TIMEOUT_MINUTES must be an integer from 1 to 240"
        );

        let error = browser_use_create_body_from_lookup(|name| {
            (name == "BROWSER_USE_SCREEN_WIDTH").then(|| "1440".to_string())
        })
        .unwrap_err();
        assert_eq!(
            error,
            "BROWSER_USE_SCREEN_WIDTH and BROWSER_USE_SCREEN_HEIGHT must be set together"
        );

        let error = browser_use_create_body_from_lookup(|name| {
            (name == "BROWSER_USE_ENABLE_RECORDING").then(|| "sometimes".to_string())
        })
        .unwrap_err();
        assert!(error.starts_with("BROWSER_USE_ENABLE_RECORDING must be one of:"));

        let error = browser_use_create_body_from_lookup(|name| {
            (name == "BROWSER_USE_PROFILE_ID").then(|| "not-a-uuid".to_string())
        })
        .unwrap_err();
        assert_eq!(error, "BROWSER_USE_PROFILE_ID must be a UUID");
    }

    #[tokio::test]
    async fn test_browser_use_websocket_url_does_not_probe() {
        let client = reqwest::Client::new();
        let ws_url =
            resolve_browser_use_ws_url(&client, "wss://browser.example/devtools/browser/123")
                .await
                .unwrap();
        assert_eq!(ws_url, "wss://browser.example/devtools/browser/123");
    }

    #[tokio::test]
    async fn test_browser_use_cdp_probe_failure_falls_back_to_websocket_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cdp_url = format!(
            "http://{}/devtools/browser/123",
            listener.local_addr().unwrap()
        );
        drop(listener);

        let client = reqwest::Client::new();
        let ws_url = resolve_browser_use_ws_url(&client, &cdp_url).await.unwrap();
        assert_eq!(ws_url, cdp_url.replacen("http://", "ws://", 1));
    }

    #[tokio::test]
    async fn test_browser_use_missing_cdp_url_stops_created_browser() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(serve_mock_http(
            listener,
            vec![
                (201, r#"{"id":"browser-123"}"#.to_string()),
                (200, "{}".to_string()),
            ],
        ));

        let client = reqwest::Client::new();
        let error = connect_browser_use_with_client(&client, &api_base, "test-key", &json!({}))
            .await
            .unwrap_err();
        assert_eq!(error, "Browser Use response missing cdpUrl");

        let requests = server.await.unwrap();
        assert!(requests[0].starts_with("POST /api/v4/browsers HTTP/1.1"));
        assert!(requests[1].starts_with("PATCH /api/v4/browsers/browser-123 HTTP/1.1"));
        assert!(requests[1].contains(r#"{"action":"stop"}"#));
    }

    #[test]
    fn test_connect_provider_unknown() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PLUGINS"]);
        guard.remove("AGENT_BROWSER_PLUGINS");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(connect_provider("unknown-provider"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown provider"));
    }

    #[test]
    fn test_connect_provider_with_supplied_registry_does_not_fallback_to_env_plugins() {
        let guard = EnvGuard::new(&["AGENT_BROWSER_PLUGINS"]);
        guard.set(
            "AGENT_BROWSER_PLUGINS",
            r#"[{"name":"env-cloud","command":"should-not-run","capabilities":["browser.provider"]}]"#,
        );

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(connect_provider_with_plugins("env-cloud", &[]));

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown provider"));
    }

    #[test]
    fn test_agentcore_env_defaults() {
        // Test that default values are used when env vars not set
        std::env::remove_var("AGENTCORE_REGION");
        std::env::remove_var("AGENTCORE_BROWSER_ID");
        std::env::remove_var("AGENTCORE_SESSION_TIMEOUT");

        // These would be used in connect() - just verify they don't panic
        let region = std::env::var("AGENTCORE_REGION")
            .or_else(|_| std::env::var("AWS_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());
        assert_eq!(region, "us-east-1");

        let browser_id =
            std::env::var("AGENTCORE_BROWSER_ID").unwrap_or_else(|_| "aws.browser.v1".to_string());
        assert_eq!(browser_id, "aws.browser.v1");
    }

    #[test]
    fn test_agentcore_session_info_storage() {
        let info = agentcore::AgentCoreSessionInfo {
            session_id: "test-session".to_string(),
            browser_identifier: "aws.browser.v1".to_string(),
            region: "us-east-1".to_string(),
            live_view_url: "https://example.com".to_string(),
        };

        agentcore::set_agentcore_info(info);
        let retrieved = get_agentcore_info();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.session_id, "test-session");
        assert_eq!(retrieved.region, "us-east-1");
    }

    #[test]
    fn test_agentcore_ws_headers_storage() {
        let headers = vec![
            (
                "Authorization".to_string(),
                "AWS4-HMAC-SHA256...".to_string(),
            ),
            ("X-Amz-Date".to_string(), "20260304T180000Z".to_string()),
        ];

        agentcore::set_agentcore_ws_headers(headers);
        let taken = take_agentcore_ws_headers();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().len(), 2);

        // Should be None after take
        let taken_again = take_agentcore_ws_headers();
        assert!(taken_again.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn test_plugin_provider_cleanup_uses_supplied_registry() {
        use std::os::unix::fs::PermissionsExt;

        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let marker_path = dir.path().join("cleanup-request.json");
        let plugin_path = dir.path().join("mock-cleanup-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat > "$1"
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"data":{}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let session = ProviderSession {
            provider: "plugin:cloud-browser".to_string(),
            session_id: r#"{"sessionId":"s1"}"#.to_string(),
        };
        let plugins = vec![crate::plugins::PluginConfig {
            name: "cloud-browser".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            args: vec![marker_path.to_string_lossy().to_string()],
            capabilities: vec![crate::plugins::CAPABILITY_BROWSER_PROVIDER.to_string()],
            ..crate::plugins::PluginConfig::default()
        }];

        rt.block_on(close_provider_session_with_plugins(&session, &plugins))
            .unwrap();

        let request = std::fs::read_to_string(marker_path).unwrap();
        assert!(request.contains(r#""type":"browser.close""#));
        assert!(request.contains(r#""sessionId":"s1""#));
    }

    #[cfg(unix)]
    #[test]
    fn test_plugin_provider_falsey_headed_env_is_false() {
        use std::os::unix::fs::PermissionsExt;

        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_HEADED",
            "AGENT_BROWSER_ENGINE",
            "AGENT_BROWSER_SESSION",
        ]);
        guard.set("AGENT_BROWSER_HEADED", "false");
        guard.set("AGENT_BROWSER_ENGINE", "chrome");
        guard.set("AGENT_BROWSER_SESSION", "provider-test");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let request_path = dir.path().join("browser-launch-request.json");
        let plugin_path = dir.path().join("mock-provider-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat > "$1"
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"browser":{"cdpUrl":"ws://127.0.0.1:9222/devtools/browser/test"}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let plugins = vec![crate::plugins::PluginConfig {
            name: "cloud-browser".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            args: vec![request_path.to_string_lossy().to_string()],
            capabilities: vec![crate::plugins::CAPABILITY_BROWSER_PROVIDER.to_string()],
            ..crate::plugins::PluginConfig::default()
        }];

        rt.block_on(connect_provider_with_plugins("cloud-browser", &plugins))
            .unwrap();

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(request_path).unwrap()).unwrap();
        assert_eq!(request["request"]["launchOptions"]["headed"], false);
    }

    #[cfg(unix)]
    #[test]
    fn test_plugin_provider_receives_command_launch_options() {
        use std::os::unix::fs::PermissionsExt;

        let guard = EnvGuard::new(&[
            "AGENT_BROWSER_COLOR_SCHEME",
            "AGENT_BROWSER_ENGINE",
            "AGENT_BROWSER_HEADED",
            "AGENT_BROWSER_SESSION",
            "AGENT_BROWSER_USER_AGENT",
        ]);
        guard.set("AGENT_BROWSER_COLOR_SCHEME", "light");
        guard.set("AGENT_BROWSER_ENGINE", "chrome");
        guard.set("AGENT_BROWSER_HEADED", "false");
        guard.set("AGENT_BROWSER_SESSION", "provider-test");
        guard.set("AGENT_BROWSER_USER_AGENT", "env-agent");

        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let request_path = dir.path().join("browser-launch-request.json");
        let plugin_path = dir.path().join("mock-provider-plugin");
        std::fs::write(
            &plugin_path,
            r#"#!/bin/sh
cat > "$1"
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"browser":{"cdpUrl":"ws://127.0.0.1:9222/devtools/browser/test"}}'
"#,
        )
        .unwrap();
        let mut perms = std::fs::metadata(&plugin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&plugin_path, perms).unwrap();

        let plugins = vec![crate::plugins::PluginConfig {
            name: "cloud-browser".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            args: vec![request_path.to_string_lossy().to_string()],
            capabilities: vec![crate::plugins::CAPABILITY_BROWSER_PROVIDER.to_string()],
            ..crate::plugins::PluginConfig::default()
        }];

        rt.block_on(connect_provider_with_plugins_and_options(
            "cloud-browser",
            &plugins,
            Some(json!({
                "colorScheme": "dark",
                "engine": "lightpanda",
                "headed": true,
                "userAgent": "cli-agent"
            })),
        ))
        .unwrap();

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(request_path).unwrap()).unwrap();
        assert_eq!(request["request"]["launchOptions"]["colorScheme"], "dark");
        assert_eq!(request["request"]["launchOptions"]["engine"], "lightpanda");
        assert_eq!(request["request"]["launchOptions"]["headed"], true);
        assert_eq!(
            request["request"]["launchOptions"]["userAgent"],
            "cli-agent"
        );
    }
}
