use serde::Serialize;
use serde_json::Value;

pub const ENVELOPE_VERSION: &str = "agent-browser.event.v1";

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventEnvelope {
    pub version: &'static str,
    pub meta: EventMeta,
    pub data: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventMeta {
    pub event_id: String,
    pub name: String,
    pub timestamp: String,
    pub cursor: String,
    pub subscription_id: String,
    pub session: String,
    pub tab: EventTab,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventTab {
    pub id: String,
    pub label: String,
    pub target_id: String,
}

impl EventEnvelope {
    pub fn new(
        name: &str,
        session: &str,
        tab_id: &str,
        tab_label: &str,
        target_id: &str,
        data: Value,
    ) -> Self {
        Self {
            version: ENVELOPE_VERSION,
            meta: EventMeta {
                event_id: uuid::Uuid::new_v4().to_string(),
                name: name.to_owned(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                cursor: String::new(),
                subscription_id: String::new(),
                session: session.to_owned(),
                tab: EventTab {
                    id: tab_id.to_owned(),
                    label: tab_label.to_owned(),
                    target_id: target_id.to_owned(),
                },
            },
            data,
        }
    }
}
