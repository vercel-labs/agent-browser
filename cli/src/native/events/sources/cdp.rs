use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::native::cdp::client::RawCdpMessage;
use crate::native::events::envelope::EventTab;
use crate::native::events::filter::{resolve, Matchers};
use crate::native::events::source::{EventLocation, EventSource, SourceSignal};
use crate::native::network::format_console_args;
use Source::{ConsoleText, Fallback, Pointer};

type Field = (&'static str, Source);

#[derive(Debug)]
enum Source {
    Pointer(&'static str),
    Fallback(&'static [&'static str]),
    ConsoleText,
}

#[derive(Debug)]
pub struct Entry {
    pub method: &'static str,
    pub policy_action: &'static str,
    fields: &'static [Field],
}

impl Entry {
    const fn new(
        method: &'static str,
        policy_action: &'static str,
        fields: &'static [Field],
    ) -> Self {
        Self {
            method,
            policy_action,
            fields,
        }
    }
}

const DIALOG_OPENED: &[Field] = &[
    ("type", Pointer("/type")),
    ("message", Pointer("/message")),
    ("url", Pointer("/url")),
    ("defaultPrompt", Pointer("/defaultPrompt")),
];
const FRAME_NAVIGATED: &[Field] = &[
    ("frameId", Pointer("/frame/id")),
    ("url", Pointer("/frame/url")),
    ("parentId", Pointer("/frame/parentId")),
];
const NAVIGATED_WITHIN_DOCUMENT: &[Field] =
    &[("frameId", Pointer("/frameId")), ("url", Pointer("/url"))];
const CONSOLE_CALLED: &[Field] = &[("level", Pointer("/type")), ("text", ConsoleText)];
const EXCEPTION_THROWN: &[Field] = &[
    (
        "text",
        Fallback(&[
            "/exceptionDetails/exception/description",
            "/exceptionDetails/text",
        ]),
    ),
    ("url", Pointer("/exceptionDetails/url")),
    ("lineNumber", Pointer("/exceptionDetails/lineNumber")),
    ("columnNumber", Pointer("/exceptionDetails/columnNumber")),
];
const TARGET_INFO: &[Field] = &[
    ("targetId", Pointer("/targetInfo/targetId")),
    ("type", Pointer("/targetInfo/type")),
    ("url", Pointer("/targetInfo/url")),
];
const TARGET_DESTROYED: &[Field] = &[("targetId", Pointer("/targetId"))];
const TARGET_CRASHED: &[Field] = &[
    ("targetId", Pointer("/targetId")),
    ("status", Pointer("/status")),
    ("errorCode", Pointer("/errorCode")),
];
const DOWNLOAD_BEGIN: &[Field] = &[
    ("guid", Pointer("/guid")),
    ("url", Pointer("/url")),
    ("suggestedFilename", Pointer("/suggestedFilename")),
    ("frameId", Pointer("/frameId")),
];
const DOWNLOAD_PROGRESS: &[Field] = &[
    ("guid", Pointer("/guid")),
    ("state", Pointer("/state")),
    ("receivedBytes", Pointer("/receivedBytes")),
    ("totalBytes", Pointer("/totalBytes")),
];

static ENTRIES: &[Entry] = &[
    Entry::new("Page.javascriptDialogOpening", "dialog", DIALOG_OPENED),
    Entry::new("Page.javascriptDialogClosed", "dialog", &[]),
    Entry::new("Page.frameNavigated", "url", FRAME_NAVIGATED),
    Entry::new(
        "Page.navigatedWithinDocument",
        "url",
        NAVIGATED_WITHIN_DOCUMENT,
    ),
    Entry::new("Runtime.consoleAPICalled", "console", CONSOLE_CALLED),
    Entry::new("Runtime.exceptionThrown", "errors", EXCEPTION_THROWN),
    Entry::new("Target.targetCreated", "tab_list", TARGET_INFO),
    Entry::new("Target.targetInfoChanged", "tab_list", TARGET_INFO),
    Entry::new("Target.targetDestroyed", "tab_list", TARGET_DESTROYED),
    Entry::new("Target.targetCrashed", "tab_list", TARGET_CRASHED),
    Entry::new("Browser.downloadWillBegin", "download", DOWNLOAD_BEGIN),
    Entry::new("Page.downloadWillBegin", "download", DOWNLOAD_BEGIN),
    Entry::new("Browser.downloadProgress", "download", DOWNLOAD_PROGRESS),
    Entry::new("Page.downloadProgress", "download", DOWNLOAD_PROGRESS),
];

/// Returns the allowlist entry for a CDP method, or an error naming the method.
pub fn lookup(method: &str) -> Result<&'static Entry, String> {
    ENTRIES.iter().find(|entry| entry.method == method).ok_or_else(|| {
        match method.split_once('.') {
            Some((domain @ ("Network" | "Fetch" | "Storage" | "DOMStorage" | "IO" | "Input" | "Debugger" | "Runtime"), _)) =>
                format!("CDP method '{method}' is not allowed: the {domain} domain is excluded from the event tap"),
            _ => format!("CDP method '{method}' is not on the event tap allowlist"),
        }
    })
}

