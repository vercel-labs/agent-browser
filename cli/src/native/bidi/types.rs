use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

/// A BiDi `BytesValue`; a bare string is accepted on input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BytesValue {
    Tagged(TaggedBytes),
    Bare(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum TaggedBytes {
    String(String),
    Base64(String),
}

impl Default for BytesValue {
    fn default() -> Self {
        Self::Bare(String::new())
    }
}

impl BytesValue {
    pub fn string(text: impl Into<String>) -> Self {
        Self::Tagged(TaggedBytes::String(text.into()))
    }

    /// Returns the text, decoding base64 (lossily) when needed.
    pub fn text(&self) -> String {
        match self {
            Self::Bare(s) | Self::Tagged(TaggedBytes::String(s)) => s.clone(),
            Self::Tagged(TaggedBytes::Base64(s)) => base64::engine::general_purpose::STANDARD
                .decode(s)
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default(),
        }
    }

    pub fn is_base64(&self) -> bool {
        matches!(self, Self::Tagged(TaggedBytes::Base64(_)))
    }

    /// Returns the raw payload, still base64 encoded when it is.
    pub fn raw(&self) -> &str {
        match self {
            Self::Bare(s) | Self::Tagged(TaggedBytes::String(s) | TaggedBytes::Base64(s)) => s,
        }
    }
}

/// A BiDi storage cookie, used for both `storage.getCookies` results and `storage.setCookie` input.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Cookie {
    pub name: String,
    pub value: BytesValue,
    pub domain: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    pub http_only: bool,
    pub secure: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same_site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiry: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct GetCookies {
    pub cookies: Vec<Cookie>,
}

/// A value passed to `script.callFunction`; values already carrying `type` or `sharedId` pass through.
#[derive(Serialize)]
#[serde(untagged)]
pub enum LocalValue {
    Passthrough(Value),
    Plain(PlainValue),
}

#[derive(Serialize)]
#[serde(tag = "type", content = "value", rename_all = "lowercase")]
pub enum PlainValue {
    String(String),
    Number(Number),
    Boolean(bool),
    Null,
    Array(Vec<LocalValue>),
    Object(Vec<(String, LocalValue)>),
}

impl From<&Value> for LocalValue {
    fn from(v: &Value) -> Self {
        match v {
            Value::Object(o) if o.contains_key("type") || o.contains_key("sharedId") => {
                Self::Passthrough(v.clone())
            }
            Value::String(s) => Self::Plain(PlainValue::String(s.clone())),
            Value::Number(n) => Self::Plain(PlainValue::Number(n.clone())),
            Value::Bool(b) => Self::Plain(PlainValue::Boolean(*b)),
            Value::Null => Self::Plain(PlainValue::Null),
            Value::Array(a) => Self::Plain(PlainValue::Array(a.iter().map(Self::from).collect())),
            Value::Object(o) => Self::Plain(PlainValue::Object(
                o.iter().map(|(k, x)| (k.clone(), Self::from(x))).collect(),
            )),
        }
    }
}

/// A value returned by `script.*`; unknown kinds decode to `Other`.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum RemoteValue {
    Node {
        #[serde(rename = "sharedId", default)]
        shared_id: Value,
    },
    Window {
        value: WindowValue,
    },
    #[serde(alias = "set")]
    Array {
        #[serde(default)]
        value: Vec<RemoteValue>,
    },
    #[serde(alias = "map")]
    Object {
        #[serde(default)]
        value: Vec<(RemoteKey, RemoteValue)>,
    },
    #[serde(alias = "boolean", alias = "bigint", alias = "date", alias = "number")]
    String {
        #[serde(default)]
        value: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
pub struct WindowValue {
    pub context: Value,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum RemoteKey {
    Text(String),
    Remote(Box<RemoteValue>),
}

impl From<RemoteValue> for Value {
    /// Nodes become `{sharedId}` and windows their context id.
    fn from(v: RemoteValue) -> Self {
        match v {
            RemoteValue::Node { shared_id } => serde_json::json!({ "sharedId": shared_id }),
            RemoteValue::Window { value } => value.context,
            RemoteValue::Array { value } => value.into_iter().map(Value::from).collect(),
            RemoteValue::Object { value } => value
                .into_iter()
                .filter_map(|(k, v)| {
                    let key = match k {
                        RemoteKey::Text(s) => s,
                        RemoteKey::Remote(r) => Value::from(*r).as_str()?.to_string(),
                    };
                    Some((key, Value::from(v)))
                })
                .collect(),
            RemoteValue::String { value } => value,
            RemoteValue::Other => Value::Null,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Header {
    pub name: String,
    pub value: BytesValue,
}

impl Header {
    pub fn string(name: String, value: String) -> Self {
        Self {
            name,
            value: BytesValue::string(value),
        }
    }
}

/// The `request` member of `network.*` events.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RequestData {
    pub request: String,
    pub url: String,
    pub method: Option<String>,
    pub headers: Vec<Header>,
    pub body_size: Option<i64>,
    pub initiator_type: Option<String>,
    pub destination: Option<String>,
}

/// The `response` member of `network.responseStarted` and `network.responseCompleted`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ResponseData {
    pub url: String,
    pub status: Option<i64>,
    pub status_text: String,
    pub mime_type: String,
    pub protocol: String,
    pub headers: Vec<Header>,
    pub body_size: Option<i64>,
    pub content: ResponseContent,
}

#[derive(Debug, Default, Deserialize)]
pub struct ResponseContent {
    pub size: Option<i64>,
}

/// Params of the `network.beforeRequestSent`, `responseStarted`, `responseCompleted` and `fetchError` events.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NetworkEvent {
    pub context: Option<String>,
    pub request: RequestData,
    pub response: ResponseData,
    pub timestamp: u64,
}

/// Params of `browsingContext.userPromptOpened`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UserPromptOpened {
    pub context: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub message: String,
    pub default_value: Option<String>,
}

/// Result of `browsingContext.locateNodes`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LocateNodes {
    pub nodes: Vec<Value>,
}

/// Result of `network.getData`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct NetworkData {
    pub bytes: BytesValue,
}
