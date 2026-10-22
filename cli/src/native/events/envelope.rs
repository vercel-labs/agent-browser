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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::native::events::buffer::EventBuffer;

    use super::{EventEnvelope, ENVELOPE_VERSION};

    #[test]
    fn keeps_bus_metadata_separate_from_page_data() {
        let mut buffer = EventBuffer::new();
        let start = buffer.start_cursor();
        let cursor = buffer.append(EventEnvelope::new(
            "change",
            "session",
            "tab",
            "Tab",
            "target",
            json!({"text": "page"}),
        ));
        let value = serde_json::to_value(buffer.read(Some(&start), 1).unwrap().remove(0)).unwrap();

        assert_eq!(value["version"], ENVELOPE_VERSION);
        assert!(!value["meta"]["eventId"].as_str().unwrap().is_empty());
        assert_eq!(value["meta"]["name"], "change");
        assert!(!value["meta"]["timestamp"].as_str().unwrap().is_empty());
        assert_eq!(value["meta"]["cursor"], cursor);
        assert_eq!(
            value["meta"]["subscriptionId"],
            cursor.split(':').next().unwrap()
        );
        assert_eq!(value["meta"]["session"], "session");
        assert_eq!(value["meta"]["tab"]["id"], "tab");
        assert_eq!(value["meta"]["tab"]["label"], "Tab");
        assert_eq!(value["meta"]["tab"]["targetId"], "target");
        assert_eq!(value["data"]["text"], "page");
        assert!(value["meta"].get("text").is_none());
    }
}
