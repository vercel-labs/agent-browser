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

    let mut cookies: Vec<Cookie> = result
        .get("cookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Chrome may return empty sameSite; default to "Lax" to match Playwright behavior
    fill_default_same_site(&mut cookies);

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

    let mut cookies: Vec<Cookie> = result
        .get("cookies")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Chrome may return empty sameSite; default to "Lax" to match Playwright behavior
    fill_default_same_site(&mut cookies);

    Ok(cookies)
}

/// When Chrome CDP returns cookies with an empty `sameSite` field, default it
/// to `"Lax"` so that saved state files always include the attribute. This
/// matches Playwright's serialization behavior and prevents session cookies
/// from being dropped during cross-origin redirects when the state is restored.
fn fill_default_same_site(cookies: &mut [Cookie]) {
    for cookie in cookies.iter_mut() {
        if cookie.same_site.is_none() {
            cookie.same_site = Some("Lax".to_string());
        }
    }
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
