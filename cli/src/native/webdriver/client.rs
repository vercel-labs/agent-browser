use serde_json::{json, Value};
use std::time::Duration;

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
            .await?;

        let element_value = response.get("value").ok_or("No element in response")?;

        element_value
            .get("element-6066-11e4-a52e-4f735466cecf")
            .or_else(|| element_value.get("ELEMENT"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or("No element ID in response".to_string())
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

    async fn get(&self, path: &str) -> Result<Value, String> {
        http_request("GET", &format!("{}{}", self.base_url, path), None).await
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        http_request("POST", &format!("{}{}", self.base_url, path), Some(body)).await
    }

    async fn delete(&self, path: &str) -> Result<Value, String> {
        http_request("DELETE", &format!("{}{}", self.base_url, path), None).await
    }
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
}

async fn http_request(method: &str, url: &str, body: Option<&Value>) -> Result<Value, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed.port().unwrap_or(80);
    let path = parsed.path();

    let addr = format!("{}:{}", host, port);
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .map_err(|_| format!("Connection timeout: {}", addr))?
    .map_err(|e| format!("Connection failed: {}", e))?;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let body_str = body
        .map(|b| serde_json::to_string(b).unwrap_or_default())
        .unwrap_or_default();

    let request = if body.is_some() {
        format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            method, path, addr, body_str.len(), body_str
        )
    } else {
        format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            method, path, addr
        )
    };

    let mut stream = stream;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("Write failed: {}", e))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|e| format!("Read failed: {}", e))?;

    let response_str = String::from_utf8_lossy(&response);
    let body_part = response_str.split("\r\n\r\n").nth(1).unwrap_or("").trim();

    // Handle chunked encoding
    let json_body = if body_part.contains('\n')
        && body_part
            .chars()
            .next()
            .map(|c| c.is_ascii_hexdigit())
            .unwrap_or(false)
    {
        // Chunked: skip chunk size lines
        body_part
            .lines()
            .filter(|l| !l.chars().all(|c| c.is_ascii_hexdigit() || c == '\r'))
            .collect::<Vec<&str>>()
            .join("")
    } else {
        body_part.to_string()
    };

    if json_body.is_empty() {
        return Ok(json!({}));
    }

    serde_json::from_str(&json_body).map_err(|e| {
        format!(
            "Invalid JSON response: {} (body: {})",
            e,
            json_body.chars().take(100).collect::<String>()
        )
    })
}
