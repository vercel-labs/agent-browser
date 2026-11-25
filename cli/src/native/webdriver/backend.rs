use std::collections::HashMap;

use serde_json::{json, Value};

use super::client::{WebDriverClient, ELEMENT_KEY};
use crate::native::element::{parse_ref, RefMap};

/// An element the browser handed out, with the iframe elements leading to its frame from the top.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ElementHandle {
    pub frames: Vec<String>,
    pub id: String,
}

/// Browser commands backed by a WebDriver session.
pub struct WebDriverBackend {
    pub(super) client: WebDriverClient,
    /// Durable ids of the elements snapshots have seen, which refs record as their node id.
    pub(super) elements: HashMap<ElementHandle, i64>,
}

impl WebDriverBackend {
    pub fn new(client: WebDriverClient) -> Self {
        Self {
            client,
            elements: HashMap::new(),
        }
    }

    /// The WebDriver session id, which scopes refs the way a CDP session does.
    pub fn session_key(&self) -> Result<String, String> {
        self.client
            .session_id_pub()
            .map(String::from)
            .ok_or("No active WebDriver session".to_string())
    }

    pub(super) fn durable_id(&mut self, handle: &ElementHandle) -> i64 {
        let next = self.elements.len() as i64 + 1;
        *self.elements.entry(handle.clone()).or_insert(next)
    }

    /// Switches from the top-level frame into the frame at the end of `frames`.
    pub(super) async fn enter(client: &WebDriverClient, frames: &[String]) -> Result<(), String> {
        for frame in frames {
            client.switch_to_frame(Some(frame)).await?;
        }
        Ok(())
    }

    /// Resolves `@eN`, `eN`, `ref=eN` or a CSS selector in the top-level frame to an element.
    async fn resolve(&self, ref_map: &RefMap, selector: &str) -> Result<ElementHandle, String> {
        let Some(ref_id) = parse_ref(selector) else {
            let id = self.client.find_element("css selector", selector).await?;
            return Ok(ElementHandle {
                frames: Vec::new(),
                id,
            });
        };
        let entry = ref_map
            .get(&ref_id)
            .ok_or_else(|| format!("Unknown ref: {ref_id}"))?;
        entry
            .backend_node_id
            .and_then(|id| self.elements.iter().find(|(_, v)| **v == id))
            .map(|(handle, _)| handle.clone())
            .ok_or_else(|| stale_ref(&ref_id))
    }

    /// Performs `action` on the element inside its frame, then returns to the top-level frame.
    /// A ref whose element or frame left the page reports that the page changed.
    async fn perform(
        &self,
        ref_map: &RefMap,
        selector: &str,
        action: Action<'_>,
    ) -> Result<(), String> {
        let handle = self.resolve(ref_map, selector).await?;
        let result = async {
            Self::enter(&self.client, &handle.frames).await?;
            if let Action::Fill(value) = action {
                self.client.clear_element(&handle.id).await?;
                return self.client.send_keys(&handle.id, value).await;
            }
            let element = json!({ ELEMENT_KEY: handle.id });
            self.client
                .execute_script(
                    "arguments[0].scrollIntoView({ block: 'center', inline: 'center' });",
                    vec![element.clone()],
                )
                .await?;
            let mut steps = vec![json!({
                "type": "pointerMove", "duration": 0, "origin": element, "x": 0, "y": 0
            })];
            if let Action::Click = action {
                steps.push(json!({ "type": "pointerDown", "button": 0 }));
                steps.push(json!({ "type": "pointerUp", "button": 0 }));
            }
            let pointer = json!({ "actions": [{
                "type": "pointer", "id": "mouse",
                "parameters": { "pointerType": "mouse" },
                "actions": steps,
            }] });
            self.client
                .execute_actions(&self.session_key()?, &pointer)
                .await
        }
        .await;
        if !handle.frames.is_empty() {
            let _ = self.client.switch_to_frame(None).await;
        }
        result.map_err(|error| match parse_ref(selector) {
            Some(ref_id) if is_stale(&error) => stale_ref(&ref_id),
            _ => error,
        })
    }

    pub async fn is_connection_alive(&self) -> bool {
        matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                self.client.get_window_handles(),
            )
            .await,
            Ok(Ok(_))
        )
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

    /// Evaluates `script` as an expression in the page's global scope, like CDP `Runtime.evaluate`.
    pub async fn evaluate(&self, script: &str) -> Result<Value, String> {
        self.client
            .execute_script("return (0, eval)(arguments[0]);", vec![json!(script)])
            .await
    }

    pub async fn screenshot(&self) -> Result<String, String> {
        self.client.screenshot().await
    }

    /// Scrolls the element into view and clicks its center with a pointer action, which also
    /// reaches elements in open shadow roots, where WebDriver Element Click fails.
    pub async fn click(&self, ref_map: &RefMap, selector: &str) -> Result<(), String> {
        self.perform(ref_map, selector, Action::Click).await
    }

    /// Scrolls the element into view and moves the pointer to its center.
    pub async fn hover(&self, ref_map: &RefMap, selector: &str) -> Result<(), String> {
        self.perform(ref_map, selector, Action::Hover).await
    }

    /// Clears the field and types `value` into it.
    pub async fn fill(&self, ref_map: &RefMap, selector: &str, value: &str) -> Result<(), String> {
        self.perform(ref_map, selector, Action::Fill(value)).await
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

/// An interaction with one element.
#[derive(Clone, Copy)]
enum Action<'a> {
    Click,
    Hover,
    Fill(&'a str),
}

/// Whether a WebDriver error means the element or its frame is no longer in the page.
fn is_stale(error: &str) -> bool {
    [
        "stale element reference",
        "no such element",
        "no such frame",
    ]
    .iter()
    .any(|code| error.starts_with(code))
}

fn stale_ref(ref_id: &str) -> String {
    format!("Stale ref @{ref_id}: the page changed since the snapshot. Run snapshot again and use a fresh ref.")
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
    "tab_keep",
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

    /// An element or frame that left the page must report a stale ref, while an ordinary
    /// interaction failure keeps its own message.
    #[test]
    fn test_only_gone_elements_count_as_stale() {
        assert!(is_stale("stale element reference: element is stale"));
        assert!(is_stale("no such element: element is not known"));
        assert!(is_stale("no such frame: frame was removed"));
        assert!(!is_stale(
            "element not interactable: could not bring element into view"
        ));
        assert!(!is_stale(
            "move target out of bounds: coordinates are out of bounds"
        ));
    }

    /// Two refs that resolved to the same id would target the wrong element on every action.
    #[test]
    fn test_durable_ids_are_stable_and_distinct() {
        let mut backend = WebDriverBackend::new(WebDriverClient::new(4444));
        let first = ElementHandle {
            frames: Vec::new(),
            id: "a".to_string(),
        };
        let second = ElementHandle {
            frames: vec!["frame".to_string()],
            id: "a".to_string(),
        };
        let first_id = backend.durable_id(&first);
        let second_id = backend.durable_id(&second);
        assert_ne!(first_id, second_id);
        assert_eq!(backend.durable_id(&first), first_id);
        assert_eq!(backend.durable_id(&second), second_id);
    }
}
