use reqwest::Method;
use serde_json::{json, Value};
use std::time::Duration;

/// The key a W3C WebDriver payload uses for an element reference.
pub const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

pub struct WebDriverClient {
    base_url: String,
    session_id: Option<String>,
}

impl WebDriverClient {
    pub fn new(port: u16) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", port),
            session_id: None,
        }
    }

    pub async fn create_session(&mut self, capabilities: Value) -> Result<Value, String> {
        let body = json!({
            "capabilities": {
                "alwaysMatch": capabilities,
            }
        });

        let response = self.post("/session", &body).await?;

        let session_id = response
            .get("value")
            .and_then(|v| v.get("sessionId"))
            .and_then(|v| v.as_str())
            .ok_or("No sessionId in response")?
            .to_string();

        self.session_id = Some(session_id);
        Ok(response)
    }

    pub async fn delete_session(&mut self) -> Result<(), String> {
        if let Some(ref sid) = self.session_id.clone() {
            let _ = self.delete(&format!("/session/{}", sid)).await;
            self.session_id = None;
        }
        Ok(())
    }

    pub async fn navigate(&self, url: &str) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(&format!("/session/{}/url", sid), &json!({ "url": url }))
            .await?;
        Ok(())
    }

    pub async fn get_window_handles(&self) -> Result<Vec<String>, String> {
        let sid = self.session_id()?;
        let response = self
            .get(&format!("/session/{}/window/handles", sid))
            .await?;
        serde_json::from_value(response["value"].clone())
            .map_err(|e| format!("Invalid window handles response: {}", e))
    }

    pub async fn get_url(&self) -> Result<String, String> {
        let sid = self.session_id()?.to_string();
        let response = self.get(&format!("/session/{}/url", sid)).await?;
        Ok(response
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn get_title(&self) -> Result<String, String> {
        let sid = self.session_id()?.to_string();
        let response = self.get(&format!("/session/{}/title", sid)).await?;
        Ok(response
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn find_element(&self, using: &str, value: &str) -> Result<String, String> {
        let sid = self.session_id()?.to_string();
        let response = self
            .post(
                &format!("/session/{}/element", sid),
                &json!({ "using": using, "value": value }),
            )
            .await
            .map_err(|error| locator_error(error, using, value))?;

        let element_value = response.get("value").ok_or("No element in response")?;
        element_id_from_value(element_value)
    }

    /// Switches to the child frame `element` of the current frame, or to the top-level frame for `None`.
    pub async fn switch_to_frame(&self, element: Option<&str>) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        let id = element.map_or(Value::Null, |e| json!({ ELEMENT_KEY: e }));
        self.post(&format!("/session/{}/frame", sid), &json!({ "id": id }))
            .await?;
        Ok(())
    }

    pub async fn switch_to_parent_frame(&self) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(&format!("/session/{}/frame/parent", sid), &json!({}))
            .await?;
        Ok(())
    }

    pub async fn send_keys(&self, element_id: &str, text: &str) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(
            &format!("/session/{}/element/{}/value", sid, element_id),
            &json!({ "text": text }),
        )
        .await?;
        Ok(())
    }

    pub async fn clear_element(&self, element_id: &str) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(
            &format!("/session/{}/element/{}/clear", sid, element_id),
            &json!({}),
        )
        .await?;
        Ok(())
    }

    pub async fn execute_script(&self, script: &str, args: Vec<Value>) -> Result<Value, String> {
        let sid = self.session_id()?.to_string();
        let response = self
            .post(
                &format!("/session/{}/execute/sync", sid),
                &json!({ "script": script, "args": args }),
            )
            .await?;
        Ok(response.get("value").cloned().unwrap_or(Value::Null))
    }

    pub async fn screenshot(&self) -> Result<String, String> {
        let sid = self.session_id()?.to_string();
        let response = self.get(&format!("/session/{}/screenshot", sid)).await?;
        Ok(response
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn get_cookies(&self) -> Result<Value, String> {
        let sid = self.session_id()?.to_string();
        let response = self.get(&format!("/session/{}/cookie", sid)).await?;
        Ok(response.get("value").cloned().unwrap_or(Value::Null))
    }

    pub async fn get_page_source(&self) -> Result<String, String> {
        let sid = self.session_id()?.to_string();
        let response = self.get(&format!("/session/{}/source", sid)).await?;
        Ok(response
            .get("value")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    pub async fn back(&self) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(&format!("/session/{}/back", sid), &json!({}))
            .await?;
        Ok(())
    }

    pub async fn forward(&self) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(&format!("/session/{}/forward", sid), &json!({}))
            .await?;
        Ok(())
    }

    pub async fn refresh(&self) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(&format!("/session/{}/refresh", sid), &json!({}))
            .await?;
        Ok(())
    }

    pub fn session_id_pub(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn new_with_session(port: u16, session_id: String) -> Self {
        Self {
            base_url: format!("http://127.0.0.1:{}", port),
            session_id: Some(session_id),
        }
    }

    pub async fn execute_actions(&self, session_id: &str, actions: &Value) -> Result<(), String> {
        self.post(&format!("/session/{}/actions", session_id), actions)
            .await?;
        Ok(())
    }

    fn session_id(&self) -> Result<&str, String> {
        self.session_id
            .as_deref()
            .ok_or("No active WebDriver session".to_string())
    }

    async fn get(&self, path: &str) -> Result<Value, String> {
        http_request(Method::GET, &format!("{}{}", self.base_url, path), None).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        http_request(
            Method::POST,
            &format!("{}{}", self.base_url, path),
            Some(body),
        )
        .await
    }

    async fn delete(&self, path: &str) -> Result<Value, String> {
        http_request(Method::DELETE, &format!("{}{}", self.base_url, path), None).await
    }
}

/// Translates a find-element failure for the CLI.
///
/// A genuine locator miss arrives as a "no such element" WebDriver error. It
/// becomes the anchored locator-miss shape the rest of the CLI produces, so it
/// keeps the selector detail and receives the AI-friendly guidance that
/// `to_ai_friendly_error` reserves for locator misses. Other errors pass through.
fn locator_error(error: String, using: &str, value: &str) -> String {
    if error.starts_with("no such element") {
        format!("No element found by {} '{}'", using, value)
    } else {
        error
    }
}

/// Extracts the element id from a WebDriver find-element `value` payload. A
/// payload without an element id is malformed and keeps the protocol-shaped message.
fn element_id_from_value(element_value: &Value) -> Result<String, String> {
    element_value
        .get(ELEMENT_KEY)
        .or_else(|| element_value.get("ELEMENT"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or("No element ID in response".to_string())
}

async fn http_request(method: Method, url: &str, body: Option<&Value>) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let mut request = client.request(method, url);
    if let Some(body) = body {
        request = request.json(body);
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;
    let status = resp.status();
    let json_body = resp
        .text()
        .await
        .map_err(|e| format!("Read failed: {}", e))?;

    if json_body.trim().is_empty() {
        if !status.is_success() {
            return Err(format!("WebDriver request failed with HTTP {}", status));
        }
        return Ok(json!({}));
    }

    let response: Value = serde_json::from_str(&json_body).map_err(|e| {
        if !status.is_success() {
            return format!(
                "WebDriver request failed with HTTP {}: {}",
                status,
                json_body.chars().take(100).collect::<String>()
            );
        }
        format!(
            "Invalid JSON response: {} (body: {})",
            e,
            json_body.chars().take(100).collect::<String>()
        )
    })?;
    if !status.is_success() {
        if let Some(error) = response["value"]["error"].as_str() {
            let message = response["value"]["message"].as_str().unwrap_or_default();
            return Err(format!("{}: {}", error, message));
        }
        return Err(format!("WebDriver request failed with HTTP {}", status));
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_new() {
        let client = WebDriverClient::new(4444);
        assert_eq!(client.base_url, "http://127.0.0.1:4444");
        assert!(client.session_id.is_none());
    }

    #[test]
    fn test_session_id_none() {
        let client = WebDriverClient::new(4444);
        let result = client.session_id();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No active WebDriver session"));
    }

    #[test]
    fn test_client_custom_port() {
        let client = WebDriverClient::new(9515);
        assert_eq!(client.base_url, "http://127.0.0.1:9515");
    }

    /// In the WebDriver engine a genuine locator miss surfaces as a
    /// "no such element" error; it must translate to the anchored
    /// locator-miss shape (selector included) so `to_ai_friendly_error`
    /// appends its guidance, exactly as the CDP engine's misses do.
    #[test]
    fn test_find_element_miss_translates_to_locator_miss() {
        let err = locator_error(
            "no such element: An element could not be located on the page using the given search parameters.".to_string(),
            "css selector",
            ".missing",
        );
        assert_eq!(err, "No element found by css selector '.missing'");
    }

    #[test]
    fn test_find_element_id_extracted_from_w3c_payload() {
        let payload = json!({ "element-6066-11e4-a52e-4f735466cecf": "abc123" });
        assert_eq!(element_id_from_value(&payload).unwrap(), "abc123");
    }

    /// A payload with neither an error nor an element id is genuinely
    /// malformed; it keeps the protocol-shaped message, which
    /// `to_ai_friendly_error` deliberately passes through unchanged.
    #[test]
    fn test_find_element_malformed_payload_keeps_protocol_message() {
        let payload = json!({ "unexpected": true });
        let err = element_id_from_value(&payload).expect_err("no id is malformed");
        assert_eq!(err, "No element ID in response");
    }

    /// A 200 OK evaluation returning `{error: ...}` is a JS value, while a non-2xx
    /// WebDriver error payload is an error.
    #[tokio::test]
    async fn test_http_200_error_property_is_value_while_non_2xx_is_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            for (status, body) in [
                ("200 OK", r#"{"value":{"error":"boom"}}"#),
                (
                    "404 Not Found",
                    r#"{"value":{"error":"no such element","message":"Missing"}}"#,
                ),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = [0_u8; 1024];
                let _ = stream.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(resp.as_bytes()).await.unwrap();
            }
        });

        let client = WebDriverClient::new_with_session(port, "s1".to_string());
        let val = client
            .execute_script("return ({error: 'boom'})", vec![])
            .await
            .expect("200 OK with error field should return the JS object");
        assert_eq!(val, json!({ "error": "boom" }));

        let err = client
            .find_element("css selector", ".missing")
            .await
            .expect_err("404 WebDriver error should fail");
        assert_eq!(err, "No element found by css selector '.missing'");
        server.await.unwrap();
    }
}
