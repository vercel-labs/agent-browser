//! Browser provider connections for remote CDP sessions.
//!
//! Supports AgentCore, Browserbase, Browserless, Browser Use, and Kernel providers.
//! Each provider returns a CDP WebSocket URL for connecting via BrowserManager.

use serde_json::{json, Value};
use std::env;
use std::time::{Duration, Instant};

const BROWSERBASE_LIVE_VIEW_TIMEOUT: Duration = Duration::from_secs(5);
const BROWSERBASE_LIVE_VIEW_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Provider-owned cleanup data retained for connection failures and shutdown.
#[derive(Debug, Clone)]
pub enum ProviderCleanup {
    Browserbase {
        session_id: String,
    },
    Browserless {
        stop_url: String,
    },
    BrowserUse {
        session_id: String,
    },
    Kernel {
        session_id: String,
    },
    AgentCore {
        session_id: String,
        region: String,
        browser_identifier: String,
    },
    Plugin {
        provider: String,
        data: Value,
    },
}

#[derive(Debug)]
enum PostConnectMetadata {
    Browserbase {
        session_id: String,
        retry_after: Option<Instant>,
    },
}

/// Provider state retained for the lifetime of an active browser connection.
#[derive(Debug)]
pub struct ActiveProvider {
    pub name: String,
    pub metadata: Option<Value>,
    cleanup: Option<ProviderCleanup>,
    post_connect_metadata: Option<PostConnectMetadata>,
}

impl ActiveProvider {
    pub fn new(name: impl Into<String>, metadata: Option<Value>) -> Self {
        Self {
            name: name.into(),
            metadata,
            cleanup: None,
            post_connect_metadata: None,
        }
    }

    pub fn with_cleanup(mut self, cleanup: ProviderCleanup) -> Self {
        self.cleanup = Some(cleanup);
        self
    }

    /// Best-effort metadata enrichment that must run only after CDP event
    /// handlers have been installed. Provider failures never fail browsing.
    pub async fn enrich_metadata_after_connect(&mut self) {
        let session_id = match &self.post_connect_metadata {
            Some(PostConnectMetadata::Browserbase {
                session_id,
                retry_after,
            }) if retry_after.is_none_or(|deadline| Instant::now() >= deadline) => {
                session_id.clone()
            }
            _ => return,
        };

        match browserbase_live_view_metadata(&session_id).await {
            Some(metadata) => {
                self.metadata = Some(metadata);
                self.post_connect_metadata = None;
            }
            None => {
                if let Some(PostConnectMetadata::Browserbase { retry_after, .. }) =
                    self.post_connect_metadata.as_mut()
                {
                    *retry_after = Some(Instant::now() + BROWSERBASE_LIVE_VIEW_RETRY_DELAY);
                }
            }
        }
    }

    pub async fn close(self, plugins: &[crate::plugins::PluginConfig]) {
        if let Some(cleanup) = self.cleanup {
            close_provider_cleanup_with_plugins(&cleanup, plugins).await;
        }
    }
}

#[derive(Debug)]
pub struct ProviderConnection {
    pub provider: String,
    pub ws_url: String,
    pub ws_headers: Option<Vec<(String, String)>>,
    pub cleanup: Option<ProviderCleanup>,
    /// If true, the WebSocket IS the page session (no Target.* commands).
    pub direct_page: bool,
    pub metadata: Option<Value>,
    post_connect_metadata: Option<PostConnectMetadata>,
}

impl ProviderConnection {
    pub fn into_active(self) -> ActiveProvider {
        ActiveProvider {
            name: self.provider,
            metadata: self.metadata,
            cleanup: self.cleanup,
            post_connect_metadata: self.post_connect_metadata,
        }
    }

    pub async fn close(self, plugins: &[crate::plugins::PluginConfig]) {
        self.into_active().close(plugins).await;
    }
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
        "browserbase" => connect_browserbase().await,
        "browserless" => {
            let (url, cleanup) = connect_browserless().await?;
            Ok(ProviderConnection {
                provider: "browserless".to_string(),
                ws_url: url,
                ws_headers: None,
                cleanup,
                direct_page: false,
                metadata: None,
                post_connect_metadata: None,
            })
        }
        "browser-use" | "browseruse" => {
            let (url, cleanup) = connect_browser_use().await?;
            Ok(ProviderConnection {
                provider: "browser-use".to_string(),
                ws_url: url,
                ws_headers: None,
                cleanup,
                direct_page: false,
                metadata: None,
                post_connect_metadata: None,
            })
        }
        "kernel" => {
            let (url, cleanup) = connect_kernel().await?;
            Ok(ProviderConnection {
                provider: "kernel".to_string(),
                ws_url: url,
                ws_headers: None,
                cleanup,
                direct_page: false,
                metadata: None,
                post_connect_metadata: None,
            })
        }
        "agentcore" => connect_agentcore().await,
        _ => {
            connect_plugin_provider_with_plugins_and_options(provider_name, plugins, launch_options)
                .await
        }
    }
}

