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
    /// Preserve the CDP partition identity when saving and restoring state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub partition_key: Option<Value>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn partitioned_cookie(site: &str, cross_site: bool) -> Value {
        json!({
            "name": "session",
            "value": "abc123",
            "domain": ".widget.example",
            "path": "/",
            "secure": true,
            "httpOnly": true,
            "sameSite": "None",
            "partitionKey": {
                "topLevelSite": site,
                "hasCrossSiteAncestor": cross_site
            }
        })
    }

    #[test]
    fn partition_key_survives_cookie_serialization() {
        for cross_site in [false, true] {
            let cdp_cookie = partitioned_cookie("https://shop.example", cross_site);
            let cookie: Cookie = serde_json::from_value(cdp_cookie.clone()).unwrap();
            let saved = serde_json::to_string(&cookie).unwrap();
            let restored: Cookie = serde_json::from_str(&saved).unwrap();
            let params = serde_json::to_value(restored).unwrap();

            assert_eq!(params["partitionKey"], cdp_cookie["partitionKey"]);
            assert_eq!(params["sameSite"], "None");
            assert_eq!(params["secure"], true);
            assert_eq!(params["httpOnly"], true);
        }
    }

    #[test]
    fn same_name_cookies_keep_their_distinct_partitions() {
        let cookies = json!([
            partitioned_cookie("https://shop.example", true),
            partitioned_cookie("https://news.example", true),
            partitioned_cookie("https://shop.example", false)
        ]);
        let parsed: Vec<Cookie> = serde_json::from_value(cookies.clone()).unwrap();
        let saved = serde_json::to_string(&parsed).unwrap();
        let restored: Vec<Cookie> = serde_json::from_str(&saved).unwrap();
        let params = serde_json::to_value(restored).unwrap();

        for index in 0..3 {
            assert_eq!(
                params[index]["partitionKey"],
                cookies[index]["partitionKey"]
            );
        }
        assert_ne!(params[0]["partitionKey"], params[1]["partitionKey"]);
        assert_ne!(params[0]["partitionKey"], params[2]["partitionKey"]);
    }

    #[test]
    fn unpartitioned_cookie_states_remain_compatible() {
        let old_state = json!({
            "name": "session",
            "value": "old",
            "domain": ".example.com",
            "path": "/"
        });
        let cookie: Cookie = serde_json::from_value(old_state).unwrap();
        let params = serde_json::to_value(cookie).unwrap();

        assert_eq!(params["value"], "old");
        assert_eq!(params["path"], "/");
        assert!(params.get("partitionKey").is_none());
    }
}
