use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug)]
struct WebDriverError {
    code: Option<String>,
    message: String,
}

impl WebDriverError {
    fn transport(message: String) -> Self {
        Self {
            code: None,
            message,
        }
    }
}

impl From<WebDriverError> for String {
    fn from(error: WebDriverError) -> Self {
        error.message
    }
}

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
            .map_err(|error| {
                if error.code.as_deref() == Some("no such element") {
                    format!("No element found by {} '{}'", using, value)
                } else {
                    error.message
                }
            })?;

        let element_value = response.get("value").ok_or("No element in response")?;
        element_id_from_value(element_value, using, value)
    }

    pub async fn click_element(&self, element_id: &str) -> Result<(), String> {
        let sid = self.session_id()?.to_string();
        self.post(
            &format!("/session/{}/element/{}/click", sid, element_id),
            &json!({}),
        )
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

    async fn get(&self, path: &str) -> Result<Value, WebDriverError> {
        http_request("GET", &format!("{}{}", self.base_url, path), None).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, WebDriverError> {
        http_request("POST", &format!("{}{}", self.base_url, path), Some(body)).await
    }

    async fn delete(&self, path: &str) -> Result<Value, WebDriverError> {
        http_request("DELETE", &format!("{}{}", self.base_url, path), None).await
    }
}

/// Extract the element id from a WebDriver find-element `value` payload.
///
/// A genuine locator miss arrives as a WebDriver error payload
/// ("no such element"), not as a malformed response. It is translated to the
/// anchored locator-miss shape the rest of the CLI produces, so it keeps the
/// selector detail and receives the AI-friendly guidance that
/// `to_ai_friendly_error` reserves for locator misses. A payload with
/// neither an error nor an element id is genuinely malformed and keeps the
/// protocol-shaped message.
fn element_id_from_value(
    element_value: &Value,
    using: &str,
    value: &str,
) -> Result<String, String> {
    if element_value
        .get("error")
        .and_then(|e| e.as_str())
        .is_some_and(|e| e == "no such element")
    {
        return Err(format!("No element found by {} '{}'", using, value));
    }

    element_value
        .get("element-6066-11e4-a52e-4f735466cecf")
        .or_else(|| element_value.get("ELEMENT"))
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or("No element ID in response".to_string())
}

async fn http_request(
    method: &str,
    url: &str,
    body: Option<&Value>,
) -> Result<Value, WebDriverError> {
    // WebDriver is a loopback service. Do not route it through environment
    // proxies, and let the HTTP client handle response framing and chunking.
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| WebDriverError::transport(format!("WebDriver client error: {}", e)))?;
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|e| WebDriverError::transport(format!("Invalid HTTP method: {}", e)))?;
    let mut request = client.request(method, url);
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request
        .send()
        .await
        .map_err(|e| WebDriverError::transport(format!("WebDriver request failed: {}", e)))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| WebDriverError::transport(format!("WebDriver response failed: {}", e)))?;
    let value = if text.trim().is_empty() && status.is_success() {
        json!({})
    } else {
        serde_json::from_str::<Value>(&text).map_err(|e| {
            WebDriverError::transport(format!(
                "Invalid WebDriver JSON response (HTTP {}): {} (body: {})",
                status.as_u16(),
                e,
                text.chars().take(100).collect::<String>()
            ))
        })?
    };
    if !status.is_success() {
        if let Some(code) = value.pointer("/value/error").and_then(Value::as_str) {
            let message = value
                .pointer("/value/message")
                .and_then(Value::as_str)
                .unwrap_or(code);
            return Err(WebDriverError {
                code: Some(code.to_string()),
                message: format!("WebDriver error ({}): {}", code, message),
            });
        }
        return Err(WebDriverError::transport(format!(
            "WebDriver HTTP {}: {}",
            status.as_u16(),
            text.chars().take(200).collect::<String>()
        )));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve_response(
        response: String,
        keep_open: bool,
    ) -> (u16, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            stream.read(&mut request).await.unwrap();
            stream.write_all(response.as_bytes()).await.unwrap();
            // A response's framing, rather than EOF, ends the HTTP message.
            if keep_open {
                let _ = stream.read(&mut request).await;
            }
        });
        (port, task)
    }

    #[tokio::test]
    async fn webdriver_reads_chunked_json_with_extensions_and_trailers() {
        let parts = [r#"{"value":""#, "cafe", r#"中文"}"#];
        let mut response = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_string();
        for part in parts {
            response.push_str(&format!("{:x};part=yes\r\n{}\r\n", part.len(), part));
        }
        response.push_str("0\r\nX-Finished: yes\r\n\r\n");
        let (port, server) = serve_response(response, false).await;
        let client = WebDriverClient::new_with_session(port, "test".into());
        let result = tokio::time::timeout(Duration::from_secs(2), client.get_title()).await;
        server.abort();
        assert_eq!(
            result
                .expect("chunked response must finish without EOF")
                .unwrap(),
            "cafe中文"
        );
    }

    #[tokio::test]
    async fn webdriver_propagates_command_errors_and_preserves_locator_misses() {
        for (code, message, find) in [
            ("invalid session id", "The session was closed", false),
            ("no such element", "No match", true),
        ] {
            let body = json!({"value": {"error": code, "message": message}}).to_string();
            let response = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let (port, server) = serve_response(response, false).await;
            let client = WebDriverClient::new_with_session(port, "test".into());
            let result = tokio::time::timeout(Duration::from_secs(2), async {
                if find {
                    client
                        .find_element("css selector", ".missing")
                        .await
                        .map(|_| ())
                } else {
                    client.navigate("https://example.com").await
                }
            })
            .await;
            server.abort();
            let error = result
                .expect("framed error response must finish")
                .unwrap_err();
            if find {
                assert_eq!(error, "No element found by css selector '.missing'");
            } else {
                assert!(error.contains(message), "{error}");
            }
        }
    }

    #[tokio::test]
    async fn webdriver_reads_a_framed_response_before_the_connection_closes() {
        let body = json!({ "value": "ready" }).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (port, server) = serve_response(response, true).await;
        let client = WebDriverClient::new_with_session(port, "test".into());
        let result = tokio::time::timeout(Duration::from_secs(2), client.get_title()).await;
        server.abort();
        assert_eq!(
            result.expect("Content-Length ends the response").unwrap(),
            "ready"
        );
    }

    #[tokio::test]
    async fn webdriver_rejects_empty_http_errors() {
        let (port, server) = serve_response(
            "HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n".into(),
            false,
        )
        .await;
        let client = WebDriverClient::new_with_session(port, "test".into());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client.navigate("https://example.com"),
        )
        .await;
        server.abort();
        assert!(result
            .expect("empty response is framed")
            .unwrap_err()
            .contains("503"));
    }

    #[tokio::test]
    async fn webdriver_preserves_error_fields_in_successful_script_results() {
        let value = json!({ "error": "application data", "message": "a normal result" });
        let body = json!({ "value": value }).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        let (port, server) = serve_response(response, false).await;
        let client = WebDriverClient::new_with_session(port, "test".into());
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client.execute_script("return window.result", vec![]),
        )
        .await;
        server.abort();
        assert_eq!(result.expect("script response is framed").unwrap(), value);
    }

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
    /// "no such element" error payload; it must translate to the anchored
    /// locator-miss shape (selector included) so `to_ai_friendly_error`
    /// appends its guidance, exactly as the CDP engine's misses do.
    #[test]
    fn test_find_element_miss_translates_to_locator_miss() {
        let payload = json!({
            "error": "no such element",
            "message": "An element could not be located on the page using the given search parameters.",
        });
        let err = element_id_from_value(&payload, "css selector", ".missing")
            .expect_err("an error payload is a miss, not an element");
        assert_eq!(err, "No element found by css selector '.missing'");
    }

    #[test]
    fn test_find_element_id_extracted_from_w3c_payload() {
        let payload = json!({ "element-6066-11e4-a52e-4f735466cecf": "abc123" });
        assert_eq!(
            element_id_from_value(&payload, "css selector", "#x").unwrap(),
            "abc123"
        );
    }

    /// A payload with neither an error nor an element id is genuinely
    /// malformed; it keeps the protocol-shaped message, which
    /// `to_ai_friendly_error` deliberately passes through unchanged.
    #[test]
    fn test_find_element_malformed_payload_keeps_protocol_message() {
        let payload = json!({ "unexpected": true });
        let err = element_id_from_value(&payload, "css selector", "#x")
            .expect_err("no id and no error is malformed");
        assert_eq!(err, "No element ID in response");
    }
}
