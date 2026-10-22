use serde_json::{Map, Value};

use crate::native::events::filter::resolve;
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
    pub engines: &'static [&'static str],
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
            engines: &["chrome"],
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

#[cfg(test)]
mod tests {
    use super::{lookup, project};
    use serde_json::json;

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
                assert_eq!(entry.engines, ["chrome"]);
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