/// Applies the entry's fixed projection and caps its string fields at `cap_bytes`.
pub fn project(entry: &Entry, params: &Value, cap_bytes: usize) -> Value {
    let mut object = Map::new();
    for (name, source) in entry.fields {
        let value = match source {
            Pointer(pointer) => resolve(params, pointer).cloned(),
            Fallback(pointers) => pointers
                .iter()
                .find_map(|pointer| resolve(params, pointer).cloned()),
            ConsoleText => params
                .get("args")
                .and_then(Value::as_array)
                .map(|args| Value::String(format_console_args(args))),
        };
        if let Some(value) = value {
            object.insert((*name).to_owned(), value);
        }
    }
    loop {
        let size = serde_json::to_vec(&object)
            .expect("projected fields always serialize")
            .len();
        if size <= cap_bytes {
            break;
        }
        let Some(Value::String(text)) = object
            .values_mut()
            .max_by_key(|value| value.as_str().map(str::len))
        else {
            break;
        };
        if text.is_empty() {
            break;
        }
        let excess = (size - cap_bytes).saturating_add(8);
        let mut end = text.len().saturating_sub(excess);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    Value::Object(object)
}

const MAX_EVENT_BYTES: usize = 16 * 1024;

pub(in crate::native::events) struct CdpMethodSource {
    entry: &'static Entry,
    session_id: String,
    target_id: String,
    targets: RwLock<HashMap<String, CdpTarget>>,
    matchers: Matchers,
}

pub(in crate::native::events) struct CdpTarget {
    pub(in crate::native::events) session_id: String,
    pub(in crate::native::events) tab: EventTab,
    pub(in crate::native::events) url: String,
}

impl CdpMethodSource {
    pub(in crate::native::events) fn new(
        entry: &'static Entry,
        session_id: String,
        target_id: String,
        targets: HashMap<String, CdpTarget>,
        matchers: Matchers,
    ) -> Self {
        Self {
            entry,
            session_id,
            target_id,
            targets: RwLock::new(targets),
            matchers,
        }
    }
}

#[async_trait]
impl EventSource for CdpMethodSource {
    fn policy_actions(&self) -> Vec<String> {
        vec![self.entry.policy_action.to_owned()]
    }

    fn target_id(&self) -> &str {
        &self.target_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn classify(&self, raw: &RawCdpMessage) -> Option<SourceSignal> {
        let event: Value = serde_json::from_str(&raw.text).ok()?;
        if event.get("method").and_then(Value::as_str) != Some(self.entry.method) {
            return None;
        }
        let params = event.get("params")?;
        let browser_event = self.entry.method.starts_with("Browser.");
        let target_event = self.entry.method.starts_with("Target.");
        let session_id = raw
            .session_id
            .as_deref()
            .or_else(|| event.get("sessionId").and_then(Value::as_str));
        let target_id = params
            .pointer("/targetInfo/targetId")
            .or_else(|| params.get("targetId"))
            .and_then(Value::as_str);
        let targets = self.targets.read().ok()?;
        let mapped = if target_event {
            target_id.and_then(|target_id| targets.get(target_id))
        } else {
            session_id.and_then(|session_id| {
                targets
                    .values()
                    .find(|target| target.session_id == session_id)
            })
        };
        if mapped.is_none() && !target_event && (!browser_event || session_id.is_some()) {
            return None;
        }
        let event_url = params
            .pointer("/targetInfo/url")
            .or_else(|| browser_event.then(|| params.get("url")).flatten())
            .and_then(Value::as_str);
        let location = mapped
            .map(|target| EventLocation {
                tab: Some(target.tab.clone()),
                url: event_url.unwrap_or(&target.url).to_owned(),
            })
            .or_else(|| {
                event_url.map(|url| EventLocation {
                    tab: None,
                    url: url.to_owned(),
                })
            });
        let data = project(self.entry, params, MAX_EVENT_BYTES);
        self.matchers.matches(&data).then_some(SourceSignal::Match {
            key: self.entry.method.to_owned(),
            name: self.entry.method.to_owned(),
            data,
            initial: false,
            location,
        })
    }

    fn update_url(&self, session_id: &str, url: &str) {
        if let Ok(mut targets) = self.targets.write() {
            if let Some(target) = targets
                .values_mut()
                .find(|target| target.session_id == session_id)
            {
                target.url = url.to_owned();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn projected_matchers_preserve_background_location_and_reject_other_sessions() {
        let source = CdpMethodSource::new(
            lookup("Page.javascriptDialogOpening").unwrap(),
            "active-session".into(),
            "active-target".into(),
            HashMap::from([(
                "background-target".into(),
                CdpTarget {
                    session_id: "background-session".into(),
                    tab: EventTab {
                        id: "t2".into(),
                        label: "background".into(),
                        target_id: "background-target".into(),
                    },
                    url: "https://background.example".into(),
                },
            )]),
            Matchers::parse(&["/type=alert".into()], &[]).unwrap(),
        );
        let classify = |session_id: &str, dialog_type: &str| {
            source.classify(&RawCdpMessage {
                text: json!({
                    "method": "Page.javascriptDialogOpening",
                    "params": {"type": dialog_type, "message": "fixture"}
                })
                .to_string(),
                session_id: Some(session_id.into()),
            })
        };

        assert!(classify("background-session", "confirm").is_none());
        assert!(classify("other-session", "alert").is_none());
        let Some(SourceSignal::Match {
            location: Some(location),
            data,
            ..
        }) = classify("background-session", "alert")
        else {
            panic!("background event should be classified");
        };

        assert_eq!(location.tab.as_ref().unwrap().id, "t2");
        assert_eq!(location.tab.as_ref().unwrap().label, "background");
        assert_eq!(location.url, "https://background.example");
        assert_eq!(data["message"], "fixture");
    }

    #[test]
    fn projection_policy_caps_and_rejections() {
        let cases = json!([
            [["Page.javascriptDialogOpening"], "dialog", 4096, {"type": "alert", "message": "hi", "url": "u", "defaultPrompt": "p", "secret": 1},
                {"type": "alert", "message": "hi", "url": "u", "defaultPrompt": "p"}],
            [["Page.javascriptDialogClosed"], "dialog", 4096, {"result": true}, {}],
            [["Page.javascriptDialogOpening"], "dialog", 4096, {"type": "confirm"}, {"type": "confirm"}],
            [["Runtime.consoleAPICalled"], "console", 4096, {"type": "info", "args": [{"type": "string", "value": "hello"}, {"type": "number", "value": 42}], "executionContextId": 3},
                {"level": "info", "text": "hello 42"}],
            [["Runtime.exceptionThrown"], "errors", 4096, {"exceptionDetails": {"exception": {"description": "boom"}, "text": "fallback", "url": "u", "lineNumber": 1, "columnNumber": 2, "secret": 3}},
                {"text": "boom", "url": "u", "lineNumber": 1, "columnNumber": 2}],
            [["Runtime.exceptionThrown"], "errors", 4096, {"exceptionDetails": {"text": "fallback"}}, {"text": "fallback"}],
            [["Runtime.exceptionThrown"], "errors", 4096, {"exceptionDetails": {"exception": {"description": null}, "text": "fallback"}}, {"text": null}],
            [["Page.frameNavigated"], "url", 4096, {"frame": {"id": "f", "url": "u", "parentId": "p", "loaderId": "secret"}}, {"frameId": "f", "url": "u", "parentId": "p"}],
            [["Page.navigatedWithinDocument"], "url", 4096, {"frameId": "f", "url": "u", "secret": 1}, {"frameId": "f", "url": "u"}],
            [["Target.targetCreated", "Target.targetInfoChanged"], "tab_list", 4096, {"targetInfo": {"targetId": "t", "type": "page", "url": "u", "title": "secret", "attached": true}},
                {"targetId": "t", "type": "page", "url": "u"}],
            [["Target.targetDestroyed"], "tab_list", 4096, {"targetId": "t", "secret": 1}, {"targetId": "t"}],
            [["Target.targetCrashed"], "tab_list", 4096, {"targetId": "t", "status": "crashed", "errorCode": 7, "secret": 1}, {"targetId": "t", "status": "crashed", "errorCode": 7}],
            [["Browser.downloadWillBegin", "Page.downloadWillBegin"], "download", 4096, {"guid": "g", "url": "u", "suggestedFilename": "s", "frameId": "f", "secret": 1},
                {"guid": "g", "url": "u", "suggestedFilename": "s", "frameId": "f"}],
            [["Browser.downloadProgress", "Page.downloadProgress"], "download", 4096, {"guid": "g", "state": "completed", "receivedBytes": 2, "totalBytes": 3, "secret": 1},
                {"guid": "g", "state": "completed", "receivedBytes": 2, "totalBytes": 3}],
            [["Runtime.consoleAPICalled"], "console", 64, {"type": "log", "args": [{"value": "x".repeat(5000)}]}, {"level": "log", "text": "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}],
            [["Runtime.consoleAPICalled"], "console", 64, {"type": "log", "args": [{"value": "é".repeat(5000)}]}, {"level": "log", "text": "ééééééééééééééé"}]
        ]);
        for case in cases.as_array().unwrap() {
            for method in case[0].as_array().unwrap() {
                let entry = lookup(method.as_str().unwrap()).unwrap();
                assert_eq!(entry.policy_action, case[1].as_str().unwrap());
                let cap = case[2].as_u64().unwrap() as usize;
                let projected = project(entry, &case[3], cap);
                assert_eq!(projected, case[4], "{method}");
                assert!(serde_json::to_vec(&projected).unwrap().len() <= cap);
                assert_eq!(project(entry, &json!({}), cap), json!({}), "{method}");
            }
        }
        for domain in [
            "Network",
            "Fetch",
            "Storage",
            "DOMStorage",
            "IO",
            "Input",
            "Debugger",
            "Runtime",
        ] {
            let method = format!("{domain}.enable");
            assert_eq!(lookup(&method).unwrap_err(), format!(
                "CDP method '{method}' is not allowed: the {domain} domain is excluded from the event tap"));
        }
        let method = "Page.frameAttached";
        assert_eq!(
            lookup(method).unwrap_err(),
            format!("CDP method '{method}' is not on the event tap allowlist")
        );
    }
}
