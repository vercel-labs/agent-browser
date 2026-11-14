use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

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