/// Close provider-owned resources with the plugin registry that created them.
pub async fn close_provider_cleanup_with_plugins(
    cleanup: &ProviderCleanup,
    plugins: &[crate::plugins::PluginConfig],
) {
    let client = reqwest::Client::new();
    match cleanup {
        ProviderCleanup::Browserbase { session_id } => {
            if let Ok(api_key) = env::var("BROWSERBASE_API_KEY") {
                let _ = client
                    .post(format!(
                        "https://api.browserbase.com/v1/sessions/{session_id}"
                    ))
                    .header("Content-Type", "application/json")
                    .header("X-BB-API-Key", &api_key)
                    .json(&serde_json::json!({ "status": "REQUEST_RELEASE" }))
                    .send()
                    .await;
            }
        }
        ProviderCleanup::Browserless { stop_url } => {
            let _ = client.delete(stop_url).send().await;
        }
        ProviderCleanup::BrowserUse { session_id } => {
            if let Ok(api_key) = env::var("BROWSER_USE_API_KEY") {
                let _ = client
                    .patch(format!(
                        "https://api.browser-use.com/api/v2/browsers/{session_id}"
                    ))
                    .header("X-Browser-Use-API-Key", &api_key)
                    .header("Content-Type", "application/json")
                    .json(&json!({ "action": "stop" }))
                    .send()
                    .await;
            }
        }
        ProviderCleanup::Kernel { session_id } => {
            if let Ok(api_key) = env::var("KERNEL_API_KEY") {
                let endpoint = env::var("KERNEL_ENDPOINT")
                    .unwrap_or_else(|_| "https://api.onkernel.com".to_string());
                let _ = client
                    .delete(format!(
                        "{}/browsers/{}",
                        endpoint.trim_end_matches('/'),
                        session_id
                    ))
                    .header("Authorization", format!("Bearer {}", api_key))
                    .send()
                    .await;
            }
        }
        ProviderCleanup::AgentCore {
            session_id,
            region,
            browser_identifier,
        } => {
            let _ = close_agentcore_session(session_id, region, browser_identifier).await;
        }
        ProviderCleanup::Plugin { provider, data } => {
            let _ = crate::plugins::close_browser_provider_with_plugins(
                provider,
                plugins,
                data.clone(),
            )
            .await;
        }
    }
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
    let cleanup = browser.cleanup.map(|data| ProviderCleanup::Plugin {
        provider: provider_name.to_string(),
        data,
    });
    Ok(ProviderConnection {
        provider: provider_name.to_string(),
        ws_url: browser.cdp_url,
        ws_headers: None,
        cleanup,
        direct_page: browser.direct_page,
        metadata: browser.metadata,
        post_connect_metadata: None,
    })
}

fn env_var_is_truthy(name: &str) -> bool {
    match env::var(name) {
        Ok(val) => !matches!(val.to_ascii_lowercase().as_str(), "0" | "false" | "no" | ""),
        Err(_) => false,
    }
}

async fn connect_browserbase() -> Result<ProviderConnection, String> {
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

    let (session_id, ws_url) = parse_browserbase_session(&json)?;
    // Browserbase requires connecting to connectUrl promptly after session
    // create (sessions time out if unused). Live-view URLs come from a separate
    // GET /v1/sessions/{id}/debug call after CDP connect succeeds.
    Ok(ProviderConnection {
        provider: "browserbase".to_string(),
        ws_url,
        ws_headers: None,
        cleanup: Some(ProviderCleanup::Browserbase {
            session_id: session_id.clone(),
        }),
        direct_page: false,
        metadata: Some(json!({ "sessionId": session_id.clone() })),
        post_connect_metadata: Some(PostConnectMetadata::Browserbase {
            session_id,
            retry_after: None,
        }),
    })
}

/// Best-effort Browserbase live-view metadata from `GET /v1/sessions/{id}/debug`.
///
/// Returns only the session-scoped `debuggerUrl` / `debuggerFullscreenUrl` fields
/// (plus `sessionId`). Omits `wsUrl` and per-page URLs from the SessionLiveUrls
/// response because those are additional capability-bearing surfaces.
pub async fn browserbase_live_view_metadata(session_id: &str) -> Option<Value> {
    let api_key = env::var("BROWSERBASE_API_KEY").ok()?;
    let client = reqwest::Client::new();
    fetch_browserbase_debug_metadata(&client, &api_key, session_id)
        .await
        .ok()
}

