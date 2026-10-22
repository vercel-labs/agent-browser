//! The passive CDP method allowlist for the event tap and its safe field projections.

use serde_json::{Map, Value};

use crate::native::events::filter::resolve;
use crate::native::network::format_console_args;

const CHROME: &[&str] = &["chrome"];

#[derive(Debug)]
struct Field {
    name: &'static str,
    source: Source,
}

#[derive(Debug)]
enum Source {
    Pointer(&'static str),
    Fallback(&'static [&'static str]),
    ConsoleText,
}

/// One allowlisted CDP method, the policy action exposing the same data, and its engines.
#[derive(Debug)]
pub struct Entry {
    pub method: &'static str,
    pub policy_action: &'static str,
    pub engines: &'static [&'static str],
    fields: &'static [Field],
}

const DIALOG_OPENED: &[Field] = &[
    Field {
        name: "type",
        source: Source::Pointer("/type"),
    },
    Field {
        name: "message",
        source: Source::Pointer("/message"),
    },
    Field {
        name: "url",
        source: Source::Pointer("/url"),
    },
    Field {
        name: "defaultPrompt",
        source: Source::Pointer("/defaultPrompt"),
    },
];

const FRAME_NAVIGATED: &[Field] = &[
    Field {
        name: "frameId",
        source: Source::Pointer("/frame/id"),
    },
    Field {
        name: "url",
        source: Source::Pointer("/frame/url"),
    },
    Field {
        name: "parentId",
        source: Source::Pointer("/frame/parentId"),
    },
];

const NAVIGATED_WITHIN_DOCUMENT: &[Field] = &[
    Field {
        name: "frameId",
        source: Source::Pointer("/frameId"),
    },
    Field {
        name: "url",
        source: Source::Pointer("/url"),
    },
];

const CONSOLE_CALLED: &[Field] = &[
    Field {
        name: "level",
        source: Source::Pointer("/type"),
    },
    Field {
        name: "text",
        source: Source::ConsoleText,
    },
];

const EXCEPTION_THROWN: &[Field] = &[
    Field {
        name: "text",
        source: Source::Fallback(&[
            "/exceptionDetails/exception/description",
            "/exceptionDetails/text",
        ]),
    },
    Field {
        name: "url",
        source: Source::Pointer("/exceptionDetails/url"),
    },
    Field {
        name: "lineNumber",
        source: Source::Pointer("/exceptionDetails/lineNumber"),
    },
    Field {
        name: "columnNumber",
        source: Source::Pointer("/exceptionDetails/columnNumber"),
    },
];

const TARGET_INFO: &[Field] = &[
    Field {
        name: "targetId",
        source: Source::Pointer("/targetInfo/targetId"),
    },
    Field {
        name: "type",
        source: Source::Pointer("/targetInfo/type"),
    },
    Field {
        name: "url",
        source: Source::Pointer("/targetInfo/url"),
    },
];

const TARGET_DESTROYED: &[Field] = &[Field {
    name: "targetId",
    source: Source::Pointer("/targetId"),
}];

const TARGET_CRASHED: &[Field] = &[
    Field {
        name: "targetId",
        source: Source::Pointer("/targetId"),
    },
    Field {
        name: "status",
        source: Source::Pointer("/status"),
    },
    Field {
        name: "errorCode",
        source: Source::Pointer("/errorCode"),
    },
];

const DOWNLOAD_BEGIN: &[Field] = &[
    Field {
        name: "guid",
        source: Source::Pointer("/guid"),
    },
    Field {
        name: "url",
        source: Source::Pointer("/url"),
    },
    Field {
        name: "suggestedFilename",
        source: Source::Pointer("/suggestedFilename"),
    },
    Field {
        name: "frameId",
        source: Source::Pointer("/frameId"),
    },
];

const DOWNLOAD_PROGRESS: &[Field] = &[
    Field {
        name: "guid",
        source: Source::Pointer("/guid"),
    },
    Field {
        name: "state",
        source: Source::Pointer("/state"),
    },
    Field {
        name: "receivedBytes",
        source: Source::Pointer("/receivedBytes"),
    },
    Field {
        name: "totalBytes",
        source: Source::Pointer("/totalBytes"),
    },
];

