use serde_json::{json, Value};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use super::client::WebDriverClient;

const APPIUM_DEFAULT_PORT: u16 = 4723;
const APPIUM_STARTUP_TIMEOUT_SECS: u64 = 30;

pub struct AppiumManager {
    pub client: WebDriverClient,
    appium_process: Option<Child>,
    pub device_udid: Option<String>,
}

impl AppiumManager {
    pub async fn connect_or_launch(device_udid: Option<&str>) -> Result<Self, String> {
        let port = APPIUM_DEFAULT_PORT;
        let client = WebDriverClient::new(port);

        // Check if Appium is already running
        if is_appium_running(port).await {
            return Ok(Self {
                client,
                appium_process: None,
                device_udid: device_udid.map(String::from),
            });
        }

        // Try to launch Appium
        let appium_process = launch_appium(port)?;

        // Wait for Appium to be ready
        wait_for_appium(port, APPIUM_STARTUP_TIMEOUT_SECS).await?;

        Ok(Self {
            client,
            appium_process: Some(appium_process),
            device_udid: device_udid.map(String::from),
        })
    }

    pub async fn create_ios_session(
        &mut self,
        device_name: Option<&str>,
        platform_version: Option<&str>,
    ) -> Result<Value, String> {
        let mut caps = json!({
            "platformName": "iOS",
            "automationName": "XCUITest",
            "browserName": "Safari",
            "noReset": true,
        });

        if let Some(name) = device_name {
            caps["deviceName"] = json!(name);
        } else {
            caps["deviceName"] = json!("iPhone");
        }

        if let Some(ver) = platform_version {
            caps["platformVersion"] = json!(ver);
        }

        if let Some(ref udid) = self.device_udid {
            caps["udid"] = json!(udid);
        }

        self.client.create_session(caps).await
    }

    pub async fn tap(&self, x: f64, y: f64) -> Result<(), String> {
        let sid = self
            .client
            .session_id_pub()
            .ok_or("No active session")?
            .to_string();
        let actions = json!({
            "actions": [{
                "type": "pointer",
                "id": "finger1",
                "parameters": { "pointerType": "touch" },
                "actions": [
                    { "type": "pointerMove", "duration": 0, "x": x as i64, "y": y as i64 },
                    { "type": "pointerDown", "button": 0 },
                    { "type": "pause", "duration": 100 },
                    { "type": "pointerUp", "button": 0 },
                ]
            }]
        });
        self.client.execute_actions(&sid, &actions).await
    }

    pub async fn swipe(
        &self,
        start_x: f64,
        start_y: f64,
        end_x: f64,
        end_y: f64,
        duration_ms: u64,
    ) -> Result<(), String> {
        let sid = self
            .client
            .session_id_pub()
            .ok_or("No active session")?
            .to_string();
        let actions = json!({
            "actions": [{
                "type": "pointer",
                "id": "finger1",
                "parameters": { "pointerType": "touch" },
                "actions": [
                    { "type": "pointerMove", "duration": 0, "x": start_x as i64, "y": start_y as i64 },
                    { "type": "pointerDown", "button": 0 },
                    { "type": "pointerMove", "duration": duration_ms, "x": end_x as i64, "y": end_y as i64 },
                    { "type": "pointerUp", "button": 0 },
                ]
            }]
        });
        self.client.execute_actions(&sid, &actions).await
    }

    pub async fn close(&mut self) -> Result<(), String> {
        let _ = self.client.delete_session().await;
        if let Some(ref mut child) = self.appium_process {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for AppiumManager {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.appium_process {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn is_appium_running(port: u16) -> bool {
    let addr = format!("127.0.0.1:{}", port);
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false)
}

fn launch_appium(port: u16) -> Result<Child, String> {
    // Try npx appium first, then direct appium
    let result = Command::new("npx")
        .args(["appium", "--relaxed-security", "--port", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();

    match result {
        Ok(child) => Ok(child),
        Err(_) => Command::new("appium")
            .args(["--relaxed-security", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                format!(
                    "Failed to launch Appium. Install it with: npm install -g appium. Error: {}",
                    e
                )
            }),
    }
}

async fn wait_for_appium(port: u16, timeout_secs: u64) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if tokio::time::Instant::now() > deadline {
            return Err("Timeout waiting for Appium to start".to_string());
        }
        if is_appium_running(port).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_appium_constants() {
        assert_eq!(APPIUM_DEFAULT_PORT, 4723);
        assert_eq!(APPIUM_STARTUP_TIMEOUT_SECS, 30);
    }
}
