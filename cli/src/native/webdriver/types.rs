use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionRequest {
    pub capabilities: Capabilities,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub always_match: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResponse {
    pub value: SessionValue,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionValue {
    pub session_id: String,
    pub capabilities: Value,
}

#[derive(Debug, Deserialize)]
pub struct WebDriverResponse {
    pub value: Value,
}

#[derive(Debug, Deserialize)]
pub struct WebDriverError {
    pub error: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementResponse {
    pub value: ElementValue,
}

#[derive(Debug, Deserialize)]
pub struct ElementValue {
    #[serde(rename = "element-6066-11e4-a52e-4f735466cecf")]
    pub element_id: Option<String>,
    #[serde(rename = "ELEMENT")]
    pub element_legacy: Option<String>,
}

impl ElementValue {
    pub fn id(&self) -> Option<&str> {
        self.element_id
            .as_deref()
            .or(self.element_legacy.as_deref())
    }
}

#[derive(Debug, Serialize)]
pub struct FindElementRequest {
    pub using: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct ExecuteScriptRequest {
    pub script: String,
    pub args: Vec<Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieRequest {
    pub cookie: CookieData,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CookieData {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
}