static ENTRIES: &[Entry] = &[
    Entry {
        method: "Page.javascriptDialogOpening",
        policy_action: "dialog",
        engines: CHROME,
        fields: DIALOG_OPENED,
    },
    Entry {
        method: "Page.javascriptDialogClosed",
        policy_action: "dialog",
        engines: CHROME,
        fields: &[],
    },
    Entry {
        method: "Page.frameNavigated",
        policy_action: "url",
        engines: CHROME,
        fields: FRAME_NAVIGATED,
    },
    Entry {
        method: "Page.navigatedWithinDocument",
        policy_action: "url",
        engines: CHROME,
        fields: NAVIGATED_WITHIN_DOCUMENT,
    },
    Entry {
        method: "Runtime.consoleAPICalled",
        policy_action: "console",
        engines: CHROME,
        fields: CONSOLE_CALLED,
    },
    Entry {
        method: "Runtime.exceptionThrown",
        policy_action: "errors",
        engines: CHROME,
        fields: EXCEPTION_THROWN,
    },
    Entry {
        method: "Target.targetCreated",
        policy_action: "tab_list",
        engines: CHROME,
        fields: TARGET_INFO,
    },
    Entry {
        method: "Target.targetInfoChanged",
        policy_action: "tab_list",
        engines: CHROME,
        fields: TARGET_INFO,
    },
    Entry {
        method: "Target.targetDestroyed",
        policy_action: "tab_list",
        engines: CHROME,
        fields: TARGET_DESTROYED,
    },
    Entry {
        method: "Target.targetCrashed",
        policy_action: "tab_list",
        engines: CHROME,
        fields: TARGET_CRASHED,
    },
    Entry {
        method: "Browser.downloadWillBegin",
        policy_action: "download",
        engines: CHROME,
        fields: DOWNLOAD_BEGIN,
    },
    Entry {
        method: "Page.downloadWillBegin",
        policy_action: "download",
        engines: CHROME,
        fields: DOWNLOAD_BEGIN,
    },
    Entry {
        method: "Browser.downloadProgress",
        policy_action: "download",
        engines: CHROME,
        fields: DOWNLOAD_PROGRESS,
    },
    Entry {
        method: "Page.downloadProgress",
        policy_action: "download",
        engines: CHROME,
        fields: DOWNLOAD_PROGRESS,
    },
];

const EXCLUDED_DOMAINS: &[&str] = &[
    "Network",
    "Fetch",
    "Storage",
    "DOMStorage",
    "IO",
    "Input",
    "Debugger",
    "Runtime",
];

/// Returns the allowlist entry for a CDP method, or an error naming the method.
pub fn lookup(method: &str) -> Result<&'static Entry, String> {
    if let Some(entry) = ENTRIES.iter().find(|entry| entry.method == method) {
        return Ok(entry);
    }
    if let Some(domain) = excluded_domain(method) {
        return Err(format!(
            "CDP method '{method}' is not allowed: the {domain} domain is excluded from the event tap"
        ));
    }
    Err(format!(
        "CDP method '{method}' is not on the event tap allowlist"
    ))
}

/// Applies the entry's fixed projection and caps its string fields at `cap_bytes`.
pub fn project(entry: &Entry, params: &Value, cap_bytes: usize) -> Value {
    let mut object = Map::new();
    for field in entry.fields {
        let value = match field.source {
            Source::Pointer(pointer) => resolve(params, pointer).cloned(),
            Source::Fallback(pointers) => pointers
                .iter()
                .find_map(|pointer| resolve(params, pointer).cloned()),
            Source::ConsoleText => params
                .get("args")
                .and_then(Value::as_array)
                .map(|args| Value::String(format_console_args(args))),
        };
        if let Some(value) = value {
            object.insert(field.name.to_owned(), value);
        }
    }
    cap(object, cap_bytes)
}

fn excluded_domain(method: &str) -> Option<&'static str> {
    let (domain, _) = method.split_once('.')?;
    EXCLUDED_DOMAINS
        .iter()
        .copied()
        .find(|candidate| candidate == &domain)
}

fn cap(mut object: Map<String, Value>, cap_bytes: usize) -> Value {
    loop {
        let size = serde_json::to_vec(&object)
            .expect("projected fields always serialize")
            .len();
        if size <= cap_bytes {
            break;
        }
        let excess = size - cap_bytes;
        let Some((key, len)) = object
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.len())))
            .max_by_key(|(_, len)| *len)
        else {
            break;
        };
        if len == 0 {
            break;
        }
        let keep = len.saturating_sub(excess.saturating_add(8));
        let truncated = object
            .get(&key)
            .and_then(Value::as_str)
            .map(|text| truncate(text, keep))
            .unwrap_or_default();
        object.insert(key, Value::String(truncated));
    }
    Value::Object(object)
}