fn parse_browserbase_session(json: &Value) -> Result<(String, String), String> {
    let session_id = json
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Browserbase response missing id".to_string())?;
    let connect_url = json
        .get("connectUrl")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Browserbase response missing connectUrl".to_string())?;
    Ok((session_id.to_string(), connect_url.to_string()))
}

async fn fetch_browserbase_debug_metadata(
    client: &reqwest::Client,
    api_key: &str,
    session_id: &str,
) -> Result<Value, String> {
    let response = client
        .get(format!(
            "https://api.browserbase.com/v1/sessions/{}/debug",
            urlencoding::encode(session_id)
        ))
        .header("x-bb-api-key", api_key)
        .timeout(BROWSERBASE_LIVE_VIEW_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("Browserbase live-view request failed: {}", e))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read Browserbase live-view response: {}", e))?;
    if !status.is_success() {
        return Err(format!(
            "Browserbase live-view API error ({}): {}",
            status.as_u16(),
            body
        ));
    }
    let json: Value = serde_json::from_str(&body)
        .map_err(|e| format!("Invalid Browserbase live-view response: {}", e))?;
    parse_browserbase_debug_metadata(session_id, &json)
}

fn parse_browserbase_debug_metadata(session_id: &str, json: &Value) -> Result<Value, String> {
    let debugger_url = json
        .get("debuggerUrl")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Browserbase live-view response missing debuggerUrl".to_string())?;
    let debugger_fullscreen_url = json
        .get("debuggerFullscreenUrl")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            "Browserbase live-view response missing debuggerFullscreenUrl".to_string()
        })?;
    Ok(json!({
        "sessionId": session_id,
        "debuggerUrl": debugger_url,
        "debuggerFullscreenUrl": debugger_fullscreen_url,
    }))
}

async fn connect_browserless() -> Result<(String, Option<ProviderCleanup>), String> {
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

    Ok((connect_url, Some(ProviderCleanup::Browserless { stop_url })))
}

async fn connect_browser_use() -> Result<(String, Option<ProviderCleanup>), String> {
    let api_key = env::var("BROWSER_USE_API_KEY")
        .map_err(|_| "BROWSER_USE_API_KEY environment variable is not set")?;

    let ws_url = format!("wss://connect.browser-use.com?apiKey={}", api_key);

    Ok((ws_url, None))
}

async fn connect_kernel() -> Result<(String, Option<ProviderCleanup>), String> {
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

    Ok((ws_url, Some(ProviderCleanup::Kernel { session_id })))
}

// ============================================================================
// AgentCore Provider (AWS Bedrock AgentCore Browser)
// ============================================================================

mod agentcore {
    use super::*;

