use std::collections::HashMap;

use serde_json::{json, Value};

use super::client::{WebDriverClient, ELEMENT_KEY};
use crate::native::element::{
    intercepted_error, not_actionable_error, parse_ref, RefMap, ACTIONABILITY_JS, BLOCKER_AT_JS,
};

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
            if let Action::Click = action {
                let preflight = self
                    .client
                    .execute_script(&click_preflight_js(), vec![element.clone()])
                    .await?;
                if let Some(reason) = preflight["reason"].as_str() {
                    return Err(not_actionable_error(selector, reason));
                }
                if let Some(blocker) = preflight["blocker"].as_str() {
                    return Err(intercepted_error(selector, blocker));
                }
                if !handle.frames.is_empty() {
                    if let (Some(x), Some(y), Some(vw), Some(vh)) = (
                        preflight["x"].as_f64(),
                        preflight["y"].as_f64(),
                        preflight["vw"].as_f64(),
                        preflight["vh"].as_f64(),
                    ) {
                        self.check_ancestor_blockers(&handle.frames, selector, x, y, vw, vh)
                            .await?;
                    }
                }
            } else {
                self.client
                    .execute_script(
                        "arguments[0].scrollIntoView({ block: 'center', inline: 'center' });",
                        vec![element.clone()],
                    )
                    .await?;
            }
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

    /// Evaluates `script` as an expression in the page's global scope, or falls back to a
    /// WebDriver function body when `script` is a statement list.
    pub async fn evaluate(&self, script: &str) -> Result<Value, String> {
        let trimmed = script.trim();
        let starts_with_return = trimmed.strip_prefix("return").is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
        });
        if starts_with_return {
            return self.client.execute_script(script, vec![]).await;
        }
        let expr = trimmed.strip_suffix(';').unwrap_or(trimmed).trim_end();
        let wrapped = format!("return (\n{expr}\n);");
        match self.client.execute_script(&wrapped, vec![]).await {
            Err(err) if is_syntax_error(&err) => self.client.execute_script(script, vec![]).await,
            other => other,
        }
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

    /// Maps an in-frame click point up through `frames` and rejects if an ancestor overlay covers it.
    async fn check_ancestor_blockers(
        &self,
        frames: &[String],
        selector: &str,
        mut x: f64,
        mut y: f64,
        mut vw: f64,
        mut vh: f64,
    ) -> Result<(), String> {
        let script = ancestor_blocker_js();
        for frame_id in frames.iter().rev() {
            self.client.switch_to_parent_frame().await?;
            let frame_el = json!({ ELEMENT_KEY: frame_id });
            let mapped = self
                .client
                .execute_script(
                    &script,
                    vec![frame_el, json!(x), json!(y), json!(vw), json!(vh)],
                )
                .await?;
            if let Some(blocker) = mapped["blocker"].as_str() {
                return Err(intercepted_error(selector, blocker));
            }
            let (Some(px), Some(py), Some(pvw), Some(pvh)) = (
                mapped["x"].as_f64(),
                mapped["y"].as_f64(),
                mapped["vw"].as_f64(),
                mapped["vh"].as_f64(),
            ) else {
                break;
            };
            x = px;
            y = py;
            vw = pvw;
            vh = pvh;
        }
        Self::enter(&self.client, frames).await
    }
}

/// Scrolls the target element into view and checks actionability and hit-test blockers.
fn click_preflight_js() -> String {
    format!(
        r#"
const el = arguments[0];
el.scrollIntoView({{ block: 'center', inline: 'center' }});
const rect = el.getClientRects()[0] || el.getBoundingClientRect();
const vw = window.innerWidth || document.documentElement.clientWidth;
const vh = window.innerHeight || document.documentElement.clientHeight;
const left = Math.max(0, Math.min(rect.left, rect.right));
const right = Math.min(vw, Math.max(rect.left, rect.right));
const top = Math.max(0, Math.min(rect.top, rect.bottom));
const bottom = Math.min(vh, Math.max(rect.top, rect.bottom));
const inView = rect.width > 0 && rect.height > 0 && right > left && bottom > top;
const notActionable = {ACTIONABILITY_JS};
const reason = notActionable(el, inView);
if (reason) return {{ reason }};
const x = Math.floor((left + right) / 2);
const y = Math.floor((top + bottom) / 2);
const blockerAt = {BLOCKER_AT_JS};
const root = el.getRootNode && el.getRootNode();
const blocker = blockerAt(document, el, x, y, false)
    || (root && root !== document && root.elementFromPoint ? blockerAt(root, el, x, y, false) : null);
return {{ blocker, x, y, vw, vh }};
"#
    )
}

