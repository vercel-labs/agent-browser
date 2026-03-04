//! Browser provider connections for remote CDP sessions.
//!
//! Supports Browserbase, Browser Use, and Kernel providers. Each provider
//! returns a CDP WebSocket URL for connecting via BrowserManager.

use serde_json::{json, Value};
use std::env;

/// Provider session info for cleanup on failure.
#[derive(Debug)]
pub struct ProviderSession {
    pub provider: String,
    pub session_id: String,
}

/// Connects to the specified browser provider and returns a CDP WebSocket URL
/// along with session info for cleanup on failure.
pub async fn connect_provider(
    provider_name: &str,
) -> Result<(String, Option<ProviderSession>), String> {
    match provider_name.to_lowercase().as_str() {
        "browserbase" => connect_browserbase().await,
        "browser-use" | "browseruse" => connect_browser_use().await,
        "kernel" => connect_kernel().await,
        "agentcore" => connect_agentcore().await,
        _ => Err(format!(
            "Unknown provider '{}'. Supported: browserbase, browser-use, kernel, agentcore",
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
                    .delete(format!(
                        "https://api.browserbase.com/v1/sessions/{}",
                        session.session_id
                    ))
                    .header("X-BB-API-Key", &api_key)
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
}

async fn connect_browserbase() -> Result<(String, Option<ProviderSession>), String> {
    let api_key = env::var("BROWSERBASE_API_KEY")
        .map_err(|_| "BROWSERBASE_API_KEY environment variable is not set")?;
    let project_id = env::var("BROWSERBASE_PROJECT_ID")
        .map_err(|_| "BROWSERBASE_PROJECT_ID environment variable is not set")?;

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.browserbase.com/v1/sessions")
        .header("Content-Type", "application/json")
        .header("X-BB-API-Key", &api_key)
        .json(&json!({ "projectId": project_id }))
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

async fn connect_browser_use() -> Result<(String, Option<ProviderSession>), String> {
    let api_key = env::var("BROWSER_USE_API_KEY")
        .map_err(|_| "BROWSER_USE_API_KEY environment variable is not set")?;

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.browser-use.com/api/v2/browsers")
        .header("Content-Type", "application/json")
        .header("X-Browser-Use-API-Key", &api_key)
        .json(&json!({}))
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
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let ws_url = json
        .get("cdp_url")
        .or_else(|| json.get("cdpUrl"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| "Browser Use response missing cdp_url or cdpUrl".to_string())?;

    Ok((
        ws_url,
        Some(ProviderSession {
            provider: "browser-use".to_string(),
            session_id,
        }),
    ))
}

async fn connect_kernel() -> Result<(String, Option<ProviderSession>), String> {
    let api_key =
        env::var("KERNEL_API_KEY").map_err(|_| "KERNEL_API_KEY environment variable is not set")?;
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
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", api_key))
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
// Requires: cargo build --features agentcore
// ============================================================================

#[cfg(feature = "agentcore")]
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
        AGENTCORE_INFO.with(|cell| cell.borrow().as_ref().map(|i| AgentCoreSessionInfo {
            session_id: i.session_id.clone(),
            browser_identifier: i.browser_identifier.clone(),
            region: i.region.clone(),
            live_view_url: i.live_view_url.clone(),
        }))
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
        let browser_id = env::var("AGENTCORE_BROWSER_ID")
            .unwrap_or_else(|_| "aws.browser.v1".to_string());
        let timeout_secs: u64 = env::var("AGENTCORE_SESSION_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3600);

        let host = format!("bedrock-agentcore.{}.amazonaws.com", region);
        let path = format!("/browsers/{}/sessions/start", urlencoding::encode(&browser_id));
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
                    json!({ "profileIdentifier": profile_id })
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

        let response = req.send().await
            .map_err(|e| format!("AgentCore request failed: {}", e))?;

        let status = response.status();
        let resp_body = response.text().await
            .map_err(|e| format!("Failed to read AgentCore response: {}", e))?;

        if !status.is_success() {
            return Err(format!("AgentCore API error ({}): {}", status.as_u16(), resp_body));
        }

        let json: Value = serde_json::from_str(&resp_body)
            .map_err(|e| format!("Invalid AgentCore response: {}", e))?;

        let session_id = json.get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "AgentCore response missing sessionId".to_string())?
            .to_string();

        let browser_identifier = json.get("browserIdentifier")
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

        let ws_path = format!("/browser-streams/{}/sessions/{}/automation", browser_identifier, session_id);
        let ws_url = format!("wss://{}{}", host, ws_path);

        let ws_headers = sign_request("GET", &format!("https://{}{}", host, ws_path), &region, None).await?;
        set_agentcore_ws_headers(ws_headers);

        Ok((
            ws_url,
            Some(ProviderSession {
                provider: "agentcore".to_string(),
                session_id,
            }),
        ))
    }

    async fn sign_request(
        method: &str,
        url: &str,
        region: &str,
        body: Option<&str>,
    ) -> Result<Vec<(String, String)>, String> {
        use hmac::{Hmac, Mac};
        use sha2::{Sha256, Digest};

        // Get credentials from environment
        let access_key = env::var("AWS_ACCESS_KEY_ID")
            .map_err(|_| "AWS_ACCESS_KEY_ID not set")?;
        let secret_key = env::var("AWS_SECRET_ACCESS_KEY")
            .map_err(|_| "AWS_SECRET_ACCESS_KEY not set")?;
        let session_token = env::var("AWS_SESSION_TOKEN").ok();

        let parsed_url = url::Url::parse(url)
            .map_err(|e| format!("Invalid URL: {}", e))?;
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
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string() // empty string hash
        };

        let canonical_uri = parsed_url.path();
        let canonical_querystring = parsed_url.query().unwrap_or("");

        let mut signed_headers = "content-type;host;x-amz-date".to_string();
        let mut canonical_headers = format!(
            "content-type:application/json\nhost:{}\nx-amz-date:{}\n",
            host, amz_date
        );

        if session_token.is_some() {
            signed_headers = "content-type;host;x-amz-date;x-amz-security-token".to_string();
            canonical_headers = format!(
                "content-type:application/json\nhost:{}\nx-amz-date:{}\nx-amz-security-token:{}\n",
                host, amz_date, session_token.as_ref().unwrap()
            );
        }

        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method, canonical_uri, canonical_querystring,
            canonical_headers, signed_headers, payload_hash
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
                .into_bytes()
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
        let path = format!("/browsers/{}/sessions/stop", urlencoding::encode(&browser_id));
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

#[cfg(feature = "agentcore")]
pub use agentcore::{get_agentcore_info, take_agentcore_ws_headers};

#[cfg(feature = "agentcore")]
async fn connect_agentcore() -> Result<(String, Option<ProviderSession>), String> {
    agentcore::connect().await
}

#[cfg(not(feature = "agentcore"))]
async fn connect_agentcore() -> Result<(String, Option<ProviderSession>), String> {
    Err("AgentCore provider requires the 'agentcore' feature. Rebuild with: cargo build --features agentcore".to_string())
}

#[cfg(feature = "agentcore")]
async fn close_agentcore_session(session_id: &str) -> Result<(), String> {
    agentcore::close_session(session_id).await
}

#[cfg(not(feature = "agentcore"))]
async fn close_agentcore_session(_session_id: &str) -> Result<(), String> {
    Ok(())
}

// Stub functions when agentcore feature is disabled
#[cfg(not(feature = "agentcore"))]
pub fn get_agentcore_info() -> Option<()> { None }

#[cfg(not(feature = "agentcore"))]
pub fn take_agentcore_ws_headers() -> Option<Vec<(String, String)>> { None }


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect_provider_unknown() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(connect_provider("unknown-provider"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown provider"));
    }

    #[test]
    fn test_connect_provider_agentcore_without_feature() {
        // Without agentcore feature, should return helpful error
        #[cfg(not(feature = "agentcore"))]
        {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let result = rt.block_on(connect_provider("agentcore"));
            assert!(result.is_err());
            assert!(result.unwrap_err().contains("agentcore"));
        }
    }

    #[cfg(feature = "agentcore")]
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

        let browser_id = std::env::var("AGENTCORE_BROWSER_ID")
            .unwrap_or_else(|_| "aws.browser.v1".to_string());
        assert_eq!(browser_id, "aws.browser.v1");
    }

    #[cfg(feature = "agentcore")]
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

    #[cfg(feature = "agentcore")]
    #[test]
    fn test_agentcore_ws_headers_storage() {
        let headers = vec![
            ("Authorization".to_string(), "AWS4-HMAC-SHA256...".to_string()),
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
}