fn truncate(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{lookup, project, Entry, ENTRIES, EXCLUDED_DOMAINS};

    #[test]
    fn every_allowlisted_method_looks_up() {
        for entry in ENTRIES {
            assert_eq!(lookup(entry.method).unwrap().method, entry.method);
        }
        assert_eq!(
            ENTRIES.len(),
            ENTRIES
                .iter()
                .map(|entry| entry.method)
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }

    #[test]
    fn entries_declare_chrome_and_an_existing_policy_action() {
        for entry in ENTRIES {
            assert_eq!(entry.engines, ["chrome"]);
            assert!(!entry.policy_action.is_empty());
        }
    }

    #[test]
    fn excluded_domains_are_refused_by_name() {
        for domain in EXCLUDED_DOMAINS {
            let method = format!("{domain}.enable");
            let error = lookup(&method).unwrap_err();
            assert!(error.contains(domain), "{error} should name {domain}");
            assert!(error.contains(&method));
        }
        let runtime = lookup("Runtime.evaluate").unwrap_err();
        assert!(runtime.contains("Runtime domain"));
        assert!(lookup("Network.requestWillBeSent")
            .unwrap_err()
            .contains("Network domain"));
    }

    #[test]
    fn an_unknown_method_is_refused_with_its_name() {
        let error = lookup("Page.frameAttached").unwrap_err();
        assert!(error.contains("Page.frameAttached"));
        assert!(error.contains("allowlist"));
    }

    #[test]
    fn projection_drops_fields_outside_the_table() {
        let entry = lookup("Page.javascriptDialogOpening").unwrap();
        let projected = project(
            entry,
            &json!({
                "type": "alert",
                "message": "Hello",
                "url": "https://example.com",
                "hasBrowserHandler": true,
                "secret": "page data"
            }),
            4096,
        );
        assert_eq!(
            projected,
            json!({
                "type": "alert",
                "message": "Hello",
                "url": "https://example.com"
            })
        );
    }

    #[test]
    fn projection_skips_fields_the_payload_does_not_have() {
        let entry = lookup("Page.javascriptDialogClosed").unwrap();
        assert_eq!(project(entry, &json!({"result": true}), 4096), json!({}));

        let dialog = lookup("Page.javascriptDialogOpening").unwrap();
        assert_eq!(
            project(dialog, &json!({"type": "confirm"}), 4096),
            json!({"type": "confirm"})
        );
    }

    #[test]
    fn projection_derives_console_level_and_text() {
        let entry = lookup("Runtime.consoleAPICalled").unwrap();
        let projected = project(
            entry,
            &json!({
                "type": "info",
                "args": [
                    {"type": "string", "value": "hello"},
                    {"type": "number", "value": 42}
                ],
                "executionContextId": 3
            }),
            4096,
        );
        assert_eq!(projected, json!({"level": "info", "text": "hello 42"}));
    }

    #[test]
    fn exception_text_prefers_the_description() {
        let entry: &Entry = lookup("Runtime.exceptionThrown").unwrap();
        let described = project(
            entry,
            &json!({"exceptionDetails": {
                "exception": {"description": "Error: boom"},
                "text": "Uncaught"
            }}),
            4096,
        );
        assert_eq!(described["text"], "Error: boom");

        let plain = project(
            entry,
            &json!({"exceptionDetails": {"text": "Uncaught"}}),
            4096,
        );
        assert_eq!(plain["text"], "Uncaught");
    }

    #[test]
    fn navigation_and_target_projections_keep_only_safe_fields() {
        let navigation = lookup("Page.frameNavigated").unwrap();
        assert_eq!(
            project(
                navigation,
                &json!({
                    "frame": {
                        "id": "FRAME",
                        "url": "https://example.com/",
                        "loaderId": "LOADER",
                        "securityOrigin": "https://example.com"
                    },
                    "type": "Navigation"
                }),
                4096
            ),
            json!({"frameId": "FRAME", "url": "https://example.com/"})
        );

        let target = lookup("Target.targetCreated").unwrap();
        assert_eq!(
            project(
                target,
                &json!({"targetInfo": {
                    "targetId": "TARGET",
                    "type": "page",
                    "url": "https://example.com/",
                    "title": "Example",
                    "attached": true
                }}),
                4096
            ),
            json!({"targetId": "TARGET", "type": "page", "url": "https://example.com/"})
        );
    }

    #[test]
    fn download_projection_keeps_the_url_and_suggested_filename() {
        let entry = lookup("Browser.downloadWillBegin").unwrap();
        let projected = project(
            entry,
            &json!({
                "frameId": "FRAME",
                "guid": "GUID",
                "url": "https://example.com/file.zip",
                "suggestedFilename": "file.zip",
                "secret": "page data"
            }),
            4096,
        );
        assert_eq!(
            projected,
            json!({
                "guid": "GUID",
                "url": "https://example.com/file.zip",
                "suggestedFilename": "file.zip",
                "frameId": "FRAME"
            })
        );
    }

    #[test]
    fn long_string_fields_are_capped() {
        let entry = lookup("Runtime.consoleAPICalled").unwrap();
        let projected = project(
            entry,
            &json!({
                "type": "log",
                "args": [{"type": "string", "value": "x".repeat(5000)}]
            }),
            64,
        );
        let serialized = serde_json::to_string(&projected).unwrap();
        assert!(serialized.len() <= 64, "{serialized}");
        let text = projected["text"].as_str().unwrap();
        assert!(!text.is_empty());
        assert!(text.len() < 5000);
    }

    #[test]
    fn capping_keeps_utf8_boundaries() {
        let entry = lookup("Page.javascriptDialogOpening").unwrap();
        let message = "é".repeat(100);
        let projected = project(entry, &json!({"type": "alert", "message": message}), 60);
        let text = projected["message"].as_str().unwrap();
        assert!(text.chars().all(|character| character == 'é'));
        assert!(serde_json::to_string(&projected).unwrap().len() <= 60);
    }
}