/// Maps a point from a child frame into the parent frame containing `arguments[0]` and checks blockers.
fn ancestor_blocker_js() -> String {
    format!(
        r#"
const [fe, x, y, vw, vh] = arguments;
const r = fe.getBoundingClientRect();
const style = getComputedStyle(fe);
const scaleX = fe.offsetWidth > 0 ? r.width / fe.offsetWidth : 1;
const scaleY = fe.offsetHeight > 0 ? r.height / fe.offsetHeight : 1;
const borderLeft = parseFloat(style.borderLeftWidth) || 0;
const borderTop = parseFloat(style.borderTopWidth) || 0;
const padLeft = parseFloat(style.paddingLeft) || 0;
const padTop = parseFloat(style.paddingTop) || 0;
const padRight = parseFloat(style.paddingRight) || 0;
const padBottom = parseFloat(style.paddingBottom) || 0;
const contentLeft = r.x + (borderLeft + padLeft) * scaleX;
const contentTop = r.y + (borderTop + padTop) * scaleY;
const contentWidth = Math.max(0, fe.clientWidth - padLeft - padRight);
const contentHeight = Math.max(0, fe.clientHeight - padTop - padBottom);
const renderedWidth = contentWidth * scaleX;
const renderedHeight = contentHeight * scaleY;
const px = Math.floor(contentLeft + (vw > 0 ? x * (renderedWidth / vw) : 0));
const py = Math.floor(contentTop + (vh > 0 ? y * (renderedHeight / vh) : 0));
const pvw = window.innerWidth || document.documentElement.clientWidth;
const pvh = window.innerHeight || document.documentElement.clientHeight;
const blockerAt = {BLOCKER_AT_JS};
const root = fe.getRootNode && fe.getRootNode();
const blocker = blockerAt(document, fe, px, py, false)
    || (root && root !== document && root.elementFromPoint ? blockerAt(root, fe, px, py, false) : null);
return {{ blocker, x: px, y: py, vw: pvw, vh: pvh }};
"#
    )
}

fn is_syntax_error(error: &str) -> bool {
    error.starts_with("javascript error:") && error.contains("SyntaxError")
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

    async fn mock_webdriver_server(
        expected: Vec<(&'static str, &'static str)>,
    ) -> (u16, tokio::task::JoinHandle<Vec<(String, Value)>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (status, resp_body) in expected {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0_u8; 4096];
                loop {
                    let n = stream.read(&mut tmp).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    let text = String::from_utf8_lossy(&buf);
                    if let Some(hdr_end) = text.find("\r\n\r\n") {
                        let content_len = text[..hdr_end]
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        if buf.len() >= hdr_end + 4 + content_len {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&buf);
                let (headers, body) = text.split_once("\r\n\r\n").unwrap();
                let req_line = headers.lines().next().unwrap().to_string();
                let payload: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                requests.push((req_line, payload));
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp_body}",
                    resp_body.len()
                );
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            requests
        });
        (port, handle)
    }

    /// Evaluates expressions without JS `eval()` so CSP pages succeed, falling back on syntax error only.
    #[tokio::test]
    async fn test_evaluate_avoids_eval_and_falls_back_only_on_syntax_error() {
        let (port, server) = mock_webdriver_server(vec![
            ("200 OK", r#"{"value":2}"#),
            ("200 OK", r#"{"value":"Title"}"#),
            (
                "500 Internal Server Error",
                r#"{"value":{"error":"javascript error","message":"SyntaxError: Unexpected token"}}"#,
            ),
            ("200 OK", r#"{"value":42}"#),
            (
                "500 Internal Server Error",
                r#"{"value":{"error":"javascript error","message":"ReferenceError: missing is not defined"}}"#,
            ),
        ])
        .await;

        let backend = WebDriverBackend::new(WebDriverClient::new_with_session(port, "s1".into()));

        assert_eq!(backend.evaluate("1 + 1").await.unwrap(), json!(2));
        assert_eq!(
            backend.evaluate("return document.title").await.unwrap(),
            json!("Title")
        );
        assert_eq!(
            backend.evaluate("const x = 42; return x").await.unwrap(),
            json!(42)
        );
        let runtime_err = backend.evaluate("missing()").await.unwrap_err();
        assert!(runtime_err.contains("ReferenceError"));

        let reqs = server.await.unwrap();
        assert_eq!(reqs.len(), 5);
        assert_eq!(reqs[0].1["script"], "return (\n1 + 1\n);");
        assert!(!reqs[0].1["script"].as_str().unwrap().contains("eval"));
        assert_eq!(reqs[1].1["script"], "return document.title");
        assert_eq!(reqs[3].1["script"], "const x = 42; return x");
    }

    /// Covered elements return an intercepted-click error instead of dispatching pointer actions.
    #[tokio::test]
    async fn test_click_rejects_covered_element_before_actions() {
        let (port, server) = mock_webdriver_server(vec![
            (
                "200 OK",
                r#"{"value":{"element-6066-11e4-a52e-4f735466cecf":"el-1"}}"#,
            ),
            ("200 OK", r#"{"value":{"blocker":"div#modal"}}"#),
        ])
        .await;

        let backend = WebDriverBackend::new(WebDriverClient::new_with_session(port, "s1".into()));
        let err = backend
            .click(&RefMap::new(), "#btn")
            .await
            .expect_err("covered element click must fail");
        assert!(err.contains("covered by <div#modal>"), "got: {err}");

        let reqs = server.await.unwrap();
        assert_eq!(reqs.len(), 2);
        assert!(reqs[1].0.contains("/execute/sync"));
    }
}