    pub async fn connect() -> Result<ProviderConnection, String> {
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

        Ok(ProviderConnection {
            provider: "agentcore".to_string(),
            ws_url,
            ws_headers: Some(ws_headers),
            cleanup: Some(ProviderCleanup::AgentCore {
                session_id: session_id.clone(),
                region: region.clone(),
                browser_identifier: browser_identifier.clone(),
            }),
            direct_page: false,
            metadata: Some(json!({
                "sessionId": session_id,
                "browserIdentifier": browser_identifier,
                "region": region,
                "liveViewUrl": live_view_url,
            })),
            post_connect_metadata: None,
        })
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

    pub async fn close_session(
        session_id: &str,
        region: &str,
        browser_id: &str,
    ) -> Result<(), String> {
        let host = format!("bedrock-agentcore.{}.amazonaws.com", region);
        let path = format!(
            "/browsers/{}/sessions/stop",
            urlencoding::encode(browser_id)
        );
        let url = format!("https://{}{}", host, path);

        let body = serde_json::to_string(&json!({ "sessionId": session_id }))
            .map_err(|e| format!("Failed to serialize close request: {}", e))?;

        let signed_headers = sign_request("PUT", &url, region, Some(&body)).await?;

        let client = reqwest::Client::new();
        let mut req = client.put(&url).body(body);
        for (key, value) in &signed_headers {
            req = req.header(key.as_str(), value.as_str());
        }

        let _ = req.send().await;
        Ok(())
    }
}

async fn connect_agentcore() -> Result<ProviderConnection, String> {
    agentcore::connect().await
}

async fn close_agentcore_session(
    session_id: &str,
    region: &str,
    browser_identifier: &str,
) -> Result<(), String> {
    agentcore::close_session(session_id, region, browser_identifier).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::EnvGuard;

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
    fn test_parse_browserbase_session_requires_id_and_connect_url() {
        let parsed = parse_browserbase_session(&json!({
            "id": "sess_123",
            "connectUrl": "wss://connect.browserbase.com/session"
        }))
        .unwrap();
        assert_eq!(parsed.0, "sess_123");
        assert_eq!(parsed.1, "wss://connect.browserbase.com/session");

        assert!(parse_browserbase_session(&json!({ "connectUrl": "wss://example.com" })).is_err());
        assert!(parse_browserbase_session(&json!({ "id": "sess_123" })).is_err());
    }

    #[test]
    fn test_parse_browserbase_debug_metadata_keeps_only_safe_live_view_fields() {
        let metadata = parse_browserbase_debug_metadata(
            "sess_123",
            &json!({
                "debuggerUrl": "https://debugger.browserbase.com/session",
                "debuggerFullscreenUrl": "https://debugger.browserbase.com/session/fullscreen",
                "wsUrl": "wss://api.browserbase.com/private-capability",
                "pages": []
            }),
        )
        .unwrap();

        assert_eq!(metadata["sessionId"], "sess_123");
        assert_eq!(
            metadata["debuggerFullscreenUrl"],
            "https://debugger.browserbase.com/session/fullscreen"
        );
        assert!(metadata.get("wsUrl").is_none());
        assert!(metadata.get("pages").is_none());
    }

    #[test]
    fn test_parse_browserbase_debug_metadata_requires_both_debugger_urls() {
        let result = parse_browserbase_debug_metadata(
            "sess_123",
            &json!({ "debuggerUrl": "https://debugger.browserbase.com/session" }),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_browserbase_debug_metadata_is_non_fatal() {
        let metadata = parse_browserbase_debug_metadata("sess_123", &json!({})).ok();
        assert!(metadata.is_none());
    }

    #[tokio::test]
    async fn test_browserbase_metadata_retry_cooldown_preserves_pending_enrichment() {
        let mut provider = ActiveProvider {
            name: "browserbase".to_string(),
            metadata: Some(json!({ "sessionId": "sess_123" })),
            cleanup: None,
            post_connect_metadata: Some(PostConnectMetadata::Browserbase {
                session_id: "sess_123".to_string(),
                retry_after: Some(Instant::now() + Duration::from_secs(60)),
            }),
        };

        provider.enrich_metadata_after_connect().await;

        assert_eq!(provider.metadata.unwrap()["sessionId"], "sess_123");
        assert!(provider.post_connect_metadata.is_some());
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
    fn test_agentcore_connection_carries_headers_metadata_and_typed_cleanup() {
        let connection = ProviderConnection {
            provider: "agentcore".to_string(),
            ws_url: "wss://example.com/automation".to_string(),
            ws_headers: Some(vec![(
                "Authorization".to_string(),
                "AWS4-HMAC-SHA256...".to_string(),
            )]),
            cleanup: Some(ProviderCleanup::AgentCore {
                session_id: "test-session".to_string(),
                region: "us-east-1".to_string(),
                browser_identifier: "aws.browser.v1".to_string(),
            }),
            direct_page: false,
            metadata: Some(json!({
                "sessionId": "test-session",
                "liveViewUrl": "https://example.com"
            })),
            post_connect_metadata: None,
        };

        assert_eq!(connection.ws_headers.as_ref().unwrap().len(), 1);
        let active = connection.into_active();
        assert_eq!(active.name, "agentcore");
        assert_eq!(active.metadata.unwrap()["sessionId"], "test-session");
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

        let cleanup = ProviderCleanup::Plugin {
            provider: "cloud-browser".to_string(),
            data: json!({ "sessionId": "s1" }),
        };
        let plugins = vec![crate::plugins::PluginConfig {
            name: "cloud-browser".to_string(),
            command: plugin_path.to_string_lossy().to_string(),
            args: vec![marker_path.to_string_lossy().to_string()],
            capabilities: vec![crate::plugins::CAPABILITY_BROWSER_PROVIDER.to_string()],
            ..crate::plugins::PluginConfig::default()
        }];

        rt.block_on(close_provider_cleanup_with_plugins(&cleanup, &plugins));

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
printf '%s' '{"protocol":"agent-browser.plugin.v1","success":true,"browser":{"cdpUrl":"ws://127.0.0.1:9222/devtools/browser/test","metadata":{"dashboard":{"url":"https://provider.example/session"}}}}'
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

        let connection = rt
            .block_on(connect_provider_with_plugins("cloud-browser", &plugins))
            .unwrap();

        let request: Value =
            serde_json::from_str(&std::fs::read_to_string(request_path).unwrap()).unwrap();
        assert_eq!(request["request"]["launchOptions"]["headed"], false);
        assert_eq!(connection.provider, "cloud-browser");
        assert_eq!(
            connection.metadata.unwrap()["dashboard"]["url"],
            "https://provider.example/session"
        );
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
