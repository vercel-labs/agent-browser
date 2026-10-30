use std::collections::VecDeque;

use serde_json::Value;

use super::{actions::TrackedRequest, network::MAX_EVENT_HISTORY_ENTRIES};

/// Request history retains the newest requests under the same count limit as
/// the native console and error buffers. This is a record-count limit only.
#[derive(Default)]
pub struct RequestHistory {
    entries: VecDeque<TrackedRequest>,
}

impl RequestHistory {
    pub fn push(&mut self, request: TrackedRequest) {
        if self.entries.len() == MAX_EVENT_HISTORY_ENTRIES {
            self.entries.pop_front();
        }
        self.entries.push_back(request);
    }

    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &TrackedRequest> {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn latest(&self, request_id: &str) -> Option<&TrackedRequest> {
        self.entries
            .iter()
            .rev()
            .find(|request| request.request_id == request_id)
    }

    pub fn update_response(
        &mut self,
        request_id: &str,
        status: Option<i64>,
        response_headers: Option<Value>,
        mime_type: Option<String>,
    ) -> bool {
        let Some(request) = self
            .entries
            .iter_mut()
            .rev()
            .find(|request| request.request_id == request_id)
        else {
            return false;
        };

        request.status = status;
        request.response_headers = response_headers;
        request.mime_type = mime_type;
        true
    }

    /// Replace the deque so clearing history releases its allocated storage.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn request(request_id: &str) -> TrackedRequest {
        TrackedRequest {
            url: format!("https://api.example/{request_id}"),
            method: "GET".to_string(),
            headers: json!({}),
            timestamp: 1,
            resource_type: "Fetch".to_string(),
            request_id: request_id.to_string(),
            post_data: None,
            status: None,
            response_headers: None,
            mime_type: None,
        }
    }

    #[test]
    fn default_limit_keeps_newest_records_and_ignores_late_evicted_updates() {
        let mut history = RequestHistory::default();
        for index in 0..=MAX_EVENT_HISTORY_ENTRIES {
            history.push(request(&index.to_string()));
        }

        assert_eq!(history.len(), MAX_EVENT_HISTORY_ENTRIES);
        assert_eq!(history.iter().next().unwrap().request_id, "1");
        assert_eq!(history.iter().next_back().unwrap().request_id, "1000");
        assert!(!history.update_response("0", Some(200), None, None));
        assert!(history.update_response("1", Some(201), None, None));
        assert_eq!(history.latest("1").unwrap().status, Some(201));
    }

    #[test]
    fn duplicate_ids_update_and_select_the_latest_retained_record() {
        let mut history = RequestHistory::default();
        history.push(request("same"));
        let mut latest = request("same");
        latest.url = "https://api.example/latest".to_string();
        history.push(latest);

        assert_eq!(
            history.latest("same").unwrap().url,
            "https://api.example/latest"
        );
        assert!(history.update_response(
            "same",
            Some(202),
            Some(json!({"x-result": "latest"})),
            Some("application/json".to_string()),
        ));
        let records: Vec<_> = history.iter().collect();
        assert_eq!(records[0].status, None);
        assert_eq!(records[1].status, Some(202));
    }

    #[test]
    fn clear_releases_and_refills_history() {
        let mut history = RequestHistory::default();
        for index in 0..MAX_EVENT_HISTORY_ENTRIES {
            history.push(request(&index.to_string()));
        }
        history.clear();
        assert!(history.is_empty());
        history.push(request("fresh"));
        assert_eq!(history.len(), 1);
        assert_eq!(history.iter().next().unwrap().request_id, "fresh");
    }
}
