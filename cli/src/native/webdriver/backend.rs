use serde_json::Value;

/// Browser commands backed by a WebDriver session.
pub struct WebDriverBackend {
    client: super::client::WebDriverClient,
}

impl WebDriverBackend {
    pub fn new(client: super::client::WebDriverClient) -> Self {
        Self { client }
    }

    pub async fn navigate(&self, url: &str) -> Result<(), String> {
        self.client.navigate(url).await
    }

    pub async fn get_url(&self) -> Result<String, String> {
        self.client.get_url().await
    }

    pub async fn get_title(&self) -> Result<String, String> {
        self.client.get_title().await
    }

    pub async fn get_content(&self) -> Result<String, String> {
        self.client.get_page_source().await
    }

    pub async fn evaluate(&self, script: &str) -> Result<Value, String> {
        self.client.execute_script(script, vec![]).await
    }

    pub async fn screenshot(&self) -> Result<String, String> {
        self.client.screenshot().await
    }

    pub async fn click(&self, selector: &str) -> Result<(), String> {
        let element_id = self.client.find_element("css selector", selector).await?;
        self.client.click_element(&element_id).await
    }

    pub async fn fill(&self, selector: &str, value: &str) -> Result<(), String> {
        let element_id = self.client.find_element("css selector", selector).await?;
        self.client.clear_element(&element_id).await?;
        self.client.send_keys(&element_id, value).await
    }

    pub async fn close(&mut self) -> Result<(), String> {
        self.client.delete_session().await
    }

    pub async fn back(&self) -> Result<(), String> {
        self.client.back().await
    }

    pub async fn forward(&self) -> Result<(), String> {
        self.client.forward().await
    }

    pub async fn reload(&self) -> Result<(), String> {
        self.client.refresh().await
    }

    pub async fn get_cookies(&self) -> Result<Value, String> {
        self.client.get_cookies().await
    }
}

/// CDP-backed backend constants for unsupported actions on WebDriver
pub const WEBDRIVER_UNSUPPORTED_ACTIONS: &[&str] = &[
    // axe-core frame injection currently relies on CDP execution contexts.
    "a11y",
    "screencast_start",
    "screencast_stop",
    "trace_start",
    "trace_stop",
    "profiler_start",
    "profiler_stop",
    "route",
    "unroute",
    "expose",
    "addscript",
    "addinitscript",
    "network",
    "har_start",
    "har_stop",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unsupported_actions() {
        assert!(WEBDRIVER_UNSUPPORTED_ACTIONS.contains(&"a11y"));
        assert!(WEBDRIVER_UNSUPPORTED_ACTIONS.contains(&"screencast_start"));
        assert!(WEBDRIVER_UNSUPPORTED_ACTIONS.contains(&"trace_start"));
        assert!(!WEBDRIVER_UNSUPPORTED_ACTIONS.contains(&"navigate"));
    }
}
